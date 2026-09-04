// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Regression guard for a CLI contract-mismatch report (medium confidence).
//!
//! A static review flagged the daemon's `#[command(name = "conductor")]` as a
//! mismatch, assuming the executable is `conductor-daemon`. Validation shows it
//! is not: the binary built from `conductor-daemon/src/main.rs` is named
//! `conductor` (see `conductor-daemon/Cargo.toml` `[[bin]] name = "conductor"`),
//! and the project's docs list `conductor` as the daemon CLI tool. `conductor-daemon`
//! is the *package* name — an internal Cargo concept, not a shell-facing
//! command. So the clap name already matches the real `conductor --help`
//! contract; the proposed "fix" to `conductor-daemon` would have *introduced*
//! the divergence it claimed to remove.
//!
//! This is the smoke test the issue suggested, but corrected to assert the real
//! binary name. It drives the actually-built binary (Cargo exposes its path via
//! `CARGO_BIN_EXE_conductor`) so it pins the genuine shell-facing usage line,
//! not just clap's in-process metadata. `--help` short-circuits inside clap
//! before any daemon/hardware code runs, so the test touches no devices.

use std::process::Command;

#[test]
fn daemon_help_usage_advertises_the_conductor_binary() {
    // Cargo sets CARGO_BIN_EXE_<bin-name> for integration tests to the path of
    // the freshly built binary — here the `conductor` daemon executable. This
    // is resolved by Cargo on every platform/profile (correct `target/<profile>`
    // dir, plus the `.exe` suffix on Windows), so it is the canonical, portable
    // way to locate the binary. A hand-rolled fallback would have to re-derive
    // the profile and platform extension and would drift from Cargo's truth.
    let bin = env!("CARGO_BIN_EXE_conductor");

    let output = Command::new(bin)
        .arg("--help")
        .output()
        .expect("running `conductor --help` should succeed");

    assert!(
        output.status.success(),
        "`conductor --help` should exit 0, got {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // clap renders the usage header from the command name, which must match the
    // invoked executable: `Usage: conductor [OPTIONS]`. The trailing space pins
    // the exact command token, guarding against a rename to `conductor-daemon`.
    assert!(
        stdout.contains("Usage: conductor "),
        "help usage line must advertise the `conductor` binary name; got:\n{stdout}"
    );

    // Negative guard: it must NOT advertise the package name as the command.
    assert!(
        !stdout.contains("Usage: conductor-daemon"),
        "help must not advertise the package name `conductor-daemon` as the command; got:\n{stdout}"
    );
}
