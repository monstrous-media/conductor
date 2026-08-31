// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Comprehensive Audit Logging for MCP Tool Executions (P4-04)
//!
//! This module provides SQLite-backed audit logging for ALL tool executions
//! with user context, risk tier tracking, and execution time metrics.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     MCP Tool Executor                        │
//! │                           │                                  │
//! │                           ▼                                  │
//! │  ┌─────────────────────────────────────────────────────────┐ │
//! │  │                    AuditLogger                          │ │
//! │  │  - log_tool_start()                                     │ │
//! │  │  - log_tool_complete()                                  │ │
//! │  │  - log_tool_error()                                     │ │
//! │  └─────────────────────────────────────────────────────────┘ │
//! │                           │                                  │
//! │                           ▼                                  │
//! │  ┌─────────────────────────────────────────────────────────┐ │
//! │  │                  SQLite Database                        │ │
//! │  │  - audit_log table                                      │ │
//! │  │  - Indexed by timestamp, tool_name, risk_tier           │ │
//! │  └─────────────────────────────────────────────────────────┘ │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Security Considerations
//!
//! - Audit logs are append-only (no UPDATE/DELETE in normal operation)
//! - Includes user context (Unix UID/GID or OAuth client_id)
//! - All tool executions logged regardless of risk tier
//! - Logs can be queried for compliance audits

mod hash_chain; // ADR-027 D13b: append-only hash chain
pub mod jsonl; // ADR-045 D5 (#2493): always-compiled append-only JSONL sink
// ADR-045 D1 (#2492): the SQLite-backed logger (rusqlite) is the only part
// of the audit subsystem gated behind `audit-db`. Types, redaction, the
// hash chain, and the outbox stay in every composition — story A2 (#2493)
// adds the always-compiled JSONL sink behind the AuditSink seam (D5).
#[cfg(feature = "audit-db")]
mod init; // Issue #1038: production AuditLogger wire-up entry point
#[cfg(feature = "audit-db")]
mod logger;
mod outbox; // ADR-034 §D8: two-phase durable audit outbox (#1902)
mod reconcile; // ADR-034 §D8.3: startup reconciliation of the outbox (#1902)
mod redaction; // ADR-027 D13c: PII redaction
mod schema;
pub mod sink; // ADR-045 D5 (#2493): the AuditSink seam

pub use hash_chain::{ChainBreak, GENESIS_PREV_HASH};
#[cfg(feature = "audit-db")]
pub use init::{create_audit_logger, default_audit_logger};
pub use jsonl::{
    JsonlAuditSink, JsonlChainBreak, JsonlSinkConfig, default_jsonl_sink, verify_jsonl_chain,
};
#[cfg(feature = "audit-db")]
pub use logger::{AuditLogger, AuditLoggerConfig};
pub use outbox::{
    AuditOutbox, MAX_OUTBOX_BYTES, OUTBOX_CAP, OutboxEntry, OutboxError, OutboxPhase, OutboxRecord,
    last_valid_entry_hash, read_entries as read_outbox_entries,
};
pub use reconcile::{
    MutationDisposition, ReconciledMutation, pending_at_crash, reconcile as reconcile_outbox,
};
pub use redaction::redact_audit_field;
pub use schema::{
    AuditEntry, AuditEventType, AuditQuery, AuditRiskTier, AuditSummary, UserContext,
};
pub use sink::AuditSink;

#[cfg(all(test, feature = "audit-db"))]
mod tests;
