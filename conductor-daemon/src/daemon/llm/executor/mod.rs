// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Tool executor for MCP tools with risk tier handling (ADR-007 Phase 2)
//!
//! The ToolExecutor provides transport-agnostic tool execution with
//! different handling based on risk tier:
//! - ReadOnly: Auto-execute immediately
//! - Stateful: Execute with logging
//! - ConfigChange: Return ConfigPlan for user approval

use super::history::{HistoryError, HistorySummary, UndoStack};
use super::plan::{ConfigChange, ConfigPlan, PlanError};
use crate::daemon::audit::{AuditRiskTier, AuditSink, UserContext};
use crate::daemon::engine_manager::{MidiLearnEvent, SharedDaemonStateRefs};
use crate::daemon::hardware_io::{ConfirmationManager, ConfirmationStatus, MidiSendMessage};
use crate::daemon::mcp_tools::{McpToolExecutor, get_tool_risk_tier};
use crate::daemon::mcp_types::{ToolCallResult, ToolRiskTier};
use crate::daemon::ratelimit::{RateLimitConfig, RateLimitError, RateLimiter};
use crate::gamepad_device::HidDeviceManager;
use conductor_core::config::{ActionConfig, Config, Trigger};
use conductor_core::device_intelligence::probe::ProbeOutcomeWire;
use conductor_core::{EventType, PatternType};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Convert MCP ToolRiskTier to Audit AuditRiskTier
fn tool_risk_to_audit_risk(tier: &ToolRiskTier) -> AuditRiskTier {
    match tier {
        ToolRiskTier::ReadOnly => AuditRiskTier::ReadOnly,
        ToolRiskTier::Stateful | ToolRiskTier::ArtifactRender => AuditRiskTier::Stateful,
        ToolRiskTier::ConfigChange => AuditRiskTier::ConfigChange,
        ToolRiskTier::HardwareIO => AuditRiskTier::HardwareIO,
        ToolRiskTier::Privileged => AuditRiskTier::Internal,
    }
}

/// Result of tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ExecutionResult {
    /// Tool executed successfully, here's the result
    Success { result: ToolCallResult },

    /// Tool requires user approval (ConfigChange tier)
    PlanCreated { plan: ConfigPlan },

    /// Tool execution logged (Stateful tier)
    Logged {
        result: ToolCallResult,
        log_entry: LogEntry,
    },

    /// HardwareIO operation requires multi-step confirmation (P4-01)
    HardwareIoConfirmation {
        status: ConfirmationStatus,
        tool_name: String,
    },

    /// Tool execution blocked by rate limiting (P4-05)
    RateLimited {
        tier: ToolRiskTier,
        current: u32,
        limit: u32,
        retry_after_secs: u64,
    },

    /// Tool execution failed
    Error { message: String },
}

/// Log entry for stateful tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: Uuid,
    pub tool_name: String,
    pub arguments: Option<Value>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub result_summary: String,
}

/// Tool executor with risk tier handling
pub struct ToolExecutor {
    /// Configuration (shared with daemon)
    live_config: Arc<crate::daemon::live_config::LiveConfig>,

    /// Pending plans awaiting user approval
    pending_plans: Arc<RwLock<HashMap<Uuid, ConfigPlan>>>,

    /// Execution log for stateful operations
    execution_log: Arc<RwLock<Vec<LogEntry>>>,

    /// Inner MCP tool executor for ReadOnly operations
    mcp_executor: McpToolExecutor,

    /// Audit sink for comprehensive tracking (P4-04; ADR-045 D5):
    /// writes go through the `AuditSink` trait seam — SQLite in `audit-db`
    /// builds, the JSONL sink as fallback. Field keeps its historical name
    /// to bound the diff; it has held a trait object since that change.
    audit_logger: Option<Arc<dyn AuditSink>>,

    /// Confirmation manager for HardwareIO operations (P4-01)
    confirmation_manager: Arc<ConfirmationManager>,

    /// Rate limiter for per-tier request throttling (P4-05)
    rate_limiter: Arc<RateLimiter>,

    /// Client ID for rate limiting (default: "local")
    client_id: String,

    /// Undo/redo history for config changes (P4-06)
    undo_stack: Arc<RwLock<UndoStack>>,

    /// MIDI Learn active flag (shared with engine_manager)
    midi_learn_active: Option<Arc<AtomicBool>>,

    /// MIDI Learn events buffer (shared with engine_manager)
    ///
    /// Producer-Consumer pattern:
    /// - Producer: EngineManager.process_input_event() pushes events
    /// - Consumer: This executor drains() events via conductor_stop_midi_learn
    /// - Ring buffer bounding enforced at push time by EngineManager
    /// - drain() is a write operation but is the intended consume pattern
    midi_learn_events: Option<Arc<Mutex<VecDeque<MidiLearnEvent>>>>,

    /// Shared daemon state refs for live status reporting
    ///
    /// When set, `conductor_get_status` reads live device_status, lifecycle_state,
    /// and statistics from the engine_manager's shared Arcs instead of returning
    /// a fallback with `connected: false`.
    daemon_state_refs: Option<SharedDaemonStateRefs>,

    /// Auto-stop timer for the current LLM-initiated MIDI Learn session.
    ///
    /// `conductor_start_learn` accepts a `timeout_seconds` argument but the
    /// LLM agent loop has no async timer of its own — it's stateless across
    /// turns and can't reliably "remember to call stop later". So the daemon
    /// owns the deadline: every start spawns a task that sleeps for the
    /// configured duration then flips `midi_learn_active` to false. The
    /// handle is stored here so a subsequent start can abort the previous
    /// timer (extending the deadline) and an explicit stop can cancel it
    /// (preventing a stale timer from prematurely ending a fresh session).
    midi_learn_timer: Arc<Mutex<Option<JoinHandle<()>>>>,

    /// Monotonic generation counter for MIDI Learn sessions.
    ///
    /// `JoinHandle::abort()` is best-effort: if the prior timer's
    /// `tokio::time::sleep` has already woken when a fresh start arrives,
    /// the abort signal can lose the race and the prior timer's body
    /// runs `active.swap(false, ...)` against the freshly-started session,
    /// silently stopping it. To close that window, every start bumps
    /// this counter (and stop bumps it too); the timer task captures
    /// the value at spawn time and only swaps if the counter still
    /// matches when it wakes — a stale wake-up is a no-op.
    midi_learn_session_gen: Arc<AtomicU64>,

    /// ADR-027 D6 — multi-dimensional LLM budget for this session. `None`
    /// disables budget enforcement (the historical behaviour; every existing
    /// constructor leaves it unset). When present, every `execute()` charges
    /// the capability dimensions this MCP surface can observe — total tool
    /// calls, ConfigChange-tier calls, and HardwareIO/MIDI output — and halts
    /// the loop with an `LlmBudgetExceeded` audit event when a quota is
    /// exhausted. Token / iteration / wall-clock dimensions are driven by the
    /// GUI agentic loop, where those quantities exist.
    budget: Option<Arc<Mutex<conductor_core::security::LlmBudgetState>>>,
}

mod execute;
mod learn_hardware;
mod lifecycle;
mod plan;
mod readonly;
mod stateful;
mod undo;

/// Build a `ConfigChange::CreateEndpoint` plan for `conductor_create_endpoint`
/// (ADR-035). Performs the eager, tool-time checks (non-empty alias,
/// channel range, alias-uniqueness across the endpoint namespace) so the caller
/// gets a clear error now rather than at plan-apply time.
///
/// ADR-035 Phase 2 removed the legacy `create_binding`/`create_connector`
/// /`create_device_identity`/`update_device_identity`/`delete_device_identity`
/// tools — `conductor_create_endpoint` is the sole MCP I/O-authoring tool.
#[allow(clippy::too_many_arguments)]
fn build_create_endpoint_plan(
    alias: String,
    direction: conductor_core::config::types::ConnectorDirection,
    protocol: Option<conductor_core::config::types::ConnectorProtocol>,
    kind: conductor_core::config::types::EndpointKind,
    description: Option<String>,
    enabled: bool,
    channels: Vec<u8>,
    config: &Config,
) -> Result<ConfigPlan, PlanError> {
    if alias.trim().is_empty() {
        return Err(PlanError::InvalidAction(
            "Endpoint alias cannot be empty".to_string(),
        ));
    }
    for &ch in &channels {
        if ch > 15 {
            return Err(PlanError::InvalidAction(format!(
                "Channel {} is out of range (must be 0-15)",
                ch
            )));
        }
    }
    // Eager alias-uniqueness across the endpoint namespace (ADR-035).
    if config.endpoints.iter().any(|e| e.alias == alias) {
        return Err(PlanError::InvalidAction(format!(
            "Endpoint alias '{}' already exists",
            alias
        )));
    }

    Ok(ConfigPlan::new(
        format!("Create endpoint '{}'", alias),
        vec![ConfigChange::CreateEndpoint {
            alias,
            direction,
            protocol,
            kind,
            description,
            enabled,
            channels,
        }],
        config,
    ))
}

/// Parse the `event` object of `conductor_explain_route_match` into
/// `(source_device_alias, raw_midi_bytes)` (ADR-036 D5). The
/// RouteEngine matches on raw MIDI, so the typed fields are assembled
/// into a 2- or 3-byte channel-voice message. Validates ranges and the
/// message type, returning a human-readable `Err` on malformed input.
fn parse_explain_event(event: &Value) -> Result<(String, Vec<u8>), String> {
    let device = event
        .get("device")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "event.device (source binding alias) is required".to_string())?
        .to_string();
    let kind = event
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "event.type is required".to_string())?;
    let channel = event
        .get("channel")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "event.channel (0-15) is required".to_string())?;
    if channel > 15 {
        return Err("event.channel must be 0-15".to_string());
    }
    let data1 = event
        .get("data1")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "event.data1 (0-127) is required".to_string())?;
    if data1 > 127 {
        return Err("event.data1 must be 0-127".to_string());
    }
    let data2 = event.get("data2").and_then(|v| v.as_u64());
    if let Some(d2) = data2
        && d2 > 127
    {
        return Err("event.data2 must be 0-127".to_string());
    }

    // (status high nibble, is the message 2-byte i.e. data2 unused)
    let (status_high, two_byte): (u8, bool) = match kind {
        "note_off" => (0x80, false),
        "note_on" => (0x90, false),
        "poly_aftertouch" => (0xA0, false),
        "cc" => (0xB0, false),
        "program_change" => (0xC0, true),
        "aftertouch" => (0xD0, true),
        "pitch_bend" => (0xE0, false),
        other => {
            return Err(format!(
                "unknown event.type '{other}' (expected note_on | note_off | cc | \
                 program_change | aftertouch | poly_aftertouch | pitch_bend)"
            ));
        }
    };
    let status = status_high | (channel as u8);
    let mut raw = vec![status, data1 as u8];
    if !two_byte {
        // 3-byte channel-voice message; data2 defaults to 0 (e.g. a
        // note_on with no velocity supplied).
        raw.push(data2.unwrap_or(0) as u8);
    }
    Ok((device, raw))
}

#[cfg(test)]
mod tests;
