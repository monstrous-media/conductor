// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-045 D5 (#2493) — the `AuditSink` seam.
//!
//! Consumers (LLM executor, IPC `SubscribeAudit`, ADR-042 listener edge,
//! path-validation audit) write to this trait, never to a concrete sink:
//!
//! - The SQLite [`AuditLogger`](super::AuditLogger) implements it behind the
//!   `audit-db` feature (hash-chain per ADR-027 D13b, rich queries).
//! - The append-only JSONL sink ([`super::jsonl::JsonlAuditSink`]) is ALWAYS
//!   compiled — the free (no-SQLite) composition still gets a durable,
//!   line-hash-chained, redacted audit trail, so ADR-042's audit guarantees
//!   are composition-independent.
//!
//! Read-side queries (`QueryAudit`) stay SQLite-only: the JSONL sink is an
//! append-only trail, tailed live via [`AuditSink::subscribe`] and verified
//! offline via [`super::jsonl::verify_jsonl_chain`].

use std::net::IpAddr;
use std::time::Duration;

use tokio::sync::broadcast;

use super::reconcile::ReconciledMutation;
use super::schema::{AuditEntry, AuditEventType, AuditRiskTier, UserContext};
use conductor_core::config::Provenance;

/// Write-side audit seam (ADR-045 D5).
///
/// Method-for-method mirror of the [`AuditLogger`](super::AuditLogger)
/// write surface consumers already use, so the SQLite impl is pure
/// delegation and swapping sinks never touches call sites. All methods are
/// best-effort: a failing sink must never take down the caller (the D5
/// fail-closed rule for network listeners is enforced at listener *startup*,
/// not per write).
pub trait AuditSink: Send + Sync {
    /// Record the start of a tool execution. Returns the entry id.
    fn log_tool_start(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        user_context: Option<UserContext>,
    ) -> String;

    /// Record a completed tool execution.
    #[allow(clippy::too_many_arguments)]
    fn log_tool_complete(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        result: Option<&str>,
        execution_time: Duration,
        user_context: Option<UserContext>,
    );

    /// Record a failed tool execution.
    #[allow(clippy::too_many_arguments)]
    fn log_tool_error(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        error_message: &str,
        execution_time: Duration,
        user_context: Option<UserContext>,
    );

    /// Record a denied tool execution.
    fn log_tool_denied(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        reason: &str,
        user_context: Option<UserContext>,
    );

    /// ADR-027 D6: record an exhausted LLM budget dimension.
    fn log_llm_budget_exceeded(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        dimension: &str,
        limit: u64,
        observed: u64,
        user_context: Option<UserContext>,
    );

    /// ADR-042: record a network-listener event. Returns the entry id.
    fn log_network_event(
        &self,
        event_type: AuditEventType,
        listener: &str,
        ip: IpAddr,
        summary: Option<&str>,
    ) -> String;

    /// ADR-034 §D2.2: record a rejected path-validation attempt.
    fn log_path_validation_failed(&self, attempted_path: &str, reason: &str) -> String;

    /// ADR-034 §D8.3: record config mutations pending at crash. Returns the
    /// number of entries emitted.
    fn log_pending_at_crash_batch(&self, pending: &[ReconciledMutation]) -> usize;

    /// Plan lifecycle (ADR-007 Phase 2 / ADR-034).
    fn log_plan_created(
        &self,
        plan_id: &str,
        changes_count: usize,
        user_context: Option<UserContext>,
    );
    fn log_plan_applied(
        &self,
        plan_id: &str,
        changes_applied: usize,
        execution_time: Duration,
        user_context: Option<UserContext>,
        provenance: Option<Provenance>,
    );
    fn log_plan_rejected(&self, plan_id: &str, user_context: Option<UserContext>);

    /// Live tail of recorded entries (post-redaction), for `SubscribeAudit`.
    fn subscribe(&self) -> broadcast::Receiver<AuditEntry>;
}

/// SQLite sink (ADR-027 D13b): pure delegation to the inherent methods.
#[cfg(feature = "audit-db")]
impl AuditSink for super::AuditLogger {
    fn log_tool_start(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        user_context: Option<UserContext>,
    ) -> String {
        Self::log_tool_start(self, tool_name, risk_tier, arguments, user_context)
    }

    fn log_tool_complete(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        result: Option<&str>,
        execution_time: Duration,
        user_context: Option<UserContext>,
    ) {
        Self::log_tool_complete(
            self,
            tool_name,
            risk_tier,
            arguments,
            result,
            execution_time,
            user_context,
        )
    }

    fn log_tool_error(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        arguments: Option<&str>,
        error_message: &str,
        execution_time: Duration,
        user_context: Option<UserContext>,
    ) {
        Self::log_tool_error(
            self,
            tool_name,
            risk_tier,
            arguments,
            error_message,
            execution_time,
            user_context,
        )
    }

    fn log_tool_denied(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        reason: &str,
        user_context: Option<UserContext>,
    ) {
        Self::log_tool_denied(self, tool_name, risk_tier, reason, user_context)
    }

    fn log_llm_budget_exceeded(
        &self,
        tool_name: &str,
        risk_tier: AuditRiskTier,
        dimension: &str,
        limit: u64,
        observed: u64,
        user_context: Option<UserContext>,
    ) {
        Self::log_llm_budget_exceeded(
            self,
            tool_name,
            risk_tier,
            dimension,
            limit,
            observed,
            user_context,
        )
    }

    fn log_network_event(
        &self,
        event_type: AuditEventType,
        listener: &str,
        ip: IpAddr,
        summary: Option<&str>,
    ) -> String {
        Self::log_network_event(self, event_type, listener, ip, summary)
    }

    fn log_path_validation_failed(&self, attempted_path: &str, reason: &str) -> String {
        Self::log_path_validation_failed(self, attempted_path, reason)
    }

    fn log_pending_at_crash_batch(&self, pending: &[ReconciledMutation]) -> usize {
        Self::log_pending_at_crash_batch(self, pending)
    }

    fn log_plan_created(
        &self,
        plan_id: &str,
        changes_count: usize,
        user_context: Option<UserContext>,
    ) {
        Self::log_plan_created(self, plan_id, changes_count, user_context)
    }

    fn log_plan_applied(
        &self,
        plan_id: &str,
        changes_applied: usize,
        execution_time: Duration,
        user_context: Option<UserContext>,
        provenance: Option<Provenance>,
    ) {
        Self::log_plan_applied(
            self,
            plan_id,
            changes_applied,
            execution_time,
            user_context,
            provenance,
        )
    }

    fn log_plan_rejected(&self, plan_id: &str, user_context: Option<UserContext>) {
        Self::log_plan_rejected(self, plan_id, user_context)
    }

    fn subscribe(&self) -> broadcast::Receiver<AuditEntry> {
        Self::subscribe(self)
    }
}
