// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Production audit-logger construction.
//!
//! D13b and D13c shipped with all unit tests passing because tests
//! constructed their own loggers, often via
//! `AuditLogger::in_memory()`. NO production code path called
//! [`AuditLogger::new`] with a disk-backed config — the daemon's
//! `ToolExecutor` was constructed with `audit_logger: None` and
//! every audit-write site was a silent no-op. The audit DB at
//! `~/Library/Application Support/conductor/audit.db` (or the
//! per-platform equivalent) never appeared on real installs.
//!
//! This module wraps the disk-backed construction so the
//! `EngineManager` has a single, tested entry point for production
//! wire-up.
//!
//! # Failure semantics
//!
//! If [`AuditLogger::new`] fails (db_path parent unwritable,
//! disk full, corrupted prior DB, schema-migration failure), this
//! returns `None` and logs an `error!`. The daemon still starts
//! — losing audit while keeping the daemon up is the lesser harm
//! per ADR-027 §D13b ("tamper-evidence, not tamper-prevention").
//! D13a (audit denial observability) will revisit this with a
//! visible "audit unavailable" surface; until then, look in the
//! daemon logs.

use std::path::Path;
use std::sync::Arc;

use super::{AuditLogger, AuditLoggerConfig};

/// Construct an [`AuditLogger`] rooted at `data_local_dir`.
///
/// Returns `Some(Arc<AuditLogger>)` on success or `None` on
/// failure (with an `error!` log). The caller threads the
/// `Option` into [`crate::daemon::llm::executor::ToolExecutor::set_audit_logger`].
///
/// `data_local_dir` should be the platform's data-local root
/// (e.g. `~/Library/Application Support` on macOS,
/// `~/.local/share` on Linux, `%LOCALAPPDATA%` on Windows). The
/// final path is `<data_local_dir>/conductor/audit.db`. Production
/// callers should pass `dirs::data_local_dir()` (via
/// [`default_audit_logger`]); tests pass a tempdir.
pub fn create_audit_logger(data_local_dir: &Path) -> Option<Arc<AuditLogger>> {
    let db_path = data_local_dir.join("conductor").join("audit.db");
    let config = AuditLoggerConfig {
        db_path: db_path.clone(),
        ..Default::default()
    };
    match AuditLogger::new(config) {
        Ok(logger) => {
            tracing::info!(
                path = %db_path.display(),
                "ADR-027 D13b/D13c: audit logger initialised"
            );
            Some(Arc::new(logger))
        }
        Err(e) => {
            tracing::error!(
                path = %db_path.display(),
                error = %e,
                "ADR-027 D13b/D13c: audit logger init FAILED — tool executions will NOT be \
                 recorded for the lifetime of this daemon. Inspect the audit DB path and rerun."
            );
            None
        }
    }
}

/// Construct an [`AuditLogger`] at the platform-default
/// data-local path, falling back to `None` (with an `error!`
/// log) if the platform refuses to surface a data dir.
///
/// Production wire-up entry point. Tests use [`create_audit_logger`]
/// directly with a tempdir to keep their state hermetic.
pub fn default_audit_logger() -> Option<Arc<AuditLogger>> {
    let dir = match dirs::data_local_dir() {
        Some(d) => d,
        None => {
            tracing::error!(
                "ADR-027 D13b/D13c: dirs::data_local_dir() returned None — cannot place audit \
                 DB at the platform-standard path. Audit disabled for this daemon's lifetime."
            );
            return None;
        }
    };
    create_audit_logger(&dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::audit::{AuditQuery, AuditRiskTier, UserContext};
    use std::time::Duration;

    /// When the daemon initialises
    /// the audit logger, the DB file MUST appear on disk at the
    /// expected path. Pre-fix the daemon never called
    /// `AuditLogger::new`, so this file never appeared and D13b/D13c
    /// were dormant in production.
    #[test]
    fn create_audit_logger_creates_db_file_at_expected_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let logger = create_audit_logger(tmp.path()).expect("logger constructed");

        let expected = tmp.path().join("conductor").join("audit.db");
        assert!(
            expected.exists(),
            "ADR-027 D13b: audit DB MUST exist on disk after `create_audit_logger` succeeds. \
             A past regression left this file silently never created. \
             Path checked: {}",
            expected.display(),
        );
        // Smoke-check the logger is functional (write + read) so
        // a regression that creates a half-broken DB file would
        // also trip this test.
        logger.log_tool_complete(
            "conductor_get_status",
            AuditRiskTier::ReadOnly,
            None,
            Some(r#"{"running":true}"#),
            Duration::from_millis(1),
            Some(UserContext::local_user()),
        );
        let count = logger.count().expect("count");
        assert_eq!(
            count, 1,
            "logger from `create_audit_logger` must accept writes; got count={count}",
        );
    }

    /// Cross-check: after `create_audit_logger` plus a write, the
    /// hash chain on disk must verify. This pins that the
    /// production wire-up calls go through D13b's hash-chain code
    /// path (not some test-only shortcut).
    #[test]
    fn create_audit_logger_writes_into_d13b_hash_chain() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let logger = create_audit_logger(tmp.path()).expect("logger constructed");

        for i in 0..3 {
            logger.log_tool_complete(
                &format!("tool_{i}"),
                AuditRiskTier::ReadOnly,
                None,
                Some(&format!(r#"{{"i":{i}}}"#)),
                Duration::from_millis(1),
                Some(UserContext::local_user()),
            );
        }
        let n = logger
            .verify_chain()
            .expect("verify_chain must succeed on a fresh, untouched DB");
        assert_eq!(
            n, 3,
            "verify_chain must report all 3 hashed rows from the production path",
        );

        // Sanity: audit-query path still works (catches schema/migration
        // regressions that the chain itself wouldn't notice).
        let entries = logger.query(&AuditQuery::default()).expect("query");
        assert_eq!(entries.len(), 3);
    }
}
