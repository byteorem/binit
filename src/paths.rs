//! Pre-flight: absolutize, resolve subst drives, classify, stat, and filter.
//!
//! Everything here happens before a single COM call, so a path that cannot
//! possibly be recycled is refused with a precise, human-authored reason
//! instead of an HRESULT.

// Opt in to `unsafe` for this file only (QueryDosDeviceW); the crate root
// denies it.
#![allow(unsafe_code)]

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

/// Component-wise containment. A naive `starts_with` on strings wrongly nests
/// `C:\foobar` under `C:\foo`.
pub fn is_nested_in(child: &Path, parent: &Path) -> bool {
    let child = components_lower(child);
    let parent = components_lower(parent);
    parent.len() < child.len() && child[..parent.len()] == parent[..]
}

fn components_lower(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// Case-insensitive key for dedup. Windows paths are case-insensitive.
fn dedup_key(path: &str) -> String {
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
        let mut next = PathBuf::from(target.trim_end_matches('\\'));
        if !remainder.is_empty() {
            next.push(remainder);
        }
        current = next;
    }

    Err(SubstError::Cycle)
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
    let resolved_text = resolved.to_string_lossy().into_owned();

    if is_unc(&resolved_text) || is_unc(input) {
        return fail(
            ErrorCode::UncNoRecycleBin,
            "network locations have no Recycle Bin",
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
        resolved: resolved_text,
    })
}

fn filter_duplicates(staged: &mut [Prepared]) {
    let mut seen: Vec<(String, String)> = Vec::new();
    for entry in staged.iter_mut() {
        let Prepared::Ready(p) = entry else { continue };
        let key = dedup_key(&p.resolved);
        if let Some((_, first)) = seen.iter().find(|(k, _)| *k == key) {
            *entry = Prepared::Skip {
                input: p.input.clone(),
                reason: SkipReason::Duplicate,
                container: first.clone(),
            };
        } else {
            seen.push((key, p.input.clone()));
        }
    }
}

/// Drop paths already covered by another argument. Recycling the container
/// first would otherwise make the nested path report a spurious `NOT_FOUND`.
/// The skip is reported, not silent.
fn filter_nested(staged: &mut [Prepared]) {
    let ready: Vec<(String, String)> = staged
        .iter()
        .filter_map(|e| match e {
            Prepared::Ready(p) => Some((p.resolved.clone(), p.input.clone())),
            _ => None,
        })
        .collect();

    for entry in staged.iter_mut() {
        let Prepared::Ready(p) = entry else { continue };
        let child = PathBuf::from(&p.resolved);
        let container = ready
            .iter()
            .find(|(resolved, _)| is_nested_in(&child, Path::new(resolved)));
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
    fn sibling_prefix_is_not_nested() {
        assert!(!is_nested_in(Path::new("C:\\foobar"), Path::new("C:\\foo")));
    }

    #[test]
    fn child_file_is_nested_in_its_directory() {
        assert!(is_nested_in(
            Path::new("C:\\dir\\file.txt"),
            Path::new("C:\\dir")
        ));
    }

    #[test]
    fn nesting_is_case_insensitive() {
        assert!(is_nested_in(
            Path::new("C:\\Dir\\File.txt"),
            Path::new("c:\\dir")
        ));
    }

    #[test]
    fn a_path_is_not_nested_in_itself() {
        assert!(!is_nested_in(Path::new("C:\\dir"), Path::new("C:\\dir")));
    }

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
    fn dedup_key_ignores_case() {
        assert_eq!(dedup_key("C:\\A.TXT"), dedup_key("c:\\a.txt"));
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

    #[test]
    fn duplicates_are_skipped_not_trashed_twice() {
        let mut staged = vec![
            Prepared::Ready(PreparedPath {
                index: 0,
                input: "C:\\a.txt".into(),
                resolved: "C:\\a.txt".into(),
            }),
            Prepared::Ready(PreparedPath {
                index: 1,
                input: "c:\\A.TXT".into(),
                resolved: "c:\\A.TXT".into(),
            }),
        ];
        filter_duplicates(&mut staged);
        assert!(matches!(staged[0], Prepared::Ready(_)));
        match &staged[1] {
            Prepared::Skip {
                reason, container, ..
            } => {
                assert_eq!(*reason, SkipReason::Duplicate);
                assert_eq!(container, "C:\\a.txt");
            }
            other => panic!("expected a skip, got {other:?}"),
        }
    }

    #[test]
    fn nested_paths_are_skipped_with_their_container() {
        let mut staged = vec![
            Prepared::Ready(PreparedPath {
                index: 0,
                input: "dir".into(),
                resolved: "C:\\dir".into(),
            }),
            Prepared::Ready(PreparedPath {
                index: 1,
                input: "dir\\file.txt".into(),
                resolved: "C:\\dir\\file.txt".into(),
            }),
            Prepared::Ready(PreparedPath {
                index: 2,
                input: "dirwise".into(),
                resolved: "C:\\dirwise".into(),
            }),
        ];
        filter_nested(&mut staged);
        assert!(matches!(staged[0], Prepared::Ready(_)));
        match &staged[1] {
            Prepared::Skip {
                reason, container, ..
            } => {
                assert_eq!(*reason, SkipReason::NestedIn);
                assert_eq!(container, "dir");
            }
            other => panic!("expected a skip, got {other:?}"),
        }
        assert!(matches!(staged[2], Prepared::Ready(_)));
    }
}
