// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-034 §D4.1 — startup precedence load chain for the
//! daemon-managed canonical config.
//!
//! On startup the daemon attempts to load a live config from one of
//! three sources, in order:
//!
//! 1. `live.toml` — the canonical, daemon-managed source of truth.
//!    If present + parses + validates, this is what we publish.
//! 2. `live.toml.known_good` — operator-confirmed snapshot. Used
//!    when `live.toml` is missing or corrupt; load emits a warn-log
//!    "recovered from known_good".
//! 3. Neither parses → returns
//!    [`StartupLoadOutcome::AwaitingConfig`]. The caller falls through
//!    to the legacy config path; if nothing resolves there either, the
//!    daemon exits with a descriptive error. (The
//!    `LifecycleState::AwaitingConfig` idle mode this outcome was named
//!    for was never wired and is reserved — see #1323.)
//!
//! Audit-side recording of which source was loaded happens at the
//! caller (it has access to the `LiveConfig` + `AuditLogger`); this
//! module is pure: file I/O and parse only.

use conductor_core::Config;
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

use super::paths::LivePaths;

/// Which file the startup chain successfully loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadedFrom {
    /// `live.toml` — the canonical source. Normal startup path.
    Live { path: PathBuf },
    /// `live.toml.known_good` — recovery path. The caller should
    /// emit a warn-log so the operator notices the recovery (the
    /// live.toml was corrupted or absent).
    KnownGoodRecovery { path: PathBuf },
}

/// Outcome of the startup-precedence load chain.
#[derive(Debug)]
pub enum StartupLoadOutcome {
    /// One of the chain's files loaded + validated.
    Loaded {
        config: Box<Config>,
        loaded_from: LoadedFrom,
        /// Rejections encountered BEFORE the successful load.
        /// Populated when `loaded_from` is `KnownGoodRecovery` and
        /// `live.toml` was present-but-invalid — the caller
        /// warn-logs the rejection alongside the "recovered from
        /// known-good" message so operators can diagnose the
        /// corruption without grepping for two separate log lines
        /// (Council #1298 round 2 fix: prior shape silently dropped
        /// the diagnostic on the successful-recovery path).
        ///
        /// Empty for the `Live` happy path (no prior rejections).
        prior_rejections: Vec<LoadRejection>,
    },
    /// Neither file is present or parses. The caller falls through to the
    /// legacy config path; an unresolvable config ultimately exits with a
    /// descriptive error (the `LifecycleState::AwaitingConfig` idle mode
    /// this is named for is reserved/unwired — ADR-034 §D4.2, #1323).
    AwaitingConfig {
        /// Diagnostic explaining why each PRESENT-BUT-INVALID source
        /// was rejected. **Absent files are deliberately silent** —
        /// `LoadRejectionReason::NotPresent` is the common
        /// fresh-install signal and would noise up operator logs
        /// if recorded. Council #1298 round 3 clarification: empty
        /// `Vec` therefore means "both files genuinely absent
        /// (fresh install)"; a populated entry means "this specific
        /// file was present but unreadable/unparseable, and here's
        /// why."
        rejections: Vec<LoadRejection>,
    },
}

/// One source's failure during the load chain.
#[derive(Debug, Clone)]
pub struct LoadRejection {
    pub path: PathBuf,
    pub reason: LoadRejectionReason,
}

#[derive(Debug, Clone, Error)]
pub enum LoadRejectionReason {
    #[error("file not present")]
    NotPresent,
    #[error("read failed: {0}")]
    ReadFailed(String),
    #[error("parse failed: {0}")]
    ParseFailed(String),
}

/// Try each file in the spec-defined order and return the first
/// successful load, or `AwaitingConfig` if all fail.
///
/// Pure function — no audit recording, no `LiveConfig` interaction.
/// Caller is responsible for translating `Loaded { loaded_from }`
/// into `Source::DaemonStartup { source_file, revision }` for the
/// audit log (deferred to the wire-up commit; this helper is the
/// pure parsing primitive).
pub fn try_load_chain(paths: &LivePaths) -> StartupLoadOutcome {
    let mut rejections = Vec::new();

    // 1. live.toml — canonical source.
    match try_load_one(&paths.live) {
        Ok(config) => {
            // Happy path: live.toml loaded, no prior rejections to
            // surface.
            return StartupLoadOutcome::Loaded {
                config: Box::new(config),
                loaded_from: LoadedFrom::Live {
                    path: paths.live.clone(),
                },
                prior_rejections: Vec::new(),
            };
        }
        Err(reason) => {
            // Only record a rejection if the file was actually
            // present — `NotPresent` is the common "fresh install"
            // case and shouldn't pollute the diagnostic list.
            if !matches!(reason, LoadRejectionReason::NotPresent) {
                rejections.push(LoadRejection {
                    path: paths.live.clone(),
                    reason,
                });
            }
        }
    }

    // 2. live.toml.known_good — recovery source.
    match try_load_one(&paths.known_good) {
        Ok(config) => {
            // Recovery path: surface ANY rejections accumulated so
            // far (typically: live.toml was corrupt) so the caller
            // can warn-log "loaded known-good because live.toml
            // was rejected for X" — a single source of truth for
            // the operator-facing diagnostic, instead of dropping
            // it on the floor (Council #1298 round 2).
            return StartupLoadOutcome::Loaded {
                config: Box::new(config),
                loaded_from: LoadedFrom::KnownGoodRecovery {
                    path: paths.known_good.clone(),
                },
                prior_rejections: rejections,
            };
        }
        Err(reason) => {
            if !matches!(reason, LoadRejectionReason::NotPresent) {
                rejections.push(LoadRejection {
                    path: paths.known_good.clone(),
                    reason,
                });
            }
        }
    }

    StartupLoadOutcome::AwaitingConfig { rejections }
}

/// Try to load one file. Distinguishes "not present" from "present
/// but bad" so the caller can decide whether to surface a warn-log
/// (present-but-bad needs operator attention) or stay silent
/// (not-present is normal for fresh installs).
///
/// Council #1298 round 1 fix: the prior implementation used
/// `path.exists()` then `fs::read()` — a TOCTOU race where a file
/// deleted between the check and the read got misclassified as
/// `ReadFailed` instead of `NotPresent`, polluting the rejection
/// list with a spurious diagnostic. Now we skip the explicit
/// existence check and let `fs::read()`'s
/// `io::ErrorKind::NotFound` discriminate.
fn try_load_one(path: &std::path::Path) -> Result<Config, LoadRejectionReason> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(LoadRejectionReason::NotPresent);
        }
        Err(e) => return Err(LoadRejectionReason::ReadFailed(e.to_string())),
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| LoadRejectionReason::ParseFailed(format!("invalid utf-8: {e}")))?;
    let config: Config =
        toml::from_str(text).map_err(|e| LoadRejectionReason::ParseFailed(e.to_string()))?;
    // Apply the same security-sensitive checks `Config::load` runs (#1475).
    // A persisted live.toml / known-good snapshot that fails the legacy
    // validator (e.g. shell-action chaining) must not be admitted into
    // the live-config startup chain.
    conductor_core::config::validation::validate_for_loading(&config)
        .map_err(|e| LoadRejectionReason::ParseFailed(format!("validation: {e}")))?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_paths() -> (TempDir, LivePaths) {
        let dir = TempDir::new().unwrap();
        let paths = LivePaths::from_state_dir(dir.path().to_path_buf());
        paths.ensure_dir().unwrap();
        (dir, paths)
    }

    fn write_valid_config(path: &std::path::Path, mode_name: &str) {
        let toml = format!(
            r#"
[[modes]]
name = "{mode_name}"
"#
        );
        fs::write(path, toml).unwrap();
    }

    #[test]
    fn empty_state_dir_yields_awaiting_config_with_no_rejections() {
        // Fresh install — neither file exists. Should not record
        // rejections (NotPresent is silent).
        let (_dir, paths) = fresh_paths();
        match try_load_chain(&paths) {
            StartupLoadOutcome::AwaitingConfig { rejections } => {
                assert!(
                    rejections.is_empty(),
                    "fresh install should be silent, got: {rejections:?}"
                );
            }
            other => panic!("expected AwaitingConfig, got: {other:?}"),
        }
    }

    #[test]
    fn live_toml_loads_when_present_and_valid() {
        let (_dir, paths) = fresh_paths();
        write_valid_config(&paths.live, "live");
        match try_load_chain(&paths) {
            StartupLoadOutcome::Loaded {
                config,
                loaded_from,
                ..
            } => {
                assert_eq!(config.modes[0].name, "live");
                assert!(matches!(loaded_from, LoadedFrom::Live { .. }));
            }
            other => panic!("expected Loaded(Live), got: {other:?}"),
        }
    }

    #[test]
    fn falls_back_to_known_good_when_live_toml_absent() {
        let (_dir, paths) = fresh_paths();
        write_valid_config(&paths.known_good, "kg");
        match try_load_chain(&paths) {
            StartupLoadOutcome::Loaded {
                config,
                loaded_from,
                ..
            } => {
                assert_eq!(config.modes[0].name, "kg");
                assert!(matches!(loaded_from, LoadedFrom::KnownGoodRecovery { .. }));
            }
            other => panic!("expected Loaded(KnownGoodRecovery), got: {other:?}"),
        }
    }

    #[test]
    fn live_toml_rejected_when_validation_fails() {
        // #1475: a persisted config with shell-action chaining (or any
        // other validate_for_loading-rejected pattern) must be rejected
        // by the live-config startup chain, mirroring Config::load.
        // Before the fix, try_load_one only parsed and the security
        // checks were silently skipped.
        let (_dir, paths) = fresh_paths();
        fs::write(
            &paths.live,
            r#"
[[modes]]
name = "evil"

[[modes.mappings]]
trigger = { type = "Note", note = 60 }
action = { type = "Shell", command = "echo ok; rm -rf /tmp/x" }
"#,
        )
        .unwrap();
        match try_load_chain(&paths) {
            StartupLoadOutcome::AwaitingConfig { rejections } => {
                assert!(
                    rejections.iter().any(|r| matches!(
                        &r.reason,
                        LoadRejectionReason::ParseFailed(msg) if msg.contains("validation")
                    )),
                    "expected a ParseFailed(validation:…) rejection, got: {rejections:?}"
                );
            }
            StartupLoadOutcome::Loaded { .. } => {
                panic!("shell-chaining config must not be admitted to the live chain")
            }
        }
    }

    #[test]
    fn falls_back_to_known_good_when_live_toml_corrupt() {
        // Recovery path — `live.toml` is present but unparseable;
        // chain falls through to known_good AND records the
        // rejection so the caller can warn-log it.
        //
        // Council #1298 round 2 pin: `prior_rejections` MUST carry
        // the live.toml ParseFailed rejection through to the
        // Loaded(KnownGoodRecovery) outcome — without it the
        // operator can't diagnose WHY recovery happened.
        let (_dir, paths) = fresh_paths();
        fs::write(&paths.live, "this is not valid toml \x00").unwrap();
        write_valid_config(&paths.known_good, "kg-recovery");
        match try_load_chain(&paths) {
            StartupLoadOutcome::Loaded {
                config,
                loaded_from,
                prior_rejections,
            } => {
                assert_eq!(config.modes[0].name, "kg-recovery");
                assert!(matches!(loaded_from, LoadedFrom::KnownGoodRecovery { .. }));
                assert_eq!(
                    prior_rejections.len(),
                    1,
                    "live.toml rejection must surface in prior_rejections"
                );
                assert!(
                    matches!(
                        prior_rejections[0].reason,
                        LoadRejectionReason::ParseFailed(_)
                    ),
                    "rejection reason should be ParseFailed for invalid TOML, got: {:?}",
                    prior_rejections[0].reason
                );
            }
            other => panic!("expected Loaded(KnownGoodRecovery), got: {other:?}"),
        }
    }

    #[test]
    fn known_good_recovery_when_live_absent_has_empty_prior_rejections() {
        // Council #1298 round 3 test-gap fix: prior_rejections is
        // ONLY populated when live.toml was present-but-invalid,
        // not when it was genuinely absent (NotPresent is silent).
        // The recovery-from-absent-live case must have an empty
        // prior_rejections vec so the caller doesn't warn-log a
        // non-existent rejection.
        let (_dir, paths) = fresh_paths();
        // live.toml absent; known_good present and valid.
        write_valid_config(&paths.known_good, "kg-recovery-from-absent");
        match try_load_chain(&paths) {
            StartupLoadOutcome::Loaded {
                loaded_from,
                prior_rejections,
                ..
            } => {
                assert!(matches!(loaded_from, LoadedFrom::KnownGoodRecovery { .. }));
                assert!(
                    prior_rejections.is_empty(),
                    "absent-live recovery must NOT surface a rejection \
                     (NotPresent is silent — fresh install vs corrupt are \
                     diagnostically distinct cases), got: {prior_rejections:?}"
                );
            }
            other => panic!("expected Loaded(KnownGoodRecovery), got: {other:?}"),
        }
    }

    #[test]
    fn live_toml_happy_path_has_empty_prior_rejections() {
        // Inverse pin: when live.toml loads directly, prior_rejections
        // is empty — no "rejections from past attempts" leaking into
        // the happy path. (Council #1298 round 2.)
        let (_dir, paths) = fresh_paths();
        write_valid_config(&paths.live, "happy");
        match try_load_chain(&paths) {
            StartupLoadOutcome::Loaded {
                prior_rejections, ..
            } => {
                assert!(
                    prior_rejections.is_empty(),
                    "live-loads-directly path must have no prior rejections, got: {prior_rejections:?}"
                );
            }
            other => panic!("expected Loaded, got: {other:?}"),
        }
    }

    #[test]
    fn both_files_corrupt_yields_awaiting_config_with_both_rejections() {
        // All-files-corrupt case — daemon enters AwaitingConfig and
        // the caller has BOTH rejections to log.
        let (_dir, paths) = fresh_paths();
        fs::write(&paths.live, "invalid \x00 toml").unwrap();
        fs::write(&paths.known_good, "also \x00 invalid").unwrap();
        match try_load_chain(&paths) {
            StartupLoadOutcome::AwaitingConfig { rejections } => {
                assert_eq!(rejections.len(), 2, "both files should be flagged");
                assert!(
                    matches!(rejections[0].reason, LoadRejectionReason::ParseFailed(_)),
                    "first rejection should be ParseFailed"
                );
            }
            other => panic!("expected AwaitingConfig with rejections, got: {other:?}"),
        }
    }

    #[test]
    fn live_toml_wins_when_both_present_and_valid() {
        // Normal steady-state: both files exist, live.toml is the
        // canonical one. Chain must pick live.toml, NOT known_good.
        let (_dir, paths) = fresh_paths();
        write_valid_config(&paths.live, "live-wins");
        write_valid_config(&paths.known_good, "kg-should-not-load");
        match try_load_chain(&paths) {
            StartupLoadOutcome::Loaded {
                config,
                loaded_from,
                ..
            } => {
                assert_eq!(
                    config.modes[0].name, "live-wins",
                    "live.toml must take precedence"
                );
                assert!(matches!(loaded_from, LoadedFrom::Live { .. }));
            }
            other => panic!("expected Loaded(Live), got: {other:?}"),
        }
    }
}
