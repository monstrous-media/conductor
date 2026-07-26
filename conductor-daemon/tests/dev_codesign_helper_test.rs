// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-029 Phase 5 (D5) — dev codesign helper smoke tests.
//!
//! Smoke tests for `scripts/dev-codesign.sh`. The script signs
//! `target/debug/` binaries with a stable ad-hoc identity so macOS
//! TCC remembers Input Monitoring grants across rebuilds (cargo run
//! invalidates the grant otherwise — see ADR-029 §D5).
//!
//! These tests run on Unix-likes (macOS + Linux); the script handles
//! non-macOS by printing "skipping" and exiting 0, so Linux CI
//! exercises that branch and macOS CI exercises the codesign branch.
//! The script-execution test is gated on `#[cfg(not(windows))]`
//! because it shells out to `bash`, which isn't on PATH in standard
//! Windows CI runners — there's no .ps1 equivalent yet, and the
//! script wouldn't run on Windows anyway. On Windows only the
//! presence/permissions check runs.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    // conductor-daemon's CARGO_MANIFEST_DIR is `<repo>/conductor-daemon`.
    // The script lives at the repo root.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("conductor-daemon must have a parent directory")
        .to_path_buf()
}

fn script_path() -> PathBuf {
    repo_root().join("scripts").join("dev-codesign.sh")
}

#[test]
fn dev_codesign_script_exists_and_is_executable() {
    let path = script_path();
    assert!(
        path.exists(),
        "scripts/dev-codesign.sh missing — Phase 5 (D5) ships this helper. \
         Path: {}",
        path.display(),
    );

    // On Unix-likes, check the executable bit. (Windows skips this; the
    // script wouldn't run on Windows anyway, and we don't ship a .ps1
    // equivalent yet.)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&path).expect("metadata read");
        let mode = metadata.permissions().mode();
        assert!(
            mode & 0o100 != 0,
            "scripts/dev-codesign.sh is not executable (owner-x bit unset, \
             mode = {:o}). Run `chmod +x scripts/dev-codesign.sh`.",
            mode,
        );
    }
}

#[cfg(not(windows))]
#[test]
fn dev_codesign_script_exits_cleanly_and_is_idempotent() {
    // Cross-platform contract: the script exits 0 in two scenarios.
    //   1. macOS with no built binaries  → prints "SKIPPED" lines
    //   2. non-macOS                     → prints "skipping" and exits 0
    //
    // Idempotency: re-signing an already-signed binary is a normal
    // codesign operation (`--force` handles it); re-skipping on
    // non-macOS is a no-op anyway. We test both contracts in one test
    // because cargo runs test fns in parallel by default and two
    // concurrent `codesign --force` calls on the same binary can race
    // on macOS's signature lock — combining them into a single
    // sequential test sidesteps the parallelism without forcing
    // `--test-threads=1` on the whole suite.
    let path = script_path();
    if !path.exists() {
        // The exists-and-executable test fails first with a clearer
        // message; bail here so we don't double-report.
        return;
    }

    for run in 1..=2 {
        let output = Command::new("bash")
            .arg(&path)
            .current_dir(repo_root())
            .output()
            .expect("run dev-codesign.sh under bash");
        assert!(
            output.status.success(),
            "scripts/dev-codesign.sh failed on run {}: {:?}\nstdout:\n{}\nstderr:\n{}",
            run,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        // Stdout assertion: prove the script actually took the branch
        // we expect on this platform. Without this, "exits 0" alone
        // would still pass if a future change accidentally made the
        // script silently no-op everywhere.
        let stdout = String::from_utf8_lossy(&output.stdout);
        if cfg!(target_os = "macos") {
            assert!(
                !stdout.trim().is_empty(),
                "scripts/dev-codesign.sh produced no stdout on macOS run {}; \
                 expected per-binary status output (e.g. \"SKIPPED\" when \
                 binaries are absent, or \"signed\" when they exist).\nstderr:\n{}",
                run,
                String::from_utf8_lossy(&output.stderr),
            );
        } else {
            assert!(
                stdout.to_ascii_lowercase().contains("skipping"),
                "scripts/dev-codesign.sh did not report the non-macOS skip \
                 branch on run {}.\nstdout:\n{}\nstderr:\n{}",
                run,
                stdout,
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }
}
