// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-034 §D4 — file-system layout for daemon-managed config.
//!
//! `$XDG_STATE_HOME/conductor/` holds the canonical state:
//! - `live.toml` — daemon-managed source of truth, 0600
//! - `live.toml.known_good` — operator-confirmed snapshot, 0600
//! - `conductor.lock` — singleton flock (acquired by D4.B.2)
//!
//! Per-UID isolation: `$XDG_STATE_HOME` defaults to
//! `~/.local/state` on Linux and `~/Library/Application Support`
//! on macOS, so the state dir is naturally per-user without
//! explicit UID handling.

use std::io;
use std::path::PathBuf;

/// Bundle of paths the daemon manages under `$XDG_STATE_HOME/conductor/`.
///
/// Construct with [`Self::from_env`] in production code (resolves
/// `$XDG_STATE_HOME` once at daemon startup) or [`Self::from_state_dir`]
/// in tests (lets the test point at a `TempDir`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePaths {
    /// State directory itself — `$XDG_STATE_HOME/conductor/`.
    pub state_dir: PathBuf,
    /// Canonical, daemon-managed config — `$XDG_STATE_HOME/conductor/live.toml`.
    pub live: PathBuf,
    /// Operator-confirmed known-good snapshot —
    /// `$XDG_STATE_HOME/conductor/live.toml.known_good`.
    pub known_good: PathBuf,
    /// Singleton lockfile (acquired in D4.B.2) —
    /// `$XDG_STATE_HOME/conductor/conductor.lock`.
    pub lockfile: PathBuf,
    /// Two-phase durable audit outbox (ADR-034 §D8) —
    /// `$XDG_STATE_HOME/conductor/audit-outbox.log`. Separate from the SQLite
    /// audit DB so audit durability survives a SQLite outage.
    pub audit_outbox: PathBuf,
}

impl LivePaths {
    /// Build paths rooted at `state_dir`. Does NOT create the
    /// directory — call [`Self::ensure_dir`] if you need that.
    ///
    /// Useful for tests: point `state_dir` at a `TempDir` and the
    /// `LiveConfig` constructed with this paths set is fully
    /// isolated from `$XDG_STATE_HOME`.
    pub fn from_state_dir(state_dir: PathBuf) -> Self {
        Self {
            live: state_dir.join("live.toml"),
            known_good: state_dir.join("live.toml.known_good"),
            lockfile: state_dir.join("conductor.lock"),
            audit_outbox: state_dir.join("audit-outbox.log"),
            state_dir,
        }
    }

    /// Resolve `$XDG_STATE_HOME/conductor/` from the environment.
    ///
    /// Resolution order (XDG Base Directory Specification §basics):
    /// 1. `$XDG_STATE_HOME` env var — honoured only if set, non-empty,
    ///    AND absolute. Per the spec: "If `$XDG_STATE_HOME` is either
    ///    not set or empty, a default equal to `\$HOME/.local/state`
    ///    should be used." We additionally reject relative paths
    ///    (XDG spec §note: "All paths set in these environment
    ///    variables must be absolute") rather than silently rooting
    ///    them at the cwd.
    /// 2. `dirs::state_dir()` — Linux: `~/.local/state`. macOS returns
    ///    `None` for state_dir (no platform-native equivalent), so we
    ///    fall through to (3).
    /// 3. `dirs::data_dir()` — macOS: `~/Library/Application Support`;
    ///    Linux: `~/.local/share` (last-resort fallback so the daemon
    ///    stays bootable on weird systems).
    /// 4. Returns an error if none resolve.
    ///
    /// **XDG-spec compliance:** the prior
    /// implementation accepted any value of `$XDG_STATE_HOME`,
    /// including empty strings and relative paths. An empty value
    /// would land state at `conductor/` (a relative path interpreted
    /// against cwd); a relative path like `state` would land it at
    /// `state/conductor/`. Both violate the spec. Now we ignore-and-
    /// fall-through unless the value is a non-empty absolute path.
    pub fn from_env() -> io::Result<Self> {
        let xdg_valid = std::env::var_os("XDG_STATE_HOME").and_then(|val| {
            let path = PathBuf::from(val);
            if path.as_os_str().is_empty() || path.is_relative() {
                None
            } else {
                Some(path)
            }
        });
        let state_dir = if let Some(xdg) = xdg_valid {
            xdg.join("conductor")
        } else if let Some(d) = dirs::state_dir() {
            d.join("conductor")
        } else if let Some(d) = dirs::data_dir() {
            d.join("conductor")
        } else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "could not resolve $XDG_STATE_HOME / state_dir / data_dir",
            ));
        };
        Ok(Self::from_state_dir(state_dir))
    }

    /// `mkdir_p` the state directory if absent. Called once on
    /// daemon startup before the first persist.
    pub fn ensure_dir(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.state_dir)
    }
}
