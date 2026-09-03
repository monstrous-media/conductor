// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Audit log schema and data types

use conductor_core::config::Provenance;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Schema version for migrations.
///
/// **v2** (ADR-027 D13b, 2026-05-02): adds `prev_hash` and
/// `entry_hash` columns to `audit_log` for the append-only hash
/// chain. Migration on open runs both
/// [`MIGRATE_V1_TO_V2_ADD_PREV_HASH`] and
/// [`MIGRATE_V1_TO_V2_ADD_ENTRY_HASH`] (these are the actual constant
/// names — an earlier revision of this doc referenced a non-existent
/// `MIGRATE_V1_TO_V2_ADD_HASH_COLUMNS`).
///
/// **v3** (ADR-034 §D4.A.3.3.B.2, 2026-05-17): adds the `provenance`
/// column to record the `Provenance { initiator, source, peer }`
/// triple for every audit entry (per ADR-034 §D6). The migration is
/// [`MIGRATE_V2_TO_V3_ADD_PROVENANCE`]. Existing v2 rows load with
/// `provenance = None` — backwards-compatible, no rehash required.
///
/// **v4** (2026-06-07): one-time chain rebuild for
/// pre-existing v3 rows that already carry non-NULL `provenance`.
/// Those rows were historically hashed without the provenance segment.
/// Once provenance became part of `CanonicalRow`, reopening such a DB
/// would otherwise yield `ChainBreak::HashMismatch` until rebuild.
///
/// **Hash chain scope:** the v3 `provenance` column IS part of
/// `CanonicalRow`/`compute_entry_hash` — tampering with it breaks
/// `verify_chain`. `CanonicalRow.provenance` uses
/// `#[serde(skip_serializing_if = "Option::is_none")]`, so a `None`
/// provenance omits the field entirely and the canonical bytes are
/// byte-identical to the pre-v3 form.
///
/// Compatibility boundary (precise): every row whose `provenance` is
/// NULL — all v2 rows, and all v3 rows that carry no provenance — verifies
/// unchanged. For historical v3 rows with non-NULL provenance, the v4
/// migration re-derives `entry_hash` from the current canonical form.
#[cfg(feature = "audit-db")]
pub const SCHEMA_VERSION: i32 = 4;

/// SQL to create the audit_log table (v3 schema — includes provenance).
#[cfg(feature = "audit-db")]
pub const CREATE_AUDIT_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS audit_log (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    tool_name TEXT,
    user_context TEXT,
    arguments TEXT,
    result TEXT,
    risk_tier TEXT NOT NULL,
    is_error INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    execution_time_ms INTEGER,
    created_at INTEGER NOT NULL,
    prev_hash TEXT,
    entry_hash TEXT,
    provenance TEXT
);

CREATE INDEX IF NOT EXISTS idx_audit_created_at ON audit_log(created_at);
CREATE INDEX IF NOT EXISTS idx_audit_tool_name ON audit_log(tool_name);
CREATE INDEX IF NOT EXISTS idx_audit_risk_tier ON audit_log(risk_tier);
CREATE INDEX IF NOT EXISTS idx_audit_user_context ON audit_log(user_context);
"#;

/// SQL to migrate a v1 audit_log table (no hash columns) to v2.
/// Adds the two columns as NULLable so existing rows survive —
/// they're treated by the chain verifier as un-hashed legacy
/// entries; new rows from v2 onwards carry hashes and form a
/// chain rooted at the v1→v2 boundary.
///
/// The migration helper `run_idempotent_alter` in
/// [`crate::daemon::audit::AuditLogger::new`] checks column
/// existence via `PRAGMA table_info` before running each ALTER,
/// so these statements are safe to run on both fresh v2 DBs
/// (where `CREATE_AUDIT_TABLE` already created the columns) and
/// existing v1 DBs. Any PRAGMA or ALTER error (DB locked,
/// corrupt file, missing table) is surfaced so the migration
/// doesn't silently bump `schema_version` to v2 while leaving
/// the columns missing.
#[cfg(feature = "audit-db")]
pub const MIGRATE_V1_TO_V2_ADD_PREV_HASH: &str = "ALTER TABLE audit_log ADD COLUMN prev_hash TEXT";
#[cfg(feature = "audit-db")]
pub const MIGRATE_V1_TO_V2_ADD_ENTRY_HASH: &str =
    "ALTER TABLE audit_log ADD COLUMN entry_hash TEXT";

/// SQL to migrate a v2 audit_log table (no provenance column) to v3.
/// Adds the `provenance` column NULLable so existing v2 rows survive
/// — they load with `provenance = None`. New rows from v3 onwards
/// populate the column with a JSON-serialised `Provenance`.
///
/// Wrapped by `run_idempotent_alter` in
/// [`crate::daemon::audit::AuditLogger::new`] so this is safe on
/// fresh v3 DBs (where `CREATE_AUDIT_TABLE` already created the
/// column) and existing v2 DBs (where the ALTER adds it).
#[cfg(feature = "audit-db")]
pub const MIGRATE_V2_TO_V3_ADD_PROVENANCE: &str =
    "ALTER TABLE audit_log ADD COLUMN provenance TEXT";

/// SQL to create the schema_version table
#[cfg(feature = "audit-db")]
pub const CREATE_SCHEMA_VERSION_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY
);
"#;

/// Types of audit events
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// Tool execution started
    ToolStart,
    /// Tool execution completed successfully
    ToolComplete,
    /// Tool execution failed with error
    ToolError,
    /// Tool execution was denied (permissions, rate limit, etc.)
    ToolDenied,
    /// Plan was created
    PlanCreated,
    /// Plan was applied
    PlanApplied,
    /// Plan was rejected
    PlanRejected,
    /// Configuration was changed
    ConfigChanged,
    /// Session started
    SessionStart,
    /// Session ended
    SessionEnd,
    /// An outbound network request was denied by the D17 egress allowlist
    /// (off-allowlist host, or DNS-rebinding to an internal address).
    EgressBlocked,
    /// ADR-042 Phase A — a packet was accepted at a network listener edge and
    /// handed to the protocol parser (network-listener activity).
    NetworkListenerActivity,
    /// ADR-042 Phase A — a network listener failed to bind its UDP socket.
    NetworkListenerBindFailed,
    /// ADR-042 Phase A (D17) — a network-origin trigger (incl. loopback
    /// OSC/Art-Net) was refused by the action-class gate for lack of
    /// `allow_sensitive_actions`.
    NetworkActionClassBlocked,
    /// ADR-042 Phase A (D11) — a listener's broad-broadcast amplification risk
    /// was acknowledged via `i_understand_amplification_risk`.
    AmplificationRiskAcknowledged,
    /// ADR-042 Phase A — a network listener's config changed across a reload
    /// (bind/unbind, ACL swap).
    NetworkListenerConfigChange,
    /// ADR-042 Phase A (Slice A.6.1) — a UDP socket bound by a prior crashed
    /// daemon was detected at startup (detection only in Phase A).
    ListenerOrphanedAtStartup,
    /// ADR-042 Phase B-early — the bind gate **withheld** a non-loopback listener
    /// (it is emitted only on the fail-closed path; a successful bind does not
    /// emit this row). The `summary` discriminates the reason (`awaiting_approval`
    /// / `registry_tampered` / `registry_unreadable` / `keychain_unavailable` /
    /// `keychain_expired`) and the arguments carry the `acl_hash` so a withheld
    /// listener's ACL is forensically recorded.
    NetworkListenerApproval,
    /// ADR-027 D6 — an LLM agentic-loop budget dimension (iterations, tool
    /// calls, tokens, wall-clock, or a capability-specific quota) was
    /// exhausted; the loop was halted.
    LlmBudgetExceeded,
    /// ADR-027 D10a — a `Shell` action was refused by the persistence write
    /// veto: its resolved argv would have written one of the daemon's own
    /// protected state directories. The process was never spawned.
    ShellVetoedByPersistenceCheck,
    /// ADR-034 §D2.2 — a caller-supplied `ReloadFromDisk` / `ImportConfig`
    /// path was refused by the safe-walk path validation (escaped the
    /// config-directory allowlist root, traversed a symlink, was not a
    /// regular file, or was owned by a foreign UID). No content was read.
    PathValidationFailed,
    /// ADR-034 §D8.3 — startup reconciliation found a config mutation that was
    /// `Pending` in the audit outbox when the previous daemon died and did NOT
    /// publish (the loaded `live.toml` revision does not match the row's
    /// `intended_revision`). The change was never applied; this event records
    /// the in-flight-at-crash mutation for the operator. Not a `Source`
    /// (provenance of an *applied* change) — the mutation was not applied.
    ConfigMutationPendingAtCrash,
}

impl AuditEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ToolStart => "tool_start",
            Self::ToolComplete => "tool_complete",
            Self::ToolError => "tool_error",
            Self::ToolDenied => "tool_denied",
            Self::PlanCreated => "plan_created",
            Self::PlanApplied => "plan_applied",
            Self::PlanRejected => "plan_rejected",
            Self::ConfigChanged => "config_changed",
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::EgressBlocked => "egress_blocked",
            Self::NetworkListenerActivity => "network_listener_activity",
            Self::NetworkListenerBindFailed => "network_listener_bind_failed",
            Self::NetworkActionClassBlocked => "network_action_class_blocked",
            Self::AmplificationRiskAcknowledged => "amplification_risk_acknowledged",
            Self::NetworkListenerConfigChange => "network_listener_config_change",
            Self::ListenerOrphanedAtStartup => "listener_orphaned_at_startup",
            Self::NetworkListenerApproval => "network_listener_approval",
            Self::LlmBudgetExceeded => "llm_budget_exceeded",
            Self::ShellVetoedByPersistenceCheck => "shell_vetoed_by_persistence_check",
            Self::PathValidationFailed => "path_validation_failed",
            Self::ConfigMutationPendingAtCrash => "config_mutation_pending_at_crash",
        }
    }

    /// Parse event type from string representation
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tool_start" => Some(Self::ToolStart),
            "tool_complete" => Some(Self::ToolComplete),
            "tool_error" => Some(Self::ToolError),
            "tool_denied" => Some(Self::ToolDenied),
            "plan_created" => Some(Self::PlanCreated),
            "plan_applied" => Some(Self::PlanApplied),
            "plan_rejected" => Some(Self::PlanRejected),
            "config_changed" => Some(Self::ConfigChanged),
            "session_start" => Some(Self::SessionStart),
            "session_end" => Some(Self::SessionEnd),
            "egress_blocked" => Some(Self::EgressBlocked),
            "network_listener_activity" => Some(Self::NetworkListenerActivity),
            "network_listener_bind_failed" => Some(Self::NetworkListenerBindFailed),
            "network_action_class_blocked" => Some(Self::NetworkActionClassBlocked),
            "amplification_risk_acknowledged" => Some(Self::AmplificationRiskAcknowledged),
            "network_listener_config_change" => Some(Self::NetworkListenerConfigChange),
            "listener_orphaned_at_startup" => Some(Self::ListenerOrphanedAtStartup),
            "network_listener_approval" => Some(Self::NetworkListenerApproval),
            "llm_budget_exceeded" => Some(Self::LlmBudgetExceeded),
            "shell_vetoed_by_persistence_check" => Some(Self::ShellVetoedByPersistenceCheck),
            "path_validation_failed" => Some(Self::PathValidationFailed),
            "config_mutation_pending_at_crash" => Some(Self::ConfigMutationPendingAtCrash),
            _ => None,
        }
    }
}

/// Risk tier for audit categorization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditRiskTier {
    /// Read-only operations (safe)
    ReadOnly,
    /// Stateful operations (tracked)
    Stateful,
    /// Configuration changes (requires approval)
    ConfigChange,
    /// Hardware I/O operations (dangerous)
    HardwareIO,
    /// Internal/system operations
    Internal,
}

impl AuditRiskTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Stateful => "stateful",
            Self::ConfigChange => "config_change",
            Self::HardwareIO => "hardware_io",
            Self::Internal => "internal",
        }
    }

    /// Parse risk tier from string representation
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read_only" => Some(Self::ReadOnly),
            "stateful" => Some(Self::Stateful),
            "config_change" => Some(Self::ConfigChange),
            "hardware_io" => Some(Self::HardwareIO),
            "internal" => Some(Self::Internal),
            _ => None,
        }
    }
}

/// User context for audit entries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserContext {
    /// Unix UID (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Unix GID (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    /// OAuth client ID (for remote access)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Session ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Remote IP address (for HTTP requests)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_ip: Option<String>,
}

impl UserContext {
    /// Create a context for a local Unix user
    #[cfg(unix)]
    pub fn local_user() -> Self {
        Self {
            uid: Some(unsafe { libc::getuid() }),
            gid: Some(unsafe { libc::getgid() }),
            client_id: None,
            session_id: None,
            remote_ip: None,
        }
    }

    /// Create a context for a local user (non-Unix fallback)
    #[cfg(not(unix))]
    pub fn local_user() -> Self {
        Self {
            uid: None,
            gid: None,
            client_id: None,
            session_id: None,
            remote_ip: None,
        }
    }

    /// Create a context for a remote OAuth client
    pub fn remote_client(client_id: String, remote_ip: Option<String>) -> Self {
        Self {
            uid: None,
            gid: None,
            client_id: Some(client_id),
            session_id: None,
            remote_ip,
        }
    }

    /// Create a context with session ID
    pub fn with_session(mut self, session_id: String) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Serialize to JSON string for storage
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Deserialize from JSON string
    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

/// A single audit log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Unique entry ID
    pub id: String,
    /// Event type
    pub event_type: AuditEventType,
    /// Tool name (if applicable)
    pub tool_name: Option<String>,
    /// User context
    pub user_context: Option<UserContext>,
    /// Tool arguments (JSON)
    pub arguments: Option<String>,
    /// Tool result (JSON, truncated if large)
    pub result: Option<String>,
    /// Risk tier
    pub risk_tier: AuditRiskTier,
    /// Whether this was an error
    pub is_error: bool,
    /// Error message (if is_error)
    pub error_message: Option<String>,
    /// Execution time
    pub execution_time: Option<Duration>,
    /// Timestamp (Unix milliseconds)
    pub created_at: i64,
    /// Mutation provenance (ADR-034 §D6, schema v3+).
    ///
    /// Populated for entries that record a config mutation — namely
    /// `PlanApplied`, `ConfigChanged`, and any `ToolComplete` /
    /// `ToolError` whose tier is `ConfigChange` or `HardwareIO`. The
    /// `Provenance { initiator, source, peer }` triple identifies
    /// **who** triggered the mutation (`Initiator`), **what was
    /// applied** (`Source`), and the authenticated peer when one
    /// exists.
    ///
    /// `Option<Provenance>` (not bare `Provenance`) so v2-era rows
    /// loaded from disk still deserialise; the
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// pair keeps the JSON wire-form clean for entries that don't
    /// carry provenance (e.g. `ReadOnly` tool calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

impl AuditEntry {
    /// Create a new audit entry with generated ID and timestamp
    pub fn new(event_type: AuditEventType, risk_tier: AuditRiskTier) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            event_type,
            tool_name: None,
            user_context: None,
            arguments: None,
            result: None,
            risk_tier,
            is_error: false,
            error_message: None,
            execution_time: None,
            created_at: chrono::Utc::now().timestamp_millis(),
            provenance: None,
        }
    }

    /// Set tool name
    pub fn with_tool(mut self, name: impl Into<String>) -> Self {
        self.tool_name = Some(name.into());
        self
    }

    /// Set user context
    pub fn with_user(mut self, ctx: UserContext) -> Self {
        self.user_context = Some(ctx);
        self
    }

    /// Set arguments (JSON string)
    pub fn with_arguments(mut self, args: impl Into<String>) -> Self {
        self.arguments = Some(args.into());
        self
    }

    /// Set result (raw JSON string).
    ///
    /// Pre-fix this truncated to
    /// 10KB by raw-byte slicing + appending `...[truncated]`,
    /// which (a) could split a UTF-8 character and (b) routinely
    /// produced invalid JSON. The downstream
    /// [`crate::daemon::audit::redact_audit_field`] would then
    /// fail to parse the truncated blob and return it unchanged
    /// — a redaction bypass for any oversize result that
    /// happened to contain secrets.
    ///
    /// Truncation now happens inside
    /// [`crate::daemon::audit::AuditLogger`] when persisting
    /// the entry — AFTER redaction — so secrets are filtered
    /// first and the truncated bytes are guaranteed to be
    /// post-redaction. `with_result` just stores the raw
    /// result string.
    pub fn with_result(mut self, result: impl Into<String>) -> Self {
        self.result = Some(result.into());
        self
    }

    /// Set execution time
    pub fn with_execution_time(mut self, duration: Duration) -> Self {
        self.execution_time = Some(duration);
        self
    }

    /// Mark as error
    pub fn with_error(mut self, message: impl Into<String>) -> Self {
        self.is_error = true;
        self.error_message = Some(message.into());
        self
    }

    /// Attach mutation provenance (ADR-034 §D6, schema v3+).
    pub fn with_provenance(mut self, prov: Provenance) -> Self {
        self.provenance = Some(prov);
        self
    }
}

/// Query parameters for audit log searches
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditQuery {
    /// Filter by tool name
    pub tool_name: Option<String>,
    /// Filter by risk tier
    pub risk_tier: Option<AuditRiskTier>,
    /// Filter by user context (JSON contains)
    pub user_context: Option<String>,
    /// Filter by event type
    pub event_type: Option<AuditEventType>,
    /// Filter errors only
    pub errors_only: bool,
    /// Start time (Unix milliseconds)
    pub start_time: Option<i64>,
    /// End time (Unix milliseconds)
    pub end_time: Option<i64>,
    /// Limit number of results
    pub limit: Option<u32>,
    /// Offset for pagination
    pub offset: Option<u32>,
}

/// Summary statistics for audit queries
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditSummary {
    /// Total number of entries matching query
    pub total_count: u64,
    /// Number of errors
    pub error_count: u64,
    /// Breakdown by risk tier
    pub by_risk_tier: std::collections::HashMap<String, u64>,
    /// Breakdown by tool name (top 10)
    pub by_tool_name: std::collections::HashMap<String, u64>,
    /// Average execution time (ms)
    pub avg_execution_time_ms: Option<f64>,
    /// Time range of entries
    pub time_range: Option<(i64, i64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_event_type_roundtrip() {
        // Cover ALL variants. If you add to
        // `AuditEventType`, add the variant here and to the `as_str` / `parse`
        // match arms.
        for event_type in [
            AuditEventType::ToolStart,
            AuditEventType::ToolComplete,
            AuditEventType::ToolError,
            AuditEventType::ToolDenied,
            AuditEventType::PlanCreated,
            AuditEventType::PlanApplied,
            AuditEventType::PlanRejected,
            AuditEventType::ConfigChanged,
            AuditEventType::SessionStart,
            AuditEventType::SessionEnd,
            AuditEventType::EgressBlocked,
            AuditEventType::NetworkListenerActivity,
            AuditEventType::NetworkListenerBindFailed,
            AuditEventType::NetworkActionClassBlocked,
            AuditEventType::AmplificationRiskAcknowledged,
            AuditEventType::NetworkListenerConfigChange,
            AuditEventType::ListenerOrphanedAtStartup,
            AuditEventType::NetworkListenerApproval,
            AuditEventType::LlmBudgetExceeded,
            AuditEventType::ShellVetoedByPersistenceCheck,
            AuditEventType::PathValidationFailed,
            AuditEventType::ConfigMutationPendingAtCrash,
        ] {
            let s = event_type.as_str();
            let parsed = AuditEventType::parse(s).unwrap();
            assert_eq!(event_type, parsed);
        }
    }

    #[test]
    fn test_audit_risk_tier_roundtrip() {
        for tier in [
            AuditRiskTier::ReadOnly,
            AuditRiskTier::Stateful,
            AuditRiskTier::ConfigChange,
            AuditRiskTier::HardwareIO,
            AuditRiskTier::Internal,
        ] {
            let s = tier.as_str();
            let parsed = AuditRiskTier::parse(s).unwrap();
            assert_eq!(tier, parsed);
        }
    }

    #[test]
    fn test_user_context_serialization() {
        let ctx = UserContext {
            uid: Some(1000),
            gid: Some(1000),
            client_id: None,
            session_id: Some("sess_123".to_string()),
            remote_ip: None,
        };

        let json = ctx.to_json();
        assert!(json.contains("1000"));
        assert!(json.contains("sess_123"));

        let parsed = UserContext::from_json(&json).unwrap();
        assert_eq!(parsed.uid, Some(1000));
        assert_eq!(parsed.session_id, Some("sess_123".to_string()));
    }

    #[test]
    fn test_audit_entry_builder() {
        let entry = AuditEntry::new(AuditEventType::ToolComplete, AuditRiskTier::ReadOnly)
            .with_tool("conductor_get_config")
            .with_arguments(r#"{"path": "modes"}"#)
            .with_result(r#"{"modes": []}"#)
            .with_execution_time(std::time::Duration::from_millis(15));

        assert_eq!(entry.tool_name, Some("conductor_get_config".to_string()));
        assert!(!entry.is_error);
        assert_eq!(
            entry.execution_time,
            Some(std::time::Duration::from_millis(15))
        );
    }

    #[test]
    fn test_audit_entry_result_stored_raw_without_truncation() {
        // Truncation moved out of
        // `with_result` and into the audit logger's
        // `insert_entry`, AFTER redaction. `with_result` now
        // stores the raw string verbatim. The pre-redaction
        // truncation was a redaction bypass (broke JSON
        // validity → redactor parse failed → secrets leaked
        // through truncation point); the post-redaction
        // truncation in `insert_entry` is integration-tested in
        // `daemon::audit::tests::d13c_oversize_result_redacts_before_truncating`.
        let large_result = "x".repeat(20 * 1024); // 20KB
        let entry = AuditEntry::new(AuditEventType::ToolComplete, AuditRiskTier::ReadOnly)
            .with_result(&large_result);

        let result = entry.result.unwrap();
        assert_eq!(
            result, large_result,
            "with_result no longer truncates — that's now the \
             logger's job, after redaction.",
        );
    }

    #[test]
    fn test_audit_entry_with_error() {
        let entry = AuditEntry::new(AuditEventType::ToolError, AuditRiskTier::ConfigChange)
            .with_tool("conductor_update_mode")
            .with_error("Mode not found");

        assert!(entry.is_error);
        assert_eq!(entry.error_message, Some("Mode not found".to_string()));
    }
}
