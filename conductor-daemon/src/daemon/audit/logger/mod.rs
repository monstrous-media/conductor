// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Audit logger implementation with SQLite storage

use super::hash_chain::{
    CanonicalRow, ChainBreak, GENESIS_PREV_HASH, canonical_persisted_bytes, compute_entry_hash,
};
use super::reconcile::{MutationDisposition, ReconciledMutation};
use super::redaction::truncate_for_storage;
#[cfg(test)]
use super::redaction::{RESULT_MAX_BYTES, TRUNCATION_MARKER};
use super::schema::{
    AuditEntry, AuditEventType, AuditQuery, AuditRiskTier, AuditSummary, CREATE_AUDIT_TABLE,
    CREATE_SCHEMA_VERSION_TABLE, MIGRATE_V1_TO_V2_ADD_ENTRY_HASH, MIGRATE_V1_TO_V2_ADD_PREV_HASH,
    MIGRATE_V2_TO_V3_ADD_PROVENANCE, SCHEMA_VERSION, UserContext,
};
use conductor_core::config::Provenance;
use rusqlite::{Connection, Result as SqliteResult, params};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

mod query;

/// Capacity of the live audit-event broadcast ring (ADR-027 D13a).
/// The SQLite log is the durable store; this channel is a
/// best-effort live tail for `conductorctl audit tail -f` /
/// `audit denied`. A consumer that falls behind by more than this
/// many events gets an explicit `Lagged(n)` rather than a silent
/// gap — the operator is told to re-query the persistent log for
/// the missed window.
const AUDIT_BROADCAST_CAPACITY: usize = 1024;

/// Configuration for the audit logger
#[derive(Debug, Clone)]
pub struct AuditLoggerConfig {
    /// Path to the SQLite database file
    pub db_path: std::path::PathBuf,
    /// Maximum age of entries before cleanup (days)
    pub max_age_days: u32,
    /// Whether to log ReadOnly operations (can be noisy)
    pub log_readonly: bool,
    /// Maximum entries before forced cleanup
    pub max_entries: u64,
    /// ADR-027 D13c: redact secret-shaped fields in
    /// `arguments` JSON before persisting. Default `true`.
    /// Toggling off is a deliberate reduced-safety choice that
    /// the operator should document — the audit DB lives on
    /// disk for `max_age_days` and ends up in user backups /
    /// support bundles, so secrets persisted here have a long
    /// disclosure tail.
    pub redact_arguments: bool,
    /// ADR-027 D13c: same as [`Self::redact_arguments`] but
    /// for the `result` JSON. Default `true`.
    pub redact_results: bool,
}

impl Default for AuditLoggerConfig {
    fn default() -> Self {
        let db_path = dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("conductor")
            .join("audit.db");

        Self {
            db_path,
            max_age_days: 90, // 3 months retention
            log_readonly: true,
            max_entries: 1_000_000, // 1M entries max
            redact_arguments: true, // D13c: secrecy-by-default
            redact_results: true,
        }
    }
}

/// Audit logger for comprehensive tool execution tracking
pub struct AuditLogger {
    conn: Arc<Mutex<Connection>>,
    config: AuditLoggerConfig,
    /// Live audit-event broadcast (ADR-027 D13a). Every
    /// persisted entry is published here AFTER the SQLite write
    /// succeeds — durable-first, so a subscriber never sees an
    /// event that isn't also in the persistent log.
    event_tx: broadcast::Sender<AuditEntry>,
}

/// Run an `ALTER TABLE ADD COLUMN` migration step idempotently,
/// using `PRAGMA table_info` to check column existence first.
/// The previous
/// implementation matched on the SQLite error message
/// "duplicate column name", which is not a stable API. Using
/// `PRAGMA table_info` is the idiomatic, version-stable way to
/// test for column existence before attempting the ALTER.
/// Any error from the PRAGMA query or from the ALTER itself
/// (DB locked, corrupt, missing table) is surfaced so the
/// migration doesn't bump `schema_version` to v2 while
/// leaving the columns missing.
fn run_idempotent_alter(conn: &Connection, column: &str, sql: &str) -> SqliteResult<()> {
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('audit_log') WHERE name = ?1",
            [column],
            |row| row.get::<_, i64>(0),
        )
        .map(|n| n > 0)?;
    if !exists {
        conn.execute(sql, [])?;
    }
    Ok(())
}

/// Recompute `prev_hash`/`entry_hash` over the hashed segment of the
/// audit table (`entry_hash IS NOT NULL`) using the CURRENT
/// `CanonicalRow` form.
///
/// This is used by schema migrations when canonical bytes change
/// (for example when provenance was brought under the hash chain).
/// Legacy v1 rows remain untouched because they keep
/// `entry_hash = NULL`.
fn rebuild_hashed_chain_segment(conn: &Connection) -> SqliteResult<usize> {
    let mut stmt = conn.prepare(
        "SELECT id, event_type, tool_name, user_context, arguments, \
                result, risk_tier, is_error, error_message, \
                execution_time_ms, created_at, provenance \
         FROM audit_log \
         WHERE entry_hash IS NOT NULL \
         ORDER BY rowid ASC",
    )?;
    type RebuildRow = (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        i32,
        Option<String>,
        Option<i64>,
        i64,
        Option<String>,
    );
    let rows: Vec<RebuildRow> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
            ))
        })?
        .collect::<SqliteResult<Vec<_>>>()?;
    drop(stmt);

    let mut prev = GENESIS_PREV_HASH.to_string();
    for (id, et, tn, uc, args, res, rt, ie, em, ems, ca, prov) in &rows {
        let canonical = CanonicalRow {
            id,
            event_type: et,
            tool_name: tn.as_deref(),
            user_context: uc.as_deref(),
            arguments: args.as_deref(),
            result: res.as_deref(),
            risk_tier: rt,
            is_error: *ie,
            error_message: em.as_deref(),
            execution_time_ms: *ems,
            created_at: *ca,
            provenance: prov.as_deref(),
        };
        let new_entry_hash = compute_entry_hash(&prev, &canonical_persisted_bytes(&canonical));
        conn.execute(
            "UPDATE audit_log SET prev_hash = ?1, entry_hash = ?2 WHERE id = ?3",
            params![prev, new_entry_hash, id],
        )?;
        prev = new_entry_hash;
    }

    Ok(rows.len())
}

impl AuditLogger {
    /// Create a new audit logger with the given configuration
    pub fn new(config: AuditLoggerConfig) -> SqliteResult<Self> {
        // Ensure parent directory exists
        if let Some(parent) = config.db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let conn = Connection::open(&config.db_path)?;

        // Enable WAL mode for better concurrent access
        conn.pragma_update(None, "journal_mode", "WAL")?;

        // Initialize schema
        conn.execute_batch(CREATE_SCHEMA_VERSION_TABLE)?;
        conn.execute_batch(CREATE_AUDIT_TABLE)?;

        // Check and update schema version
        let current_version: i32 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        if current_version < SCHEMA_VERSION {
            // ADR-027 D13b: v1→v2 adds prev_hash + entry_hash
            // for the append-only hash chain. `run_idempotent_alter`
            // checks column existence via `PRAGMA table_info`
            // before running the ALTER, so this is idempotent
            // on both fresh v2 DBs (columns already present) and
            // existing v1 DBs (columns absent, ALTER adds them).
            // Any PRAGMA or ALTER error (DB locked, corrupt, etc.)
            // surfaces so the migration doesn't bump schema_version
            // while leaving columns missing.
            if current_version < 2 {
                run_idempotent_alter(&conn, "prev_hash", MIGRATE_V1_TO_V2_ADD_PREV_HASH)?;
                run_idempotent_alter(&conn, "entry_hash", MIGRATE_V1_TO_V2_ADD_ENTRY_HASH)?;
            }
            // ADR-034 §D4.A.3.3.B.2 (2026-05-17): v2→v3 adds the
            // `provenance` column. Existing v2 rows survive with
            // NULL provenance; new v3+ rows populate it with a
            // JSON-serialised `Provenance` triple.
            if current_version < 3 {
                run_idempotent_alter(&conn, "provenance", MIGRATE_V2_TO_V3_ADD_PROVENANCE)?;
            }
            // Once provenance became part of
            // `CanonicalRow`, older v3 rows that already carry
            // non-NULL provenance needed a one-time re-hash to keep
            // `verify_chain()` green after upgrade.
            if current_version < 4 {
                let rebuilt = rebuild_hashed_chain_segment(&conn)?;
                debug!(
                    "Audit migration v4: rebuilt hash chain over {} rows",
                    rebuilt
                );
            }
            conn.execute(
                "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )?;
            info!(
                "Audit database migrated from v{} to v{}",
                current_version, SCHEMA_VERSION
            );
        }

        info!("Audit logger initialized at {:?}", config.db_path);

        let (event_tx, _) = broadcast::channel(AUDIT_BROADCAST_CAPACITY);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            config,
            event_tx,
        })
    }

    /// Create an in-memory audit logger (for testing)
    pub fn in_memory() -> SqliteResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(CREATE_SCHEMA_VERSION_TABLE)?;
        conn.execute_batch(CREATE_AUDIT_TABLE)?;

        let (event_tx, _) = broadcast::channel(AUDIT_BROADCAST_CAPACITY);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            config: AuditLoggerConfig {
                db_path: std::path::PathBuf::from(":memory:"),
                ..Default::default()
            },
            event_tx,
        })
    }

    /// Subscribe to the live audit-event stream (ADR-027 D13a).
    /// Each persisted entry is broadcast here after the
    /// SQLite write succeeds. A receiver that falls behind
    /// [`AUDIT_BROADCAST_CAPACITY`] events gets an explicit
    /// `RecvError::Lagged(n)` — the persistent log
    /// ([`Self::query`]) remains the complete record for any
    /// window the live consumer missed.
    pub fn subscribe(&self) -> broadcast::Receiver<AuditEntry> {
        self.event_tx.subscribe()
    }

    /// Log a tool execution start
    pub fn log_tool_start(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        user_context: Option<UserContext>,
    ) -> String {
        // Skip logging ReadOnly if configured
        if !self.config.log_readonly && risk_tier == AuditRiskTier::ReadOnly {
            return uuid::Uuid::new_v4().to_string();
        }

        let mut entry = AuditEntry::new(AuditEventType::ToolStart, risk_tier).with_tool(tool_name);

        if let Some(args) = arguments {
            entry = entry.with_arguments(args);
        }
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }

        self.insert_entry(&entry);
        entry.id
    }

    /// Record a network-listener audit event (ADR-042 Phase A). Network events
    /// are `Internal` tier; the listener alias goes in `tool_name` and the
    /// relevant `ip` plus an optional disposition `summary` are carried as JSON
    /// arguments. Used for `NetworkListenerActivity` (accepted packets),
    /// `NetworkListenerBindFailed`, `NetworkActionClassBlocked`, and the other
    /// `Network*` variants. `ip` is the packet source for activity/block events
    /// and the bind host for bind-failed / orphaned events (the neutral name
    /// is used rather than `sender`).
    ///
    /// Edge **rejections** (off-ACL / rate-limited packets) are deliberately
    /// NOT persisted through here — they emit dedup'd `tracing` only (spec §A.5,
    /// reject-audit = tracing-only), so a spoofed flood can't bloat the DB.
    pub fn log_network_event(
        &self,
        event_type: AuditEventType,
        listener: &str,
        ip: IpAddr,
        summary: Option<&str>,
    ) -> String {
        let args = serde_json::json!({
            "ip": ip.to_string(),
            "summary": summary,
        });
        let entry = AuditEntry::new(event_type, AuditRiskTier::Internal)
            .with_tool(listener)
            .with_arguments(args.to_string());
        self.insert_entry(&entry);
        entry.id
    }

    /// ADR-034 §D2.2 — record a `ReloadFromDisk` / `ImportConfig` path that the
    /// safe-walk validation refused. `attempted_path` is the caller-supplied
    /// path (recorded for forensics — it is the operator's own local path over
    /// a trusted IPC band); `reason` is the coarse rejection discriminator
    /// (e.g. `symlink_in_path`, `not_beneath_root`, `owner_mismatch`).
    pub fn log_path_validation_failed(&self, attempted_path: &str, reason: &str) -> String {
        let args = serde_json::json!({
            "path": attempted_path,
            "reason": reason,
        });
        let entry = AuditEntry::new(
            AuditEventType::PathValidationFailed,
            AuditRiskTier::Internal,
        )
        .with_tool("reload_from_disk_or_import")
        .with_error(reason)
        .with_arguments(args.to_string());
        self.insert_entry(&entry);
        entry.id
    }

    /// ADR-034 §D8.3 — record one config mutation that startup reconciliation
    /// found `Pending` at crash and that did NOT publish (its
    /// `intended_revision` does not match the loaded `live.toml`). `Internal`
    /// tier (a system-recovery event, never operator-initiated). The mutation
    /// `id` and `intended_revision` are recorded for forensics. Returns the
    /// generated audit entry id. Persistence is **best-effort** (like every
    /// `log_*` helper): the id is returned even if the underlying
    /// [`Self::insert_entry`] silently early-returns on a SQLite/lock error, so
    /// the id is not a guarantee the row reached disk.
    pub fn log_config_mutation_pending_at_crash(
        &self,
        mutation_id: &str,
        intended_revision: Option<&str>,
    ) -> String {
        let args = serde_json::json!({
            "mutation_id": mutation_id,
            "intended_revision": intended_revision,
        });
        let entry = AuditEntry::new(
            AuditEventType::ConfigMutationPendingAtCrash,
            AuditRiskTier::Internal,
        )
        .with_tool("config_mutation")
        .with_arguments(args.to_string());
        self.insert_entry(&entry);
        entry.id
    }

    /// ADR-034 §D8.3 — emit one [`AuditEventType::ConfigMutationPendingAtCrash`]
    /// event per mutation surfaced by startup reconciliation
    /// ([`crate::daemon::live_config::LiveConfig::pending_at_crash`]). Returns
    /// the number of mutations for which an event was **emitted** (i.e.
    /// `pending.len()` — one emit attempt each; 0 for an empty slice, the common
    /// clean-shutdown case). Each emit is best-effort (see
    /// [`Self::log_config_mutation_pending_at_crash`]), so this count is emit
    /// attempts, not a guarantee every row was durably written. Idempotency
    /// across restarts is bounded by the outbox lifecycle: once a flusher
    /// (sub-slice B2) drains resolved rows, a re-surfaced pending row reflects a
    /// still-unresolved mutation.
    ///
    /// **Caller contract:** every element MUST have
    /// [`MutationDisposition::PendingAtCrash`] — each is emitted as a
    /// `ConfigMutationPendingAtCrash` event unconditionally. Pass only the
    /// already-filtered slice from
    /// [`crate::daemon::live_config::LiveConfig::pending_at_crash`] (or
    /// [`crate::daemon::audit::pending_at_crash`]); a debug build asserts the
    /// disposition so a future caller that forgets the filter fails loudly in
    /// tests rather than silently mislabelling resolved mutations.
    pub fn log_pending_at_crash_batch(&self, pending: &[ReconciledMutation]) -> usize {
        for m in pending {
            debug_assert!(
                matches!(m.disposition, MutationDisposition::PendingAtCrash),
                "log_pending_at_crash_batch requires PendingAtCrash disposition, \
                 got {:?} for id {}",
                m.disposition,
                m.id,
            );
            self.log_config_mutation_pending_at_crash(&m.id, m.intended_revision.as_deref());
        }
        pending.len()
    }

    /// Log a successful tool execution completion
    pub fn log_tool_complete(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        result: Option<&str>,
        execution_time: Duration,
        user_context: Option<UserContext>,
    ) {
        // Skip logging ReadOnly if configured
        if !self.config.log_readonly && risk_tier == AuditRiskTier::ReadOnly {
            return;
        }

        let mut entry = AuditEntry::new(AuditEventType::ToolComplete, risk_tier)
            .with_tool(tool_name)
            .with_execution_time(execution_time);

        if let Some(args) = arguments {
            entry = entry.with_arguments(args);
        }
        if let Some(res) = result {
            entry = entry.with_result(res);
        }
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }

        self.insert_entry(&entry);
    }

    /// Log a tool execution error
    pub fn log_tool_error(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        error_message: &str,
        execution_time: Duration,
        user_context: Option<UserContext>,
    ) {
        let mut entry = AuditEntry::new(AuditEventType::ToolError, risk_tier)
            .with_tool(tool_name)
            .with_error(error_message)
            .with_execution_time(execution_time);

        if let Some(args) = arguments {
            entry = entry.with_arguments(args);
        }
        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }

        self.insert_entry(&entry);
    }

    /// Log a tool execution denial (permissions, rate limit, etc.)
    pub fn log_tool_denied(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        reason: &str,
        user_context: Option<UserContext>,
    ) {
        let mut entry = AuditEntry::new(AuditEventType::ToolDenied, risk_tier)
            .with_tool(tool_name)
            .with_error(reason);

        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }

        self.insert_entry(&entry);
    }

    /// Log an ADR-027 D6 LLM-budget violation. `dimension` is the
    /// snake_case budget key that tripped (e.g. `max_tool_calls_per_session`);
    /// `tool_name` is the call that would have overshot the budget.
    pub fn log_llm_budget_exceeded(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        dimension: &str,
        limit: u64,
        observed: u64,
        user_context: Option<UserContext>,
    ) {
        let mut entry = AuditEntry::new(AuditEventType::LlmBudgetExceeded, risk_tier)
            .with_tool(tool_name)
            .with_error(format!(
                "LLM budget exceeded: {} would reach {} (limit {})",
                dimension, observed, limit
            ));

        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }

        self.insert_entry(&entry);
    }

    /// Log a plan creation
    pub fn log_plan_created(
        &self,
        plan_id: &str,
        changes_count: usize,
        user_context: Option<UserContext>,
    ) {
        let mut entry = AuditEntry::new(AuditEventType::PlanCreated, AuditRiskTier::ConfigChange)
            .with_arguments(format!(
                r#"{{"plan_id": "{}", "changes_count": {}}}"#,
                plan_id, changes_count
            ));

        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }

        self.insert_entry(&entry);
    }

    /// Log a plan application
    ///
    /// D4.A.3.3.B.2: accepts an optional `Provenance` triple per
    /// ADR-034 §D6 — the same value the caller passed to
    /// `LiveConfig::mutate` flows through here so the persisted
    /// audit row records *who* initiated the apply, *what was
    /// applied*, and the authenticated peer if one exists.
    pub fn log_plan_applied(
        &self,
        plan_id: &str,
        changes_applied: usize,
        execution_time: Duration,
        user_context: Option<UserContext>,
        provenance: Option<Provenance>,
    ) {
        let mut entry = AuditEntry::new(AuditEventType::PlanApplied, AuditRiskTier::ConfigChange)
            .with_arguments(format!(
                r#"{{"plan_id": "{}", "changes_applied": {}}}"#,
                plan_id, changes_applied
            ))
            .with_execution_time(execution_time);

        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }
        if let Some(prov) = provenance {
            entry = entry.with_provenance(prov);
        }

        self.insert_entry(&entry);
    }

    /// Log a plan rejection
    pub fn log_plan_rejected(&self, plan_id: &str, user_context: Option<UserContext>) {
        let mut entry = AuditEntry::new(AuditEventType::PlanRejected, AuditRiskTier::ConfigChange)
            .with_arguments(format!(r#"{{"plan_id": "{}"}}"#, plan_id));

        if let Some(ctx) = user_context {
            entry = entry.with_user(ctx);
        }

        self.insert_entry(&entry);
    }

    /// Insert an entry into the database
    fn insert_entry(&self, entry: &AuditEntry) {
        // Pre-compute every value
        // that doesn't need DB access BEFORE acquiring the SQLite
        // mutex. Redaction parses + walks JSON (potentially
        // multi-KB payloads) and was previously running while
        // holding the lock, blocking every concurrent audit
        // writer for the full parse+walk duration. Now the lock
        // is held only across the SELECT(prev_hash) → INSERT
        // pair plus a fixed-size sha256 — bounded CPU work, not
        // input-size-dependent.
        let user_context_json = entry.user_context.as_ref().map(|c| c.to_json());
        let execution_time_ms = entry.execution_time.map(|d| d.as_millis() as i64);

        // ADR-027 D13c — redact secret-shaped fields in
        // `arguments` and `result` JSON before persisting (each
        // gated on its own config flag, default-on). The audit
        // DB lives on disk for `max_age_days` and ends up in
        // user backups; persisting verbatim secrets here has a
        // long disclosure tail. Tool name, risk tier,
        // success/failure, and timing all stay intact — that's
        // the audit value the operator needs.
        //
        // **Order matters**: redaction runs BEFORE the D13b
        // canonical row is built so the hash chain protects the
        // post-redaction bytes — the same bytes a verifier reads
        // back from disk. Hashing pre-redaction values would
        // mean every legitimate write trips the chain (in-memory
        // != persisted) and verify_chain would always fail.
        let arguments_for_storage = if self.config.redact_arguments {
            super::redaction::redact_audit_field(entry.arguments.as_deref())
        } else {
            entry.arguments.clone()
        };
        // Redact, THEN truncate.
        // Pre-fix `with_result` truncated by raw-byte slicing,
        // which (a) could split a UTF-8 character and (b)
        // routinely produced invalid JSON — the redactor's
        // parse-then-walk pipeline would then fail to parse
        // the truncated string and return it unchanged, leaking
        // secrets that lived past the truncation point. Now
        // `with_result` stores the raw string and truncation
        // happens here, after redaction has already replaced
        // any secret-shaped values.
        let result_redacted = if self.config.redact_results {
            super::redaction::redact_audit_field(entry.result.as_deref())
        } else {
            entry.result.clone()
        };
        let result_for_storage = result_redacted.map(truncate_for_storage);

        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to acquire audit database lock: {}", e);
                return;
            }
        };

        // ADR-027 D13b — append-only hash chain. Read the
        // most-recent entry's `entry_hash` to use as our
        // `prev_hash`; if the table is empty, we're the genesis
        // row and use [`GENESIS_PREV_HASH`].
        //
        // Same-process serialization is enforced by the
        // `Mutex<Connection>` lock we hold across SELECT and
        // INSERT here — concurrent same-process callers can't
        // fork the chain. The unguarded case is **multiple
        // separate processes / connections to the same
        // audit.db file**: there the SELECT-then-INSERT race
        // is real and would let two writers compute the same
        // prev_hash, breaking the chain. Conductor today only
        // opens one AuditLogger per daemon process, so this
        // doesn't bite. If multi-process audit writing ever
        // ships, switch this code path to wrap the read+insert
        // in a SQLite `BEGIN IMMEDIATE` transaction.
        // Use SQLite's auto-incrementing `rowid` (strictly
        // monotonic on insert order) rather than `created_at`
        // (millisecond resolution, ties on rapid inserts) or
        // the row's `id` (random UUID, no temporal ordering).
        // This must agree with `verify_chain`'s ORDER BY rowid
        // ASC so the inserter and verifier see the same chain.
        // Treat ONLY
        // `QueryReturnedNoRows` as the genesis case. Any other
        // error here (DB locked, schema mismatch, corruption)
        // would otherwise have caused a fresh-genesis insert
        // that re-roots the chain mid-audit-log without
        // surfacing the failure. Now we abort the insert and
        // log the underlying error — the audit entry is lost
        // (we can't write without a valid prev_hash), but the
        // chain integrity stays intact and the failure is
        // visible.
        let prev_hash: String = match conn.query_row(
            "SELECT entry_hash FROM audit_log \
             WHERE entry_hash IS NOT NULL \
             ORDER BY rowid DESC \
             LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(h) => h,
            Err(rusqlite::Error::QueryReturnedNoRows) => GENESIS_PREV_HASH.to_string(),
            Err(e) => {
                error!(
                    "Failed to read prev_hash for audit chain (skipping insert \
                     to preserve chain integrity): {}",
                    e,
                );
                return;
            }
        };

        // Hash the persisted-form
        // bytes (the raw values bound to the SQL params), NOT
        // the in-memory `&AuditEntry`. The verifier reads the
        // same persisted-form values from SQLite, so both sides
        // derive identical bytes and the chain detects every
        // byte-level mutation. With D13c, the persisted bytes
        // are the post-redaction bytes — see the redaction
        // block above.
        // ADR-034 §D4.A.3.3.B.2: serialise the optional `Provenance`
        // to JSON for storage in the v3 column. NULL when the entry
        // carries no provenance (e.g. ReadOnly tool calls). Computed
        // BEFORE the canonical row so it feeds the hash chain:
        // the SAME serialized string is both stored AND hashed, so the
        // verifier (which reads the stored column) recomputes an
        // identical hash, and any tamper of the column is detectable.
        let provenance_json = entry
            .provenance
            .as_ref()
            .and_then(|p| serde_json::to_string(p).ok());

        let canonical = CanonicalRow {
            id: &entry.id,
            event_type: entry.event_type.as_str(),
            tool_name: entry.tool_name.as_deref(),
            user_context: user_context_json.as_deref(),
            arguments: arguments_for_storage.as_deref(),
            result: result_for_storage.as_deref(),
            risk_tier: entry.risk_tier.as_str(),
            is_error: entry.is_error as i32,
            error_message: entry.error_message.as_deref(),
            execution_time_ms,
            created_at: entry.created_at,
            provenance: provenance_json.as_deref(),
        };
        let entry_hash = compute_entry_hash(&prev_hash, &canonical_persisted_bytes(&canonical));

        let result = conn.execute(
            r#"
            INSERT INTO audit_log (
                id, event_type, tool_name, user_context, arguments, result,
                risk_tier, is_error, error_message, execution_time_ms, created_at,
                prev_hash, entry_hash, provenance
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            "#,
            params![
                entry.id,
                entry.event_type.as_str(),
                entry.tool_name,
                user_context_json,
                arguments_for_storage,
                result_for_storage,
                entry.risk_tier.as_str(),
                entry.is_error as i32,
                entry.error_message,
                execution_time_ms,
                entry.created_at,
                prev_hash,
                entry_hash,
                provenance_json,
            ],
        );

        if let Err(e) = result {
            error!("Failed to insert audit entry: {}", e);
        } else {
            debug!(
                "Audit: {} {} (tier: {})",
                entry.event_type.as_str(),
                entry.tool_name.as_deref().unwrap_or("n/a"),
                entry.risk_tier.as_str()
            );

            // ADR-027 D13a — publish to the live stream
            // ONLY after the durable SQLite write succeeds, so a
            // `conductorctl audit tail` subscriber never observes
            // an event that isn't also in the persistent log.
            //
            // The broadcast payload carries the POST-redaction /
            // post-truncation `arguments` + `result` (the same
            // bytes persisted) — the live tail must not leak
            // secret-shaped values that D13c stripped from disk.
            //
            // Drop the SQLite mutex before the (lock-free) send so
            // a slow broadcast path can't extend the DB-write
            // critical section.
            drop(conn);
            let mut streamed = entry.clone();
            streamed.arguments = arguments_for_storage;
            streamed.result = result_for_storage;
            // `send` errors only when there are zero receivers —
            // expected and harmless (nobody's tailing). The
            // persistent log is the durable record regardless.
            let _ = self.event_tx.send(streamed);
        }
    }

    /// Walk the audit log in insertion order and verify the
    /// hash chain end-to-end. ADR-027 D13b.
    ///
    /// Returns `Ok(verified_count)` when the chain is intact —
    /// every row's stored `entry_hash` matches a re-derivation
    /// from `(prev_hash, canonical_persisted_bytes)`, every
    /// row's `prev_hash` matches the previous row's
    /// `entry_hash`, and the first row's `prev_hash` is
    /// [`GENESIS_PREV_HASH`]. Returns `Err(ChainBreak)`
    /// describing the first detected integrity failure or DB
    /// error (the verifier returns `ChainBreak::DbError` for
    /// non-integrity issues like lock-poisoning so triage can
    /// distinguish "log was tampered" from "couldn't read it").
    ///
    /// **Tamper-evidence, not tamper-prevention.** An attacker
    /// with write access to the SQLite file can still mutate
    /// rows; this function makes the mutation visible the next
    /// time it's called. Schedule it from
    /// `conductorctl audit verify` (D13a observability layer)
    /// or from a periodic background task; the spec considers
    /// detection-on-demand sufficient for the threat model.
    pub fn verify_chain(&self) -> Result<usize, ChainBreak> {
        let conn = self.conn.lock().map_err(|e| ChainBreak::DbError {
            details: format!("audit DB mutex poisoned: {e}"),
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, event_type, tool_name, user_context, arguments, \
                        result, risk_tier, is_error, error_message, \
                        execution_time_ms, created_at, prev_hash, entry_hash, \
                        provenance \
                 FROM audit_log \
                 ORDER BY rowid ASC",
            )
            .map_err(|e| ChainBreak::DbError {
                details: format!("prepare verify_chain stmt: {e}"),
            })?;

        // Read raw column values — no lossy enum/JSON parsing.
        // Strings stay as the
        // exact bytes that were written; ms stays as the i64
        // that was written. Both insert and verify build the
        // hash from this same byte-shape.
        let rows = stmt
            .query_map([], |row| {
                Ok(StoredChainRow {
                    id: row.get(0)?,
                    event_type: row.get(1)?,
                    tool_name: row.get(2)?,
                    user_context: row.get(3)?,
                    arguments: row.get(4)?,
                    result: row.get(5)?,
                    risk_tier: row.get(6)?,
                    is_error: row.get(7)?,
                    error_message: row.get(8)?,
                    execution_time_ms: row.get(9)?,
                    created_at: row.get(10)?,
                    prev_hash: row.get(11)?,
                    entry_hash: row.get(12)?,
                    provenance: row.get(13)?,
                })
            })
            .map_err(|e| ChainBreak::DbError {
                details: format!("query verify_chain rows: {e}"),
            })?;

        let mut expected_prev = GENESIS_PREV_HASH.to_string();
        let mut count = 0usize;
        let mut first_row = true;
        for row in rows {
            let row = row.map_err(|e| ChainBreak::DbError {
                details: format!("decode audit row: {e}"),
            })?;
            let entry_id = row.id.clone();
            // Legacy v1
            // rows (pre-D13b) carry NULL in BOTH hash columns —
            // `schema.rs` documents that they "stay NULL" and
            // that the v2 segment forms a chain "rooted at the
            // v1→v2 boundary". Skip them at the head of the
            // table; once we've started verifying (first_row
            // false), a NULL hash IS corruption.
            let (stored_prev, stored_hash) = match (row.prev_hash, row.entry_hash) {
                (None, None) if first_row => continue,
                (Some(p), Some(h)) => (p, h),
                _ => {
                    return Err(ChainBreak::MissingHash { entry_id });
                }
            };

            if let Some(ms) = row.execution_time_ms
                && ms < 0
            {
                return Err(ChainBreak::NegativeExecutionTime { entry_id, ms });
            }

            if first_row && stored_prev != GENESIS_PREV_HASH {
                return Err(ChainBreak::BrokenGenesis {
                    entry_id,
                    found_prev: stored_prev,
                });
            }
            if !first_row && stored_prev != expected_prev {
                return Err(ChainBreak::BrokenLink {
                    entry_id,
                    expected_prev,
                    found_prev: stored_prev,
                });
            }
            let canonical = CanonicalRow {
                id: &row.id,
                event_type: &row.event_type,
                tool_name: row.tool_name.as_deref(),
                user_context: row.user_context.as_deref(),
                arguments: row.arguments.as_deref(),
                result: row.result.as_deref(),
                risk_tier: &row.risk_tier,
                is_error: row.is_error,
                error_message: row.error_message.as_deref(),
                execution_time_ms: row.execution_time_ms,
                created_at: row.created_at,
                provenance: row.provenance.as_deref(),
            };
            let recomputed =
                compute_entry_hash(&stored_prev, &canonical_persisted_bytes(&canonical));
            if recomputed != stored_hash {
                return Err(ChainBreak::HashMismatch {
                    entry_id,
                    recomputed,
                    stored: stored_hash,
                });
            }
            expected_prev = stored_hash;
            first_row = false;
            count += 1;
        }
        Ok(count)
    }

    /// Clean up old entries
    /// Delete audit entries older than `max_age_days`.
    ///
    /// **D13b interaction:**
    /// retention deletion fundamentally conflicts with an
    /// append-only hash chain — any DELETE leaves the new
    /// first-row's `prev_hash` pointing at a now-missing
    /// previous-row's `entry_hash`, which `verify_chain`
    /// reports as `BrokenLink` (indistinguishable from an
    /// attacker deletion).
    ///
    /// This implementation rolls a new chain segment on each
    /// cleanup: after deleting expired rows, the first
    /// surviving row's `prev_hash` is rewritten to
    /// [`GENESIS_PREV_HASH`] and its `entry_hash` is
    /// recomputed against the new prev. The chain validates
    /// from there forward; the segment boundary is implicit
    /// (the row whose `prev_hash == GENESIS` mid-table is the
    /// post-cleanup re-root).
    ///
    /// This is a deliberate tamper-detection trade-off: a
    /// privileged attacker who replaces all surviving rows
    /// with their own chain (rooted at GENESIS) cannot be
    /// distinguished from a legitimate cleanup. ADR-027 §D13b
    /// accepts this as the cost of detection-on-demand
    /// without external anchoring; the assumed adversary is
    /// "casual UPDATE-without-fixup", not "privileged DB
    /// rewriter". Stronger guarantees (signed entries,
    /// periodic external anchoring) are tracked under D15.
    ///
    /// Operators who need to DETECT cleanup-vs-tamper should
    /// run `verify_chain()` BEFORE cleanup and log the result.
    pub fn cleanup(&self) -> SqliteResult<u64> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| rusqlite::Error::ExecuteReturnedResults)?;

        let cutoff = chrono::Utc::now().timestamp_millis()
            - (self.config.max_age_days as i64 * 24 * 60 * 60 * 1000);

        // Wrap DELETE +
        // chain-rebuild in a single SQLite transaction so the
        // audit_log is updated atomically. Pre-fix the DELETE
        // committed immediately and the per-row UPDATEs each
        // ran in their own implicit transaction; if cleanup
        // crashed mid-rebuild the audit log was left with a
        // partially-rewritten chain that `verify_chain` would
        // report as BrokenLink. With this transaction either
        // the whole cleanup commits or the DELETE rolls back —
        // chain integrity preserved.
        let tx = conn.transaction()?;

        let deleted = tx.execute(
            "DELETE FROM audit_log WHERE created_at < ?1",
            params![cutoff],
        )?;

        if deleted > 0 {
            info!("Audit cleanup: removed {} old entries", deleted);

            // Rebuild the chain over surviving v2 rows so
            // verify_chain still passes. Each surviving v2 row
            // (in rowid order) gets a new prev_hash + new
            // entry_hash:
            //   - first surviving v2 row: prev = GENESIS
            //   - subsequent rows: prev = previous-row's-new-entry_hash
            //
            // Re-rooting only the first row would leave row 2's
            // stored prev_hash pointing at row 1's OLD entry_hash;
            // since we recompute row 1's entry_hash, row 2's
            // chain link breaks. So we walk the full surviving
            // v2 table and rebuild forward.
            //
            // Scope the
            // rebuild to rows where `entry_hash IS NOT NULL`.
            // Legacy v1 rows (pre-D13b) sit at the head of the
            // table with both hash columns NULL; the migration
            // contract is that they "stay NULL". Pre-fix the
            // rebuild swept them in and silently backfilled
            // their hashes — verify_chain's leading-NULL-skip
            // (above) plus this scoped rebuild together honour
            // the documented design.
            let rows = rebuild_hashed_chain_segment(&tx)?;
            debug!("Audit cleanup: rebuilt chain over {} surviving rows", rows);
        }

        tx.commit()?;
        Ok(deleted as u64)
    }

    /// Get the total number of entries
    pub fn count(&self) -> SqliteResult<u64> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| rusqlite::Error::ExecuteReturnedResults)?;

        // rusqlite 0.40 dropped the blanket FromSql for u64; COUNT is
        // non-negative, so read as i64 and widen.
        conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| {
            Ok(row.get::<_, i64>(0)? as u64)
        })
    }

    /// **Test-only.** Lock the connection and return a guard so
    /// integration tests can simulate direct-DB tamper (e.g.
    /// `UPDATE audit_log SET ...` to test that
    /// [`verify_chain`](Self::verify_chain) detects it).
    /// Production callers MUST NOT use this — bypasses the
    /// hash chain by design.
    #[cfg(test)]
    pub(super) fn conn_for_tests(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("test conn lock")
    }
}

/// Raw-column row read by [`AuditLogger::verify_chain`].
/// Holds the persisted bytes verbatim (strings as stored,
/// JSON as a blob, ms as the i64 the column holds) so the
/// canonical-hash bytes are byte-identical to what was hashed
/// at insert time. Replaced the
/// old `AuditChainRow` which decoded to in-memory enums and
/// `Option<UserContext>` — that decoding was lossy and let
/// some tampering go undetected.
struct StoredChainRow {
    id: String,
    event_type: String,
    tool_name: Option<String>,
    user_context: Option<String>,
    arguments: Option<String>,
    result: Option<String>,
    risk_tier: String,
    is_error: i32,
    error_message: Option<String>,
    execution_time_ms: Option<i64>,
    created_at: i64,
    prev_hash: Option<String>,
    entry_hash: Option<String>,
    provenance: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_logger_creation() {
        let logger = AuditLogger::in_memory().unwrap();
        assert_eq!(logger.count().unwrap(), 0);
    }

    #[test]
    fn log_network_event_persists_a_queryable_entry() {
        let logger = AuditLogger::in_memory().unwrap();
        let sender: IpAddr = "127.0.0.1".parse().unwrap();
        logger.log_network_event(
            AuditEventType::NetworkListenerActivity,
            "osc_in",
            sender,
            Some("accepted"),
        );
        assert_eq!(logger.count().unwrap(), 1);

        let query = AuditQuery {
            event_type: Some(AuditEventType::NetworkListenerActivity),
            ..AuditQuery::default()
        };
        let entries = logger.query(&query).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tool_name.as_deref(), Some("osc_in"));
        assert_eq!(entries[0].risk_tier, AuditRiskTier::Internal);
        let args = entries[0].arguments.as_deref().unwrap();
        assert!(args.contains("127.0.0.1"), "sender recorded: {args}");
        assert!(args.contains("accepted"), "summary recorded: {args}");
    }

    #[test]
    fn truncate_for_storage_respects_hard_cap() {
        // Pre-fix
        // truncated input at RESULT_MAX_BYTES then APPENDED
        // the marker, so the stored string overflowed the cap
        // by `marker.len()`. Now budget reserves marker space
        // up front; final string length must be <= cap.
        let oversize = "x".repeat(RESULT_MAX_BYTES * 3);
        let truncated = truncate_for_storage(oversize);
        assert!(
            truncated.len() <= RESULT_MAX_BYTES,
            "truncate_for_storage produced {} bytes; hard cap \
             is {}. Review fix regressed.",
            truncated.len(),
            RESULT_MAX_BYTES,
        );
        assert!(
            truncated.ends_with(TRUNCATION_MARKER),
            "marker must be present so operators see the \
             truncation; got tail: {:?}",
            &truncated[truncated.len().saturating_sub(20)..],
        );
    }

    #[test]
    fn truncate_for_storage_passes_through_under_cap() {
        let small = "fits comfortably under the cap".to_string();
        let original = small.clone();
        let out = truncate_for_storage(small);
        assert_eq!(out, original);
    }

    #[test]
    fn test_log_tool_complete() {
        let logger = AuditLogger::in_memory().unwrap();

        logger.log_tool_complete(
            "conductor_get_config",
            AuditRiskTier::ReadOnly,
            Some(r#"{"path": "modes"}"#),
            Some(r#"{"modes": []}"#),
            Duration::from_millis(15),
            Some(UserContext::local_user()),
        );

        assert_eq!(logger.count().unwrap(), 1);

        let entries = logger.query(&AuditQuery::default()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].tool_name,
            Some("conductor_get_config".to_string())
        );
        assert_eq!(entries[0].event_type, AuditEventType::ToolComplete);
        assert!(!entries[0].is_error);
    }

    #[test]
    fn test_log_tool_error() {
        let logger = AuditLogger::in_memory().unwrap();

        logger.log_tool_error(
            "conductor_update_mode",
            AuditRiskTier::ConfigChange,
            Some(r#"{"name": "test"}"#),
            "Mode not found",
            Duration::from_millis(5),
            None,
        );

        let entries = logger
            .query(&AuditQuery {
                errors_only: true,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_error);
        assert_eq!(entries[0].error_message, Some("Mode not found".to_string()));
    }

    // ── ADR-034 §D8.3 — pending-at-crash audit emission ──────────

    #[test]
    fn log_config_mutation_pending_at_crash_persists_id_and_revision() {
        let logger = AuditLogger::in_memory().unwrap();

        let row_id = logger.log_config_mutation_pending_at_crash("mut-abc", Some("rev-7"));
        assert!(!row_id.is_empty());

        let entries = logger.query(&AuditQuery::default()).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.event_type, AuditEventType::ConfigMutationPendingAtCrash);
        // Internal tier — a system-recovery event, always logged.
        assert_eq!(e.risk_tier, AuditRiskTier::Internal);
        // The mutation id and intended revision are recorded for forensics.
        let args = e.arguments.as_deref().unwrap_or("");
        assert!(args.contains("mut-abc"), "args missing id: {args}");
        assert!(args.contains("rev-7"), "args missing revision: {args}");
    }

    #[test]
    fn log_config_mutation_pending_at_crash_tolerates_missing_revision() {
        let logger = AuditLogger::in_memory().unwrap();
        logger.log_config_mutation_pending_at_crash("mut-no-rev", None);

        let entries = logger.query(&AuditQuery::default()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].event_type,
            AuditEventType::ConfigMutationPendingAtCrash
        );
    }

    #[test]
    fn log_pending_at_crash_batch_emits_one_event_per_pending_mutation() {
        let logger = AuditLogger::in_memory().unwrap();
        let pending = vec![
            ReconciledMutation {
                id: "m1".to_string(),
                disposition: MutationDisposition::PendingAtCrash,
                intended_revision: Some("r1".to_string()),
            },
            ReconciledMutation {
                id: "m2".to_string(),
                disposition: MutationDisposition::PendingAtCrash,
                intended_revision: None,
            },
        ];

        let n = logger.log_pending_at_crash_batch(&pending);
        assert_eq!(n, 2);
        assert_eq!(logger.count().unwrap(), 2);
        let entries = logger.query(&AuditQuery::default()).unwrap();
        assert!(
            entries
                .iter()
                .all(|e| e.event_type == AuditEventType::ConfigMutationPendingAtCrash)
        );
    }

    #[test]
    fn log_pending_at_crash_batch_is_noop_on_empty() {
        let logger = AuditLogger::in_memory().unwrap();
        let n = logger.log_pending_at_crash_batch(&[]);
        assert_eq!(n, 0);
        assert_eq!(logger.count().unwrap(), 0);
    }

    #[test]
    fn test_log_plan_operations() {
        let logger = AuditLogger::in_memory().unwrap();

        logger.log_plan_created("plan_123", 3, None);
        logger.log_plan_applied("plan_123", 3, Duration::from_millis(100), None, None);

        let entries = logger.query(&AuditQuery::default()).unwrap();
        assert_eq!(entries.len(), 2);

        // Most recent first
        assert_eq!(entries[0].event_type, AuditEventType::PlanApplied);
        assert_eq!(entries[1].event_type, AuditEventType::PlanCreated);
    }

    #[test]
    fn test_skip_readonly_when_disabled() {
        // Exercises the `log_readonly` config branch directly. Uses
        // `AuditLogger::new` (not `in_memory()`, which hardcodes the
        // default `log_readonly: true`) with an in-memory SQLite path,
        // so the test actually validates the behaviour its name claims.
        //
        // The previous version
        // built a `log_readonly: false` config but never passed it to
        // a logger — it asserted on a separate `in_memory()` instance
        // and so only proved "in-memory logs everything".
        let logger = AuditLogger::new(AuditLoggerConfig {
            log_readonly: false,
            db_path: std::path::PathBuf::from(":memory:"),
            ..AuditLoggerConfig::default()
        })
        .unwrap();

        // ReadOnly tier — must be skipped because log_readonly = false.
        logger.log_tool_complete(
            "conductor_get_config",
            AuditRiskTier::ReadOnly,
            None,
            None,
            Duration::from_millis(10),
            None,
        );
        assert_eq!(
            logger.count().unwrap(),
            0,
            "ReadOnly entry should be skipped when log_readonly = false"
        );

        // Non-ReadOnly tier — still logged under the same config; the
        // skip only applies to ReadOnly.
        logger.log_tool_complete(
            "conductor_apply_config",
            AuditRiskTier::ConfigChange,
            None,
            None,
            Duration::from_millis(10),
            None,
        );
        assert_eq!(
            logger.count().unwrap(),
            1,
            "ConfigChange entry should still be logged when log_readonly = false"
        );
    }
}
