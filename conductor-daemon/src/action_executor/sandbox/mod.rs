// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 §D10b — OS-level sandboxing for `Shell` actions.
//!
//! Shell actions would otherwise run with the daemon's full filesystem and
//! network privileges. This module confines them at spawn time using the
//! platform's native sandbox:
//!
//! - **macOS**: a Seatbelt profile applied via the `sandbox_init(3)` C API
//!   in the child between `fork` and `exec` (a `pre_exec` hook). We bypass the
//!   deprecated `sandbox-exec` CLI per the ADR-027 D10b Council review — the
//!   profiles are stable even though the CLI is not.
//! - **Linux**: a Landlock ruleset (kernel 5.13+) built in the parent and
//!   committed in the child via `restrict_self()` in a `pre_exec` hook.
//! - **Other platforms** (Windows, Linux < 5.13 without Landlock): no OS
//!   sandbox. The daemon either spawns unconfined (with a warning) or fails
//!   closed, depending on `security.shell.allow_unsandboxed`.
//!
//! ### Enforcement model
//!
//! The default profile confines the two dimensions the per-action override
//! exposes: **filesystem writes** (denied except declared subtrees + the
//! always-safe temp / `/dev/null` paths) and **network egress** (denied
//! unless opted in). Reads and exec stay broadly allowed so ordinary commands
//! keep working. A full deny-default fork/exec jail is intentionally out of
//! scope for this slice — it breaks most real shell actions and is the part
//! the epic flagged for a dedicated soak period.

use conductor_core::config::types::ShellSandboxConfig;
use std::path::PathBuf;
use std::process::Command;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// A resolved, expanded sandbox policy for a single shell action.
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    /// Absolute, `~`-expanded filesystem subtrees the action may write to.
    /// Empty ⇒ the default (no writes outside the always-safe temp paths).
    pub fs_write_allow: Vec<PathBuf>,
    /// Allow network egress from the sandboxed action. `false` ⇒ deny.
    pub network: bool,
}

impl SandboxPolicy {
    /// Build a policy from an optional per-action override. `None` yields the
    /// default deny-write / deny-network confinement.
    pub fn from_config(cfg: Option<&ShellSandboxConfig>) -> Self {
        let Some(cfg) = cfg else {
            return Self::default();
        };
        let mut fs_write_allow = Vec::with_capacity(cfg.fs_write.len());
        for raw in &cfg.fs_write {
            match expand_path(raw) {
                Some(abs) => fs_write_allow.push(abs),
                // Surface dropped entries so a misconfigured (relative) path
                // isn't silently ignored.
                None => tracing::warn!(
                    path = %raw,
                    "ADR-027 D10b: sandbox fs_write path is not absolute after ~ expansion; ignoring it"
                ),
            }
        }
        Self {
            fs_write_allow,
            network: cfg.network,
        }
    }
}

/// Result of installing sandbox confinement on a [`Command`].
#[derive(Debug, PartialEq, Eq)]
pub enum SandboxOutcome {
    /// A `pre_exec` confinement hook was installed; the child will be sandboxed.
    Sandboxed,
    /// The platform/kernel cannot sandbox, but `allow_unsandboxed = true`, so
    /// the action will spawn unconfined. `reason` is logged as a warning.
    ///
    /// Only constructed on Linux (old kernels) / unsupported platforms — never
    /// on macOS, where Seatbelt is always available; the `allow(dead_code)`
    /// keeps the `-D warnings` build green on macOS where the variant is unused.
    #[allow(dead_code)]
    Unsandboxed { reason: String },
}

/// The daemon could not sandbox the action and `allow_unsandboxed = false`.
#[derive(Debug, PartialEq, Eq)]
pub struct SandboxRefused {
    pub reason: String,
}

/// Configure `command` so the spawned child is OS-sandboxed per `policy`.
///
/// Installs a `pre_exec` confinement hook on platforms that support it. The
/// hook runs in the child after `fork` and before `exec`, so it confines the
/// child without touching the daemon.
///
/// Returns:
/// - `Ok(Sandboxed)` — confinement installed.
/// - `Ok(Unsandboxed { reason })` — no OS sandbox here, but
///   `allow_unsandboxed` permits spawning anyway (caller should warn).
/// - `Err(SandboxRefused { reason })` — no OS sandbox and
///   `allow_unsandboxed = false`; the caller must NOT spawn.
pub fn apply_to_command(
    command: &mut Command,
    policy: &SandboxPolicy,
    allow_unsandboxed: bool,
) -> Result<SandboxOutcome, SandboxRefused> {
    #[cfg(target_os = "macos")]
    {
        // macOS always has Seatbelt; `allow_unsandboxed` is irrelevant here.
        let _ = allow_unsandboxed;
        macos::apply(command, policy)
    }
    #[cfg(target_os = "linux")]
    {
        linux::apply(command, policy, allow_unsandboxed)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // Windows and other platforms: no supported OS sandbox (ADR-027
        // §D10b documents Windows as out-of-scope).
        let _ = (command, policy);
        let reason = "no OS shell sandbox on this platform".to_string();
        if allow_unsandboxed {
            Ok(SandboxOutcome::Unsandboxed { reason })
        } else {
            Err(SandboxRefused { reason })
        }
    }
}

/// Expand a leading `~` to `$HOME` and return an absolute path, or `None` if
/// the path is relative after expansion (relative write-allow entries are
/// meaningless for an OS profile and are dropped with the caller's knowledge).
fn expand_path(raw: &str) -> Option<PathBuf> {
    let expanded: PathBuf = if let Some(rest) = raw.strip_prefix("~/") {
        let home = std::env::var_os("HOME")?;
        PathBuf::from(home).join(rest)
    } else if raw == "~" {
        PathBuf::from(std::env::var_os("HOME")?)
    } else {
        PathBuf::from(raw)
    };
    expanded.is_absolute().then_some(expanded)
}
