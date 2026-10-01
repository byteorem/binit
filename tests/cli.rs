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

#[test]
fn drive_root_is_refused_even_in_dry_run() {
    // `--dry-run` only: never point the real binary at a drive root.
    for arg in [r"C:\", "C:", "C:/"] {
        let out = binit()
            .args(["--dry-run", "--json", arg])
            .assert()
            .code(1)
            .get_output()
            .clone();
        let doc = json(&out.stdout);
        assert_eq!(doc["failed"][0]["code"], "DRIVE_ROOT", "{arg}");
        assert!(doc["recycled"].as_array().unwrap().is_empty(), "{arg}");
    }
}

#[test]
fn trailing_separator_is_the_same_path() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("build")).unwrap();

    let out = binit()
        .current_dir(dir.path())
        .args(["--dry-run", "--json", "build", r"build\"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let doc = json(&out.stdout);
    assert_eq!(doc["recycled"].as_array().unwrap().len(), 1);
    assert_eq!(doc["skipped"][0]["reason"], "duplicate");
    assert_eq!(doc["skipped"][0]["container"], "build");
}

#[test]
fn glob_expands_to_every_match() {
    let dir = TempDir::new().unwrap();
    for name in ["a.txt", "b.txt", "c.log"] {
        fs::write(dir.path().join(name), "").unwrap();
    }

    let out = binit()
        .current_dir(dir.path())
        .args(["--dry-run", "--json", "*.txt"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let doc = json(&out.stdout);
    let names: Vec<&str> = doc["recycled"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a.txt", "b.txt"]);
}

#[test]
fn f_reports_a_missing_path_as_skipped() {
    let dir = TempDir::new().unwrap();
    let out = binit()
        .args(["-f", "--json"])
        .arg(dir.path().join("gone.txt"))
        .assert()
        .code(0)
        .get_output()
        .clone();
    let doc = json(&out.stdout);
    assert_eq!(doc["skipped"][0]["reason"], "missing");
    assert!(doc["skipped"][0]["container"].is_null());
    assert!(doc["failed"].as_array().unwrap().is_empty());
}

#[test]
fn f_is_not_a_force_flag() {
    // Refusals and other failures still fail under -f.
    let out = binit()
        .args(["-f", "--json", r"\\localhost\nope\file.txt", ""])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let doc = json(&out.stdout);
    assert_eq!(doc["failed"][0]["code"], "UNC_NO_RECYCLE_BIN");
    assert_eq!(doc["failed"][1]["code"], "EMPTY_PATH");
    assert!(doc["skipped"].as_array().unwrap().is_empty());
}

#[test]
fn recursive_long_flag_is_accepted() {
    let dir = TempDir::new().unwrap();
    binit()
        .args(["--recursive", "-n"])
        .arg(dir.path())
        .assert()
        .code(0);
}

#[test]
fn force_is_refused_with_a_pointer_to_the_powershell_cmdlet() {
    let out = binit()
        .args(["--force", "whatever.txt"])
        .assert()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no --force"), "{stderr}");
    assert!(stderr.contains("Remove-Item"), "{stderr}");
    assert!(!stderr.contains("-- --force"), "{stderr}");
}

#[test]
fn files_from_file_appends_after_positional_paths() {
    let dir = TempDir::new().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(dir.path().join(name), "").unwrap();
    }
    // CRLF, a blank line, a whitespace-only line, and a BOM.
    fs::write(
        dir.path().join("list.lst"),
        "\u{feff}b.txt\r\n\r\n   \r\nc.txt\r\n",
    )
    .unwrap();

    let out = binit()
        .current_dir(dir.path())
        .args(["--dry-run", "--json", "a.txt", "--files-from", "list.lst"])
        .assert()
        .code(0)
        .get_output()
        .clone();
    let doc = json(&out.stdout);
    let names: Vec<&str> = doc["recycled"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a.txt", "b.txt", "c.txt"]);
}

#[test]
fn files_from_stdin_with_dash() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "").unwrap();

    let out = binit()
        .current_dir(dir.path())
        .args(["--dry-run", "--json", "--files-from", "-"])
        .write_stdin("a.txt\n")
        .assert()
        .code(0)
        .get_output()
        .clone();
    assert_eq!(json(&out.stdout)["recycled"][0]["path"], "a.txt");
}

#[test]
fn files_from_lines_are_literal_paths() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "").unwrap();

    let out = binit()
        .current_dir(dir.path())
        .args(["--dry-run", "--json", "--files-from", "-"])
        .write_stdin("*.txt\n")
        .assert()
        .code(1)
        .get_output()
        .clone();
    let doc = json(&out.stdout);
    assert_eq!(doc["failed"][0]["code"], "NOT_FOUND");
    assert!(doc["recycled"].as_array().unwrap().is_empty());
}

#[test]
fn unreadable_files_from_is_a_usage_error() {
    let dir = TempDir::new().unwrap();
    let out = binit()
        .arg("--files-from")
        .arg(dir.path().join("no-such-list.lst"))
        .assert()
        .code(2)
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot read --files-from"), "{stderr}");
}

#[test]
fn preflight_scales_linearly() {
    // The quadratic version took 17 s for 5,000 paths in release; this has to
    // finish in a fraction of that even in a debug build. The list goes in by
    // file because 5,000 paths overflow the 32,767-character command line.
    let dir = TempDir::new().unwrap();
    let mut list = String::new();
    for n in 0..5_000 {
        let file = dir.path().join(format!("file-{n:05}.txt"));
        fs::write(&file, "").unwrap();
        list.push_str(&format!("{}\n", file.display()));
    }
    let list_path = dir.path().join("list.lst");
    fs::write(&list_path, list).unwrap();

    let started = std::time::Instant::now();
    binit()
        .args(["--dry-run", "--quiet", "--files-from"])
        .arg(&list_path)
        .assert()
        .code(0);
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "5,000 paths took {elapsed:?}"
    );
}
