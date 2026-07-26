// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! CLI integration tests for conductor-capture (#1418).
//!
//! conductor-capture is in early development: the session, storage, and
//! upload paths are not implemented. These tests assert that action
//! subcommands fail honestly (non-zero exit, "not yet implemented"
//! message) instead of panicking or printing a false success message.
//!
//! The binary is located via Cargo's `CARGO_BIN_EXE_<name>` env var, so
//! these tests need no extra dependencies.

use std::io::Write;
use std::process::{Command, Output, Stdio};

/// Path to the freshly built `conductor-capture` binary (set by Cargo).
const BIN: &str = env!("CARGO_BIN_EXE_conductor-capture");

/// Run `conductor-capture` with `args`, feeding `stdin` to the process.
fn run(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn conductor-capture");
    // A command that fails immediately may exit before reading stdin —
    // a broken pipe here is expected, not a test failure.
    if let Some(mut sink) = child.stdin.take() {
        let _ = sink.write_all(stdin.as_bytes());
    }
    child.wait_with_output().expect("wait conductor-capture")
}

/// Assert a subcommand fails with a clean "not yet implemented" error —
/// non-zero exit, no panic, no false success message.
fn assert_not_implemented(args: &[&str], stdin: &str) {
    let out = run(args, stdin);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "`{}` must exit non-zero, not report success",
        args.join(" ")
    );
    assert!(
        stderr.contains("not yet implemented"),
        "`{}` must fail with a 'not yet implemented' message; stderr: {stderr}",
        args.join(" ")
    );
    assert!(
        !stderr.contains("panicked"),
        "`{}` must fail cleanly, not panic; stderr: {stderr}",
        args.join(" ")
    );
}

#[test]
fn start_fails_cleanly_without_panicking() {
    // Regression: `start` previously panicked via todo! after consent.
    assert_not_implemented(&["start"], "y\n");
}

#[test]
fn stop_fails_instead_of_reporting_saved() {
    assert_not_implemented(&["stop", "--name", "demo"], "");
}

#[test]
fn pause_fails_instead_of_reporting_success() {
    assert_not_implemented(&["pause"], "");
}

#[test]
fn resume_fails_instead_of_reporting_success() {
    assert_not_implemented(&["resume"], "");
}

#[test]
fn delete_fails_instead_of_reporting_deleted() {
    assert_not_implemented(&["delete", "demo", "--force"], "");
}

#[test]
fn import_fails_instead_of_reporting_complete() {
    // The path is portable (std::env::temp_dir) — `import` fails before
    // reading it, but a hardcoded /tmp would not exist on Windows.
    let import_path = std::env::temp_dir().join("conductor-capture-import.json");
    assert_not_implemented(&["import", import_path.to_str().unwrap()], "");
}

#[test]
fn info_fails_instead_of_showing_a_placeholder() {
    // `info` previously printed a "Capture: <name>" header for a capture
    // that cannot exist (no storage layer) — misleading. It now fails.
    assert_not_implemented(&["info", "demo"], "");
}

#[test]
fn upload_fails_instead_of_reporting_complete() {
    assert_not_implemented(&["upload", "demo"], "");
}

#[test]
fn export_fails_and_creates_no_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out_path = dir.path().join("export-out.json");
    assert_not_implemented(
        &["export", "demo", "--output", out_path.to_str().unwrap()],
        "",
    );
    assert!(
        !out_path.exists(),
        "export must not create its output file when unimplemented"
    );
}

#[test]
fn list_still_succeeds() {
    // Read-only display commands are unaffected by #1418.
    let out = run(&["list"], "");
    assert!(
        out.status.success(),
        "list is read-only and must still exit 0"
    );
}
