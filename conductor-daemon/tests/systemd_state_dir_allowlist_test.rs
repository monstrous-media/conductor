// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! The packaged systemd unit must include the state directory
//! that `StateManager::get_state_dir()` writes — otherwise a
//! hardened systemd launch (`ProtectHome=read-only`) blocks the
//! daemon's first write to `~/.conductor` and startup fails.
//!
//! The bug: `get_state_dir()` returns `%h/.conductor` on Linux, but
//! the unit's `ReadWritePaths` listed `%h/.config/conductor`,
//! `%h/.local/state/conductor`, and `%h/.local/share/conductor` —
//! none of which match. Fresh install under systemd → daemon error
//! at `StateManager::new()`.
//!
//! This regression test is a packaging contract: parse the shipped
//! systemd unit, extract `ReadWritePaths`, and assert
//! `%h/.conductor` (the path `get_state_dir()` derives on Linux) is
//! one of the allowed prefixes.

use std::path::PathBuf;

fn systemd_unit_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("systemd")
        .join("conductor.service")
}

/// Parse ALL `ReadWritePaths=` lines out of the systemd unit and
/// return the accumulated space-separated paths. systemd permits
/// repeated `ReadWritePaths=` directives — each entry is additive
/// (the kernel sees the union).
///
/// Tolerate the systemd grammar variants likely to
/// appear in future revisions of our own file:
///
/// - Whitespace around `=`: `ReadWritePaths = ...` is equivalent
///   to `ReadWritePaths=...` per systemd.unit(5).
/// - Path-suppressing `-` prefix: `ReadWritePaths=-/path/that/may/not/exist`
///   means the same as `ReadWritePaths=/path/that/may/not/exist`
///   but doesn't fail if the path doesn't exist at unit load. We
///   strip it for comparison so the path-existence assertion still
///   matches.
/// - Comment lines (`#` / `;`): skipped so a path-shaped string
///   inside a comment doesn't count as granted.
///
/// Continuation lines (`\` at EOL) and quoting tricks aren't
/// present in this file and aren't supported here — extend the
/// parser if a future systemd unit needs them.
fn read_write_paths_from_unit() -> Vec<String> {
    let contents =
        std::fs::read_to_string(systemd_unit_path()).expect("read packaged systemd unit");
    let mut paths = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // Tolerate `ReadWritePaths = …` and `ReadWritePaths=…`.
        // Strip the key, then any whitespace, then the `=` sign.
        let Some(rest) = line.strip_prefix("ReadWritePaths") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(values) = rest.strip_prefix('=') else {
            continue;
        };
        // Strip the systemd path-suppressing `-` prefix from each
        // path for comparison; the semantic identity is the path
        // itself, not the suppression marker.
        paths.extend(
            values
                .split_whitespace()
                .map(|s| s.trim_start_matches('-').to_string()),
        );
    }
    paths
}

/// THE regression test. The Linux `get_state_dir()`
/// returns `~/.conductor` (see state.rs:204-209). The systemd unit
/// MUST grant write access to that path, otherwise daemon startup
/// fails under `ProtectHome=read-only`.
///
/// Asserts the literal `%h/.conductor` (systemd's home-directory
/// expansion equivalent) is among the unit's `ReadWritePaths`.
#[test]
fn systemd_read_write_paths_includes_dot_conductor_state_dir() {
    let paths = read_write_paths_from_unit();
    assert!(
        !paths.is_empty(),
        "ReadWritePaths line not found or empty in systemd unit — \
         hardened launch will block ALL writes outside the read-only home"
    );
    assert!(
        paths.iter().any(|p| p == "%h/.conductor"),
        "systemd unit's ReadWritePaths MUST include `%h/.conductor` — \
         that's where `StateManager::get_state_dir()` writes on Linux. \
         Without this, a hardened systemd launch \
         (ProtectHome=read-only) blocks startup state creation. \
         Current ReadWritePaths: {paths:?}"
    );
}

/// Defensive companion: also verify the existing XDG paths remain
/// in the allowlist (don't regress them while adding the new one).
/// `LivePaths` uses `$XDG_STATE_HOME/conductor` (typically
/// `~/.local/state/conductor`) which is already granted — keep it.
#[test]
fn systemd_read_write_paths_preserves_xdg_state_path() {
    let paths = read_write_paths_from_unit();
    // Mirror the empty-vector guard from the sibling
    // test above. A malformed unit with no ReadWritePaths at all
    // would fail the existence assertion with a less actionable
    // message; this guard surfaces the parse failure explicitly.
    assert!(
        !paths.is_empty(),
        "ReadWritePaths line not found or empty — see sibling test"
    );
    assert!(
        paths.iter().any(|p| p == "%h/.local/state/conductor"),
        "ReadWritePaths must continue to include the XDG state path \
         used by LivePaths; got: {paths:?}"
    );
}
