//! End-to-end checks of the CLI contract that never touch the real Recycle
//! Bin: every case here is refused in pre-flight or runs under `--dry-run`.

use std::fs;

use assert_cmd::Command;
use tempfile::TempDir;

fn binit() -> Command {
    let mut cmd = Command::cargo_bin("binit").expect("binary builds");
    cmd.env("NO_COLOR", "1");
    cmd
}

fn json(stdout: &[u8]) -> serde_json::Value {
    serde_json::from_slice(stdout).expect("stdout is a JSON document")
}

#[test]
fn no_arguments_is_a_usage_error() {
    binit().assert().code(2);
}

#[test]
fn unc_path_is_refused_before_touching_the_network() {
    // `\\localhost\nope` does not exist; pre-flight refuses on shape alone,
    // so this never resolves the share.
    let out = binit()
        .arg(r"\\localhost\nope\file.txt")
        .assert()
        .code(1)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("binit: refused:"), "{stderr}");
    assert!(stderr.contains("permanently"), "{stderr}");
    assert!(stderr.contains("Nothing was deleted"), "{stderr}");
    assert!(stderr.contains("Remove-Item"), "{stderr}");
    assert!(stderr.contains("refused 1"), "{stderr}");
}

#[test]
fn unc_refusal_in_json() {
    let out = binit()
        .args(["--json", r"\\localhost\nope\file.txt"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert!(out.stderr.is_empty(), "--json must keep stderr silent");
    let doc = json(&out.stdout);
    assert_eq!(doc["version"], 1);
    assert_eq!(doc["failed"][0]["code"], "UNC_NO_RECYCLE_BIN");
    assert_eq!(doc["failed"][0]["path"], r"\\localhost\nope\file.txt");
    assert!(doc["recycled"].as_array().unwrap().is_empty());
}

#[test]
fn dry_run_reports_without_touching_the_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("keep-me.txt");
    fs::write(&file, "hello").unwrap();

    let out = binit()
        .args(["--dry-run", "--json"])
        .arg(&file)
        .assert()
        .code(0)
        .get_output()
        .clone();
    let doc = json(&out.stdout);
    assert_eq!(doc["dry_run"], true);
    assert_eq!(doc["recycled"][0]["path"], file.to_string_lossy().as_ref());
    assert!(
        doc["recycled"][0]["resolved"]
            .as_str()
            .unwrap()
            .ends_with("keep-me.txt")
    );
    assert!(file.exists(), "dry-run must not move the file");
}

#[test]
fn dry_run_summary_uses_would_trash() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("a.txt");
    fs::write(&file, "").unwrap();

    binit()
        .arg("-n")
        .arg(&file)
        .assert()
        .code(0)
        .stderr("would trash 1 item\n");
}

#[test]
fn nested_and_duplicate_arguments_are_skipped_not_failed() {
    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let inner = sub.join("inner.txt");
    fs::write(&inner, "").unwrap();

    let out = binit()
        .args(["--dry-run", "--json"])
        .arg(&sub)
        .arg(&inner)
        .arg(&sub)
        .assert()
        .code(0)
        .get_output()
        .clone();
    let doc = json(&out.stdout);
    assert_eq!(doc["recycled"].as_array().unwrap().len(), 1);
    let skipped = doc["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 2);
    assert_eq!(skipped[0]["reason"], "nested_in");
    assert_eq!(skipped[1]["reason"], "duplicate");
    assert!(doc["failed"].as_array().unwrap().is_empty());
}

#[test]
fn missing_file_is_not_found() {
    let dir = TempDir::new().unwrap();
    let out = binit()
        .arg("--json")
        .arg(dir.path().join("does-not-exist.txt"))
        .assert()
        .code(1)
        .get_output()
        .clone();
    let doc = json(&out.stdout);
    assert_eq!(doc["failed"][0]["code"], "NOT_FOUND");
    assert_eq!(doc["failed"][0]["message"], "no such file or directory");
}

#[test]
fn unmatched_glob_is_reported_not_swallowed() {
    let dir = TempDir::new().unwrap();
    let out = binit()
        .current_dir(dir.path())
        .args(["--json", "*.nothing-matches-this"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let doc = json(&out.stdout);
    assert_eq!(doc["failed"][0]["code"], "NOT_FOUND");
    assert_eq!(doc["failed"][0]["message"], "no files match this pattern");
}

#[test]
fn empty_argument_is_rejected() {
    let out = binit()
        .args(["--json", ""])
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert_eq!(json(&out.stdout)["failed"][0]["code"], "EMPTY_PATH");
}

#[test]
fn rm_compat_flags_are_accepted() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("a.txt");
    fs::write(&file, "").unwrap();

    binit().args(["-rf", "-n"]).arg(&file).assert().code(0);
    assert!(file.exists());
}

#[test]
fn help_mentions_the_no_force_promise() {
    binit()
        .arg("--help")
        .assert()
        .code(0)
        .stdout(predicates::str::contains("There is no --force"));
}
