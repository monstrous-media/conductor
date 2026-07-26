// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-029 Phase 3 (D3) — `conductorctl permissions` subcommand smoke tests.
//!
//! Black-box integration test that runs the built `conductorctl` binary
//! as a subprocess and verifies the new `permissions` subcommand:
//!
//! - shows up in top-level help
//! - help output mentions `--check` and `--open-input-monitoring`
//!   (without ever actually invoking `--open-input-monitoring`, so
//!   System Settings doesn't pop open during CI)
//! - `--check` exits cleanly on the current platform
//!
//! Permission detection itself is a thin layer over platform APIs; the
//! detailed semantics (IOReturn classification, gilrs probe) live in
//! `conductor_daemon::permissions::macos` and have unit tests there.

use std::path::PathBuf;
use std::process::Command;

/// Returns true only when `help` exposes `flag` as a standalone option token
/// (e.g. `--check`), not as a substring of prose or of a longer flag.
///
/// #1534: the previous inline checks were of the form
/// `stdout.contains("--check") || stdout.contains("check")` — and likewise
/// for the open-input-monitoring flag, which also accepted the prose phrase
/// "Open System Settings". In each case the loose disjunct subsumed the
/// flag-token one and matched ANY occurrence in descriptive prose, so a help
/// page that dropped the real flag but kept descriptive text (e.g. "check
/// your permissions", "Open System Settings") would still pass.
///
/// A plain `contains(flag)` is not enough either: `contains("--check")` also
/// matches a longer flag like `--checklist` (Council + Copilot review). clap
/// renders each option on its own line as `      --check    <description>`
/// (and `--flag=VALUE` in usage forms), so we split on whitespace plus
/// `=` / `,` and compare WHOLE tokens. That pins the real CLI surface and
/// rejects both prose substrings and longer flags.
fn help_exposes_flag(help: &str, flag: &str) -> bool {
    help.split(|c: char| c.is_whitespace() || c == '=' || c == ',')
        .any(|token| token == flag)
}

fn conductorctl_path() -> PathBuf {
    // Cargo places the binary alongside the test binary in
    // target/<profile>/deps. The simplest portable lookup is the
    // CARGO_BIN_EXE_<name> env var that cargo sets when building tests
    // for a crate that has [[bin]] entries.
    if let Some(p) = option_env!("CARGO_BIN_EXE_conductorctl") {
        return PathBuf::from(p);
    }
    // Fallback for older cargo versions: walk up from the test binary.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("workspace root")
        .join("target")
        .join("debug")
        .join("conductorctl")
}

#[test]
fn permissions_subcommand_appears_in_help() {
    // Top-level `--help` should list the new command alongside Status,
    // Reload, etc.
    let output = Command::new(conductorctl_path())
        .arg("--help")
        .output()
        .expect("conductorctl --help");

    assert!(output.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("permissions"),
        "Top-level --help is missing the `permissions` subcommand. \
         Expected per ADR-029 §D3. Output:\n{}",
        stdout,
    );
}

#[test]
fn permissions_help_describes_check_and_open_flags() {
    // `conductorctl permissions --help` should describe both --check
    // and --open-input-monitoring (or equivalents — accept either flag
    // or subcommand form, whichever the implementation uses).
    let output = Command::new(conductorctl_path())
        .args(["permissions", "--help"])
        .output()
        .expect("conductorctl permissions --help");

    assert!(output.status.success(), "permissions --help should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mentions_check = help_exposes_flag(&stdout, "--check");
    let mentions_open = help_exposes_flag(&stdout, "--open-input-monitoring");
    assert!(
        mentions_check && mentions_open,
        "permissions --help should expose the --check and --open-input-monitoring \
         flag tokens (not just describe them in prose). Got:\n{}",
        stdout,
    );
}

#[test]
fn flag_matcher_rejects_prose_only_and_accepts_the_real_flag() {
    // #1534 regression guard. These cases pin exactly the gap the old
    // assertions let through, for BOTH flags:
    //
    //   - prose that mentions a flag's keyword but exposes no `--flag` token
    //     MUST be rejected (the old logic accepted it via the loose disjunct);
    //   - clap-style help that lists the real flag token MUST be accepted.

    // --check: old guard was `contains("--check") || contains("check")`.
    let check_prose = "Inspect permissions. Use this command to check your current grant status.";
    assert!(
        check_prose.contains("check"),
        "sanity: the prose case really does contain the bare word 'check' the old \
         assertion keyed on"
    );
    assert!(
        !help_exposes_flag(check_prose, "--check"),
        "prose mentioning 'check' but with no --check flag must NOT satisfy the contract"
    );

    // --open-input-monitoring: old guard also accepted the prose phrase
    // "Open System Settings", which appears in the flag's *description*.
    let open_prose = "Open System Settings to review Input Monitoring permissions.";
    assert!(
        open_prose.contains("Open System Settings"),
        "sanity: the prose case contains the phrase the old assertion keyed on"
    );
    assert!(
        !help_exposes_flag(open_prose, "--open-input-monitoring"),
        "prose mentioning 'Open System Settings' but with no flag token must NOT satisfy \
         the contract"
    );

    // A LONGER flag that merely has `--check` as a prefix must NOT satisfy
    // the `--check` contract — the matcher compares whole tokens, not
    // substrings (Council + Copilot review: `contains("--check")` would have
    // wrongly matched `--checklist`).
    let longer_flag_help = "Options:\n      --checklist    Print a checklist and exit\n";
    assert!(
        longer_flag_help.contains("--check"),
        "sanity: --checklist really does contain --check as a substring"
    );
    assert!(
        !help_exposes_flag(longer_flag_help, "--check"),
        "a longer flag (--checklist) must NOT satisfy the --check contract"
    );

    // Real clap-style help that lists both flag tokens must be accepted.
    let real_help = "Usage: conductorctl permissions [OPTIONS]\n\nOptions:\n      \
                     --check    Probe the daemon's TCC grants and report the result\n      \
                     --open-input-monitoring    Open System Settings ...\n";
    assert!(
        help_exposes_flag(real_help, "--check"),
        "help that lists the --check flag must satisfy the contract"
    );
    assert!(
        help_exposes_flag(real_help, "--open-input-monitoring"),
        "help that lists the --open-input-monitoring flag must satisfy the contract"
    );
}

#[test]
fn permissions_check_exits_cleanly_on_current_platform() {
    // The check command must not fail; on platforms without a consent
    // gate it prints an informative message and returns 0.
    let output = Command::new(conductorctl_path())
        .args(["permissions", "--check"])
        .output()
        .expect("conductorctl permissions --check");

    assert!(
        output.status.success(),
        "`conductorctl permissions --check` exited non-zero: {:?}\n\
         stdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Output should mention what was checked. Looking for either macOS
    // language ("Input Monitoring") or the cross-platform fallback
    // ("no consent gate") so the test passes on Linux / Windows CI.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("Input Monitoring") || combined.contains("consent gate"),
        "`conductorctl permissions --check` produced no recognisable status output. \
         stdout:\n{}\nstderr:\n{}",
        stdout,
        stderr,
    );

    // PR #997 round-13 review: hard-coded paths in human output misled users
    // on non-default installs; the daemon path next to conductorctl should be
    // resolved at runtime (`resolve_sibling_daemon_path` → `<dir>/conductor`).
    //
    // #1535: the old assertion accepted EITHER "resolved from this" OR
    // "typical install path" — but the GUI-app line ALWAYS prints "typical
    // install path", so the OR was always satisfied even if daemon resolution
    // always fell back (the exact regression this test guards). Make it
    // conditional on whether the sibling daemon binary actually exists:
    //   - sibling present (the normal `cargo test` workspace build) → the
    //     daemon path MUST be the resolved-from-sibling label;
    //   - sibling genuinely absent → the typical-install fallback is correct.
    #[cfg(target_os = "macos")]
    {
        let sibling_daemon = conductorctl_path().with_file_name("conductor");
        if sibling_daemon.exists() {
            assert!(
                stdout.contains("resolved from this conductorctl"),
                "macOS `--check` must RESOLVE the daemon path from the sibling \
                 binary at {sibling_daemon:?} (it exists), not fall back to the \
                 typical install path; stdout:\n{stdout}",
            );
        } else {
            // Assert the DAEMON-line fallback specifically (not just "typical
            // install path", which the GUI line always prints) so this branch
            // isn't a tautology either.
            assert!(
                stdout.contains("couldn't resolve a sibling binary next to conductorctl"),
                "macOS `--check` should use the daemon typical-install fallback \
                 when no sibling binary exists; stdout:\n{stdout}",
            );
        }
    }
}
