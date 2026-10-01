//! Pre-flight: absolutize, resolve subst drives, classify, stat, and filter.
//!
//! Everything here happens before a single COM call, so a path that cannot
//! possibly be recycled is refused with a precise, human-authored reason
//! instead of an HRESULT.

// Opt in to `unsafe` for this file only (QueryDosDeviceW); the crate root
// denies it.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::report::{ErrorCode, SkipReason};

/// The longest path string classic Win32 accepts: `MAX_PATH` (260) minus the
/// NUL terminator. The shell handles longer paths poorly and the Recycle Bin
/// will not take them, so refuse up front.
pub const MAX_PATH_CHARS: usize = 259;

/// A subst chain longer than this is a cycle (`subst Y: X:\` + `subst X: Y:\`).
/// The C reference loops forever here.
const MAX_SUBST_DEPTH: usize = 16;

#[derive(Debug, Clone)]
pub struct PreparedPath {
    /// Position in the original argument list; the sink is seeded with this.
    pub index: usize,
    /// Exactly what the user typed. Every message quotes this, not `resolved`,
    /// so a subst drive reads back as `Y:\file.txt`.
    pub input: String,
    pub resolved: String,
    /// Case-folded `resolved`: the identity used for dedup and nesting, so each
    /// path is folded once rather than on every comparison.
    pub key: String,
}

#[derive(Debug, Clone)]
pub enum Prepared {
    Ready(PreparedPath),
    Skip {
        input: String,
        reason: SkipReason,
        container: String,
    },
    Fail {
        input: String,
        resolved: Option<String>,
        code: ErrorCode,
        message: String,
    },
}

/// True for network paths, including the `\\?\UNC\server\share` form.
/// `\\.\` and `\\?\C:\` are device/long-path prefixes, not UNC.
pub fn is_unc(path: &str) -> bool {
    let chars: Vec<char> = path.chars().collect();
    let sep = |c: char| c == '\\' || c == '/';
    if chars.len() < 3 || !sep(chars[0]) || !sep(chars[1]) {
        return false;
    }
    let rest: String = chars[2..].iter().collect::<String>().to_ascii_lowercase();
    if rest.starts_with('?') || rest.starts_with('.') {
        let after = rest[1..].trim_start_matches(['\\', '/']);
        return after.starts_with("unc\\") || after.starts_with("unc/");
    }
    true
}

pub fn is_too_long(path: &str) -> bool {
    path.chars().count() > MAX_PATH_CHARS
}

/// A drive root in any spelling: `C:`, `C:\`, `C:/`, `\\?\C:\`.
fn is_drive_root(path: &str) -> bool {
    let rest = path.strip_prefix("\\\\?\\").unwrap_or(path);
    // `drive_letter` guarantees two ASCII bytes, so `rest[2..]` is a boundary.
    drive_letter(Path::new(rest)).is_some() && rest[2..].chars().all(|c| c == '\\' || c == '/')
}

/// Strip trailing separators so `build` and `build\` are the same path, and so
/// `symlink_metadata` does not follow a link written as `link\`. A drive root
/// keeps its separator: `C:` alone means "the current directory on C:".
fn trim_trailing_separators(path: &str) -> String {
    let trimmed = path.trim_end_matches(['\\', '/']);
    if is_drive_root(trimmed) {
        format!("{trimmed}\\")
    } else if trimmed.is_empty() {
        path.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Case-insensitive key for dedup and nesting. Windows paths are
/// case-insensitive.
fn path_key(path: &str) -> String {
    path.to_lowercase()
}

#[derive(Debug)]
pub enum SubstError {
    Cycle,
}

/// Follow a `subst` chain to the real path.
///
/// `QueryDosDeviceW` answers `\??\C:\some\dir` for a subst drive and
/// `\Device\HarddiskVolumeN` for a real volume, so only a `\??\` target is a
/// substitution worth rewriting.
pub fn resolve_subst(path: &Path) -> Result<PathBuf, SubstError> {
    let mut current = path.to_path_buf();
    let mut seen: Vec<char> = Vec::new();

    for _ in 0..MAX_SUBST_DEPTH {
        let Some(letter) = drive_letter(&current) else {
            return Ok(current);
        };
        if seen.contains(&letter) {
            return Err(SubstError::Cycle);
        }
        seen.push(letter);

        let Some(target) = query_dos_device(letter)
            .as_deref()
            .and_then(dos_target_to_path)
        else {
            return Ok(current);
        };

        let text = current.to_string_lossy().into_owned();
        let remainder = text[2..].trim_start_matches(['\\', '/']);
        current = PathBuf::from(join_subst(&target, remainder));
    }

    Err(SubstError::Cycle)
}

/// Append the part of the path after the drive letter to a subst target.
///
/// Built as a string because `PathBuf::push` onto a bare `G:` (what trimming
/// `G:\` leaves) yields the drive-relative `G:dev\x.txt`.
fn join_subst(target: &str, remainder: &str) -> String {
    let base = target.trim_end_matches('\\');
    if !remainder.is_empty() {
        format!("{base}\\{remainder}")
    } else if is_drive_root(base) {
        format!("{base}\\")
    } else {
        base.to_string()
    }
}

/// Translate a `QueryDosDeviceW` answer into a usable path, or `None` when the
/// drive letter is a real volume rather than a mapping.
///
/// Substs answer `\??\C:\dir`; `net use` drives answer `\??\UNC\server\share`,
/// which has to become `\\server\share` or it turns into a bogus relative path
/// and the UNC refusal never fires. Real volumes answer
/// `\Device\HarddiskVolumeN` and are left alone.
fn dos_target_to_path(target: &str) -> Option<String> {
    let rest = target.strip_prefix("\\??\\")?;
    if rest
        .get(..4)
        .is_some_and(|p| p.eq_ignore_ascii_case("UNC\\"))
    {
        return Some(format!("\\\\{}", &rest[4..]));
    }
    Some(rest.to_string())
}

fn drive_letter(path: &Path) -> Option<char> {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && (bytes[0] as char).is_ascii_alphabetic() {
        Some((bytes[0] as char).to_ascii_uppercase())
    } else {
        None
    }
}

fn query_dos_device(letter: char) -> Option<String> {
    use windows_core::{HSTRING, PWSTR};

    use crate::bindings::QueryDosDeviceW;

    let name = HSTRING::from(format!("{letter}:"));
    let mut buffer = vec![0u16; 1024];
    let capacity = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    // SAFETY: `name` outlives the call; `buffer` is live for the call and
    // `capacity` is its exact length in UTF-16 units.
    let len =
        unsafe { QueryDosDeviceW(&name, Some(PWSTR(buffer.as_mut_ptr())), capacity) } as usize;
    if len == 0 {
        return None;
    }
    // The result is a NUL-separated multi-string; only the first entry is the
    // active mapping.
    let end = buffer[..len.min(buffer.len())]
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(len.min(buffer.len()));
    Some(String::from_utf16_lossy(&buffer[..end]))
}

/// Run every argument through the pre-flight pipeline, preserving input order.
pub fn prepare(inputs: &[String]) -> Vec<Prepared> {
    let mut staged: Vec<Prepared> = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| prepare_one(index, input))
        .collect();

    filter_duplicates(&mut staged);
    filter_nested(&mut staged);
    staged
}

fn prepare_one(index: usize, input: &str) -> Prepared {
    let fail = |code: ErrorCode, message: &str, resolved: Option<String>| Prepared::Fail {
        input: input.to_string(),
        resolved,
        code,
        message: message.to_string(),
    };

    if input.trim().is_empty() {
        return fail(ErrorCode::EmptyPath, "empty path argument", None);
    }

    let absolute = match std::path::absolute(input) {
        Ok(p) => p,
        Err(e) => return fail(ErrorCode::NotFound, &e.to_string(), None),
    };

    let resolved = match resolve_subst(&absolute) {
        Ok(p) => p,
        Err(SubstError::Cycle) => {
            return fail(
                ErrorCode::SubstCycle,
                "the subst drive mapping for this path is circular",
                None,
            );
        }
    };
    let resolved_text = trim_trailing_separators(&resolved.to_string_lossy());

    if is_unc(&resolved_text) || is_unc(input) {
        return fail(
            ErrorCode::UncNoRecycleBin,
            "network locations have no Recycle Bin",
            Some(resolved_text),
        );
    }

    // `input` is checked too: `C:` absolutizes to the current directory on C:,
    // which would otherwise hide that the user typed a bare drive.
    if is_drive_root(&resolved_text) || is_drive_root(input.trim()) {
        return fail(
            ErrorCode::DriveRoot,
            "a drive root cannot be moved to the Recycle Bin",
            Some(resolved_text),
        );
    }

    if is_too_long(&resolved_text) {
        return fail(
            ErrorCode::PathTooLong,
            "path exceeds 259 characters, which the Recycle Bin cannot accept",
            Some(resolved_text),
        );
    }

    // Deliberately `symlink_metadata`, never `canonicalize`: canonicalize
    // follows symlinks (we would recycle the target instead of the link) and
    // returns `\\?\` paths the shell handles poorly.
    if let Err(e) = std::fs::symlink_metadata(&resolved) {
        // An unmatched glob reaches us verbatim (the shell, or `wild`, leaves it
        // alone) and Windows rejects the wildcard as an invalid filename. Report
        // that as "not found" rather than an opaque shell error — and never
        // silently, which is what most trash tools do.
        let unmatched_pattern = input.contains('*') || input.contains('?');
        let (code, message) = match e.kind() {
            _ if unmatched_pattern => (ErrorCode::NotFound, "no files match this pattern"),
            std::io::ErrorKind::NotFound => (ErrorCode::NotFound, "no such file or directory"),
            std::io::ErrorKind::PermissionDenied => (ErrorCode::AccessDenied, "access denied"),
            std::io::ErrorKind::InvalidFilename | std::io::ErrorKind::InvalidInput => {
                (ErrorCode::NotFound, "no such file or directory")
            }
            _ => (ErrorCode::ShellError, "cannot read path"),
        };
        return fail(code, message, Some(resolved_text));
    }

    Prepared::Ready(PreparedPath {
        index,
        input: input.to_string(),
        key: path_key(&resolved_text),
        resolved: resolved_text,
    })
}

fn filter_duplicates(staged: &mut [Prepared]) {
    // key -> input of the first argument with that path
    let mut seen: HashMap<String, String> = HashMap::new();
    for entry in staged.iter_mut() {
        let Prepared::Ready(p) = entry else { continue };
        if let Some(first) = seen.get(&p.key) {
            *entry = Prepared::Skip {
                input: p.input.clone(),
                reason: SkipReason::Duplicate,
                container: first.clone(),
            };
        } else {
            seen.insert(p.key.clone(), p.input.clone());
        }
    }
}

/// Drop paths already covered by another argument. Recycling the container
/// first would otherwise make the nested path report a spurious `NOT_FOUND`.
/// The skip is reported, not silent.
///
/// Each path looks up its own ancestors, so the cost is paths x depth. Walking
/// ancestors (rather than comparing strings) keeps the match component-wise:
/// `C:\foobar` is not inside `C:\foo`.
fn filter_nested(staged: &mut [Prepared]) {
    // key -> (input position, input)
    let ready: HashMap<String, (usize, String)> = staged
        .iter()
        .filter_map(|e| match e {
            Prepared::Ready(p) => Some((p.key.clone(), (p.index, p.input.clone()))),
            _ => None,
        })
        .collect();

    for entry in staged.iter_mut() {
        let Prepared::Ready(p) = entry else { continue };
        // The earliest argument wins when several ancestors are also arguments.
        let container = Path::new(&p.key)
            .ancestors()
            .skip(1)
            .filter_map(|ancestor| ready.get(ancestor.to_str()?))
            .min_by_key(|(position, _)| *position);
        if let Some((_, container_input)) = container {
            *entry = Prepared::Skip {
                input: p.input.clone(),
                reason: SkipReason::NestedIn,
                container: container_input.clone(),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_volumes_are_not_mappings() {
        assert_eq!(dos_target_to_path("\\Device\\HarddiskVolume3"), None);
    }

    #[test]
    fn subst_targets_resolve_to_their_directory() {
        assert_eq!(
            dos_target_to_path("\\??\\C:\\some\\dir").as_deref(),
            Some("C:\\some\\dir")
        );
    }

    #[test]
    fn mapped_network_drives_resolve_back_to_unc() {
        let resolved = dos_target_to_path("\\??\\UNC\\srv\\share").unwrap();
        assert_eq!(resolved, "\\\\srv\\share");
        assert!(is_unc(&resolved));
    }

    #[test]
    fn detects_unc_paths() {
        assert!(is_unc("\\\\srv\\share"));
        assert!(is_unc("\\\\srv\\share\\file.txt"));
        assert!(is_unc("//srv/share"));
        assert!(is_unc("\\\\?\\UNC\\srv\\share"));
    }

    #[test]
    fn local_and_long_path_prefixes_are_not_unc() {
        assert!(!is_unc("C:\\dir\\file.txt"));
        assert!(!is_unc("\\\\?\\C:\\dir"));
        assert!(!is_unc("\\\\.\\PhysicalDrive0"));
        assert!(!is_unc("\\single"));
    }

    #[test]
    fn long_path_boundary_is_259() {
        let at_limit = format!("C:\\{}", "a".repeat(MAX_PATH_CHARS - 3));
        assert_eq!(at_limit.chars().count(), 259);
        assert!(!is_too_long(&at_limit));
        assert!(is_too_long(&format!("{at_limit}b")));
    }

    #[test]
    fn empty_and_whitespace_arguments_are_rejected() {
        for arg in ["", "   ", "\t"] {
            match prepare_one(0, arg) {
                Prepared::Fail { code, .. } => assert_eq!(code, ErrorCode::EmptyPath),
                other => panic!("expected EMPTY_PATH, got {other:?}"),
            }
        }
    }

    fn ready(index: usize, input: &str, resolved: &str) -> Prepared {
        Prepared::Ready(PreparedPath {
            index,
            input: input.into(),
            key: path_key(resolved),
            resolved: resolved.into(),
        })
    }

    /// `Some((reason, container))` for a skip, `None` for a still-ready entry.
    fn skip_of(entry: &Prepared) -> Option<(SkipReason, &str)> {
        match entry {
            Prepared::Skip {
                reason, container, ..
            } => Some((*reason, container)),
            _ => None,
        }
    }

    #[test]
    fn duplicates_are_skipped_not_trashed_twice() {
        let mut staged = vec![
            ready(0, "C:\\a.txt", "C:\\a.txt"),
            ready(1, "c:\\A.TXT", "c:\\A.TXT"),
        ];
        filter_duplicates(&mut staged);
        assert!(skip_of(&staged[0]).is_none());
        assert_eq!(
            skip_of(&staged[1]),
            Some((SkipReason::Duplicate, "C:\\a.txt"))
        );
    }

    #[test]
    fn trailing_separator_does_not_defeat_dedup() {
        let mut staged = vec![
            ready(0, "build", &trim_trailing_separators("C:\\w\\build")),
            ready(1, "build\\", &trim_trailing_separators("C:\\w\\build\\")),
            ready(2, "build/", &trim_trailing_separators("C:\\w\\build/")),
        ];
        filter_duplicates(&mut staged);
        assert!(skip_of(&staged[0]).is_none());
        assert_eq!(skip_of(&staged[1]), Some((SkipReason::Duplicate, "build")));
        assert_eq!(skip_of(&staged[2]), Some((SkipReason::Duplicate, "build")));
    }

    #[test]
    fn trailing_separators_are_stripped_but_not_from_a_drive_root() {
        assert_eq!(trim_trailing_separators("C:\\w\\build\\\\"), "C:\\w\\build");
        assert_eq!(trim_trailing_separators("C:\\"), "C:\\");
        assert_eq!(trim_trailing_separators("C:"), "C:\\");
        assert_eq!(trim_trailing_separators("C:/"), "C:\\");
    }

    #[test]
    fn nested_paths_are_skipped_with_their_container() {
        let mut staged = vec![
            ready(0, "dir", "C:\\dir"),
            ready(1, "dir\\file.txt", "C:\\dir\\file.txt"),
            ready(2, "dirwise", "C:\\dirwise"),
        ];
        filter_nested(&mut staged);
        assert!(skip_of(&staged[0]).is_none());
        assert_eq!(skip_of(&staged[1]), Some((SkipReason::NestedIn, "dir")));
        // `C:\dirwise` shares a string prefix with `C:\dir`, not a component.
        assert!(skip_of(&staged[2]).is_none());
    }

    #[test]
    fn nesting_is_case_insensitive_and_a_path_is_not_nested_in_itself() {
        let mut staged = vec![
            ready(0, "c:\\dir", "c:\\dir"),
            ready(1, "deep", "C:\\Dir\\A\\B\\File.txt"),
        ];
        filter_nested(&mut staged);
        assert!(skip_of(&staged[0]).is_none());
        assert_eq!(skip_of(&staged[1]), Some((SkipReason::NestedIn, "c:\\dir")));
    }

    #[test]
    fn the_earliest_argument_is_named_as_container() {
        let mut staged = vec![
            ready(0, "inner", "C:\\a\\b"),
            ready(1, "outer", "C:\\a"),
            ready(2, "leaf", "C:\\a\\b\\c"),
        ];
        filter_nested(&mut staged);
        assert_eq!(skip_of(&staged[0]), Some((SkipReason::NestedIn, "outer")));
        assert!(skip_of(&staged[1]).is_none());
        assert_eq!(skip_of(&staged[2]), Some((SkipReason::NestedIn, "inner")));
    }

    #[test]
    fn drive_roots_are_recognised_in_every_spelling() {
        for root in ["C:", "c:\\", "C:/", "C:\\\\", "\\\\?\\C:\\"] {
            assert!(is_drive_root(root), "{root}");
        }
        for not_root in ["C:\\dir", "C:dir", "\\\\srv\\share", "", "C", "dir"] {
            assert!(!is_drive_root(not_root), "{not_root}");
        }
    }

    #[test]
    fn drive_roots_are_refused_in_preflight() {
        for arg in ["C:\\", "C:", "C:/"] {
            match prepare_one(0, arg) {
                Prepared::Fail { code, .. } => assert_eq!(code, ErrorCode::DriveRoot, "{arg}"),
                other => panic!("expected DRIVE_ROOT for {arg}, got {other:?}"),
            }
        }
    }

    #[test]
    fn subst_join_keeps_the_separator_after_a_root_target() {
        // `Q:\dev\x.txt` with `subst Q: G:\` once became the drive-relative
        // `G:dev\x.txt`.
        assert_eq!(join_subst("G:\\", "dev\\x.txt"), "G:\\dev\\x.txt");
        assert_eq!(join_subst("G:\\", ""), "G:\\");
        assert_eq!(join_subst("C:\\some\\dir", "f.txt"), "C:\\some\\dir\\f.txt");
        assert_eq!(join_subst("C:\\some\\dir\\", ""), "C:\\some\\dir");
    }
}
