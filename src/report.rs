//! Outcome collection, error taxonomy, human/JSON rendering, and exit-code
//! mapping.
//!
//! Pure data: nothing here touches COM or the filesystem. The only Win32 call
//! is `FormatMessage`, via `windows::core::Error`, to turn an HRESULT into
//! prose.

#![forbid(unsafe_code)]

use std::fmt;
use std::io::{IsTerminal, Write};

use windows_core::{HRESULT, WIN32_ERROR};

use crate::bindings::{
    E_ACCESSDENIED, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION,
};

pub const JSON_VERSION: u32 = 1;

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_FATAL: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// `PreDeleteItem` vetoed: the shell was about to delete permanently.
    NotRecyclable,
    UncNoRecycleBin,
    SubstCycle,
    NotFound,
    AccessDenied,
    InUse,
    PathTooLong,
    EmptyPath,
    ShellError,
    NoResult,
    ComInitFailed,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::NotRecyclable => "NOT_RECYCLABLE",
            ErrorCode::UncNoRecycleBin => "UNC_NO_RECYCLE_BIN",
            ErrorCode::SubstCycle => "SUBST_CYCLE",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::AccessDenied => "ACCESS_DENIED",
            ErrorCode::InUse => "IN_USE",
            ErrorCode::PathTooLong => "PATH_TOO_LONG",
            ErrorCode::EmptyPath => "EMPTY_PATH",
            ErrorCode::ShellError => "SHELL_ERROR",
            ErrorCode::NoResult => "NO_RESULT",
            ErrorCode::ComInitFailed => "COM_INIT_FAILED",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why pre-flight dropped an argument without failing the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Same path as an earlier argument.
    Duplicate,
    /// Lives inside a directory that is also being trashed.
    NestedIn,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::Duplicate => "duplicate",
            SkipReason::NestedIn => "nested_in",
        }
    }
}

/// The HRESULT form of a Win32 error code (`0x8007xxxx`).
fn win32(code: i32) -> HRESULT {
    WIN32_ERROR(code.cast_unsigned()).to_hresult()
}

/// Map a shell HRESULT to an error code and a human message.
pub fn classify(hr: i32) -> (ErrorCode, String) {
    let hresult = HRESULT(hr);
    let code = if hresult == E_ACCESSDENIED {
        ErrorCode::AccessDenied
    } else if hresult == win32(ERROR_SHARING_VIOLATION) {
        ErrorCode::InUse
    } else if hresult == win32(ERROR_FILE_NOT_FOUND) || hresult == win32(ERROR_PATH_NOT_FOUND) {
        ErrorCode::NotFound
    } else {
        ErrorCode::ShellError
    };
    let message = windows_core::Error::from_hresult(hresult)
        .message()
        .trim()
        .to_string();
    let message = if message.is_empty() {
        format!("shell error {}", format_hresult(hr))
    } else {
        message
    };
    (code, message)
}

#[derive(Debug, Clone)]
pub struct Recycled {
    pub path: String,
    pub resolved: String,
}

#[derive(Debug, Clone)]
pub struct Skipped {
    pub path: String,
    pub reason: SkipReason,
    pub container: String,
}

#[derive(Debug, Clone)]
pub struct Failed {
    /// `None` for a fatal environment failure, which has no path to name.
    pub path: Option<String>,
    pub resolved: Option<String>,
    pub code: ErrorCode,
    pub hresult: Option<i32>,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub dry_run: bool,
    pub recycled: Vec<Recycled>,
    pub skipped: Vec<Skipped>,
    pub failed: Vec<Failed>,
}

impl Report {
    pub fn new(dry_run: bool) -> Self {
        Report {
            dry_run,
            ..Default::default()
        }
    }

    pub fn exit_code(&self) -> i32 {
        if self.failed.is_empty() {
            EXIT_OK
        } else {
            EXIT_FAILED
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::{Map, Value};

        let failed: Vec<Value> = self
            .failed
            .iter()
            .map(|f| {
                // Built key by key so the documented order survives the
                // optional fields.
                let mut o = Map::new();
                if let Some(path) = &f.path {
                    o.insert("path".into(), Value::String(path.clone()));
                }
                if let Some(resolved) = &f.resolved {
                    o.insert("resolved".into(), Value::String(resolved.clone()));
                }
                o.insert("code".into(), Value::String(f.code.as_str().into()));
                if let Some(hr) = f.hresult {
                    o.insert("hresult".into(), Value::String(format_hresult(hr)));
                }
                o.insert("message".into(), Value::String(f.message.clone()));
                Value::Object(o)
            })
            .collect();

        let mut doc = serde_json::json!({
            "version": JSON_VERSION,
            "recycled": self.recycled.iter().map(|r| serde_json::json!({
                "path": r.path,
                "resolved": r.resolved,
            })).collect::<Vec<_>>(),
            "skipped": self.skipped.iter().map(|s| serde_json::json!({
                "path": s.path,
                "reason": s.reason.as_str(),
                "container": s.container,
            })).collect::<Vec<_>>(),
            "failed": failed,
        });
        if self.dry_run {
            doc["dry_run"] = Value::Bool(true);
        }
        doc
    }
}

pub fn format_hresult(hr: i32) -> String {
    format!("0x{:08X}", hr.cast_unsigned())
}

/// Terminal styling. Color is opt-out and only ever applied when the stream
/// that carries it — stderr — is a TTY.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub color: bool,
}

impl Style {
    pub fn detect(no_color_flag: bool) -> Self {
        let disabled = no_color_flag
            || std::env::var_os("NO_COLOR").is_some()
            || std::env::var("TERM").is_ok_and(|t| t == "dumb")
            || !std::io::stderr().is_terminal();
        Style { color: !disabled }
    }

    fn wrap(self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn red(self, s: &str) -> String {
        self.wrap("31", s)
    }

    pub fn dim(self, s: &str) -> String {
        self.wrap("2", s)
    }
}

pub struct EmitOptions {
    pub verbose: bool,
    pub quiet: bool,
    pub json: bool,
    pub style: Style,
}

/// stdout: primary data only (the `-v` list, or the JSON document).
/// stderr: summary, warnings, errors.
pub fn emit(report: &Report, opts: &EmitOptions) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let stderr = std::io::stderr();
    let mut err = stderr.lock();

    if opts.json {
        let _ = writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&report.to_json()).unwrap_or_else(|_| "{}".into())
        );
        let _ = out.flush();
        return;
    }

    if opts.verbose {
        for r in &report.recycled {
            let _ = writeln!(out, "{}", r.path);
        }
        let _ = out.flush();
    }

    for s in &report.skipped {
        let _ = writeln!(
            err,
            "{} {}: {}",
            opts.style.dim("binit: skipped"),
            s.path,
            skip_explanation(s)
        );
    }

    for f in &report.failed {
        let _ = write!(err, "{}", render_failure(f, opts.style));
    }

    if !opts.quiet
        && let Some(line) = summary_line(report)
    {
        let _ = writeln!(err, "{line}");
    }
    let _ = err.flush();
}

fn skip_explanation(s: &Skipped) -> String {
    match s.reason {
        SkipReason::NestedIn => format!("already covered by {}", s.container),
        SkipReason::Duplicate => format!("duplicate of {}", s.container),
    }
}

pub fn summary_line(report: &Report) -> Option<String> {
    let mut parts = Vec::new();
    let verb = if report.dry_run {
        "would trash"
    } else {
        "trashed"
    };
    if !report.recycled.is_empty() {
        parts.push(format!(
            "{verb} {} {}",
            report.recycled.len(),
            plural(report.recycled.len())
        ));
    }
    if !report.skipped.is_empty() {
        parts.push(format!("skipped {}", report.skipped.len()));
    }
    // "refused" is the veto; anything else is an ordinary failure. Conflating
    // them would blunt the word that carries the tool's whole point.
    let refused = report
        .failed
        .iter()
        .filter(|f| {
            matches!(
                f.code,
                ErrorCode::NotRecyclable | ErrorCode::UncNoRecycleBin
            )
        })
        .count();
    if refused > 0 {
        parts.push(format!("refused {refused}"));
    }
    if report.failed.len() > refused {
        parts.push(format!("failed {}", report.failed.len() - refused));
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(", "))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "item" } else { "items" }
}

/// The long form for the two cases a user is most likely to hit while confused.
/// It has to say three things — refused, it would have been permanent, nothing
/// was deleted — and then offer the deliberate way out.
fn render_failure(f: &Failed, style: Style) -> String {
    // A fatal environment failure has no path to name.
    let Some(path) = f.path.as_deref() else {
        return format!("{} {}\n", style.red("binit: error:"), f.message);
    };
    match f.code {
        ErrorCode::NotRecyclable => format!(
            "{} {}\n  This location has no Recycle Bin, so the file could only be deleted\n  permanently. binit never does that. Nothing was deleted.\n  To delete it anyway: Remove-Item '{}'\n",
            style.red("binit: refused:"),
            path,
            path
        ),
        ErrorCode::UncNoRecycleBin => format!(
            "{} {}\n  Network locations have no Recycle Bin, so the file could only be\n  deleted permanently. binit never does that. Nothing was deleted.\n  To delete it anyway: Remove-Item '{}'\n",
            style.red("binit: refused:"),
            path,
            path
        ),
        _ => format!("{} {}: {}\n", style.red("binit: error:"), path, f.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(code: ErrorCode) -> Failed {
        Failed {
            path: Some("Y:\\c.txt".into()),
            resolved: Some("C:\\real\\c.txt".into()),
            code,
            hresult: Some(0x8000_4004u32.cast_signed()),
            message: "the Recycle Bin is not available for this location".into(),
        }
    }

    #[test]
    fn json_has_all_three_arrays_when_empty() {
        let doc = Report::new(false).to_json();
        assert_eq!(doc["version"], 1);
        assert!(doc["recycled"].as_array().unwrap().is_empty());
        assert!(doc["skipped"].as_array().unwrap().is_empty());
        assert!(doc["failed"].as_array().unwrap().is_empty());
        assert!(doc.get("dry_run").is_none());
    }

    #[test]
    fn json_matches_documented_shape() {
        let mut r = Report::new(false);
        r.recycled.push(Recycled {
            path: "C:\\a.txt".into(),
            resolved: "C:\\a.txt".into(),
        });
        r.skipped.push(Skipped {
            path: "C:\\dir\\b.txt".into(),
            reason: SkipReason::NestedIn,
            container: "C:\\dir".into(),
        });
        r.failed.push(failed(ErrorCode::NotRecyclable));

        let doc = r.to_json();
        assert_eq!(doc["recycled"][0]["path"], "C:\\a.txt");
        assert_eq!(doc["recycled"][0]["resolved"], "C:\\a.txt");
        assert_eq!(doc["skipped"][0]["reason"], "nested_in");
        assert_eq!(doc["skipped"][0]["container"], "C:\\dir");
        assert_eq!(doc["failed"][0]["code"], "NOT_RECYCLABLE");
        assert_eq!(doc["failed"][0]["hresult"], "0x80004004");
        assert_eq!(doc["failed"][0]["resolved"], "C:\\real\\c.txt");
        assert_eq!(
            doc["failed"][0]["message"],
            "the Recycle Bin is not available for this location"
        );
        let keys: Vec<&str> = doc["failed"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["path", "resolved", "code", "hresult", "message"]);
    }

    #[test]
    fn fatal_failure_omits_path_in_json() {
        let mut r = Report::new(false);
        r.failed.push(Failed {
            path: None,
            resolved: None,
            code: ErrorCode::ComInitFailed,
            hresult: None,
            message: "boom".into(),
        });
        let doc = r.to_json();
        assert!(doc["failed"][0].get("path").is_none());
        assert_eq!(doc["failed"][0]["code"], "COM_INIT_FAILED");
    }

    #[test]
    fn dry_run_is_flagged_in_json() {
        assert_eq!(Report::new(true).to_json()["dry_run"], true);
    }

    #[test]
    fn exit_zero_when_everything_recycled() {
        let mut r = Report::new(false);
        r.recycled.push(Recycled {
            path: "a".into(),
            resolved: "a".into(),
        });
        assert_eq!(r.exit_code(), EXIT_OK);
    }

    #[test]
    fn skips_alone_do_not_fail_the_run() {
        let mut r = Report::new(false);
        r.skipped.push(Skipped {
            path: "C:\\dir\\b.txt".into(),
            reason: SkipReason::NestedIn,
            container: "C:\\dir".into(),
        });
        assert_eq!(r.exit_code(), EXIT_OK);
    }

    #[test]
    fn any_failure_exits_one() {
        let mut r = Report::new(false);
        r.recycled.push(Recycled {
            path: "a".into(),
            resolved: "a".into(),
        });
        r.failed.push(failed(ErrorCode::NotRecyclable));
        assert_eq!(r.exit_code(), EXIT_FAILED);
    }

    #[test]
    fn hresult_formatting_is_eight_digit_hex() {
        assert_eq!(format_hresult(0x8007_0020u32.cast_signed()), "0x80070020");
    }

    #[test]
    fn classify_maps_the_common_win32_errors() {
        assert_eq!(
            classify(0x8007_0005u32.cast_signed()).0,
            ErrorCode::AccessDenied
        );
        assert_eq!(classify(0x8007_0020u32.cast_signed()).0, ErrorCode::InUse);
        assert_eq!(
            classify(0x8007_0002u32.cast_signed()).0,
            ErrorCode::NotFound
        );
        assert_eq!(
            classify(0x8007_0003u32.cast_signed()).0,
            ErrorCode::NotFound
        );
        assert_eq!(
            classify(0x8000_4005u32.cast_signed()).0,
            ErrorCode::ShellError
        );
    }

    #[test]
    fn refusal_says_all_three_things() {
        let text = render_failure(&failed(ErrorCode::NotRecyclable), Style { color: false });
        assert!(text.contains("refused"));
        assert!(text.contains("permanently"));
        assert!(text.contains("Nothing was deleted"));
        assert!(text.contains("Remove-Item"));
    }

    #[test]
    fn summary_counts_each_bucket() {
        let mut r = Report::new(false);
        r.recycled.push(Recycled {
            path: "a".into(),
            resolved: "a".into(),
        });
        r.failed.push(failed(ErrorCode::NotRecyclable));
        assert_eq!(summary_line(&r).unwrap(), "trashed 1 item, refused 1");
        assert!(summary_line(&Report::new(false)).is_none());
    }
}
