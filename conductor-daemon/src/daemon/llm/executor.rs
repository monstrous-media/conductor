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

    /// Audit sink for comprehensive tracking (P4-04; ADR-045 D5 #2493):
    /// writes go through the `AuditSink` trait seam — SQLite in `audit-db`
    /// builds, the JSONL sink as fallback. Field keeps its historical name
    /// to bound the diff; it has held a trait object since #2493.
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

    /// Shared daemon state refs for live status reporting (#107)
    ///
    /// When set, `conductor_get_status` reads live device_status, lifecycle_state,
    /// and statistics from the engine_manager's shared Arcs instead of returning
    /// a fallback with `connected: false`.
    daemon_state_refs: Option<SharedDaemonStateRefs>,

    /// Auto-stop timer for the current LLM-initiated MIDI Learn session (#1053).
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

    /// Monotonic generation counter for MIDI Learn sessions (#1059 review).
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

impl ToolExecutor {
    /// Create a new tool executor
    pub fn new(live_config: Arc<crate::daemon::live_config::LiveConfig>) -> Self {
        Self {
            live_config,
            pending_plans: Arc::new(RwLock::new(HashMap::new())),
            execution_log: Arc::new(RwLock::new(Vec::new())),
            mcp_executor: McpToolExecutor::new(),
            audit_logger: None,
            confirmation_manager: Arc::new(ConfirmationManager::new()),
            rate_limiter: Arc::new(RateLimiter::new()),
            client_id: "local".to_string(),
            undo_stack: Arc::new(RwLock::new(UndoStack::new())),
            midi_learn_active: None,
            midi_learn_events: None,
            daemon_state_refs: None,
            midi_learn_timer: Arc::new(Mutex::new(None)),
            midi_learn_session_gen: Arc::new(AtomicU64::new(0)),
            budget: None,
        }
    }

    /// Create a new tool executor with audit logging (P4-04)
    pub fn with_audit_logger(
        live_config: Arc<crate::daemon::live_config::LiveConfig>,
        audit_logger: Arc<dyn AuditSink>,
    ) -> Self {
        Self {
            live_config,
            pending_plans: Arc::new(RwLock::new(HashMap::new())),
            execution_log: Arc::new(RwLock::new(Vec::new())),
            mcp_executor: McpToolExecutor::new(),
            audit_logger: Some(audit_logger),
            confirmation_manager: Arc::new(ConfirmationManager::new()),
            rate_limiter: Arc::new(RateLimiter::new()),
            client_id: "local".to_string(),
            undo_stack: Arc::new(RwLock::new(UndoStack::new())),
            midi_learn_active: None,
            midi_learn_events: None,
            daemon_state_refs: None,
            midi_learn_timer: Arc::new(Mutex::new(None)),
            midi_learn_session_gen: Arc::new(AtomicU64::new(0)),
            budget: None,
        }
    }

    /// Create a new tool executor with custom rate limit config (P4-05)
    pub fn with_rate_limit_config(
        live_config: Arc<crate::daemon::live_config::LiveConfig>,
        rate_limit_config: RateLimitConfig,
    ) -> Self {
        Self {
            live_config,
            pending_plans: Arc::new(RwLock::new(HashMap::new())),
            execution_log: Arc::new(RwLock::new(Vec::new())),
            mcp_executor: McpToolExecutor::new(),
            audit_logger: None,
            confirmation_manager: Arc::new(ConfirmationManager::new()),
            rate_limiter: Arc::new(RateLimiter::with_config(rate_limit_config)),
            client_id: "local".to_string(),
            undo_stack: Arc::new(RwLock::new(UndoStack::new())),
            midi_learn_active: None,
            midi_learn_events: None,
            daemon_state_refs: None,
            midi_learn_timer: Arc::new(Mutex::new(None)),
            midi_learn_session_gen: Arc::new(AtomicU64::new(0)),
            budget: None,
        }
    }

    // D4.A.3.3.B.1: `new_with_config(Arc<RwLock<Config>>)` retired —
    // every constructor variant now takes `Arc<LiveConfig>` directly.
    // The dead-code helper that lazily wrapped a separate `Config`
    // `Arc<RwLock<...>>` no longer compiles under the new typing and
    // had zero callers across the workspace, so it's removed outright
    // rather than mechanically translated.

    /// Create a new tool executor with MIDI Learn state (ADR-007 Phase 2)
    ///
    /// This constructor accepts shared MIDI Learn state from the engine_manager,
    /// enabling the conductor_start_midi_learn and conductor_stop_midi_learn
    /// tools to control MIDI Learn mode.
    pub fn with_midi_learn_state(
        live_config: Arc<crate::daemon::live_config::LiveConfig>,
        midi_learn_active: Arc<AtomicBool>,
        midi_learn_events: Arc<Mutex<VecDeque<MidiLearnEvent>>>,
    ) -> Self {
        Self {
            live_config,
            pending_plans: Arc::new(RwLock::new(HashMap::new())),
            execution_log: Arc::new(RwLock::new(Vec::new())),
            mcp_executor: McpToolExecutor::new(),
            audit_logger: None,
            confirmation_manager: Arc::new(ConfirmationManager::new()),
            rate_limiter: Arc::new(RateLimiter::new()),
            client_id: "local".to_string(),
            undo_stack: Arc::new(RwLock::new(UndoStack::new())),
            midi_learn_active: Some(midi_learn_active),
            midi_learn_events: Some(midi_learn_events),
            daemon_state_refs: None,
            midi_learn_timer: Arc::new(Mutex::new(None)),
            midi_learn_session_gen: Arc::new(AtomicU64::new(0)),
            budget: None,
        }
    }

    /// Create a new tool executor with MIDI Learn state and daemon state (#107)
    ///
    /// This constructor accepts both MIDI Learn state and shared daemon state refs,
    /// enabling live status reporting for `conductor_get_status` via IPC.
    pub fn with_daemon_state(
        live_config: Arc<crate::daemon::live_config::LiveConfig>,
        midi_learn_active: Arc<AtomicBool>,
        midi_learn_events: Arc<Mutex<VecDeque<MidiLearnEvent>>>,
        daemon_state_refs: SharedDaemonStateRefs,
    ) -> Self {
        Self {
            live_config,
            pending_plans: Arc::new(RwLock::new(HashMap::new())),
            execution_log: Arc::new(RwLock::new(Vec::new())),
            mcp_executor: McpToolExecutor::new(),
            audit_logger: None,
            confirmation_manager: Arc::new(ConfirmationManager::new()),
            rate_limiter: Arc::new(RateLimiter::new()),
            client_id: "local".to_string(),
            undo_stack: Arc::new(RwLock::new(UndoStack::new())),
            midi_learn_active: Some(midi_learn_active),
            midi_learn_events: Some(midi_learn_events),
            daemon_state_refs: Some(daemon_state_refs),
            midi_learn_timer: Arc::new(Mutex::new(None)),
            midi_learn_session_gen: Arc::new(AtomicU64::new(0)),
            budget: None,
        }
    }

    /// Set the client ID for rate limiting (P4-05)
    pub fn set_client_id(&mut self, client_id: String) {
        self.client_id = client_id;
    }

    /// Get the rate limiter (P4-05)
    pub fn rate_limiter(&self) -> &Arc<RateLimiter> {
        &self.rate_limiter
    }

    /// Set the audit logger after construction
    pub fn set_audit_logger(&mut self, audit_logger: Arc<dyn AuditSink>) {
        self.audit_logger = Some(audit_logger);
    }

    /// Attach an ADR-027 D6 LLM budget to this session (enables enforcement).
    /// The daemon compiles the budget from the file-only `[security.llm]`
    /// block and shares one [`LlmBudgetState`] per LLM session.
    ///
    /// [`LlmBudgetState`]: conductor_core::security::LlmBudgetState
    pub fn set_budget_state(
        &mut self,
        budget: Arc<Mutex<conductor_core::security::LlmBudgetState>>,
    ) {
        self.budget = Some(budget);
    }

    /// Set MIDI Learn state references (shared with engine_manager)
    ///
    /// This connects the ToolExecutor to the engine_manager's MIDI Learn
    /// state, enabling the conductor_start_midi_learn and conductor_stop_midi_learn
    /// tools to control the MIDI Learn mode.
    pub fn set_midi_learn_state(
        &mut self,
        active: Arc<AtomicBool>,
        events: Arc<Mutex<VecDeque<MidiLearnEvent>>>,
    ) {
        self.midi_learn_active = Some(active);
        self.midi_learn_events = Some(events);
    }

    // D4.A.3.3.B.1: `set_config()` retired. Engine_manager no longer
    // needs to push a separate config snapshot into the executor —
    // both share the same `Arc<LiveConfig>` and reads come from
    // `live_config.load()`. Callers that previously did
    // `tool_executor.set_config(Some(cfg))` should be deleted (the
    // executor now sees mutations as soon as they publish through
    // `LiveConfig::mutate`).

    /// Retrieve the current config snapshot.
    ///
    /// D4.A.3.3.B.1: returns `Config` directly (not `Option<Config>`)
    /// since `LiveConfig` is always populated post-`EngineManager::new`.
    /// Existing callers wrapping the result in `Some(...)` should be
    /// updated to drop the `Option`.
    pub fn get_config(&self) -> Config {
        (*self.live_config.load().config).clone()
    }

    /// Execute a tool call with risk tier handling
    ///
    /// # Arguments
    /// * `tool_name` - Name of the tool to execute
    /// * `arguments` - Tool arguments as JSON
    /// * `caller_ctx` - ADR-027 D1-pinned peer identity from the IPC
    ///   accept loop. **Every call goes through
    ///   `security::gate::enforce`** (PR-D activation). When `Some`,
    ///   the supplied trust band drives the decision table directly.
    ///   When `None`, the method substitutes
    ///   `CallerContext::synthetic_unpinned()` (trust band
    ///   `Untrusted`) so an unpinned IPC peer is denied for
    ///   anything beyond `ReadOnly` / `Stateful` /
    ///   `ArtifactRender`. Daemon-internal callers that have
    ///   already been gate-checked at an outer boundary should
    ///   pass `Some(CallerContext::internal_trusted())` to be
    ///   admitted as `GuiTrusted`. Lib unit-test builds keep
    ///   `SecurityPolicy::default().shadow_mode = true` via
    ///   `cfg(test)` so the existing fixture pattern of passing
    ///   `None` still produces today's behaviour for the
    ///   ToolExecutor unit tests; production builds enforce.
    ///
    /// # Returns
    /// - ReadOnly tools: `ExecutionResult::Success`
    /// - Stateful tools: `ExecutionResult::Logged`
    /// - ConfigChange tools: `ExecutionResult::PlanCreated`
    /// - HardwareIO tools: `ExecutionResult::HardwareIoConfirmation`
    /// - Rate limited: `ExecutionResult::RateLimited`
    /// - Gate denial: `ExecutionResult::Error` with the
    ///   `DenialReason` rendered into the message
    pub async fn execute(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
        caller_ctx: Option<&crate::security::CallerContext>,
    ) -> ExecutionResult {
        let risk_tier = get_tool_risk_tier(tool_name);
        debug!(
            "Executing tool '{}' with risk tier {:?}",
            tool_name, risk_tier
        );

        // ADR-027 D5/D1 wiring: consult the security gate before
        // any tool work. PR-B added the call site, PR-C wired
        // `RequirePlan` / `RequireConfirmation` to the existing
        // per-tier handlers, PR-D activated enforcement
        // (`SecurityPolicy::default().shadow_mode = false` in
        // production) and closed the gate-bypass-on-`None` gap.
        //
        // Order (PR-C revised after Copilot round-2 on #1103):
        //   1. Gate first. `Deny` returns immediately without
        //      consuming rate-limit quota — denials shouldn't
        //      waste throttling budget, and the denial reason is
        //      more informative than `RateLimited` would be.
        //   2. Rate-limit check (applies to every path that
        //      passes the gate).
        //   3. Route per gate decision (`RequirePlan` →
        //      `execute_config_change`, `RequireConfirmation` →
        //      `execute_hardware_io`) or fall through to
        //      per-tier dispatch.
        //
        // None-handling (PR-D): a `None` caller_ctx means the
        // IPC accept loop couldn't pin the peer (Linux < 5.3
        // with no `pidfd_open`, same-uid TCC anomaly, etc.).
        // Such peers go through the gate as a synthetic
        // `Untrusted` — with `shadow_mode = false` they'll be
        // denied for anything beyond ReadOnly. Daemon-internal
        // callers (the inline `conductor_*_plugin` arms below
        // that send a `DaemonCommand::IpcRequest` to the daemon
        // command channel for plugin management) pass
        // `Some(CallerContext::internal_trusted())` explicitly
        // so they reach the gate as `GuiTrusted` and route
        // through Plan/Apply for ConfigChange just like a
        // verified GUI peer would.
        let ctx_for_gate: std::borrow::Cow<'_, crate::security::CallerContext> = match caller_ctx {
            Some(ctx) => std::borrow::Cow::Borrowed(ctx),
            None => std::borrow::Cow::Owned(crate::security::CallerContext::synthetic_unpinned()),
        };

        // Track which gate-routed handler (if any) the gate
        // selected, so we can run the rate-limiter between the
        // gate and the handler call. `GateRoute::FallThrough`
        // covers `Allow` / `AllowWithAudit` — both want per-tier
        // dispatch.
        enum GateRoute {
            FallThrough,
            ToConfigChange,
            ToHardwareIo,
        }
        let mut gate_route = GateRoute::FallThrough;

        {
            let ctx = ctx_for_gate.as_ref();
            let policy = crate::security::SecurityPolicy::default();
            match crate::security::enforce(risk_tier, ctx, &policy) {
                crate::security::GateDecision::Allow
                | crate::security::GateDecision::AllowWithAudit => {
                    // Fall through to rate-limit + per-tier
                    // dispatch. `AllowWithAudit` is treated as
                    // `Allow` for PR-B purposes — D13a's audit-
                    // stream emission is a follow-up sub-piece;
                    // the audit logger already records every
                    // tool execution via the per-tier handler's
                    // own log calls.
                }
                crate::security::GateDecision::Deny(reason) => {
                    // **Return BEFORE rate-limit** so denied
                    // requests don't consume throttling budget
                    // (Copilot review on PR #1103, round-2). The
                    // gate's denial reason is also more
                    // informative than `RateLimited` would be.
                    // Use `{}` (Display) not `{:?}` (Debug) — the
                    // `Display` impl on `DenialReason` renders
                    // natural-language reasoning operators / end
                    // users can act on.
                    warn!(
                        "Gate denied tool '{}' (tier {:?}, trust {:?}): {}",
                        tool_name, risk_tier, ctx.trust_level, reason
                    );
                    if let Some(ref logger) = self.audit_logger {
                        logger.log_tool_denied(
                            tool_name,
                            tool_risk_to_audit_risk(&risk_tier),
                            &format!("Gate denied: {}", reason),
                            Some(UserContext::local_user()),
                        );
                    }
                    return ExecutionResult::Error {
                        message: format!("Gate denied tool '{}': {}", tool_name, reason),
                    };
                }
                crate::security::GateDecision::RequirePlan(_req) => {
                    // ADR-027 D5/D1 wiring (PR-C): route to the
                    // existing Plan/Apply machinery. The gate's
                    // RequirePlan decision means this tool needs
                    // a user-confirmable plan before its mutation
                    // applies (ADR-007 D2). `execute_config_change`
                    // returns `ExecutionResult::PlanCreated { plan }`
                    // and the GUI / CLI submits the plan_id back
                    // via `ApplyPlan` to confirm. Rate-limit
                    // applies between here and the handler call
                    // (see below). Audit attribution today: the
                    // handler calls `log_plan_created` (not
                    // `log_tool_complete`) — the plan event
                    // doesn't include the originating tool_name /
                    // args, which is a known pre-existing gap
                    // (Copilot round-2 on #1103) for a future PR
                    // that adds tool-attributable plan logging.
                    debug!(
                        "Gate routed tool '{}' (tier {:?}) to Plan/Apply via RequirePlan",
                        tool_name, risk_tier
                    );
                    gate_route = GateRoute::ToConfigChange;
                }
                crate::security::GateDecision::RequireConfirmation(_req) => {
                    // ADR-027 D5/D1 wiring (PR-C): route to the
                    // existing hardware-IO confirmation machinery
                    // (ADR-027 D7 partial). The handler returns
                    // `ExecutionResult::HardwareIoConfirmation`
                    // with a confirmation token; the GUI prompts
                    // the user, and the token is submitted back
                    // via the same tool with
                    // `args.confirmation_token` set. Rate-limit
                    // applies between here and the handler call.
                    debug!(
                        "Gate routed tool '{}' (tier {:?}) to HardwareIO confirmation via RequireConfirmation",
                        tool_name, risk_tier
                    );
                    gate_route = GateRoute::ToHardwareIo;
                }
            }
        }

        // Rate-limit check (P4-05). Runs AFTER the gate has had
        // a chance to deny — gate-denied requests skip this and
        // get a clean denial reason. Runs BEFORE both gate-
        // routed handlers (Plan/Apply, HardwareIO confirmation)
        // and per-tier dispatch — every "proceed" path is
        // throttled equally.
        match self
            .rate_limiter
            .check_and_record(&self.client_id, risk_tier)
        {
            Ok(_) => {
                // Rate limit check passed, continue with execution
            }
            Err(RateLimitError::Exceeded {
                tier,
                current,
                limit,
                retry_after_secs,
            }) => {
                warn!(
                    "Rate limit exceeded for tool '{}': {}/{} requests for {:?} tier",
                    tool_name, current, limit, tier
                );

                // Audit log the rate limit denial
                if let Some(ref logger) = self.audit_logger {
                    logger.log_tool_denied(
                        tool_name,
                        tool_risk_to_audit_risk(&risk_tier),
                        &format!(
                            "Rate limit exceeded: {}/{} requests in window. Retry after {}s",
                            current, limit, retry_after_secs
                        ),
                        Some(UserContext::local_user()),
                    );
                }

                return ExecutionResult::RateLimited {
                    tier,
                    current,
                    limit,
                    retry_after_secs,
                };
            }
        }

        // ADR-027 D6 — multi-dimensional LLM budget. Charge AFTER the gate
        // and rate-limiter have admitted the call (a denied/throttled call
        // shouldn't consume budget) and BEFORE any handler runs. This MCP
        // surface can observe three of the D6 dimensions: every tool call,
        // ConfigChange-tier calls, and HardwareIO/MIDI output. The token /
        // iteration / wall-clock dimensions live in the GUI agentic loop.
        // On exhaustion we halt with an `LlmBudgetExceeded` audit event —
        // satisfying "the daemon halts the loop with a clear audit event".
        if let Some(ref budget) = self.budget {
            let mut state = budget.lock().await;
            let charge = state.charge_tool_call().and_then(|()| match risk_tier {
                ToolRiskTier::ConfigChange => state.charge_config_change(),
                ToolRiskTier::HardwareIO => state.charge_midi_out(1),
                _ => Ok(()),
            });
            if let Err(exceeded) = charge {
                // Drop the lock before the (synchronous) audit insert.
                drop(state);
                warn!(
                    "LLM budget exceeded for tool '{}' (tier {:?}): {}",
                    tool_name, risk_tier, exceeded
                );
                if let Some(ref logger) = self.audit_logger {
                    logger.log_llm_budget_exceeded(
                        tool_name,
                        tool_risk_to_audit_risk(&risk_tier),
                        exceeded.dimension.as_str(),
                        exceeded.limit,
                        exceeded.observed,
                        Some(UserContext::local_user()),
                    );
                }
                return ExecutionResult::Error {
                    message: format!("LLM budget exceeded for tool '{}': {}", tool_name, exceeded),
                };
            }
        }

        // Now route per the gate decision (if any). Gate-routed
        // paths still hit `execute_config_change` /
        // `execute_hardware_io` — same handlers the per-tier
        // dispatch below would have invoked, just selected via
        // gate decision instead of `risk_tier` matching.
        match gate_route {
            GateRoute::ToConfigChange => {
                return self.execute_config_change(tool_name, arguments).await;
            }
            GateRoute::ToHardwareIo => {
                return self.execute_hardware_io(tool_name, arguments).await;
            }
            GateRoute::FallThrough => {
                // Fall through to per-tier dispatch below.
            }
        }

        // Per-tier dispatch (no gate consulted, or gate said
        // Allow / AllowWithAudit).
        match risk_tier {
            ToolRiskTier::ReadOnly => self.execute_readonly(tool_name, arguments).await,
            ToolRiskTier::Stateful | ToolRiskTier::ArtifactRender => {
                self.execute_stateful(tool_name, arguments).await
            }
            ToolRiskTier::ConfigChange => self.execute_config_change(tool_name, arguments).await,
            ToolRiskTier::HardwareIO => self.execute_hardware_io(tool_name, arguments).await,
            ToolRiskTier::Privileged => ExecutionResult::Error {
                message: format!(
                    "Tool '{}' has risk tier {:?} which is not yet supported",
                    tool_name, risk_tier
                ),
            },
        }
    }

    /// Fetch device data for conductor_list_devices tool
    async fn fetch_devices_data() -> Option<Value> {
        let mut devices_data = json!({
            "midi_devices": [],
            "hid_devices": []
        });

        // Enumerate MIDI devices with warmup pattern for fresh results (#104)
        // Uses spawn_blocking to avoid blocking the tokio runtime
        let midi_devices = crate::daemon::device_utils::enumerate_midi_devices_fresh_async().await;
        let midi_json: Vec<Value> = midi_devices
            .iter()
            .map(|d| {
                json!({
                    "index": d.port_index,
                    "name": d.port_name,
                    "type": "midi_input"
                })
            })
            .collect();
        devices_data["midi_devices"] = json!(midi_json);

        // Enumerate HID/gamepad devices via spawn_blocking to avoid blocking tokio
        let hid_result = tokio::task::spawn_blocking(HidDeviceManager::list_gamepads).await;
        if let Ok(Ok(gamepads)) = hid_result {
            let hid_devices: Vec<Value> = gamepads
                .iter()
                .enumerate()
                .map(|(idx, (_id, name, uuid))| {
                    json!({
                        "index": idx,
                        "name": name,
                        "uuid": uuid,
                        "type": "hid_gamepad"
                    })
                })
                .collect();
            devices_data["hid_devices"] = json!(hid_devices);
        }

        Some(devices_data)
    }

    /// Execute a ReadOnly tool immediately
    async fn execute_readonly(&self, tool_name: &str, arguments: Option<Value>) -> ExecutionResult {
        let start_time = Instant::now();
        let args_json = arguments.as_ref().map(|a| a.to_string());
        let risk_tier = get_tool_risk_tier(tool_name);
        let audit_tier = tool_risk_to_audit_risk(&risk_tier);

        // ADR-025 Phase 1: control-state tools route to a dedicated handler
        // that needs the live Arc<PhysicalControlStateStore> rather than a
        // serialized status snapshot. Fall through to the standard path
        // otherwise.
        let control_state_ref = self
            .daemon_state_refs
            .as_ref()
            .map(|r| r.control_state.as_ref());
        if let Some(result) = crate::daemon::llm::control_state_tools::handle_readonly(
            tool_name,
            arguments.as_ref(),
            control_state_ref,
        ) {
            // Audit log and return.
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                if result.is_error == Some(true) {
                    let error_msg = result
                        .content
                        .first()
                        .map(|c| match c {
                            crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                            crate::daemon::mcp_types::ToolContent::Image { .. } => {
                                "Image error".to_string()
                            }
                            crate::daemon::mcp_types::ToolContent::Resource { text, .. } => {
                                text.clone().unwrap_or_else(|| "Resource error".to_string())
                            }
                        })
                        .unwrap_or_else(|| "Unknown error".to_string());
                    logger.log_tool_error(
                        tool_name,
                        audit_tier,
                        args_json.as_deref(),
                        &error_msg,
                        execution_time,
                        Some(UserContext::local_user()),
                    );
                } else {
                    logger.log_tool_complete(
                        tool_name,
                        audit_tier,
                        args_json.as_deref(),
                        result_json.as_deref(),
                        execution_time,
                        Some(UserContext::local_user()),
                    );
                }
            }
            return ExecutionResult::Success { result };
        }

        // ADR-026 Phase 2: SysEx identity ReadOnly tools. Both pull
        // from the shared `ProbeCoordinator` cache populated by
        // `conductor_probe_device_identity` / probe-on-connect (Phase 3).
        if matches!(
            tool_name,
            "conductor_get_device_identity" | "conductor_list_device_identities"
        ) {
            let Some(refs) = self.daemon_state_refs.as_ref() else {
                return ExecutionResult::Error {
                    message:
                        "Daemon state refs not available — identity lookup requires running daemon"
                            .to_string(),
                };
            };
            let coord = &refs.probe_coordinator;
            let result = match tool_name {
                "conductor_get_device_identity" => {
                    let port_name = arguments
                        .as_ref()
                        .and_then(|a| a.get("port_name"))
                        .and_then(|v| v.as_str());
                    let Some(port) = port_name else {
                        return ExecutionResult::Error {
                            message: "Missing required argument: port_name".to_string(),
                        };
                    };
                    // Phase 3.A: cache returns (identity, confidence).
                    // Surface confidence in the response shape so GUI
                    // and LLM callers can render the badge without a
                    // second round-trip. `null` for both when unprobed.
                    let cached = coord.cached(port);
                    let payload = json!({
                        "port_name": port,
                        "identity": cached.as_ref().map(|(id, _)| id),
                        "confidence": cached.as_ref().map(|(_, c)| c),
                    });
                    crate::daemon::mcp_types::ToolCallResult::json(&payload)
                }
                "conductor_list_device_identities" => {
                    let snapshot = coord.snapshot();
                    let entries: Vec<serde_json::Value> = snapshot
                        .into_iter()
                        .map(|(port, identity, confidence)| {
                            json!({
                                "port_name": port,
                                "identity": identity,
                                "confidence": confidence,
                            })
                        })
                        .collect();
                    let payload = json!({ "identities": entries });
                    crate::daemon::mcp_types::ToolCallResult::json(&payload)
                }
                _ => unreachable!(),
            };
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
            return ExecutionResult::Success { result };
        }

        // (ADR-035 follow-up #2052: `conductor_list_connectors` was removed —
        // its runtime connectors+status view is a subset of
        // `conductor_get_resolved_routing_graph` (below), which reports the same
        // per-connector `connected`/`bound_port` plus route resolution.)

        // ADR-042 Phase B-early (#1899) — B.7 visibility: report the
        // network-approval HMAC key's rotation status. Report-only and
        // infallible at the tool level — a missing key / unavailable backend
        // degrades to a structured "unavailable" payload (mirroring
        // `conductorctl security status`), never an ExecutionResult::Error: a
        // status probe must not hard-fail. Reads the OS keychain directly, so
        // it needs no `daemon_state_refs`.
        if tool_name == "conductor_security_status" {
            let payload = crate::daemon::llm::security_status::payload().await;
            let result = crate::daemon::mcp_types::ToolCallResult::json(&payload);
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
            return ExecutionResult::Success { result };
        }

        // ADR-031 §3.4 / #1598 Phase 1 — runtime-resolved routing graph.
        // The canonical view the GUI should render: connectors from the
        // live registry (bindings lowered, explicit `[[connectors]]`
        // folded in) and routes resolved against that registry so
        // `from_missing`/`to_missing` surface validator-bypassed paths.
        // Distinct from `conductor_get_routing_graph` which returns the
        // declared/config view. Extends line 855's resolver-of-record
        // principle from action execution to graph rendering. Requires
        // `daemon_state_refs` (reads the live registry / input manager).
        if tool_name == "conductor_get_resolved_routing_graph" {
            // #1598 Phase 2 Step C — read from AUTHORITATIVE sources
            // (input_manager + device_output_map), not from
            // `LiveConnector.bound_port` / `.connected`. The registry's
            // runtime fields are initialised to `None`/`false` by
            // `from_config` and nothing populates them — Bindings panel
            // showed devices connected while Routing Graph showed
            // everything unbound until this read site was rewritten.
            // See `resolved_routing_graph.rs` module doc + memory
            // `[[tdd-must-exercise-production-data-path]]` for the lesson.
            let Some(refs) = self.daemon_state_refs.as_ref() else {
                return ExecutionResult::Error {
                    message:
                        "Daemon state refs not available — routing graph requires running daemon"
                            .to_string(),
                };
            };
            // Lock-ordering note (Copilot finding on PR #1633): acquire
            // the locks-with-await BEFORE the connector_registry
            // RwLockReadGuard so we never hold the registry guard
            // across an `.await`. Holding a guard across await opens
            // a deadlock window with any path that takes input_manager
            // first and then tries to acquire registry.
            //
            // Order:
            // 1. `device_output_map` — lock-free ArcSwap load.
            // 2. `input_manager.lock()` — short-lived Mutex; snapshot
            //    bindings and immediately drop.
            // 3. `live_config.load()` — lock-free ArcSwap.
            // 4. `connector_registry.read()` — held only across the
            //    SYNCHRONOUS response build below; no `.await` until
            //    the function returns.

            let output_map_arc = refs.device_output_map.load();

            // Loaded here (lock-free ArcSwap) so its endpoint set feeds
            // `reachable_output_ports` below; reused for `routes` at build time.
            let snap = self.live_config.load();

            // #2203: the live set of MIDI output ports, so an output endpoint
            // whose resolved port isn't actually present (e.g. an input-only
            // target) reports connected=false instead of a misleading green.
            // `enumerate_output_ports` is a synchronous midir scan, so offload it
            // to a blocking thread rather than stalling the Tokio runtime (same
            // pattern `enumerate_output_ports_async` uses). This `.await` is
            // BEFORE the connector_registry read guard below, so the
            // lock-ordering invariant (no `.await` while holding the guard) is
            // preserved. A join failure degrades safely to "no outputs available".
            //
            // #2421: a midir scan from THIS process does not list the virtual
            // output ports the daemon itself created, so `reachable_output_ports`
            // folds in the enabled MidiVirtualPort endpoints the daemon
            // materializes. Without it, a working daemon virtual output rendered
            // red in Endpoints + Routing Graph while Discovered Ports showed it
            // green (those views derive status from a separate enumeration). The
            // fold-in is gated on `virtual_ports_available()` so platforms that
            // cannot create virtual ports (Windows) don't report them connected
            // (Copilot review on PR #2443).
            let enumerated =
                tokio::task::spawn_blocking(crate::daemon::output_resolver::enumerate_output_ports)
                    .await
                    .unwrap_or_default();
            let available_outputs: std::collections::HashSet<String> =
                crate::daemon::output_resolver::reachable_output_ports(
                    enumerated,
                    &snap.config.endpoints,
                    conductor_core::midi_output::MidiOutputManager::virtual_ports_available(),
                );

            let input_bindings: Vec<_> = {
                let manager_guard = refs.input_manager.lock().await;
                manager_guard
                    .as_ref()
                    .map(|mgr| {
                        // Build entries inside this closure so `is_device_enabled`
                        // can be queried while `mgr` is in scope (#1626).
                        mgr.get_device_bindings()
                            .into_iter()
                            .filter(|(_, _, _, is_configured)| *is_configured)
                            .map(|(device_id, port_name, connected, _)| {
                                // #1626: runtime mute = NOT enabled (ADR-009 4b).
                                let muted = !mgr.is_device_enabled(&device_id);
                                crate::daemon::llm::resolved_routing_graph::InputBindingEntry {
                                    alias: device_id.as_str().to_string(),
                                    port_name,
                                    connected,
                                    muted,
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };

            // Acquired last + dropped at end of scope; no `.await`
            // after this point.
            let registry = refs.connector_registry.read().await;

            let mut sorted: Vec<_> = registry
                .iter()
                .map(
                    |(_alias, live)| crate::daemon::llm::resolved_routing_graph::ConnectorView {
                        config: &live.config,
                    },
                )
                .collect();
            sorted.sort_by_key(|c| c.alias());

            let payload =
                crate::daemon::llm::resolved_routing_graph::build_resolved_routing_graph_response(
                    &sorted,
                    &input_bindings,
                    &output_map_arc,
                    &available_outputs,
                    &snap.config.routes,
                );
            let result = crate::daemon::mcp_types::ToolCallResult::json(&payload);
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
            return ExecutionResult::Success { result };
        }

        // ADR-036 D5 / Slice 9 (#1667): explain why each route fires or is
        // skipped for a hypothetical event, against the LIVE RouteEngine.
        if tool_name == "conductor_explain_route_match" {
            let Some(refs) = self.daemon_state_refs.as_ref() else {
                return ExecutionResult::Error {
                    message:
                        "Daemon state refs not available — explain_route_match requires running daemon"
                            .to_string(),
                };
            };
            let args = arguments.as_ref();
            let Some(active_mode) = args
                .and_then(|a| a.get("active_mode"))
                .and_then(|v| v.as_str())
            else {
                return ExecutionResult::Error {
                    message: "conductor_explain_route_match requires 'active_mode' (string)"
                        .to_string(),
                };
            };
            let Some(event) = args.and_then(|a| a.get("event")) else {
                return ExecutionResult::Error {
                    message: "conductor_explain_route_match requires an 'event' object".to_string(),
                };
            };
            let (source_alias, raw) = match parse_explain_event(event) {
                Ok(v) => v,
                Err(message) => return ExecutionResult::Error { message },
            };

            let route_engine = refs.route_engine.load();
            let explanations = route_engine.explain_route_match(&source_alias, &raw, active_mode);
            let payload = serde_json::json!({
                "device": source_alias,
                "active_mode": active_mode,
                "event": super::super::dispatch_trace::summarize_midi(&raw),
                "routes": explanations,
                "note": if explanations.is_empty() {
                    serde_json::Value::String(format!(
                        "no routes have from = '{source_alias}' — nothing to evaluate"
                    ))
                } else {
                    serde_json::Value::Null
                },
            });
            let result = crate::daemon::mcp_types::ToolCallResult::json(&payload);
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
            return ExecutionResult::Success { result };
        }

        // ADR-036 §8 / Slice 9 (#1667): return the last N dispatch
        // decisions from the bounded trace ring buffer.
        if tool_name == "conductor_get_dispatch_trace" {
            let Some(refs) = self.daemon_state_refs.as_ref() else {
                return ExecutionResult::Error {
                    message:
                        "Daemon state refs not available — get_dispatch_trace requires running daemon"
                            .to_string(),
                };
            };
            // `last`: default 32, capped at 256 (issue #1667).
            let last = arguments
                .as_ref()
                .and_then(|a| a.get("last"))
                .and_then(|v| v.as_u64())
                .map(|n| n.min(256) as usize)
                .unwrap_or(32);
            let entries = refs.dispatch_trace.last(last);
            let payload = serde_json::json!({
                "count": entries.len(),
                "requested": last,
                "entries": entries,
            });
            let result = crate::daemon::mcp_types::ToolCallResult::json(&payload);
            if let Some(ref logger) = self.audit_logger {
                let execution_time = start_time.elapsed();
                let result_json = serde_json::to_string(&result).ok();
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
            return ExecutionResult::Success { result };
        }

        // D4.A.3.3.B.1: snapshot config via LiveConfig (lock-free ArcSwap read).
        // Snap binding outlives `config_ref` so the &Config reference into the
        // snapshot Arc stays valid across the mcp_executor.execute() await.
        let snap = self.live_config.load();
        let config_ref = Some(snap.config.as_ref());

        // Fetch device data if this is the list_devices tool
        let devices_data = if tool_name == "conductor_list_devices" {
            Self::fetch_devices_data().await
        } else {
            None
        };

        // Get live status data from daemon state (#107)
        let status_data = if let Some(refs) = &self.daemon_state_refs {
            let state = refs.get_daemon_state().await;
            Some(state.to_status_json())
        } else {
            None
        };

        // ADR-022 D7: Pass event_stats from EngineManager via SharedDaemonStateRefs
        let event_stats_ref = self.daemon_state_refs.as_ref().map(|r| &*r.event_stats);
        let result = self
            .mcp_executor
            .execute(
                tool_name,
                arguments,
                status_data,
                devices_data,
                config_ref,
                event_stats_ref,
            )
            .await;

        // Audit log the execution
        if let Some(ref logger) = self.audit_logger {
            let execution_time = start_time.elapsed();
            let result_json = serde_json::to_string(&result).ok();

            if result.is_error == Some(true) {
                let error_msg = result
                    .content
                    .first()
                    .map(|c| match c {
                        crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                        crate::daemon::mcp_types::ToolContent::Image { .. } => {
                            "Image error".to_string()
                        }
                        crate::daemon::mcp_types::ToolContent::Resource { text, .. } => {
                            text.clone().unwrap_or_else(|| "Resource error".to_string())
                        }
                    })
                    .unwrap_or_else(|| "Unknown error".to_string());
                logger.log_tool_error(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    &error_msg,
                    execution_time,
                    Some(UserContext::local_user()),
                );
            } else {
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
        }

        ExecutionResult::Success { result }
    }

    /// Execute a Stateful tool with logging
    async fn execute_stateful(&self, tool_name: &str, arguments: Option<Value>) -> ExecutionResult {
        let start_time = Instant::now();
        let args_json = arguments.as_ref().map(|a| a.to_string());
        let risk_tier = get_tool_risk_tier(tool_name);
        let audit_tier = tool_risk_to_audit_risk(&risk_tier);

        // For now, stateful tools are handled same as readonly but with logging
        // In Phase 2, MIDI Learn will be implemented here
        // D4.A.3.3.B.1: snapshot config via LiveConfig (lock-free).
        let snap = self.live_config.load();
        let config_ref = Some(snap.config.as_ref());

        // Execute the tool
        let result = self
            .execute_stateful_tool(tool_name, arguments.clone(), config_ref)
            .await;
        let execution_time = start_time.elapsed();

        // Create log entry
        let log_entry = LogEntry {
            id: Uuid::new_v4(),
            tool_name: tool_name.to_string(),
            arguments: arguments.clone(),
            timestamp: chrono::Utc::now(),
            result_summary: self.summarize_result(&result),
        };

        // Store log entry
        {
            let mut log = self.execution_log.write().await;
            log.push(log_entry.clone());
            info!("Logged stateful tool execution: {}", tool_name);
        }

        // Audit log the execution (P4-04)
        if let Some(ref logger) = self.audit_logger {
            let result_json = serde_json::to_string(&result).ok();

            if result.is_error == Some(true) {
                let error_msg = result
                    .content
                    .first()
                    .map(|c| match c {
                        crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                        crate::daemon::mcp_types::ToolContent::Image { .. } => {
                            "Image error".to_string()
                        }
                        crate::daemon::mcp_types::ToolContent::Resource { text, .. } => {
                            text.clone().unwrap_or_else(|| "Resource error".to_string())
                        }
                    })
                    .unwrap_or_else(|| "Unknown error".to_string());
                logger.log_tool_error(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    &error_msg,
                    execution_time,
                    Some(UserContext::local_user()),
                );
            } else {
                logger.log_tool_complete(
                    tool_name,
                    audit_tier,
                    args_json.as_deref(),
                    result_json.as_deref(),
                    execution_time,
                    Some(UserContext::local_user()),
                );
            }
        }

        ExecutionResult::Logged { result, log_entry }
    }

    /// Execute a stateful tool
    async fn execute_stateful_tool(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
        _config: Option<&Config>,
    ) -> ToolCallResult {
        // ADR-025 Phase 1: control-state reset routes to its dedicated
        // handler, which needs the live store Arc (not a snapshot).
        let control_state_ref = self
            .daemon_state_refs
            .as_ref()
            .map(|r| r.control_state.as_ref());
        if let Some(result) = crate::daemon::llm::control_state_tools::handle_stateful(
            tool_name,
            arguments.as_ref(),
            control_state_ref,
        ) {
            return result;
        }

        match tool_name {
            "conductor_start_learn" | "conductor_start_midi_learn" => {
                let timeout_seconds = arguments
                    .as_ref()
                    .and_then(|a| a.get("timeout_seconds"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(30);

                // Check if MIDI Learn state is available
                match &self.midi_learn_active {
                    Some(active) => {
                        // Bump the session generation BEFORE flipping
                        // active=true so any prior timer that wakes
                        // during this start window sees a stale gen
                        // and skips its swap (#1059 review). The
                        // returned value is the unique ID for this
                        // session; the spawned timer captures it and
                        // checks for match before stopping.
                        let my_gen = self.midi_learn_session_gen.fetch_add(1, Ordering::SeqCst) + 1;

                        // Set MIDI Learn active. `swap` returns the
                        // prior value so the tool result can tell the
                        // LLM whether it just preempted an active
                        // session (#1053 follow-up — without this
                        // signal the LLM rapid-restarts learn without
                        // acknowledging the restart to the user).
                        let was_already_active = active.swap(true, Ordering::SeqCst);

                        // Spawn the daemon-side auto-stop timer (#1053). The
                        // LLM agent loop can't reliably "remember to call
                        // stop later" — it has no async timer and is
                        // stateless across turns — so the deadline lives
                        // here. A subsequent start aborts this timer and
                        // installs a fresh one (extending the deadline);
                        // an explicit conductor_stop_learn aborts it
                        // (preventing the timer from firing late and
                        // stopping a fresh session that came after).
                        {
                            let mut timer_guard = self.midi_learn_timer.lock().await;
                            if let Some(prev) = timer_guard.take() {
                                prev.abort();
                            }
                            let active_for_timer = active.clone();
                            let session_gen_for_timer = self.midi_learn_session_gen.clone();
                            *timer_guard = Some(tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_secs(timeout_seconds)).await;
                                // Generation check (#1059 race fix): if a
                                // newer session has replaced us, the counter
                                // has advanced. Skip the swap entirely so
                                // we don't stop the freshly-started session.
                                if session_gen_for_timer.load(Ordering::SeqCst) != my_gen {
                                    return;
                                }
                                // `swap` is atomic — only the first writer
                                // (this timer or an explicit stop) sees the
                                // previous true. If `false` was already set
                                // by an explicit stop, this no-ops.
                                if active_for_timer.swap(false, Ordering::SeqCst) {
                                    info!(
                                        "MIDI Learn mode auto-stopped by daemon timeout ({}s)",
                                        timeout_seconds
                                    );
                                }
                            }));
                        }

                        info!(
                            "MIDI Learn mode started via LLM tool (timeout: {}s, daemon-enforced{})",
                            timeout_seconds,
                            if was_already_active {
                                ", restarted"
                            } else {
                                ""
                            }
                        );

                        let (message, instructions) = if was_already_active {
                            (
                                "MIDI Learn mode RESTARTED — your previous start_learn call was preempted by this one. The timer is now fresh. NOTE: events captured during the prior session remain in the buffer; the next conductor_stop_midi_learn will return events accumulated across both sessions. Press a button/pad on your controller.".to_string(),
                                "You MUST acknowledge to the user in chat that you restarted Learn (e.g. 'Restarting Learn — try again') before doing anything else. Otherwise call conductor_stop_midi_learn when input is received. The daemon will auto-stop after timeout_seconds even with no explicit stop call.".to_string(),
                            )
                        } else {
                            (
                                "MIDI Learn mode started. Press a button/pad on your controller.".to_string(),
                                "Use conductor_stop_midi_learn to stop and retrieve captured events. The daemon will auto-stop the session after timeout_seconds even if no explicit stop call arrives.".to_string(),
                            )
                        };

                        ToolCallResult::json(&json!({
                            "success": true,
                            "was_already_active": was_already_active,
                            "message": message,
                            "timeout_seconds": timeout_seconds,
                            "instructions": instructions,
                        }))
                    }
                    None => {
                        // Graceful fallback when running in test mode or standalone
                        warn!(
                            "MIDI Learn start requested but state not connected to engine manager"
                        );
                        ToolCallResult::json(&json!({
                            "success": true,
                            "message": "MIDI Learn mode started (simulation mode - no device connected).",
                            "timeout_seconds": timeout_seconds,
                            "simulation": true,
                            "instructions": "Connect to a device via the daemon for full MIDI Learn functionality."
                        }))
                    }
                }
            }
            "conductor_stop_learn" | "conductor_stop_midi_learn" => {
                // Check if MIDI Learn state is available
                match (&self.midi_learn_active, &self.midi_learn_events) {
                    (Some(active), Some(events)) => {
                        // Bump session generation so any pending timer's
                        // wake check fails (#1059 race fix). Belt-and-
                        // braces with the abort() below — even if abort
                        // loses the race, the gen check makes the timer's
                        // body a no-op.
                        self.midi_learn_session_gen.fetch_add(1, Ordering::SeqCst);

                        // Stop MIDI Learn
                        active.store(false, Ordering::SeqCst);

                        // Cancel the auto-stop timer (#1053) so it doesn't
                        // fire late and stop a fresh session that came
                        // after this explicit stop. No-op if the timer
                        // already fired or no session was active.
                        {
                            let mut timer_guard = self.midi_learn_timer.lock().await;
                            if let Some(prev) = timer_guard.take() {
                                prev.abort();
                            }
                        }

                        // Drain captured events
                        let captured_events: Vec<MidiLearnEvent> = {
                            let mut events_guard = events.lock().await;
                            events_guard.drain(..).collect()
                        };

                        let event_count = captured_events.len();
                        info!(
                            "MIDI Learn mode stopped via LLM tool ({} events captured)",
                            event_count
                        );

                        // Analyze events to suggest a trigger config
                        let suggested_trigger = self.analyze_midi_learn_events(&captured_events);

                        ToolCallResult::json(&json!({
                            "success": true,
                            "message": format!("MIDI Learn stopped. {} events captured.", event_count),
                            "events": captured_events,
                            "suggested_trigger": suggested_trigger,
                            "event_count": event_count
                        }))
                    }
                    _ => {
                        // Graceful fallback when running in test mode or standalone
                        warn!(
                            "MIDI Learn stop requested but state not connected to engine manager"
                        );
                        ToolCallResult::json(&json!({
                            "success": true,
                            "message": "MIDI Learn stopped (simulation mode).",
                            "events": [],
                            "pattern": null,
                            "event_count": 0,
                            "simulation": true
                        }))
                    }
                }
            }
            // v4.23.0: Multi-device stateful tools (ADR-009 Phase 5)
            "conductor_set_device_enabled" => match &self.daemon_state_refs {
                Some(refs) => {
                    let device_id = arguments
                        .as_ref()
                        .and_then(|a| a.get("device_id"))
                        .and_then(|v| v.as_str());
                    let enabled = arguments
                        .as_ref()
                        .and_then(|a| a.get("enabled"))
                        .and_then(|v| v.as_bool());

                    match (device_id, enabled) {
                        (Some(id), Some(en)) => {
                            let dev_id = conductor_core::identity::DeviceId::from_alias(id);
                            let mut guard = refs.input_manager.lock().await;
                            if let Some(ref mut mgr) = *guard {
                                mgr.set_device_enabled(&dev_id, en);
                                let action = if en { "enabled" } else { "muted" };
                                ToolCallResult::json(&json!({
                                    "device_id": id,
                                    "enabled": en,
                                    "message": format!("Device '{}' {}", id, action)
                                }))
                            } else {
                                ToolCallResult::error("Input manager not available")
                            }
                        }
                        _ => ToolCallResult::error(
                            "Missing required arguments: device_id (string), enabled (boolean)",
                        ),
                    }
                }
                None => ToolCallResult::error("Daemon state not available"),
            },
            "conductor_scan_ports" => match &self.daemon_state_refs {
                Some(refs) => {
                    if let Err(e) = refs
                        .command_tx
                        .send(crate::daemon::types::DaemonCommand::HotPlugCheck)
                        .await
                    {
                        ToolCallResult::error(&format!("Failed to trigger rescan: {}", e))
                    } else {
                        ToolCallResult::json(&json!({
                            "message": "Port rescan triggered"
                        }))
                    }
                }
                None => ToolCallResult::error("Daemon state not available"),
            },
            // v4.26.69: Switch active mode by name
            // DEPRECATED (ADR-040): switches the mode without touching any manual
            // lock — prefer conductor_set_mode. Behaviour unchanged (the
            // description was the inaccurate part, Copilot #2290).
            "conductor_switch_mode" => {
                let mode_name = arguments
                    .as_ref()
                    .and_then(|a| a.get("mode"))
                    .and_then(|v| v.as_str());

                match mode_name {
                    Some(name) => {
                        // Phase 2 - Issue #321: Validate mode exists in config
                        let mode_index =
                            _config.and_then(|cfg| cfg.modes.iter().position(|m| m.name == name));

                        match mode_index {
                            Some(idx) => {
                                // Phase 2 - Issue #321: Send mode change command to daemon
                                match &self.daemon_state_refs {
                                    Some(refs) => {
                                        // Council review fix: use send().await instead of try_send to avoid silent drops
                                        if let Err(e) = refs
                                            .command_tx
                                            .send(crate::daemon::types::DaemonCommand::ModeChange {
                                                mode: name.to_string(),
                                            })
                                            .await
                                        {
                                            warn!(
                                                "Failed to send mode change command (channel closed): {}",
                                                e
                                            );
                                            return ToolCallResult::error(&format!(
                                                "Failed to trigger mode change (daemon shutting down): {}",
                                                e
                                            ));
                                        }

                                        info!(
                                            "Mode change to '{}' (index {}) requested via MCP tool",
                                            name, idx
                                        );
                                        ToolCallResult::json(&json!({
                                            "success": true,
                                            "mode_name": name,
                                            "mode_index": idx,
                                            "message": format!("Mode change to '{}' triggered", name)
                                        }))
                                    }
                                    None => {
                                        // Fallback when daemon state refs not available (test mode)
                                        warn!(
                                            "Mode switch requested but daemon state refs not available"
                                        );
                                        ToolCallResult::json(&json!({
                                            "success": true,
                                            "mode_name": name,
                                            "mode_index": idx,
                                            "message": format!("Mode change to '{}' validated (simulation mode)", name),
                                            "simulation": true
                                        }))
                                    }
                                }
                            }
                            None => {
                                let available: Vec<&str> = _config
                                    .map(|cfg| cfg.modes.iter().map(|m| m.name.as_str()).collect())
                                    .unwrap_or_default();
                                ToolCallResult::error(&format!(
                                    "Mode '{}' not found. Available modes: {:?}",
                                    name, available
                                ))
                            }
                        }
                    }
                    None => ToolCallResult::error("Missing required argument: mode (string)"),
                }
            }
            // ADR-040 D4 §4.2 (Slice 4c) — mode-lock tools (shared helpers; same
            // command-channel path as conductor_switch_mode above).
            "conductor_set_mode" => match &self.daemon_state_refs {
                Some(refs) => {
                    crate::daemon::mode_mcp::set_mode(&refs.command_tx, arguments.as_ref()).await
                }
                None => ToolCallResult::error("Daemon state not available"),
            },
            "conductor_unlock_mode" => match &self.daemon_state_refs {
                Some(refs) => crate::daemon::mode_mcp::unlock_mode(&refs.command_tx).await,
                None => ToolCallResult::error("Daemon state not available"),
            },
            "conductor_mode_status" => match &self.daemon_state_refs {
                Some(refs) => crate::daemon::mode_mcp::mode_status(&refs.command_tx).await,
                None => ToolCallResult::error("Daemon state not available"),
            },
            // Phase 1 - Issue #323: Switch profile
            "conductor_switch_profile" => {
                let profile_name = arguments
                    .as_ref()
                    .and_then(|a| a.get("profile_name"))
                    .and_then(|v| v.as_str());
                let config_path = arguments
                    .as_ref()
                    .and_then(|a| a.get("config_path"))
                    .and_then(|v| v.as_str());
                // #2564 D5 (additive): optional GUI profile id so the daemon can
                // persist/report the identity the GUI keys by.
                let profile_id = arguments
                    .as_ref()
                    .and_then(|a| a.get("profile_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);

                match (profile_name, config_path) {
                    (Some(name), Some(path)) => {
                        // Validate profile path using shared helper
                        let validated_path = match crate::daemon::types::validate_profile_path(path)
                        {
                            Ok(path) => path,
                            Err(e) => {
                                return ToolCallResult::error(&e);
                            }
                        };
                        let path = validated_path.display().to_string();

                        match &self.daemon_state_refs {
                            Some(refs) => {
                                // Phase 2 S7: Synchronous profile switch — await result
                                let (result_tx, result_rx) = tokio::sync::oneshot::channel();
                                if let Err(e) = refs
                                    .command_tx
                                    .send(crate::daemon::types::DaemonCommand::ProfileSwitch {
                                        profile_name: name.to_string(),
                                        config_path: path.to_string(),
                                        profile_id,
                                        result_tx: Some(result_tx),
                                    })
                                    .await
                                {
                                    return ToolCallResult::error(&format!(
                                        "Failed to trigger profile switch: {}",
                                        e
                                    ));
                                }

                                // Wait for result with timeout
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(10),
                                    result_rx,
                                )
                                .await
                                {
                                    Ok(Ok(Ok(activated_name))) => {
                                        info!(
                                            "Profile '{}' activated via MCP tool",
                                            activated_name
                                        );
                                        ToolCallResult::json(&json!({
                                            "success": true,
                                            "profile_name": activated_name,
                                            "config_path": path,
                                            "message": format!("Profile '{}' activated successfully", activated_name)
                                        }))
                                    }
                                    Ok(Ok(Err(err))) => {
                                        warn!("Profile switch failed via MCP tool: {}", err);
                                        ToolCallResult::error(&format!(
                                            "Profile switch failed: {}",
                                            err
                                        ))
                                    }
                                    Ok(Err(_)) => ToolCallResult::error(
                                        "Profile switch result channel closed",
                                    ),
                                    Err(_) => {
                                        ToolCallResult::error("Profile switch timed out after 10s")
                                    }
                                }
                            }
                            None => {
                                warn!(
                                    "Profile switch requested but daemon state refs not available"
                                );
                                ToolCallResult::json(&json!({
                                    "success": true,
                                    "profile_name": name,
                                    "config_path": path,
                                    "message": format!("Profile switch to '{}' validated (simulation mode)", name),
                                    "simulation": true
                                }))
                            }
                        }
                    }
                    _ => ToolCallResult::error(
                        "Missing required arguments: profile_name and config_path",
                    ),
                }
            }

            // Phase 1 - Issue #323: Get active profile
            "conductor_get_active_profile" => match &self.daemon_state_refs {
                Some(refs) => {
                    let profile = (**refs.active_profile.load()).clone();
                    ToolCallResult::json(&json!({
                        "active_profile": profile
                    }))
                }
                None => ToolCallResult::json(&json!({
                    "active_profile": null
                })),
            },

            // GUI-only profile tools (ADR-023: profile state lives in GUI, not daemon)
            // These should be intercepted frontend-side; if they reach the daemon,
            // return a clear error rather than falling through to "unknown tool".
            "conductor_list_profiles" | "conductor_create_profile" | "conductor_delete_profile" => {
                ToolCallResult::error(super::super::mcp_tools::GUI_ONLY_TOOL_ERROR)
            }

            "conductor_list_plugins"
            | "conductor_plugin_info"
            | "conductor_enable_plugin"
            | "conductor_disable_plugin" => {
                // Plugin tools require daemon state — route via IPC command channel
                match &self.daemon_state_refs {
                    Some(refs) => {
                        let ipc_cmd = match tool_name {
                            "conductor_list_plugins" => {
                                crate::daemon::types::IpcCommand::ListPlugins
                            }
                            "conductor_plugin_info" => {
                                crate::daemon::types::IpcCommand::GetPluginInfo
                            }
                            "conductor_enable_plugin" => {
                                crate::daemon::types::IpcCommand::EnablePlugin
                            }
                            "conductor_disable_plugin" => {
                                crate::daemon::types::IpcCommand::DisablePlugin
                            }
                            _ => unreachable!(),
                        };
                        let args = arguments.clone().unwrap_or(json!({}));
                        let request = crate::daemon::types::IpcRequest {
                            id: uuid::Uuid::new_v4().to_string(),
                            command: ipc_cmd,
                            args,
                        };
                        // Send command and await response via command channel
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        if let Err(e) =
                            refs.command_tx
                                .send(crate::daemon::types::DaemonCommand::IpcRequest {
                                    request,
                                    // PR-D: Internal-origin daemon command
                                    // (no external peer). Today the
                                    // receiving plugin-management arms in
                                    // `engine_manager::handle_ipc_request`
                                    // (`IpcCommand::ListPlugins` / `GetPluginInfo`
                                    // / `EnablePlugin` / `DisablePlugin`)
                                    // don't consult `caller_ctx` — they
                                    // dispatch directly without going
                                    // through `ToolExecutor::execute` —
                                    // so this field is currently inert
                                    // for those handlers. We pass
                                    // `internal_trusted` rather than
                                    // `None` deliberately as
                                    // future-proofing: when those
                                    // handlers are eventually wired
                                    // through the gate (or other
                                    // gate-aware handlers grow that
                                    // also dispatch via this channel),
                                    // `GuiTrusted` is the correct trust
                                    // band for a daemon-internal call
                                    // whose outer LLM tool boundary has
                                    // already been gate-checked. Closes
                                    // the PR-B `TODO(PR-D, gate-bypass
                                    // on None)` for this site at the
                                    // type level even though the
                                    // runtime effect is currently nil.
                                    caller_ctx: Some(
                                        crate::security::CallerContext::internal_trusted(),
                                    ),
                                    response_tx: resp_tx,
                                })
                                .await
                        {
                            return ToolCallResult::error(&format!(
                                "Failed to send plugin command: {}",
                                e
                            ));
                        }
                        match resp_rx.await {
                            Ok(response) => ToolCallResult::json(&json!(response)),
                            Err(e) => ToolCallResult::error(&format!(
                                "Failed to receive plugin response: {}",
                                e
                            )),
                        }
                    }
                    None => ToolCallResult::error(
                        "Plugin management not available (no daemon connection)",
                    ),
                }
            }

            // LLM Editor tools (ADR-017 Phase 2C) — return structured data for GUI.
            //
            // These tools are intentionally "advisory" not "imperative": they return
            // success JSON describing the desired editor state but do NOT directly
            // mutate the frontend. The LLM communicates the result to the user, who
            // then acts in the GUI. The frontend's agentic tool loop intercepts these
            // tool names and applies the changes to the MappingEditor store client-side.
            "conductor_set_mapping_editor" => {
                let trigger = arguments.as_ref().and_then(|a| a.get("trigger").cloned());
                let action = arguments.as_ref().and_then(|a| a.get("action").cloned());
                let description = arguments
                    .as_ref()
                    .and_then(|a| a.get("description"))
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let mode = arguments
                    .as_ref()
                    .and_then(|a| a.get("mode"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Default")
                    .to_string();

                ToolCallResult::json(&json!({
                    "status": "editor_opened",
                    "trigger": trigger,
                    "action": action,
                    "description": description,
                    "mode": mode,
                    "instructions": "The GUI MappingEditor has been populated with this data. The user can review and save."
                }))
            }
            "conductor_update_mapping_editor" => {
                // LLMs sometimes double-encode nested objects as JSON strings.
                // Try as_object() first, then fall back to parsing the string.
                let fields_value = arguments.as_ref().and_then(|a| a.get("fields"));
                let fields = fields_value.and_then(|f| {
                    f.as_object().cloned().or_else(|| {
                        f.as_str().and_then(|s| {
                            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(s)
                                .ok()
                        })
                    })
                });

                if fields.as_ref().is_none_or(|f| f.is_empty()) {
                    return ToolCallResult::error(
                        "Missing or empty required parameter: fields (must be a JSON object or JSON-encoded string)",
                    );
                }

                let fields_map = fields.unwrap();
                let updated_keys: Vec<&String> = fields_map.keys().collect();

                ToolCallResult::json(&json!({
                    "status": "fields_updated",
                    "fields": fields_map,
                    "updated_keys": updated_keys,
                    "instructions": "The MappingEditor fields have been updated. The user can review the changes."
                }))
            }

            _ => ToolCallResult::error(&format!("Unknown stateful tool: {}", tool_name)),
        }
    }

    /// Analyze captured MIDI Learn events to suggest a trigger pattern
    fn analyze_midi_learn_events(&self, events: &[MidiLearnEvent]) -> Option<Value> {
        if events.is_empty() {
            return None;
        }

        // Find the most common event type and suggest a trigger
        // This is a simple analysis - more sophisticated pattern detection
        // is handled by the GUI's MidiLearnSession

        // Look for pattern events first (detected by EventProcessor)
        for event in events.iter().rev() {
            if let Some(pattern_type) = &event.pattern_type {
                match pattern_type {
                    PatternType::LongPress => {
                        if let Some(note) = event.note {
                            return Some(json!({
                                "type": "LongPress",
                                "note": note,
                                "duration_ms": event.pattern_duration_ms.unwrap_or(2000)
                            }));
                        }
                    }
                    PatternType::DoubleTap => {
                        if let Some(note) = event.note {
                            return Some(json!({
                                "type": "DoubleTap",
                                "note": note,
                                "timeout_ms": event.pattern_timeout_ms.unwrap_or(300)
                            }));
                        }
                    }
                    PatternType::Chord => {
                        if let Some(notes) = &event.pattern_notes {
                            return Some(json!({
                                "type": "Chord",
                                "notes": notes,
                                "window_ms": event.pattern_timeout_ms.unwrap_or(100)
                            }));
                        }
                    }
                    PatternType::GamepadChord => {
                        if let Some(buttons) = &event.pattern_buttons {
                            return Some(json!({
                                "type": "GamepadButtonChord",
                                "buttons": buttons,
                                "window_ms": event.pattern_timeout_ms.unwrap_or(100)
                            }));
                        }
                    }
                    PatternType::MediumPress => {
                        if let Some(note) = event.note {
                            return Some(json!({
                                "type": "Note",
                                "note": note,
                                "velocity_min": 1
                            }));
                        }
                    }
                    // ContextSwitch is not an input gesture — it annotates a
                    // state transition, never a Learn-suggested trigger.
                    PatternType::ContextSwitch => {}
                }
            }
        }

        // Fall back to simple event analysis
        if let Some(first_event) = events.first() {
            match first_event.event_type {
                EventType::NoteOn => {
                    if let Some(note) = first_event.note {
                        // Check for VelocityRange: 3+ presses of the same note with velocity range > 30
                        let same_note_velocities: Vec<u8> = events
                            .iter()
                            .filter(|e| e.event_type == EventType::NoteOn && e.note == Some(note))
                            .filter_map(|e| e.velocity)
                            .collect();

                        if same_note_velocities.len() >= 3 {
                            let min_vel = *same_note_velocities.iter().min().unwrap_or(&1);
                            let max_vel = *same_note_velocities.iter().max().unwrap_or(&127);
                            if max_vel - min_vel > 30 {
                                // Suggest VelocityRange with soft/medium/hard zones.
                                // #2134: emit the CANONICAL `Trigger::VelocityRange`
                                // field names (`soft_max` / `medium_max`), not a
                                // `ranges` object. The previous `ranges` shape did
                                // not match the enum variant, so applying the
                                // suggestion made serde silently drop it and the
                                // trigger defaulted to soft_max=40/medium_max=80,
                                // discarding the learned thresholds.
                                let range = max_vel - min_vel;
                                let soft_max = min_vel + range / 3;
                                let medium_max = min_vel + 2 * range / 3;
                                return Some(json!({
                                    "type": "VelocityRange",
                                    "note": note,
                                    "soft_max": soft_max,
                                    "medium_max": medium_max
                                }));
                            }
                        }

                        return Some(json!({
                            "type": "Note",
                            "note": note,
                            "velocity_min": first_event.velocity.unwrap_or(1)
                        }));
                    }
                }
                EventType::Cc => {
                    if let Some(cc) = first_event.cc {
                        return Some(json!({
                            "type": "CC",
                            "cc": cc
                        }));
                    }
                }
                EventType::Encoder => {
                    if let Some(cc) = first_event.cc {
                        return Some(json!({
                            "type": "EncoderTurn",
                            "cc": cc,
                            "direction": "Any"
                        }));
                    }
                }
                EventType::PitchBend => {
                    return Some(json!({
                        "type": "PitchBend",
                        "bend_range": [-8192, 8191]
                    }));
                }
                EventType::Aftertouch => {
                    return Some(json!({
                        "type": "Aftertouch",
                        "pressure_min": 1
                    }));
                }
                EventType::PolyPressure => {
                    // PolyPressure (polyphonic aftertouch) maps to Aftertouch trigger
                    // (the Trigger enum has no PolyPressure variant — Aftertouch is the closest match)
                    return Some(json!({
                        "type": "Aftertouch",
                        "pressure_min": 1
                    }));
                }
                EventType::GamepadButton => {
                    if let Some(button) = first_event.button {
                        return Some(json!({
                            "type": "GamepadButton",
                            "button": button
                        }));
                    }
                }
                EventType::GamepadAxis => {
                    if let Some(axis) = first_event.axis {
                        return Some(json!({
                            "type": "GamepadAnalogStick",
                            "axis": axis
                        }));
                    }
                }
                EventType::GamepadTrigger => {
                    if let Some(trigger) = first_event.trigger {
                        return Some(json!({
                            "type": "GamepadTrigger",
                            "trigger": trigger
                        }));
                    }
                }
                // ADR-025 Phase 1: a single PC capture suggests the
                // ProgramChange trigger. Channel comes from the captured
                // event (defaults to 0 in MidiLearnEvent::default).
                EventType::ProgramChange => {
                    if let Some(pc) = first_event.pc {
                        return Some(json!({
                            "type": "ProgramChange",
                            "pc": pc,
                            "channel": first_event.channel,
                        }));
                    }
                }
                _ => {}
            }
        }

        None
    }

    /// Execute a ConfigChange tool by creating a plan
    async fn execute_config_change(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> ExecutionResult {
        let args_json = arguments.as_ref().map(|a| a.to_string());

        // D4.A.3.3.B.1: snapshot config via LiveConfig — always loaded
        // post-EngineManager::new, so the legacy "Configuration not loaded"
        // branch became unreachable. Keep the `&Config` binding alive via the
        // snap Arc; create_plan_for_tool needs `&Config` for hashing.
        let snap = self.live_config.load();
        let config: &Config = snap.config.as_ref();

        // Parse arguments and create plan
        let plan_result = self.create_plan_for_tool(tool_name, arguments, config);

        match plan_result {
            Ok(plan) => {
                let plan_id = plan.id;
                let changes_count = plan.changes.len();

                // Store the plan
                {
                    let mut plans = self.pending_plans.write().await;
                    plans.insert(plan_id, plan.clone());
                    info!("Created plan {} for tool '{}'", plan_id, tool_name);
                }

                // Audit log plan creation (P4-04)
                if let Some(ref logger) = self.audit_logger {
                    logger.log_plan_created(
                        &plan_id.to_string(),
                        changes_count,
                        Some(UserContext::local_user()),
                    );
                }

                ExecutionResult::PlanCreated { plan }
            }
            Err(e) => {
                // Audit log the error
                if let Some(ref logger) = self.audit_logger {
                    logger.log_tool_error(
                        tool_name,
                        AuditRiskTier::ConfigChange,
                        args_json.as_deref(),
                        &e.to_string(),
                        std::time::Duration::ZERO,
                        Some(UserContext::local_user()),
                    );
                }
                ExecutionResult::Error {
                    message: e.to_string(),
                }
            }
        }
    }

    /// Execute a HardwareIO tool with multi-step confirmation (P4-01)
    async fn execute_hardware_io(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> ExecutionResult {
        let start_time = Instant::now();
        let args = arguments.unwrap_or(json!({}));
        let args_json = serde_json::to_string(&args).ok();

        let confirmation_token = args.get("confirmation_token").and_then(|t| t.as_str());

        let status = match tool_name {
            "conductor_send_sysex" => {
                let device = match args.get("device").and_then(|d| d.as_str()) {
                    Some(d) => d,
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: device".to_string(),
                        };
                    }
                };

                let data: Vec<u8> = match args.get("data").and_then(|d| d.as_array()) {
                    Some(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_u64().map(|n| n as u8))
                        .collect(),
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: data (array of bytes)".to_string(),
                        };
                    }
                };

                match self.confirmation_manager.request_sysex_confirmation(
                    device,
                    &data,
                    confirmation_token,
                ) {
                    Ok(status) => status,
                    Err(e) => {
                        return ExecutionResult::Error {
                            message: e.to_string(),
                        };
                    }
                }
            }

            "conductor_device_reset" => {
                let device = match args.get("device").and_then(|d| d.as_str()) {
                    Some(d) => d,
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: device".to_string(),
                        };
                    }
                };

                let reset_type = match args.get("reset_type").and_then(|r| r.as_str()) {
                    Some(r) => r,
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: reset_type".to_string(),
                        };
                    }
                };

                match self.confirmation_manager.request_reset_confirmation(
                    device,
                    reset_type,
                    confirmation_token,
                ) {
                    Ok(status) => status,
                    Err(e) => {
                        return ExecutionResult::Error {
                            message: e.to_string(),
                        };
                    }
                }
            }

            "conductor_send_midi" => {
                let port = match args.get("port").and_then(|p| p.as_str()) {
                    Some(p) => p,
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: port".to_string(),
                        };
                    }
                };

                let messages: Vec<MidiSendMessage> =
                    match args.get("messages").and_then(|m| m.as_array()) {
                        Some(arr) => {
                            let mut msgs = Vec::new();
                            for (i, item) in arr.iter().enumerate() {
                                match serde_json::from_value::<MidiSendMessage>(item.clone()) {
                                    Ok(msg) => {
                                        if let Err(e) = msg.validate() {
                                            return ExecutionResult::Error {
                                                message: format!(
                                                    "Invalid message at index {}: {}",
                                                    i, e
                                                ),
                                            };
                                        }
                                        msgs.push(msg);
                                    }
                                    Err(e) => {
                                        return ExecutionResult::Error {
                                            message: format!(
                                                "Failed to parse message at index {}: {}",
                                                i, e
                                            ),
                                        };
                                    }
                                }
                            }
                            msgs
                        }
                        None => {
                            return ExecutionResult::Error {
                                message: "Missing required argument: messages (array)".to_string(),
                            };
                        }
                    };

                if messages.is_empty() {
                    return ExecutionResult::Error {
                        message: "Messages array must not be empty".to_string(),
                    };
                }

                // Build byte representations for audit
                let byte_descriptions: Vec<String> = messages
                    .iter()
                    .filter_map(|m| m.to_bytes().ok().map(|b| format!("{:02X?}", b)))
                    .collect();

                match self.confirmation_manager.request_midi_send_confirmation(
                    port,
                    &messages,
                    confirmation_token,
                ) {
                    Ok(status) => match &status {
                        ConfirmationStatus::Confirmed { .. } => {
                            // Auto-confirmed — return success with byte details
                            ConfirmationStatus::Confirmed {
                                result: format!(
                                    "Approved: {} MIDI message(s) to '{}': [{}]",
                                    messages.len(),
                                    port,
                                    byte_descriptions.join(", ")
                                ),
                            }
                        }
                        _ => status,
                    },
                    Err(e) => {
                        return ExecutionResult::Error {
                            message: e.to_string(),
                        };
                    }
                }
            }

            "conductor_probe_device_identity" => {
                let port_name = match args.get("port_name").and_then(|p| p.as_str()) {
                    Some(p) => p.to_string(),
                    None => {
                        return ExecutionResult::Error {
                            message: "Missing required argument: port_name".to_string(),
                        };
                    }
                };

                // The Identity Request is `F0 7E 7F 06 01 F7` — universal
                // and benign. `SysExValidator::validate()` expects the
                // *inner* SysEx data only (without F0/F7 framing) per its
                // contract; passing the full frame causes the validator
                // to read 0xF0 as a manufacturer ID, classify the message
                // as `UnknownManufacturer` (which DOES require user
                // confirmation), and break the auto-confirm path. Slice
                // to bytes 1..5 = `[7E, 7F, 06, 01]` so the validator
                // hits the Universal Non-Realtime → IdentityRequest path
                // which is `requires_confirmation() == false`.
                use conductor_core::device_intelligence::sysex_identity::IDENTITY_REQUEST;
                let validator_data = &IDENTITY_REQUEST[1..IDENTITY_REQUEST.len() - 1];
                match self.confirmation_manager.request_sysex_confirmation(
                    &port_name,
                    validator_data,
                    confirmation_token,
                ) {
                    Ok(ConfirmationStatus::Confirmed { .. }) => {
                        // Dispatch the actual probe through the daemon
                        // command channel. The engine_manager handler
                        // resolves the paired output and runs the sync
                        // probe via spawn_blocking, then sends the
                        // outcome back through the response channel.
                        let Some(state_refs) = self.daemon_state_refs.as_ref() else {
                            return ExecutionResult::Error {
                                message:
                                    "Daemon state refs not available — probe requires running daemon"
                                        .to_string(),
                            };
                        };
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        if let Err(e) = state_refs
                            .command_tx
                            .send(crate::daemon::DaemonCommand::ProbeDeviceIdentity {
                                port_name: port_name.clone(),
                                response_tx: resp_tx,
                            })
                            .await
                        {
                            return ExecutionResult::Error {
                                message: format!("Failed to dispatch probe command: {}", e),
                            };
                        }
                        // Bound the wait so a stalled engine loop
                        // (shutdown, hung task) surfaces a clear error
                        // instead of hanging the MCP client. Sizing:
                        // Phase 1.A serialises probes globally with a
                        // 1 s reply timeout, so queue_wait grows
                        // roughly linearly with concurrent probes —
                        // probe-on-connect (Phase 3) can dispatch one
                        // per device discovered in a hot-plug burst.
                        // 30 s comfortably covers ~25-device bursts
                        // (25 × 1 s reply + scheduling slack) while
                        // still failing fast for genuine deadlocks. A
                        // single healthy probe never approaches this.
                        // `probe_outcome` is `Result<ProbeResult,
                        // ProbeStartError>` — the *overall* probe
                        // outcome, NOT a `ProbeResult`. Naming
                        // mirrors `ProbeOutcomeWire` to make the
                        // collapse below read clearly.
                        let probe_outcome =
                            match tokio::time::timeout(std::time::Duration::from_secs(30), resp_rx)
                                .await
                            {
                                Ok(Ok(o)) => o,
                                Ok(Err(_)) => {
                                    return ExecutionResult::Error {
                                        message:
                                            "Probe response channel closed before result arrived"
                                                .to_string(),
                                    };
                                }
                                Err(_) => {
                                    return ExecutionResult::Error {
                                        message:
                                            "Probe timed out waiting for daemon response (>30s)"
                                                .to_string(),
                                    };
                                }
                            };
                        // Phase 3.B.1: collapse the
                        // `Result<ProbeResult, ProbeStartError>` through
                        // `ProbeOutcomeWire` for the JSON wire format —
                        // produces the flat `{"status": "..."}` shape
                        // Phase 2 MCP callers parse. Build the fallback
                        // via `json!()` so any quotes/newlines in the
                        // serde error message are properly escaped — raw
                        // `format!()` would produce invalid JSON.
                        let wire = ProbeOutcomeWire::from(probe_outcome);
                        let outcome_json = serde_json::to_string(&wire).unwrap_or_else(|e| {
                            serde_json::json!({
                                "error": format!("serialize: {}", e),
                            })
                            .to_string()
                        });
                        ConfirmationStatus::Confirmed {
                            result: outcome_json,
                        }
                    }
                    Ok(other_status) => other_status,
                    Err(e) => {
                        return ExecutionResult::Error {
                            message: e.to_string(),
                        };
                    }
                }
            }

            _ => {
                return ExecutionResult::Error {
                    message: format!("Unknown HardwareIO tool: {}", tool_name),
                };
            }
        };

        // Audit log based on status
        if let Some(ref logger) = self.audit_logger {
            let execution_time = start_time.elapsed();
            match &status {
                ConfirmationStatus::Confirmed { .. } => {
                    logger.log_tool_complete(
                        tool_name,
                        AuditRiskTier::HardwareIO,
                        args_json.as_deref(),
                        Some(&format!("{:?}", status)),
                        execution_time,
                        Some(UserContext::local_user()),
                    );
                }
                ConfirmationStatus::Blocked { reason } => {
                    logger.log_tool_denied(
                        tool_name,
                        AuditRiskTier::HardwareIO,
                        reason,
                        Some(UserContext::local_user()),
                    );
                }
                ConfirmationStatus::RequiresConfirmation { .. } => {
                    // Log that confirmation was requested
                    logger.log_tool_start(
                        tool_name,
                        AuditRiskTier::HardwareIO,
                        args_json.as_deref(),
                        Some(UserContext::local_user()),
                    );
                }
                ConfirmationStatus::InvalidToken { reason } => {
                    logger.log_tool_error(
                        tool_name,
                        AuditRiskTier::HardwareIO,
                        args_json.as_deref(),
                        reason,
                        execution_time,
                        Some(UserContext::local_user()),
                    );
                }
            }
        }

        ExecutionResult::HardwareIoConfirmation {
            status,
            tool_name: tool_name.to_string(),
        }
    }

    /// Create a ConfigPlan for a ConfigChange tool
    fn create_plan_for_tool(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
        config: &Config,
    ) -> Result<ConfigPlan, PlanError> {
        let args = arguments.unwrap_or(json!({}));

        match tool_name {
            "conductor_create_mapping" => {
                let mode = args
                    .get("mode")
                    .and_then(|m| m.as_str())
                    .ok_or_else(|| PlanError::InvalidAction("Missing 'mode' argument".to_string()))?
                    .to_string();

                let trigger: Trigger =
                    serde_json::from_value(args.get("trigger").cloned().ok_or_else(|| {
                        PlanError::InvalidTrigger("Missing 'trigger' argument".to_string())
                    })?)
                    .map_err(|e| PlanError::InvalidTrigger(e.to_string()))?;

                let action: ActionConfig =
                    serde_json::from_value(args.get("action").cloned().ok_or_else(|| {
                        PlanError::InvalidAction("Missing 'action' argument".to_string())
                    })?)
                    .map_err(|e| PlanError::InvalidAction(e.to_string()))?;

                let description = args
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string());

                // ADR-038 Slice 6: optional let_through (default false = swallow).
                let let_through = args
                    .get("let_through")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                // Validate mode exists
                if !config.modes.iter().any(|m| m.name == mode) {
                    return Err(PlanError::ModeNotFound(mode));
                }

                Ok(ConfigPlan::new(
                    format!("Create new mapping in mode '{}'", mode),
                    vec![ConfigChange::CreateMapping {
                        mode,
                        trigger,
                        action,
                        description,
                        let_through,
                    }],
                    config,
                ))
            }

            // ADR-025 Phase 2.H — focused tool for authoring context-
            // switch mappings. Reuses the CreateMapping change but
            // enforces that `action` is PcContextSwitch / CcContextSwitch
            // so the LLM gets a clear error instead of silently
            // authoring a non-routing mapping.
            "conductor_set_context_mapping" => {
                let mode = args
                    .get("mode")
                    .and_then(|m| m.as_str())
                    .ok_or_else(|| PlanError::InvalidAction("Missing 'mode' argument".to_string()))?
                    .to_string();

                let trigger: Trigger =
                    serde_json::from_value(args.get("trigger").cloned().ok_or_else(|| {
                        PlanError::InvalidTrigger("Missing 'trigger' argument".to_string())
                    })?)
                    .map_err(|e| PlanError::InvalidTrigger(e.to_string()))?;

                let action: ActionConfig =
                    serde_json::from_value(args.get("action").cloned().ok_or_else(|| {
                        PlanError::InvalidAction("Missing 'action' argument".to_string())
                    })?)
                    .map_err(|e| PlanError::InvalidAction(e.to_string()))?;

                if !matches!(
                    action,
                    ActionConfig::PcContextSwitch { .. } | ActionConfig::CcContextSwitch { .. }
                ) {
                    return Err(PlanError::InvalidAction(
                        "conductor_set_context_mapping expects action.type = 'PcContextSwitch' or 'CcContextSwitch'; use conductor_create_mapping for other action shapes".to_string(),
                    ));
                }

                let description = args
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string());

                if !config.modes.iter().any(|m| m.name == mode) {
                    return Err(PlanError::ModeNotFound(mode));
                }

                Ok(ConfigPlan::new(
                    format!("Create context-switch mapping in mode '{}'", mode),
                    vec![ConfigChange::CreateMapping {
                        mode,
                        trigger,
                        action,
                        description,
                        // Context-switch mappings consume the event (route by
                        // prior state); let-through doesn't apply.
                        let_through: false,
                    }],
                    config,
                ))
            }

            "conductor_update_mapping" => {
                let mode = args
                    .get("mode")
                    .and_then(|m| m.as_str())
                    .ok_or_else(|| PlanError::InvalidAction("Missing 'mode' argument".to_string()))?
                    .to_string();

                let index = args.get("index").and_then(|i| i.as_u64()).ok_or_else(|| {
                    PlanError::InvalidAction("Missing 'index' argument".to_string())
                })? as usize;

                let trigger: Trigger =
                    serde_json::from_value(args.get("trigger").cloned().ok_or_else(|| {
                        PlanError::InvalidTrigger("Missing 'trigger' argument".to_string())
                    })?)
                    .map_err(|e| PlanError::InvalidTrigger(e.to_string()))?;

                let action: ActionConfig =
                    serde_json::from_value(args.get("action").cloned().ok_or_else(|| {
                        PlanError::InvalidAction("Missing 'action' argument".to_string())
                    })?)
                    .map_err(|e| PlanError::InvalidAction(e.to_string()))?;

                let description = args
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string());

                // Validate mode and index
                let mode_obj = config
                    .modes
                    .iter()
                    .find(|m| m.name == mode)
                    .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

                if index >= mode_obj.mappings.len() {
                    return Err(PlanError::IndexOutOfRange {
                        mode: mode.clone(),
                        index,
                        count: mode_obj.mappings.len(),
                    });
                }

                Ok(ConfigPlan::new(
                    format!("Update mapping {} in mode '{}'", index, mode),
                    vec![ConfigChange::UpdateMapping {
                        mode,
                        index,
                        trigger,
                        action,
                        description,
                    }],
                    config,
                ))
            }

            "conductor_delete_mapping" => {
                let mode = args
                    .get("mode")
                    .and_then(|m| m.as_str())
                    .ok_or_else(|| PlanError::InvalidAction("Missing 'mode' argument".to_string()))?
                    .to_string();

                let index = args.get("index").and_then(|i| i.as_u64()).ok_or_else(|| {
                    PlanError::InvalidAction("Missing 'index' argument".to_string())
                })? as usize;

                // Validate mode and index
                let mode_obj = config
                    .modes
                    .iter()
                    .find(|m| m.name == mode)
                    .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

                if index >= mode_obj.mappings.len() {
                    return Err(PlanError::IndexOutOfRange {
                        mode: mode.clone(),
                        index,
                        count: mode_obj.mappings.len(),
                    });
                }

                Ok(ConfigPlan::new(
                    format!("Delete mapping {} in mode '{}'", index, mode),
                    vec![ConfigChange::DeleteMapping { mode, index }],
                    config,
                ))
            }

            "conductor_batch_changes" => {
                // P3-07: Batch operations support
                let operations = args
                    .get("operations")
                    .and_then(|o| o.as_array())
                    .ok_or_else(|| {
                        PlanError::InvalidAction("Missing 'operations' array".to_string())
                    })?;

                if operations.is_empty() {
                    return Err(PlanError::InvalidAction(
                        "Operations array is empty".to_string(),
                    ));
                }

                let mut changes = Vec::new();
                let mut descriptions = Vec::new();

                for (idx, op) in operations.iter().enumerate() {
                    let op_type = op.get("type").and_then(|t| t.as_str()).ok_or_else(|| {
                        PlanError::InvalidAction(format!("Operation {} missing 'type' field", idx))
                    })?;

                    let change = match op_type {
                        "create_mapping" | "CreateMapping" => {
                            let mode = op
                                .get("mode")
                                .and_then(|m| m.as_str())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'mode'",
                                        idx
                                    ))
                                })?
                                .to_string();

                            // Validate mode exists
                            if !config.modes.iter().any(|m| m.name == mode) {
                                return Err(PlanError::ModeNotFound(mode));
                            }

                            let trigger: Trigger = serde_json::from_value(
                                op.get("trigger").cloned().ok_or_else(|| {
                                    PlanError::InvalidTrigger(format!(
                                        "Operation {} missing 'trigger'",
                                        idx
                                    ))
                                })?,
                            )
                            .map_err(|e| PlanError::InvalidTrigger(e.to_string()))?;

                            let action: ActionConfig = serde_json::from_value(
                                op.get("action").cloned().ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'action'",
                                        idx
                                    ))
                                })?,
                            )
                            .map_err(|e| PlanError::InvalidAction(e.to_string()))?;

                            let description = op
                                .get("description")
                                .and_then(|d| d.as_str())
                                .map(|s| s.to_string());

                            // ADR-038 Slice 6: optional let_through in batch ops too.
                            let let_through = op
                                .get("let_through")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);

                            let desc_str = description.as_deref().unwrap_or("mapping");
                            descriptions.push(format!("Create '{}' in '{}'", desc_str, mode));

                            ConfigChange::CreateMapping {
                                mode,
                                trigger,
                                action,
                                description,
                                let_through,
                            }
                        }

                        "update_mapping" | "UpdateMapping" => {
                            let mode = op
                                .get("mode")
                                .and_then(|m| m.as_str())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'mode'",
                                        idx
                                    ))
                                })?
                                .to_string();

                            let index =
                                op.get("index").and_then(|i| i.as_u64()).ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'index'",
                                        idx
                                    ))
                                })? as usize;

                            // Validate mode and index
                            let mode_obj = config
                                .modes
                                .iter()
                                .find(|m| m.name == mode)
                                .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

                            if index >= mode_obj.mappings.len() {
                                return Err(PlanError::IndexOutOfRange {
                                    mode: mode.clone(),
                                    index,
                                    count: mode_obj.mappings.len(),
                                });
                            }

                            let trigger: Trigger = serde_json::from_value(
                                op.get("trigger").cloned().ok_or_else(|| {
                                    PlanError::InvalidTrigger(format!(
                                        "Operation {} missing 'trigger'",
                                        idx
                                    ))
                                })?,
                            )
                            .map_err(|e| PlanError::InvalidTrigger(e.to_string()))?;

                            let action: ActionConfig = serde_json::from_value(
                                op.get("action").cloned().ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'action'",
                                        idx
                                    ))
                                })?,
                            )
                            .map_err(|e| PlanError::InvalidAction(e.to_string()))?;

                            let description = op
                                .get("description")
                                .and_then(|d| d.as_str())
                                .map(|s| s.to_string());

                            descriptions.push(format!("Update mapping {} in '{}'", index, mode));

                            ConfigChange::UpdateMapping {
                                mode,
                                index,
                                trigger,
                                action,
                                description,
                            }
                        }

                        "delete_mapping" | "DeleteMapping" => {
                            let mode = op
                                .get("mode")
                                .and_then(|m| m.as_str())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'mode'",
                                        idx
                                    ))
                                })?
                                .to_string();

                            let index =
                                op.get("index").and_then(|i| i.as_u64()).ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'index'",
                                        idx
                                    ))
                                })? as usize;

                            // Validate mode and index
                            let mode_obj = config
                                .modes
                                .iter()
                                .find(|m| m.name == mode)
                                .ok_or_else(|| PlanError::ModeNotFound(mode.clone()))?;

                            if index >= mode_obj.mappings.len() {
                                return Err(PlanError::IndexOutOfRange {
                                    mode: mode.clone(),
                                    index,
                                    count: mode_obj.mappings.len(),
                                });
                            }

                            descriptions.push(format!("Delete mapping {} in '{}'", index, mode));

                            ConfigChange::DeleteMapping { mode, index }
                        }

                        "create_mode" | "CreateMode" => {
                            let name = op
                                .get("name")
                                .and_then(|n| n.as_str())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'name'",
                                        idx
                                    ))
                                })?
                                .to_string();

                            let color = op
                                .get("color")
                                .and_then(|c| c.as_str())
                                .map(|s| s.to_string());

                            descriptions.push(format!("Create mode '{}'", name));

                            ConfigChange::CreateMode { name, color }
                        }

                        "delete_mode" | "DeleteMode" => {
                            let name = op
                                .get("name")
                                .and_then(|n| n.as_str())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'name'",
                                        idx
                                    ))
                                })?
                                .to_string();

                            descriptions.push(format!("Delete mode '{}'", name));

                            ConfigChange::DeleteMode { name }
                        }

                        // ADR-031 P3 § 5.4 (#1143 slice 8) —
                        // `update_route` completes the route-mutation
                        // trio (create/delete/update). Total-replace
                        // semantics: the LLM supplies the full new
                        // shape, and `apply()` swaps the whole
                        // RouteConfig at `index`. Required args:
                        // `index`, `from`, `to`. Optional:
                        // `transform`, `filter`, `enabled`,
                        // `description`. Same TOCTOU stability
                        // story as `delete_route`.
                        "update_route" | "UpdateRoute" => {
                            let index = op
                                .get("index")
                                .and_then(|i| i.as_u64())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'index' (must be a non-negative integer)",
                                        idx
                                    ))
                                })? as usize;

                            let from = op
                                .get("from")
                                .and_then(|f| f.as_str())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'from'",
                                        idx
                                    ))
                                })?
                                .to_string();
                            if from.trim().is_empty() {
                                return Err(PlanError::InvalidAction(format!(
                                    "Operation {} 'from' cannot be empty",
                                    idx
                                )));
                            }

                            let to = op
                                .get("to")
                                .and_then(|t| t.as_str())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'to'",
                                        idx
                                    ))
                                })?
                                .to_string();
                            if to.trim().is_empty() {
                                return Err(PlanError::InvalidAction(format!(
                                    "Operation {} 'to' cannot be empty",
                                    idx
                                )));
                            }

                            let transform = op
                                .get("transform")
                                .cloned()
                                .map(serde_json::from_value)
                                .transpose()
                                .map_err(|e| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} invalid transform: {}",
                                        idx, e
                                    ))
                                })?;

                            let filter = op
                                .get("filter")
                                .cloned()
                                .map(serde_json::from_value)
                                .transpose()
                                .map_err(|e| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} invalid filter: {}",
                                        idx, e
                                    ))
                                })?;

                            let enabled =
                                op.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);

                            let description = op
                                .get("description")
                                .and_then(|d| d.as_str())
                                .map(|s| s.to_string());

                            descriptions.push(format!(
                                "Update route at index {} ('{}' → '{}')",
                                index, from, to
                            ));

                            ConfigChange::UpdateRoute {
                                index,
                                from,
                                to,
                                transform,
                                filter,
                                enabled,
                                description,
                            }
                        }

                        // ADR-031 P3 § 5.4 (#1143 slice 7) — paired
                        // with `create_route` per spec; `delete_route`
                        // also goes through batch_changes by design
                        // (no singleton tool). Takes a 0-based `index`
                        // into `config.routes` as it stands at apply
                        // time. The plan's TOCTOU base_state_hash
                        // guards against the underlying list mutating
                        // between plan creation and approval.
                        "delete_route" | "DeleteRoute" => {
                            let index = op
                                .get("index")
                                .and_then(|i| i.as_u64())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'index' (must be a non-negative integer)",
                                        idx
                                    ))
                                })? as usize;

                            descriptions.push(format!("Delete route at index {}", index));

                            ConfigChange::DeleteRoute { index }
                        }

                        // ADR-031 P3 § 5.4 (#1143 slice 5) — accept
                        // `create_route` inside a batch so the LLM can
                        // build a routing-setup plan with several
                        // routes in one approval round-trip.
                        // Singleton-tool form (`conductor_create_route`)
                        // is deliberately NOT planned per spec § 5.4 —
                        // route mutations always go through batch_changes.
                        "create_route" | "CreateRoute" => {
                            let from = op
                                .get("from")
                                .and_then(|f| f.as_str())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'from'",
                                        idx
                                    ))
                                })?
                                .to_string();
                            if from.trim().is_empty() {
                                return Err(PlanError::InvalidAction(format!(
                                    "Operation {} 'from' cannot be empty",
                                    idx
                                )));
                            }

                            let to = op
                                .get("to")
                                .and_then(|t| t.as_str())
                                .ok_or_else(|| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} missing 'to'",
                                        idx
                                    ))
                                })?
                                .to_string();
                            if to.trim().is_empty() {
                                return Err(PlanError::InvalidAction(format!(
                                    "Operation {} 'to' cannot be empty",
                                    idx
                                )));
                            }

                            let transform = op
                                .get("transform")
                                .cloned()
                                .map(serde_json::from_value)
                                .transpose()
                                .map_err(|e| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} invalid transform: {}",
                                        idx, e
                                    ))
                                })?;

                            let filter = op
                                .get("filter")
                                .cloned()
                                .map(serde_json::from_value)
                                .transpose()
                                .map_err(|e| {
                                    PlanError::InvalidAction(format!(
                                        "Operation {} invalid filter: {}",
                                        idx, e
                                    ))
                                })?;

                            let enabled =
                                op.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);

                            let description = op
                                .get("description")
                                .and_then(|d| d.as_str())
                                .map(|s| s.to_string());

                            descriptions.push(format!("Create route '{}' → '{}'", from, to));

                            ConfigChange::CreateRoute {
                                from,
                                to,
                                transform,
                                filter,
                                enabled,
                                description,
                            }
                        }

                        _ => {
                            return Err(PlanError::InvalidAction(format!(
                                "Operation {} has unknown type: {}",
                                idx, op_type
                            )));
                        }
                    };

                    changes.push(change);
                }

                let description = format!(
                    "Batch operation ({} changes): {}",
                    changes.len(),
                    descriptions.join(", ")
                );

                Ok(ConfigPlan::new(description, changes, config))
            }

            "conductor_create_endpoint" => {
                use conductor_core::config::types::{
                    ConnectorDirection, ConnectorProtocol, EndpointKind,
                };

                let alias = args
                    .get("alias")
                    .and_then(|a| a.as_str())
                    .ok_or_else(|| {
                        PlanError::InvalidAction("Missing 'alias' argument".to_string())
                    })?
                    .to_string();

                // `direction` is REQUIRED for endpoints (ADR-035 §4.1 R2 P1 — no
                // default; forcing it avoids binding a network listener as
                // implicitly Bidirectional).
                let direction: ConnectorDirection = args
                    .get("direction")
                    .ok_or_else(|| {
                        PlanError::InvalidAction(
                            "Missing 'direction' argument (required for endpoints — ADR-035 §4.1)"
                                .to_string(),
                        )
                    })
                    .and_then(|v| {
                        serde_json::from_value(v.clone()).map_err(|e| {
                            PlanError::InvalidAction(format!("Invalid direction: {}", e))
                        })
                    })?;

                // `protocol` is optional — inferred from `kind` at load when omitted.
                let protocol: Option<ConnectorProtocol> = args
                    .get("protocol")
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(|e| PlanError::InvalidAction(format!("Invalid protocol: {}", e)))?;

                // `kind` is the internally-tagged `type` + its variant fields,
                // which sit at the top level of the args (mirroring EndpointConfig).
                // EndpointKind ignores the common fields it doesn't recognize.
                let kind: EndpointKind = serde_json::from_value(args.clone()).map_err(|e| {
                    PlanError::InvalidAction(format!(
                        "Invalid endpoint `type`/fields (expected a `type` of Matcher/OscEndpoint/ArtNetEndpoint/MidiVirtualPort plus its fields): {}",
                        e
                    ))
                })?;

                let description = args
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string());

                let enabled = args
                    .get("enabled")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(true);

                let channels: Vec<u8> = args
                    .get("channels")
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(|e| PlanError::InvalidAction(format!("Invalid channels: {}", e)))?
                    .unwrap_or_default();

                build_create_endpoint_plan(
                    alias,
                    direction,
                    protocol,
                    kind,
                    description,
                    enabled,
                    channels,
                    config,
                )
            }

            _ => Err(PlanError::InvalidAction(format!(
                "Unknown ConfigChange tool: {}",
                tool_name
            ))),
        }
    }

    /// Get a pending plan by ID
    pub async fn get_plan(&self, plan_id: &Uuid) -> Option<ConfigPlan> {
        let plans = self.pending_plans.read().await;
        plans.get(plan_id).cloned()
    }

    /// Apply a pending plan atomically (P3-07)
    ///
    /// # Arguments
    /// * `plan_id` - ID of the plan to apply
    ///
    /// # Returns
    /// - Ok(changes_count) if plan was applied successfully
    /// - Err(PlanError) if plan not found, expired, or config changed
    ///
    /// Uses atomic apply: all changes succeed or none are applied.
    /// Records the change in undo history (P4-06).
    pub async fn apply_plan(&self, plan_id: &Uuid) -> Result<usize, PlanError> {
        let start_time = Instant::now();

        // Remove plan from pending
        let plan = {
            let mut plans = self.pending_plans.write().await;
            plans.remove(plan_id).ok_or(PlanError::NotFound(*plan_id))?
        };

        // D4.A.3.3.B.1: apply plan via LiveConfig.
        //
        // Route the mutation through `live_config.mutate_replace_whole` so
        // both engine_manager and any future LiveConfig subscriber see the
        // change through the same atomic publication. Undo history is
        // recorded AFTER a successful publish (Copilot review #1283 round 1):
        // if `mutate_replace_whole` errors (CAS conflict, compile failure),
        // a pre-recorded undo entry would describe an apply that never
        // happened, and a subsequent `undo()` would silently fast-forward
        // the config to a phantom inverse.
        //
        // Provenance: `Initiator::Llm { provider, model, plan_id }`.
        // D4.A.3.3.B.2 (#1282) records the SAME Provenance value into
        // the audit log via `log_plan_applied(..., Some(provenance))`,
        // so both sinks (LiveConfig publish + audit row) agree on who
        // initiated the apply. `provider`/`model` remain placeholders
        // until the calling LLM session can thread its identity here
        // — that wiring is the open follow-up; this PR ensures the
        // pipeline carries the value once it's set.
        let pre_config = (*self.live_config.load().config).clone();
        let plan_description = plan.description.clone();
        let plan_changes = plan.changes.clone();
        let plan_id_string = plan_id.to_string();
        let provenance = conductor_core::config::Provenance {
            initiator: conductor_core::config::Initiator::Llm {
                provider: "tbd".to_string(),
                model: "tbd".to_string(),
                plan_id: plan_id_string,
            },
            source: conductor_core::config::Source::InMemoryEdit,
            peer: None,
        };
        // #1320: use `try_mutate_replace_whole` so a failed
        // `apply_atomic` aborts the publish — pre-fix, the old
        // `mutate_replace_whole` would publish the candidate
        // regardless of apply success, bumping `state_generation`
        // for a no-op mutation.
        //
        // The closure captures `apply_outcome` (Ok(count) or
        // Err(PlanError)) for the caller to re-derive after the
        // helper returns. The helper's `Ok(())` / `Err(msg)` return
        // is just the abort signal — the rich error stays in
        // `apply_outcome`.
        let mut apply_outcome: Option<Result<usize, super::PlanError>> = None;
        let mutate_result = self
            .live_config
            .try_mutate_replace_whole(provenance.clone(), |cfg| {
                let outcome = plan.apply_atomic(cfg);
                let signal = match &outcome {
                    Ok(_) => Ok(()),
                    Err(e) => Err(e.to_string()),
                };
                apply_outcome = Some(outcome);
                signal
            })
            .await;
        let result: Result<usize, super::PlanError> = match mutate_result {
            Ok(_) => apply_outcome.unwrap_or(Ok(0)),
            Err(crate::daemon::live_config::MutateError::MutatorAborted(_)) => {
                // The closure signalled abort because `apply_atomic`
                // failed; the rich error is in apply_outcome.
                apply_outcome.unwrap_or_else(|| {
                    Err(super::PlanError::InvalidAction(
                        "apply aborted but closure did not capture the error".into(),
                    ))
                })
            }
            Err(other) => {
                return Err(super::PlanError::InvalidAction(format!(
                    "live_config mutate failed: {other}"
                )));
            }
        };
        let execution_time = start_time.elapsed();

        // Record undo only on a successful publish (see contract note above).
        if result.is_ok() {
            let mut undo_stack = self.undo_stack.write().await;
            if let Err(e) = undo_stack.record(
                *plan_id,
                plan_description.clone(),
                plan_changes,
                &pre_config,
            ) {
                warn!("Failed to record undo history: {}", e);
                // Continue — undo not being recorded is not fatal.
            }
        }

        match result {
            Ok(changes_count) => {
                info!("Applied plan {} with {} changes", plan_id, changes_count);

                // Audit log plan application (P4-04 + D4.A.3.3.B.2:
                // pass the same Provenance used for the LiveConfig
                // mutation — both audit + LiveConfig agree on the
                // initiator).
                if let Some(ref logger) = self.audit_logger {
                    logger.log_plan_applied(
                        &plan_id.to_string(),
                        changes_count,
                        execution_time,
                        Some(UserContext::local_user()),
                        Some(provenance.clone()),
                    );
                }

                Ok(changes_count)
            }
            Err(e) => {
                // Audit log the failure
                if let Some(ref logger) = self.audit_logger {
                    logger.log_tool_error(
                        "apply_plan",
                        AuditRiskTier::ConfigChange,
                        Some(&format!(r#"{{"plan_id": "{}"}}"#, plan_id)),
                        &e.to_string(),
                        execution_time,
                        Some(UserContext::local_user()),
                    );
                }
                Err(e)
            }
        }
    }

    /// Reject a pending plan
    pub async fn reject_plan(&self, plan_id: &Uuid) -> Result<(), PlanError> {
        let mut plans = self.pending_plans.write().await;
        plans.remove(plan_id).ok_or(PlanError::NotFound(*plan_id))?;
        info!("Rejected plan {}", plan_id);

        // Audit log plan rejection (P4-04)
        if let Some(ref logger) = self.audit_logger {
            logger.log_plan_rejected(&plan_id.to_string(), Some(UserContext::local_user()));
        }

        Ok(())
    }

    /// Get all pending plans
    pub async fn list_pending_plans(&self) -> Vec<ConfigPlan> {
        let plans = self.pending_plans.read().await;
        plans.values().cloned().collect()
    }

    /// Clean up expired plans
    pub async fn cleanup_expired_plans(&self) {
        let mut plans = self.pending_plans.write().await;
        let expired: Vec<Uuid> = plans
            .iter()
            .filter(|(_, p)| p.is_expired())
            .map(|(id, _)| *id)
            .collect();

        for id in expired {
            plans.remove(&id);
            warn!("Cleaned up expired plan {}", id);
        }
    }

    /// Get execution log
    pub async fn get_execution_log(&self) -> Vec<LogEntry> {
        let log = self.execution_log.read().await;
        log.clone()
    }

    /// Clear execution log
    pub async fn clear_execution_log(&self) {
        let mut log = self.execution_log.write().await;
        log.clear();
    }

    /// Summarize a tool result for logging
    fn summarize_result(&self, result: &ToolCallResult) -> String {
        if result.is_error == Some(true) {
            "Error".to_string()
        } else {
            "Success".to_string()
        }
    }

    // ==================== Undo/Redo Methods (P4-06) ====================

    /// Check if undo is available
    pub async fn can_undo(&self) -> bool {
        let stack = self.undo_stack.read().await;
        stack.can_undo()
    }

    /// Check if redo is available
    pub async fn can_redo(&self) -> bool {
        let stack = self.undo_stack.read().await;
        stack.can_redo()
    }

    /// Get summary of changes that can be undone
    pub async fn undo_summary(&self, limit: usize) -> Vec<HistorySummary> {
        let stack = self.undo_stack.read().await;
        stack.undo_summary(limit)
    }

    /// Get summary of changes that can be redone
    pub async fn redo_summary(&self, limit: usize) -> Vec<HistorySummary> {
        let stack = self.undo_stack.read().await;
        stack.redo_summary(limit)
    }

    /// Undo the last configuration change
    ///
    /// Returns the description of the undone change on success.
    pub async fn undo(&self) -> Result<String, HistoryError> {
        // Get the inverse changes from the undo stack
        let (description, inverse_changes) = {
            let mut stack = self.undo_stack.write().await;
            let entry = stack.undo()?;
            (entry.description.clone(), entry.inverse_changes.clone())
        };

        // D4.A.3.3.B.1: apply undo via LiveConfig. Same Provenance pattern as
        // apply_plan — Initiator::Llm with placeholder provider/model until
        // B.2 wires the live values.
        let mut apply_err: Option<HistoryError> = None;
        let mutate_result = self
            .live_config
            .mutate_replace_whole(
                conductor_core::config::Provenance {
                    initiator: conductor_core::config::Initiator::Llm {
                        provider: "tbd".to_string(),
                        model: "tbd".to_string(),
                        // D4.A.3.3.B.1 stub: deliberate non-UUID sentinel —
                        // B.2 (#1282) routes undo/redo through a proper
                        // session-scoped plan id once audit consumes the
                        // value. Free-text descriptions could contain
                        // user-supplied content and shouldn't leak into
                        // a UUID-typed field downstream (Copilot review
                        // #1283 round 1).
                        plan_id: "undo-placeholder".to_string(),
                    },
                    source: conductor_core::config::Source::InMemoryEdit,
                    peer: None,
                },
                |cfg| {
                    for change in inverse_changes {
                        if let Err(e) = super::plan::apply_change(cfg, change) {
                            apply_err = Some(HistoryError::ApplyFailed(e.to_string()));
                            return;
                        }
                    }
                },
            )
            .await;
        if let Err(e) = mutate_result {
            return Err(HistoryError::ApplyFailed(format!(
                "live_config mutate failed: {e}"
            )));
        }
        if let Some(e) = apply_err {
            return Err(e);
        }

        info!("Undid change: {}", description);

        // Audit log the undo
        if let Some(ref logger) = self.audit_logger {
            logger.log_tool_complete(
                "undo",
                AuditRiskTier::ConfigChange,
                Some(&format!(r#"{{"description": "{}"}}"#, description)),
                Some(&json!({"action": "undo", "description": description}).to_string()),
                std::time::Duration::from_millis(0),
                Some(UserContext::local_user()),
            );
        }

        Ok(description)
    }

    /// Redo a previously undone configuration change
    ///
    /// Returns the description of the redone change on success.
    pub async fn redo(&self) -> Result<String, HistoryError> {
        // Get the forward changes from the undo stack
        let (description, forward_changes) = {
            let mut stack = self.undo_stack.write().await;
            let entry = stack.redo()?;
            (entry.description.clone(), entry.forward_changes.clone())
        };

        // D4.A.3.3.B.1: apply redo via LiveConfig — same pattern as undo above.
        let mut apply_err: Option<HistoryError> = None;
        let mutate_result = self
            .live_config
            .mutate_replace_whole(
                conductor_core::config::Provenance {
                    initiator: conductor_core::config::Initiator::Llm {
                        provider: "tbd".to_string(),
                        model: "tbd".to_string(),
                        // See undo() above for the sentinel rationale.
                        plan_id: "redo-placeholder".to_string(),
                    },
                    source: conductor_core::config::Source::InMemoryEdit,
                    peer: None,
                },
                |cfg| {
                    for change in forward_changes {
                        if let Err(e) = super::plan::apply_change(cfg, change) {
                            apply_err = Some(HistoryError::ApplyFailed(e.to_string()));
                            return;
                        }
                    }
                },
            )
            .await;
        if let Err(e) = mutate_result {
            return Err(HistoryError::ApplyFailed(format!(
                "live_config mutate failed: {e}"
            )));
        }
        if let Some(e) = apply_err {
            return Err(e);
        }

        info!("Redid change: {}", description);

        // Audit log the redo
        if let Some(ref logger) = self.audit_logger {
            logger.log_tool_complete(
                "redo",
                AuditRiskTier::ConfigChange,
                Some(&format!(r#"{{"description": "{}"}}"#, description)),
                Some(&json!({"action": "redo", "description": description}).to_string()),
                std::time::Duration::from_millis(0),
                Some(UserContext::local_user()),
            );
        }

        Ok(description)
    }

    /// Clear undo history
    pub async fn clear_undo_history(&self) {
        let mut stack = self.undo_stack.write().await;
        stack.clear();
    }

    /// Get number of changes that can be undone
    pub async fn undo_count(&self) -> usize {
        let stack = self.undo_stack.read().await;
        stack.undo_count()
    }

    /// Get number of changes that can be redone
    pub async fn redo_count(&self) -> usize {
        let stack = self.undo_stack.read().await;
        stack.redo_count()
    }
}

/// Build a `ConfigChange::CreateEndpoint` plan for `conductor_create_endpoint`
/// (ADR-035 Slice 8). Performs the eager, tool-time checks (non-empty alias,
/// channel range, alias-uniqueness across the endpoint namespace) so the caller
/// gets a clear error now rather than at plan-apply time.
///
/// ADR-035 Phase 2 (#1748) removed the legacy `create_binding`/`create_connector`
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
/// `(source_device_alias, raw_midi_bytes)` (ADR-036 D5 / Slice 9). The
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
mod tests {
    use super::*;
    use conductor_core::config::{Mapping, Mode};

    /// D4.A.3.3.B.1 test helper: wrap a `Config` into `Arc<LiveConfig>` so
    /// existing tests can construct ToolExecutor variants without
    /// having to spell out the LiveConfig boilerplate. Consumes `config`
    /// — for tests that need to inspect the post-mutation config, clone
    /// the returned Arc and call `.load()` after the mutation.
    fn live_config_arc(config: Config) -> Arc<crate::daemon::live_config::LiveConfig> {
        Arc::new(
            crate::daemon::live_config::LiveConfig::new(config)
                .expect("LiveConfig::new failed in test"),
        )
    }

    fn create_test_config() -> Config {
        Config {
            mcp: Default::default(),
            per_app_modes: None,
            config_meta: Default::default(),
            security: Default::default(),
            endpoints: vec![],
            modes: vec![Mode {
                name: "Default".to_string(),
                color: Some("blue".to_string()),
                mappings: vec![Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: Some(1),
                        channel: None,
                        device: None,
                    },
                    action: ActionConfig::Keystroke {
                        keys: "c".to_string(),
                        modifiers: vec!["cmd".to_string()],
                    },
                    description: Some("Copy".to_string()),
                    let_through: false,
                }],
            }],
            global_mappings: vec![],
            logging: None,
            advanced_settings: Default::default(),
            last_selected_mode: None,
            default_mode: None,
            led: None,
            event_console: None,
            routes: vec![],
        }
    }

    #[tokio::test]
    async fn test_tool_executor_readonly_auto_executes() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let result = executor.execute("conductor_get_config", None, None).await;

        match result {
            ExecutionResult::Success { result } => {
                assert!(result.is_error.is_none());
            }
            _ => panic!("Expected Success result for ReadOnly tool"),
        }
    }

    #[tokio::test]
    async fn test_tool_executor_stateful_logs_execution() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // Register conductor_start_midi_learn as Stateful for this test
        let result = executor
            .execute_stateful("conductor_start_midi_learn", None)
            .await;

        match result {
            ExecutionResult::Logged { result, log_entry } => {
                assert!(result.is_error.is_none());
                assert_eq!(log_entry.tool_name, "conductor_start_midi_learn");
            }
            _ => panic!("Expected Logged result for Stateful tool"),
        }

        // Check log was stored
        let log = executor.get_execution_log().await;
        assert_eq!(log.len(), 1);
    }

    #[tokio::test]
    async fn test_tool_executor_config_change_returns_plan() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
            "description": "Paste"
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 1);
                assert!(plan.description.contains("Default"));

                // Plan should be stored
                let pending = executor.list_pending_plans().await;
                assert_eq!(pending.len(), 1);
            }
            _ => panic!("Expected PlanCreated result for ConfigChange tool"),
        }
    }

    #[tokio::test]
    async fn test_apply_plan() {
        let config = create_test_config();
        let config_arc = live_config_arc(config);
        let executor = ToolExecutor::new(config_arc.clone());

        // Create a plan
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
            "description": "Paste"
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        // Apply the plan
        executor
            .apply_plan(&plan_id)
            .await
            .expect("Failed to apply plan");

        // D4.A.3.3.B.1: Verify config was updated via LiveConfig snapshot.
        let snap = config_arc.load();
        let config = snap.config.as_ref();
        assert_eq!(config.modes[0].mappings.len(), 2);
        assert_eq!(
            config.modes[0].mappings[1].description,
            Some("Paste".to_string())
        );

        // Plan should be removed
        let pending = executor.list_pending_plans().await;
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn test_reject_plan() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // Create a plan
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        // Reject the plan
        executor
            .reject_plan(&plan_id)
            .await
            .expect("Failed to reject plan");

        // Plan should be removed
        let pending = executor.list_pending_plans().await;
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn test_create_mapping_validates_trigger_format() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // Invalid trigger format
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "InvalidType" },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(message.contains("trigger") || message.contains("Trigger"));
            }
            _ => panic!("Expected Error for invalid trigger"),
        }
    }

    #[tokio::test]
    async fn test_create_mapping_validates_action_format() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // Invalid action format
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "InvalidAction" }
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(message.contains("action") || message.contains("Action"));
            }
            _ => panic!("Expected Error for invalid action"),
        }
    }

    /// ADR-038 Slice 6: conductor_create_mapping accepts `let_through` + a `Tap`
    /// action and threads both into the planned ConfigChange.
    #[tokio::test]
    async fn test_create_mapping_threads_let_through_and_tap() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 40 },
            "action": { "type": "Tap", "message": "note {note} vel {velocity}" },
            "let_through": true
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let ExecutionResult::PlanCreated { plan } = result else {
            panic!("expected PlanCreated, got {result:?}");
        };
        match &plan.changes[0] {
            ConfigChange::CreateMapping {
                action,
                let_through,
                ..
            } => {
                assert!(*let_through, "let_through arg must thread into the change");
                assert!(
                    matches!(action, conductor_core::ActionConfig::Tap { message } if message == "note {note} vel {velocity}"),
                    "Tap action must parse from the create_mapping arg, got {action:?}"
                );
            }
            other => panic!("expected CreateMapping, got {other:?}"),
        }
    }

    /// `let_through` defaults to false when omitted (pre-ADR-038 swallow).
    #[tokio::test]
    async fn test_create_mapping_let_through_defaults_false() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 41 },
            "action": { "type": "Keystroke", "keys": "x", "modifiers": [] }
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let ExecutionResult::PlanCreated { plan } = result else {
            panic!("expected PlanCreated, got {result:?}");
        };
        match &plan.changes[0] {
            ConfigChange::CreateMapping { let_through, .. } => {
                assert!(!*let_through, "omitted let_through must default to false");
            }
            other => panic!("expected CreateMapping, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_delete_mapping_returns_plan() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "mode": "Default",
            "index": 0
        });

        let result = executor
            .execute("conductor_delete_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 1);
                match &plan.changes[0] {
                    ConfigChange::DeleteMapping { mode, index } => {
                        assert_eq!(mode, "Default");
                        assert_eq!(*index, 0);
                    }
                    _ => panic!("Expected DeleteMapping change"),
                }
            }
            _ => panic!("Expected PlanCreated result"),
        }
    }

    #[tokio::test]
    async fn test_update_mapping_returns_plan() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "mode": "Default",
            "index": 0,
            "trigger": { "type": "Note", "note": 36, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "x", "modifiers": ["cmd"] },
            "description": "Cut"
        });

        let result = executor
            .execute("conductor_update_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 1);
                match &plan.changes[0] {
                    ConfigChange::UpdateMapping {
                        mode,
                        index,
                        description,
                        ..
                    } => {
                        assert_eq!(mode, "Default");
                        assert_eq!(*index, 0);
                        assert_eq!(description, &Some("Cut".to_string()));
                    }
                    _ => panic!("Expected UpdateMapping change"),
                }
            }
            _ => panic!("Expected PlanCreated result"),
        }
    }

    #[tokio::test]
    async fn test_batch_changes_with_create_route_operation() {
        // ADR-031 P3 § 5.4 (#1143 slice 5) — a batch containing a
        // single `create_route` op produces a one-change plan with
        // the `CreateRoute` variant populated end-to-end (JSON parse
        // → ConfigChange enum → plan storage).
        use crate::daemon::llm::ConfigChange;
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": [{
                "type": "create_route",
                "from": "mikro",
                "to": "absynth",
                "description": "split lower keys"
            }]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 1);
                match &plan.changes[0] {
                    ConfigChange::CreateRoute {
                        from,
                        to,
                        enabled,
                        description,
                        ..
                    } => {
                        assert_eq!(from, "mikro");
                        assert_eq!(to, "absynth");
                        assert!(enabled, "enabled defaults to true");
                        assert_eq!(description.as_deref(), Some("split lower keys"));
                    }
                    other => panic!("Expected CreateRoute, got {other:?}"),
                }
            }
            other => panic!("Expected PlanCreated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_batch_changes_rejects_create_route_missing_from() {
        // Required-field enforcement at batch-dispatch time — the
        // user gets a clear error rather than a synthetic empty-from
        // plan.
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": [{
                "type": "create_route",
                "to": "absynth"
            }]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.to_lowercase().contains("from"),
                    "error message must name the missing field; got: {message}"
                );
            }
            other => panic!("Expected Error for missing 'from', got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_batch_changes_with_update_route_operation() {
        // ADR-031 P3 § 5.4 (#1143 slice 8) — `update_route` op
        // produces a `ConfigChange::UpdateRoute` populated end-to-end
        // (JSON parse → enum → plan).
        use crate::daemon::llm::ConfigChange;
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": [{
                "type": "update_route",
                "index": 0,
                "from": "mikro",
                "to": "absynth",
                "enabled": false,
                "description": "muted route"
            }]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 1);
                match &plan.changes[0] {
                    ConfigChange::UpdateRoute {
                        index,
                        from,
                        to,
                        enabled,
                        description,
                        ..
                    } => {
                        assert_eq!(*index, 0);
                        assert_eq!(from, "mikro");
                        assert_eq!(to, "absynth");
                        assert!(!enabled, "explicit enabled=false must propagate");
                        assert_eq!(description.as_deref(), Some("muted route"));
                    }
                    other => panic!("Expected UpdateRoute, got {other:?}"),
                }
            }
            other => panic!("Expected PlanCreated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_batch_changes_rejects_update_route_missing_index() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": [{
                "type": "update_route",
                "from": "a",
                "to": "b"
            }]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.to_lowercase().contains("index"),
                    "error must mention missing 'index'; got: {message}"
                );
            }
            other => panic!("Expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_batch_changes_with_delete_route_operation() {
        // ADR-031 P3 § 5.4 (#1143 slice 7) — `delete_route` op
        // parses the required `index` field and lands a
        // `ConfigChange::DeleteRoute` in the plan.
        use crate::daemon::llm::ConfigChange;
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": [{
                "type": "delete_route",
                "index": 0
            }]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 1);
                match &plan.changes[0] {
                    ConfigChange::DeleteRoute { index } => {
                        assert_eq!(*index, 0);
                    }
                    other => panic!("Expected DeleteRoute, got {other:?}"),
                }
            }
            other => panic!("Expected PlanCreated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_batch_changes_rejects_delete_route_missing_index() {
        // `index` is required for DeleteRoute — there's no sensible
        // default (which route would it delete?). The arm must
        // surface a clear "missing index" error.
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": [{ "type": "delete_route" }]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.to_lowercase().contains("index"),
                    "error must mention the missing field; got: {message}"
                );
            }
            other => panic!("Expected Error for missing 'index', got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_batch_changes_create_route_alongside_create_mapping() {
        // The whole point of batch is that you can combine route +
        // mapping creation in one approval round-trip. Pins that
        // shape: 1 route + 1 mapping produce a single 2-change plan.
        use crate::daemon::llm::ConfigChange;
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": [
                {
                    "type": "create_route",
                    "from": "mikro",
                    "to": "absynth"
                },
                {
                    "type": "create_mapping",
                    "mode": "Default",
                    "trigger": { "type": "Note", "note": 40, "velocity_min": 1 },
                    "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
                }
            ]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 2);
                assert!(
                    matches!(plan.changes[0], ConfigChange::CreateRoute { .. }),
                    "first change must be CreateRoute"
                );
                assert!(
                    matches!(plan.changes[1], ConfigChange::CreateMapping { .. }),
                    "second change must be CreateMapping"
                );
            }
            other => panic!("Expected PlanCreated, got {other:?}"),
        }
    }

    // =========================================================================
    // Batch Operations Tests (P3-07)
    // =========================================================================

    #[tokio::test]
    async fn test_batch_changes_creates_multi_change_plan() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": [
                {
                    "type": "create_mapping",
                    "mode": "Default",
                    "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
                    "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
                    "description": "Paste"
                },
                {
                    "type": "create_mapping",
                    "mode": "Default",
                    "trigger": { "type": "Note", "note": 38, "velocity_min": 1 },
                    "action": { "type": "Keystroke", "keys": "z", "modifiers": ["cmd"] },
                    "description": "Undo"
                }
            ]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 2);
                assert!(plan.description.contains("2 changes"));
            }
            _ => panic!("Expected PlanCreated result for batch changes"),
        }
    }

    #[tokio::test]
    async fn test_batch_changes_with_multiple_operation_types() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": [
                {
                    "type": "create_mapping",
                    "mode": "Default",
                    "trigger": { "type": "Note", "note": 40, "velocity_min": 1 },
                    "action": { "type": "Keystroke", "keys": "a", "modifiers": ["cmd"] }
                },
                {
                    "type": "update_mapping",
                    "mode": "Default",
                    "index": 0,
                    "trigger": { "type": "Note", "note": 36, "velocity_min": 1 },
                    "action": { "type": "Keystroke", "keys": "x", "modifiers": ["cmd"] },
                    "description": "Cut"
                }
            ]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 2);
                match &plan.changes[0] {
                    ConfigChange::CreateMapping { mode, .. } => assert_eq!(mode, "Default"),
                    _ => panic!("Expected CreateMapping"),
                }
                match &plan.changes[1] {
                    ConfigChange::UpdateMapping { index, .. } => assert_eq!(*index, 0),
                    _ => panic!("Expected UpdateMapping"),
                }
            }
            _ => panic!("Expected PlanCreated"),
        }
    }

    #[tokio::test]
    async fn test_batch_changes_fails_on_invalid_mode() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": [
                {
                    "type": "create_mapping",
                    "mode": "NonExistent",
                    "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
                    "action": { "type": "Keystroke", "keys": "v", "modifiers": [] }
                }
            ]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(message.contains("NonExistent") || message.contains("Mode not found"));
            }
            _ => panic!("Expected Error for invalid mode"),
        }
    }

    #[tokio::test]
    async fn test_batch_changes_empty_operations_fails() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "operations": []
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(message.contains("empty"));
            }
            _ => panic!("Expected Error for empty operations"),
        }
    }

    #[tokio::test]
    async fn test_apply_batch_plan_atomic() {
        let config = create_test_config();
        let config_arc = live_config_arc(config);
        let executor = ToolExecutor::new(config_arc.clone());

        let args = json!({
            "operations": [
                {
                    "type": "create_mapping",
                    "mode": "Default",
                    "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
                    "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
                    "description": "Paste"
                },
                {
                    "type": "create_mapping",
                    "mode": "Default",
                    "trigger": { "type": "Note", "note": 38, "velocity_min": 1 },
                    "action": { "type": "Keystroke", "keys": "z", "modifiers": ["cmd"] },
                    "description": "Undo"
                }
            ]
        });

        let result = executor
            .execute("conductor_batch_changes", Some(args), None)
            .await;

        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        // Apply the plan
        let changes_applied = executor
            .apply_plan(&plan_id)
            .await
            .expect("Failed to apply plan");
        assert_eq!(changes_applied, 2);

        // D4.A.3.3.B.1: Verify config was updated via LiveConfig snapshot.
        let snap = config_arc.load();
        let config = snap.config.as_ref();
        assert_eq!(config.modes[0].mappings.len(), 3); // 1 original + 2 new
    }

    // =========================================================================
    // Audit Logging Integration Tests (P4-04)
    // =========================================================================

    #[tokio::test]
    async fn test_executor_with_audit_logger() {
        use crate::daemon::audit::{AuditEventType, AuditLogger, AuditQuery};

        let config = create_test_config();
        let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
        let executor =
            ToolExecutor::with_audit_logger(live_config_arc(config), audit_logger.clone());

        // Execute a read-only tool
        let result = executor.execute("conductor_get_config", None, None).await;
        assert!(matches!(result, ExecutionResult::Success { .. }));

        // Verify audit entry was created
        let entries = audit_logger.query(&AuditQuery::default()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event_type, AuditEventType::ToolComplete);
        assert_eq!(
            entries[0].tool_name,
            Some("conductor_get_config".to_string())
        );
    }

    #[tokio::test]
    async fn test_audit_logs_plan_creation() {
        use crate::daemon::audit::{AuditEventType, AuditLogger, AuditQuery};

        let config = create_test_config();
        let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
        let executor =
            ToolExecutor::with_audit_logger(live_config_arc(config), audit_logger.clone());

        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;
        assert!(matches!(result, ExecutionResult::PlanCreated { .. }));

        // Verify plan created audit entry
        let entries = audit_logger
            .query(&AuditQuery {
                event_type: Some(AuditEventType::PlanCreated),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn test_audit_logs_plan_apply() {
        use crate::daemon::audit::{AuditEventType, AuditLogger, AuditQuery};

        let config = create_test_config();
        let config_arc = live_config_arc(config);
        let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
        let executor = ToolExecutor::with_audit_logger(config_arc.clone(), audit_logger.clone());

        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
        });

        let plan_id = match executor
            .execute("conductor_create_mapping", Some(args), None)
            .await
        {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        // Apply the plan
        executor
            .apply_plan(&plan_id)
            .await
            .expect("Apply should succeed");

        // Verify plan applied audit entry
        let entries = audit_logger
            .query(&AuditQuery {
                event_type: Some(AuditEventType::PlanApplied),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].execution_time.is_some());
    }

    #[tokio::test]
    async fn test_audit_logs_plan_rejection() {
        use crate::daemon::audit::{AuditEventType, AuditLogger, AuditQuery};

        let config = create_test_config();
        let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
        let executor =
            ToolExecutor::with_audit_logger(live_config_arc(config), audit_logger.clone());

        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
        });

        let plan_id = match executor
            .execute("conductor_create_mapping", Some(args), None)
            .await
        {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        // Reject the plan
        executor
            .reject_plan(&plan_id)
            .await
            .expect("Reject should succeed");

        // Verify plan rejected audit entry
        let entries = audit_logger
            .query(&AuditQuery {
                event_type: Some(AuditEventType::PlanRejected),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn test_audit_logs_tool_error() {
        use crate::daemon::audit::{AuditLogger, AuditQuery};

        let config = create_test_config();
        let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
        let executor =
            ToolExecutor::with_audit_logger(live_config_arc(config), audit_logger.clone());

        // Invalid mode should cause an error in plan creation
        let args = json!({
            "mode": "NonExistent",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;
        assert!(matches!(result, ExecutionResult::Error { .. }));

        // Verify error was logged
        let entries = audit_logger
            .query(&AuditQuery {
                errors_only: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_error);
        assert!(
            entries[0]
                .error_message
                .as_ref()
                .unwrap()
                .contains("NonExistent")
        );
    }

    #[tokio::test]
    async fn test_audit_logs_stateful_tool() {
        use crate::daemon::audit::{AuditLogger, AuditQuery, AuditRiskTier as AuditTier};

        let config = create_test_config();
        let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
        let executor =
            ToolExecutor::with_audit_logger(live_config_arc(config), audit_logger.clone());

        // Execute a stateful tool
        let result = executor
            .execute("conductor_start_midi_learn", None, None)
            .await;
        assert!(matches!(result, ExecutionResult::Logged { .. }));

        // Verify audit entry was created with correct risk tier
        let entries = audit_logger
            .query(&AuditQuery {
                risk_tier: Some(AuditTier::Stateful),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].tool_name,
            Some("conductor_start_midi_learn".to_string())
        );
    }

    // =========================================================================
    // HardwareIO Tier Tests (P4-01)
    // =========================================================================

    #[tokio::test]
    async fn test_hardware_io_sysex_requires_confirmation() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // SysEx with unknown manufacturer requires confirmation
        let args = json!({
            "device": "Test Device",
            "data": [0x55, 0x01, 0x02, 0x03]
        });

        let result = executor
            .execute("conductor_send_sysex", Some(args), None)
            .await;

        match result {
            ExecutionResult::HardwareIoConfirmation { status, tool_name } => {
                assert_eq!(tool_name, "conductor_send_sysex");
                match status {
                    ConfirmationStatus::RequiresConfirmation { token, .. } => {
                        assert!(!token.id.is_empty());
                    }
                    _ => panic!("Expected RequiresConfirmation status"),
                }
            }
            _ => panic!("Expected HardwareIoConfirmation result"),
        }
    }

    #[tokio::test]
    async fn test_hardware_io_sysex_low_risk_auto_approved() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // Identity request is low risk and auto-approved
        let args = json!({
            "device": "Test Device",
            "data": [0x7E, 0x00, 0x06, 0x01]  // Universal Non-Realtime Identity Request
        });

        let result = executor
            .execute("conductor_send_sysex", Some(args), None)
            .await;

        match result {
            ExecutionResult::HardwareIoConfirmation { status, .. } => match status {
                ConfirmationStatus::Confirmed { .. } => {}
                _ => panic!("Expected Confirmed status for low-risk operation"),
            },
            _ => panic!("Expected HardwareIoConfirmation result"),
        }
    }

    #[tokio::test]
    async fn test_hardware_io_sysex_confirmation_flow() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // Step 1: Request without token
        let args1 = json!({
            "device": "Test Device",
            "data": [0x55, 0x01, 0x02, 0x03]
        });

        let result1 = executor
            .execute("conductor_send_sysex", Some(args1), None)
            .await;

        let token_id = match result1 {
            ExecutionResult::HardwareIoConfirmation { status, .. } => match status {
                ConfirmationStatus::RequiresConfirmation { token, .. } => token.id,
                _ => panic!("Expected RequiresConfirmation"),
            },
            _ => panic!("Expected HardwareIoConfirmation"),
        };

        // Step 2: Confirm with token
        let args2 = json!({
            "device": "Test Device",
            "data": [0x55, 0x01, 0x02, 0x03],
            "confirmation_token": token_id
        });

        let result2 = executor
            .execute("conductor_send_sysex", Some(args2), None)
            .await;

        match result2 {
            ExecutionResult::HardwareIoConfirmation { status, .. } => match status {
                ConfirmationStatus::Confirmed { .. } => {}
                _ => panic!("Expected Confirmed status after token submission"),
            },
            _ => panic!("Expected HardwareIoConfirmation"),
        }
    }

    #[tokio::test]
    async fn test_hardware_io_device_reset_requires_confirmation() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "device": "Test Device",
            "reset_type": "factory"
        });

        let result = executor
            .execute("conductor_device_reset", Some(args), None)
            .await;

        match result {
            ExecutionResult::HardwareIoConfirmation { status, tool_name } => {
                assert_eq!(tool_name, "conductor_device_reset");
                match status {
                    ConfirmationStatus::RequiresConfirmation {
                        risk_assessment, ..
                    } => {
                        assert_eq!(risk_assessment.level, "high");
                        assert!(!risk_assessment.reversible);
                    }
                    _ => panic!("Expected RequiresConfirmation status"),
                }
            }
            _ => panic!("Expected HardwareIoConfirmation result"),
        }
    }

    #[tokio::test]
    async fn test_hardware_io_blocked_firmware_update() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // Large data block that looks like firmware
        let mut data = vec![0x00, 0x21, 0x09]; // NI manufacturer
        data.extend(vec![0xAA; 2000]); // Large block

        let args = json!({
            "device": "Test Device",
            "data": data
        });

        let result = executor
            .execute("conductor_send_sysex", Some(args), None)
            .await;

        match result {
            ExecutionResult::HardwareIoConfirmation { status, .. } => match status {
                ConfirmationStatus::Blocked { reason } => {
                    assert!(reason.contains("Blocked") || reason.contains("firmware"));
                }
                _ => panic!("Expected Blocked status for firmware-like data"),
            },
            _ => panic!("Expected HardwareIoConfirmation result"),
        }
    }

    #[tokio::test]
    async fn test_hardware_io_missing_device_argument() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "data": [0x7E, 0x00, 0x06, 0x01]
            // Missing "device" argument
        });

        let result = executor
            .execute("conductor_send_sysex", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(message.contains("device"));
            }
            _ => panic!("Expected Error for missing device argument"),
        }
    }

    // conductor_send_midi tests (v4.26.67)
    // =========================================================================

    #[tokio::test]
    async fn test_send_midi_auto_confirms() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "port": "Virtual Output",
            "messages": [
                { "type": "note_on", "channel": 1, "note": 60, "velocity": 100 }
            ]
        });

        let result = executor
            .execute("conductor_send_midi", Some(args), None)
            .await;

        match result {
            ExecutionResult::HardwareIoConfirmation { status, tool_name } => {
                assert_eq!(tool_name, "conductor_send_midi");
                match status {
                    ConfirmationStatus::Confirmed { result } => {
                        assert!(result.contains("1 MIDI message"));
                        assert!(result.contains("[90, 3C, 64]"));
                    }
                    _ => panic!("Expected auto-Confirmed for standard MIDI"),
                }
            }
            _ => panic!("Expected HardwareIoConfirmation result"),
        }
    }

    #[tokio::test]
    async fn test_send_midi_invalid_channel() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "port": "Virtual Output",
            "messages": [
                { "type": "note_on", "channel": 17, "note": 60, "velocity": 100 }
            ]
        });

        let result = executor
            .execute("conductor_send_midi", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(message.contains("Channel 17 out of range"));
            }
            _ => panic!("Expected Error for invalid channel"),
        }
    }

    // -----------------------------------------------------------------
    // ADR-026 Phase 2 — SysEx identity MCP tools
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn test_probe_device_identity_missing_port_name() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let result = executor
            .execute("conductor_probe_device_identity", Some(json!({})), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.contains("port_name"),
                    "missing-arg error should mention port_name; got: {}",
                    message
                );
            }
            other => panic!("expected Error for missing port_name, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_probe_device_identity_auto_confirms_then_errors_without_state() {
        // The Identity Request (`F0 7E 7F 06 01 F7`) is a low-risk
        // universal SysEx message — `SysExValidator::validate()` must
        // categorise it as `IdentityRequest`, which auto-confirms
        // without any user prompt. If the validator misclassifies (e.g.
        // because the F0/F7 frame bytes leak into the payload it
        // sees), the path returns `RequiresConfirmation` and the
        // probe never dispatches — that's the bug the 8th-pass
        // review caught.
        //
        // After auto-confirmation succeeds, the executor tries to
        // dispatch the probe via the daemon command channel. With no
        // `SharedDaemonStateRefs` attached this errors with a clear
        // "Daemon state refs not available" message. That's the only
        // observable signal in this test that the auto-confirm path
        // ran to completion.
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let result = executor
            .execute(
                "conductor_probe_device_identity",
                Some(json!({ "port_name": "fake-port" })),
                None,
            )
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.contains("daemon") || message.contains("Daemon"),
                    "expected the post-confirm dispatch to error with a daemon-state-refs \
                     message — if you see a SysEx-confirmation-related error here, the \
                     validator probably saw the F0/F7 frame bytes (regression)",
                );
            }
            ExecutionResult::HardwareIoConfirmation { status, .. } => {
                panic!(
                    "probe must auto-confirm: expected Error after confirmation, got \
                     HardwareIoConfirmation status={:?}. RequiresConfirmation here means \
                     the validator misclassified the Identity Request — likely the F0/F7 \
                     frame bytes leaked into the validator input.",
                    status
                );
            }
            other => panic!(
                "expected Error after auto-confirm dispatch, got {:?}",
                other
            ),
        }
    }

    #[tokio::test]
    async fn test_get_device_identity_missing_port_name() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let result = executor
            .execute("conductor_get_device_identity", Some(json!({})), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.contains("port_name") || message.contains("daemon"),
                    "should error on missing port_name OR missing state refs; got: {}",
                    message
                );
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_get_device_identity_requires_daemon_state() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let result = executor
            .execute(
                "conductor_get_device_identity",
                Some(json!({ "port_name": "any-port" })),
                None,
            )
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.contains("daemon"),
                    "should error on missing state refs; got: {}",
                    message
                );
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_list_device_identities_requires_daemon_state() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let result = executor
            .execute("conductor_list_device_identities", None, None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.contains("daemon"),
                    "should error on missing state refs; got: {}",
                    message
                );
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_probe_tool_classified_as_hardware_io() {
        // Verify the risk-tier registration: probe is HardwareIO,
        // get / list are ReadOnly. Catches the regression where a new
        // tool's risk tier mapping is forgotten.
        use crate::daemon::mcp_tools::get_tool_risk_tier;
        use crate::daemon::mcp_types::ToolRiskTier;
        assert_eq!(
            get_tool_risk_tier("conductor_probe_device_identity"),
            ToolRiskTier::HardwareIO
        );
        assert_eq!(
            get_tool_risk_tier("conductor_get_device_identity"),
            ToolRiskTier::ReadOnly
        );
        assert_eq!(
            get_tool_risk_tier("conductor_list_device_identities"),
            ToolRiskTier::ReadOnly
        );
    }

    #[tokio::test]
    async fn test_security_status_classified_as_readonly() {
        // ADR-042 #1899 B.7 — `conductor_security_status` reports the
        // network-approval HMAC key's rotation status; it only reads, so it
        // must be ReadOnly (never a mutating tier).
        use crate::daemon::mcp_tools::get_tool_risk_tier;
        use crate::daemon::mcp_types::ToolRiskTier;
        assert_eq!(
            get_tool_risk_tier("conductor_security_status"),
            ToolRiskTier::ReadOnly
        );
    }

    // ADR-042 #1899 B.7 — the `conductor_security_status` payload builders and
    // their shape tests live in the sibling `security_status` module. Here we
    // only pin the tool's risk-tier classification (above); the executor
    // handler is a thin wrapper that audit-logs and returns Success.

    #[tokio::test]
    async fn test_send_midi_missing_port() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "messages": [
                { "type": "note_on", "channel": 1, "note": 60 }
            ]
        });

        let result = executor
            .execute("conductor_send_midi", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(message.contains("port"));
            }
            _ => panic!("Expected Error for missing port"),
        }
    }

    #[tokio::test]
    async fn test_send_midi_multiple_messages() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "port": "Virtual Output",
            "messages": [
                { "type": "note_on", "channel": 1, "note": 60, "velocity": 100 },
                { "type": "cc", "channel": 1, "controller": 7, "value": 64 },
                { "type": "program_change", "channel": 2, "program": 5 }
            ]
        });

        let result = executor
            .execute("conductor_send_midi", Some(args), None)
            .await;

        match result {
            ExecutionResult::HardwareIoConfirmation { status, .. } => match status {
                ConfirmationStatus::Confirmed { result } => {
                    assert!(result.contains("3 MIDI message"));
                }
                _ => panic!("Expected Confirmed"),
            },
            _ => panic!("Expected HardwareIoConfirmation"),
        }
    }

    // P4-05: Rate Limiting Tests

    #[tokio::test]
    async fn test_rate_limiting_allows_under_limit() {
        use crate::daemon::ratelimit::TierLimits;

        let config = create_test_config();
        let rate_config = RateLimitConfig {
            enabled: true,
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 100,
                ..Default::default()
            },
            global_limit: 0,
        };

        let executor = ToolExecutor::with_rate_limit_config(live_config_arc(config), rate_config);

        // Should succeed under limit
        let result = executor.execute("conductor_get_config", None, None).await;

        match result {
            ExecutionResult::Success { .. } => {}
            ExecutionResult::RateLimited { .. } => panic!("Should not be rate limited under limit"),
            _ => panic!("Expected Success result"),
        }
    }

    #[tokio::test]
    async fn test_rate_limiting_blocks_over_limit() {
        use crate::daemon::ratelimit::TierLimits;

        let config = create_test_config();
        let rate_config = RateLimitConfig {
            enabled: true,
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 2, // Very low limit
                ..Default::default()
            },
            global_limit: 0,
        };

        let executor = ToolExecutor::with_rate_limit_config(live_config_arc(config), rate_config);

        // First two requests should succeed
        for _ in 0..2 {
            let result = executor.execute("conductor_get_config", None, None).await;
            match result {
                ExecutionResult::Success { .. } => {}
                _ => panic!("Expected Success for requests under limit"),
            }
        }

        // Third request should be rate limited
        let result = executor.execute("conductor_get_config", None, None).await;

        match result {
            ExecutionResult::RateLimited {
                tier,
                current,
                limit,
                ..
            } => {
                assert_eq!(tier, ToolRiskTier::ReadOnly);
                assert_eq!(current, 2);
                assert_eq!(limit, 2);
            }
            _ => panic!("Expected RateLimited result"),
        }
    }

    #[tokio::test]
    async fn test_rate_limiting_per_tier_isolation() {
        use crate::daemon::ratelimit::TierLimits;

        let config = create_test_config();
        let rate_config = RateLimitConfig {
            enabled: true,
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 2,
                config_change: 2,
                ..Default::default()
            },
            global_limit: 0,
        };

        let executor = ToolExecutor::with_rate_limit_config(live_config_arc(config), rate_config);

        // Exhaust ReadOnly limit
        for _ in 0..2 {
            executor.execute("conductor_get_config", None, None).await;
        }

        // ConfigChange should still work (different tier)
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { .. } => {}
            ExecutionResult::RateLimited { .. } => {
                panic!("ConfigChange should not be affected by ReadOnly limit")
            }
            _ => panic!("Expected PlanCreated result"),
        }
    }

    #[tokio::test]
    async fn test_rate_limiting_disabled() {
        use crate::daemon::ratelimit::TierLimits;

        let config = create_test_config();
        let rate_config = RateLimitConfig {
            enabled: false, // Rate limiting disabled
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 1, // Would be exceeded if enabled
                ..Default::default()
            },
            global_limit: 0,
        };

        let executor = ToolExecutor::with_rate_limit_config(live_config_arc(config), rate_config);

        // Should succeed many times when disabled
        for _ in 0..10 {
            let result = executor.execute("conductor_get_config", None, None).await;
            match result {
                ExecutionResult::Success { .. } => {}
                ExecutionResult::RateLimited { .. } => {
                    panic!("Should not be rate limited when disabled")
                }
                _ => panic!("Expected Success result"),
            }
        }
    }

    #[tokio::test]
    async fn test_rate_limiting_global_limit() {
        use crate::daemon::ratelimit::TierLimits;

        let config = create_test_config();
        let rate_config = RateLimitConfig {
            enabled: true,
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 100,
                stateful: 100,
                ..Default::default()
            },
            global_limit: 3, // Low global limit
        };

        let executor = ToolExecutor::with_rate_limit_config(live_config_arc(config), rate_config);

        // Use 3 requests across different tiers
        executor.execute("conductor_get_config", None, None).await;
        executor
            .execute("conductor_start_midi_learn", None, None)
            .await;
        executor.execute("conductor_get_config", None, None).await;

        // 4th request should hit global limit
        let result = executor.execute("conductor_get_config", None, None).await;

        match result {
            ExecutionResult::RateLimited { .. } => {}
            _ => panic!("Expected RateLimited due to global limit"),
        }
    }

    #[tokio::test]
    async fn test_rate_limiter_accessor() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // Should be able to access rate limiter for inspection/reset
        let limiter = executor.rate_limiter();
        let usage = limiter.get_usage("local");
        assert!(usage.contains_key(&ToolRiskTier::ReadOnly));
    }

    // =========================================================================
    // Undo/Redo Tests (P4-06)
    // =========================================================================

    #[tokio::test]
    async fn test_undo_last_change() {
        let config = create_test_config();
        let config_arc = live_config_arc(config);
        let executor = ToolExecutor::new(config_arc.clone());

        // Create and apply a plan
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
            "description": "Paste"
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        executor.apply_plan(&plan_id).await.unwrap();

        // Verify mapping was added
        {
            let snap = config_arc.load();
            let config = snap.config.as_ref();
            assert_eq!(config.modes[0].mappings.len(), 2);
        }

        // Should be able to undo
        assert!(executor.can_undo().await);
        assert_eq!(executor.undo_count().await, 1);

        // Undo the change
        let description = executor.undo().await.unwrap();
        assert!(description.contains("Default"));

        // Verify mapping was removed
        {
            let snap = config_arc.load();
            let config = snap.config.as_ref();
            assert_eq!(config.modes[0].mappings.len(), 1);
        }

        // Should no longer be able to undo
        assert!(!executor.can_undo().await);
    }

    #[tokio::test]
    async fn test_redo_undone_change() {
        let config = create_test_config();
        let config_arc = live_config_arc(config);
        let executor = ToolExecutor::new(config_arc.clone());

        // Create and apply a plan
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
            "description": "Paste"
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        executor.apply_plan(&plan_id).await.unwrap();

        // Undo the change
        executor.undo().await.unwrap();

        // Verify mapping was removed
        {
            let snap = config_arc.load();
            let config = snap.config.as_ref();
            assert_eq!(config.modes[0].mappings.len(), 1);
        }

        // Should be able to redo
        assert!(executor.can_redo().await);
        assert_eq!(executor.redo_count().await, 1);

        // Redo the change
        let description = executor.redo().await.unwrap();
        assert!(description.contains("Default"));

        // Verify mapping was restored
        {
            let snap = config_arc.load();
            let config = snap.config.as_ref();
            assert_eq!(config.modes[0].mappings.len(), 2);
        }

        // Should no longer be able to redo
        assert!(!executor.can_redo().await);
    }

    #[tokio::test]
    async fn test_undo_stack_limit() {
        let config = create_test_config();
        let config_arc = live_config_arc(config);
        let executor = ToolExecutor::new(config_arc.clone());

        // Apply multiple plans
        for i in 0..5 {
            let args = json!({
                "mode": "Default",
                "trigger": { "type": "Note", "note": 40 + i, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "a", "modifiers": ["cmd"] },
                "description": format!("Mapping {}", i)
            });

            let result = executor
                .execute("conductor_create_mapping", Some(args), None)
                .await;

            let plan_id = match result {
                ExecutionResult::PlanCreated { plan } => plan.id,
                _ => panic!("Expected PlanCreated"),
            };

            executor.apply_plan(&plan_id).await.unwrap();
        }

        // Should have 5 undoable changes
        assert_eq!(executor.undo_count().await, 5);
    }

    #[tokio::test]
    async fn test_undo_nothing_to_undo() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // No changes made, undo should fail
        assert!(!executor.can_undo().await);

        let result = executor.undo().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_redo_nothing_to_redo() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // No undone changes, redo should fail
        assert!(!executor.can_redo().await);

        let result = executor.redo().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_new_change_clears_redo_history() {
        let config = create_test_config();
        let config_arc = live_config_arc(config);
        let executor = ToolExecutor::new(config_arc.clone());

        // Apply two plans
        for i in 0..2 {
            let args = json!({
                "mode": "Default",
                "trigger": { "type": "Note", "note": 40 + i, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "a", "modifiers": ["cmd"] },
                "description": format!("Mapping {}", i)
            });

            let result = executor
                .execute("conductor_create_mapping", Some(args), None)
                .await;

            let plan_id = match result {
                ExecutionResult::PlanCreated { plan } => plan.id,
                _ => panic!("Expected PlanCreated"),
            };

            executor.apply_plan(&plan_id).await.unwrap();
        }

        // Undo one
        executor.undo().await.unwrap();
        assert_eq!(executor.redo_count().await, 1);

        // Apply new change
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 50, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "b", "modifiers": ["cmd"] },
            "description": "New mapping"
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        executor.apply_plan(&plan_id).await.unwrap();

        // Redo history should be cleared
        assert_eq!(executor.redo_count().await, 0);
        assert!(!executor.can_redo().await);
    }

    #[tokio::test]
    async fn test_undo_summary() {
        let config = create_test_config();
        let config_arc = live_config_arc(config);
        let executor = ToolExecutor::new(config_arc.clone());

        // Apply multiple plans
        for i in 0..3 {
            let args = json!({
                "mode": "Default",
                "trigger": { "type": "Note", "note": 40 + i, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "a", "modifiers": ["cmd"] },
                "description": format!("Mapping {}", i)
            });

            let result = executor
                .execute("conductor_create_mapping", Some(args), None)
                .await;

            let plan_id = match result {
                ExecutionResult::PlanCreated { plan } => plan.id,
                _ => panic!("Expected PlanCreated"),
            };

            executor.apply_plan(&plan_id).await.unwrap();
        }

        // Get summary
        let summary = executor.undo_summary(10).await;
        assert_eq!(summary.len(), 3);

        // Most recent first - descriptions contain the mode name from plan creation
        for s in &summary {
            assert!(s.description.contains("Default"));
            assert_eq!(s.changes_count, 1);
        }
    }

    #[tokio::test]
    async fn test_clear_undo_history() {
        let config = create_test_config();
        let config_arc = live_config_arc(config);
        let executor = ToolExecutor::new(config_arc.clone());

        // Apply a plan
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
            "description": "Paste"
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        executor.apply_plan(&plan_id).await.unwrap();

        assert!(executor.can_undo().await);

        // Clear history
        executor.clear_undo_history().await;

        assert!(!executor.can_undo().await);
        assert_eq!(executor.undo_count().await, 0);
    }

    // =========================================================================
    // Constructor Tests (LLM Council Feedback v4.13.2)
    // =========================================================================

    #[tokio::test]
    async fn test_new_with_live_config_uses_passed_config() {
        // D4.A.3.3.B.1: renamed from `test_new_with_config_uses_passed_config`
        // — the legacy `new_with_config(Arc<RwLock<Config>>)` constructor
        // retired alongside the `Arc<RwLock<Option<Config>>>` migration. The
        // test now verifies the standard `ToolExecutor::new(Arc<LiveConfig>)`
        // path preserves config identity, which is the only remaining shape.
        use crate::daemon::mcp_types::ToolContent;

        let mut config = create_test_config();
        // ADR-035 removed `[device]`; mutate a serializing field (the mode
        // name) instead to prove the passed config's identity is preserved.
        config.modes[0].name = "LLM Council Test Mode".to_string();

        let executor = ToolExecutor::new(live_config_arc(config));

        let result = executor.execute("conductor_get_config", None, None).await;

        match result {
            ExecutionResult::Success { result } => {
                let content = result.content.first().expect("Should have content");
                let text = match content {
                    ToolContent::Text { text } => text,
                    _ => panic!("Expected text content"),
                };
                assert!(
                    text.contains("LLM Council Test Mode"),
                    "Config should contain our test mode name, got: {}",
                    text
                );
            }
            ExecutionResult::Error { message } => {
                panic!("Expected Success result, got error: {}", message);
            }
            _ => panic!("Expected Success result for ReadOnly tool"),
        }
    }

    #[tokio::test]
    async fn test_get_config_returns_modified_after_apply() {
        let config = create_test_config();
        let config_arc = live_config_arc(config);
        let executor = ToolExecutor::new(config_arc.clone());

        // Verify initial state — D4.A.3.3.B.1: get_config() now returns
        // Config directly (no Option), since LiveConfig is always loaded.
        let initial = executor.get_config();
        assert_eq!(initial.modes[0].mappings.len(), 1);

        // Create and apply a plan
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
            "description": "Paste"
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;
        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        executor.apply_plan(&plan_id).await.unwrap();

        // get_config should return the modified config with 2 mappings
        let modified = executor.get_config();
        assert_eq!(modified.modes[0].mappings.len(), 2);
        assert_eq!(
            modified.modes[0].mappings[1].description,
            Some("Paste".to_string())
        );
    }

    #[tokio::test]
    async fn test_analyze_midi_learn_note_on_returns_note_trigger() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let events = vec![MidiLearnEvent {
            event_type: EventType::NoteOn,
            note: Some(36),
            velocity: Some(100),
            ..Default::default()
        }];

        let result = executor.analyze_midi_learn_events(&events);
        assert!(result.is_some());
        let trigger = result.unwrap();
        assert_eq!(trigger["type"], "Note");
        assert_eq!(trigger["note"], 36);
    }

    #[tokio::test]
    async fn test_analyze_midi_learn_chord_returns_chord_trigger() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let events = vec![MidiLearnEvent {
            event_type: EventType::NoteOn,
            pattern_type: Some(PatternType::Chord),
            pattern_notes: Some(vec![36, 40, 44]),
            pattern_timeout_ms: Some(100),
            ..Default::default()
        }];

        let result = executor.analyze_midi_learn_events(&events);
        assert!(result.is_some());
        let trigger = result.unwrap();
        assert_eq!(trigger["type"], "Chord");
        assert_eq!(trigger["notes"], json!([36, 40, 44]));
    }

    #[tokio::test]
    async fn test_analyze_midi_learn_program_change_returns_pc_trigger() {
        // ADR-025 Phase 1: a single PC capture must suggest a
        // ProgramChange trigger so foot-controller bank stomps can be
        // mapped via Learn.
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let events = vec![MidiLearnEvent {
            event_type: EventType::ProgramChange,
            pc: Some(12),
            channel: 0,
            ..Default::default()
        }];

        let result = executor
            .analyze_midi_learn_events(&events)
            .expect("PC event should yield a trigger suggestion");
        assert_eq!(result["type"], "ProgramChange");
        assert_eq!(result["pc"], 12);
        assert_eq!(result["channel"], 0);
    }

    #[tokio::test]
    async fn test_analyze_midi_learn_program_change_carries_channel() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let events = vec![MidiLearnEvent {
            event_type: EventType::ProgramChange,
            pc: Some(42),
            channel: 5,
            ..Default::default()
        }];

        let result = executor.analyze_midi_learn_events(&events).unwrap();
        assert_eq!(result["channel"], 5);
    }

    #[tokio::test]
    async fn test_analyze_midi_learn_cc_returns_cc_trigger() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let events = vec![MidiLearnEvent {
            event_type: EventType::Cc,
            cc: Some(1),
            value: Some(64),
            ..Default::default()
        }];

        let result = executor.analyze_midi_learn_events(&events);
        assert!(result.is_some());
        let trigger = result.unwrap();
        assert_eq!(trigger["type"], "CC");
        assert_eq!(trigger["cc"], 1);
    }

    #[tokio::test]
    async fn test_analyze_midi_learn_encoder_returns_encoder_trigger() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let events = vec![MidiLearnEvent {
            event_type: EventType::Encoder,
            cc: Some(16),
            ..Default::default()
        }];

        let result = executor.analyze_midi_learn_events(&events);
        assert!(result.is_some());
        let trigger = result.unwrap();
        assert_eq!(trigger["type"], "EncoderTurn");
        assert_eq!(trigger["cc"], 16);
    }

    #[tokio::test]
    async fn test_analyze_midi_learn_gamepad_chord_uses_pattern_buttons() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let events = vec![MidiLearnEvent {
            event_type: EventType::NoteOn,
            pattern_type: Some(PatternType::GamepadChord),
            pattern_buttons: Some(vec![128, 129, 130]),
            pattern_timeout_ms: Some(100),
            ..Default::default()
        }];

        let result = executor.analyze_midi_learn_events(&events);
        assert!(result.is_some());
        let trigger = result.unwrap();
        assert_eq!(trigger["type"], "GamepadButtonChord");
        assert_eq!(trigger["buttons"], json!([128, 129, 130]));
    }

    #[tokio::test]
    async fn test_analyze_midi_learn_velocity_range_suggestion() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // 3+ note presses with velocity range > 30 → should suggest VelocityRange
        let events = vec![
            MidiLearnEvent {
                event_type: EventType::NoteOn,
                note: Some(36),
                velocity: Some(30),
                ..Default::default()
            },
            MidiLearnEvent {
                event_type: EventType::NoteOn,
                note: Some(36),
                velocity: Some(80),
                ..Default::default()
            },
            MidiLearnEvent {
                event_type: EventType::NoteOn,
                note: Some(36),
                velocity: Some(127),
                ..Default::default()
            },
        ];

        let result = executor.analyze_midi_learn_events(&events);
        assert!(result.is_some());
        let trigger = result.unwrap();
        assert_eq!(trigger["type"], "VelocityRange");
        assert_eq!(trigger["note"], 36);

        // #2134: the suggestion must use the CANONICAL `soft_max` / `medium_max`
        // fields, not a `ranges` object — otherwise applying it silently drops
        // the thresholds. velocities 30/80/127 → min 30, max 127, range 97 →
        // soft_max = 30 + 97/3 = 62, medium_max = 30 + 2*97/3 = 94.
        assert!(
            trigger.get("ranges").is_none(),
            "must not emit the incompatible `ranges` shape; got {trigger}"
        );
        assert_eq!(trigger["soft_max"], 62);
        assert_eq!(trigger["medium_max"], 94);

        // Round-trip: the suggestion must deserialize into Trigger::VelocityRange
        // and preserve the learned thresholds — the real cross-helper contract.
        let parsed: conductor_core::config::types::Trigger =
            serde_json::from_value(trigger.clone())
                .expect("suggestion must deserialize into Trigger::VelocityRange");
        match parsed {
            conductor_core::config::types::Trigger::VelocityRange {
                note,
                soft_max,
                medium_max,
                ..
            } => {
                assert_eq!(note, 36);
                assert_eq!(soft_max, Some(62));
                assert_eq!(medium_max, Some(94));
            }
            other => panic!("expected Trigger::VelocityRange, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_analyze_midi_learn_uniform_velocity_returns_note() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // 3 presses with similar velocity (range < 30) → plain Note
        let events = vec![
            MidiLearnEvent {
                event_type: EventType::NoteOn,
                note: Some(36),
                velocity: Some(90),
                ..Default::default()
            },
            MidiLearnEvent {
                event_type: EventType::NoteOn,
                note: Some(36),
                velocity: Some(100),
                ..Default::default()
            },
            MidiLearnEvent {
                event_type: EventType::NoteOn,
                note: Some(36),
                velocity: Some(95),
                ..Default::default()
            },
        ];

        let result = executor.analyze_midi_learn_events(&events);
        assert!(result.is_some());
        let trigger = result.unwrap();
        assert_eq!(trigger["type"], "Note");
    }

    // conductor_switch_mode tests (v4.26.69)
    // =========================================================================

    #[tokio::test]
    async fn test_switch_mode_valid_name() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({ "mode": "Default" });
        let result = executor
            .execute("conductor_switch_mode", Some(args), None)
            .await;

        match result {
            ExecutionResult::Logged { result, .. } => {
                assert!(result.is_error.is_none());
                let text = match &result.content[0] {
                    crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                    _ => panic!("Expected text content"),
                };
                let json: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(json["success"], true);
                assert_eq!(json["mode_name"], "Default");
                assert_eq!(json["mode_index"], 0);
            }
            _ => panic!("Expected Logged result for Stateful tool"),
        }
    }

    #[tokio::test]
    async fn test_switch_mode_invalid_name() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({ "mode": "NonExistent" });
        let result = executor
            .execute("conductor_switch_mode", Some(args), None)
            .await;

        match result {
            ExecutionResult::Logged { result, .. } => {
                assert_eq!(result.is_error, Some(true));
                let text = match &result.content[0] {
                    crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                    _ => panic!("Expected text content"),
                };
                assert!(text.contains("not found"));
            }
            _ => panic!("Expected Logged result for Stateful tool"),
        }
    }

    #[tokio::test]
    async fn test_switch_mode_missing_argument() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let result = executor
            .execute("conductor_switch_mode", Some(json!({})), None)
            .await;

        match result {
            ExecutionResult::Logged { result, .. } => {
                assert_eq!(result.is_error, Some(true));
                let text = match &result.content[0] {
                    crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                    _ => panic!("Expected text content"),
                };
                assert!(text.contains("Missing required argument"));
            }
            _ => panic!("Expected Logged result"),
        }
    }

    // ─── ADR-027 D6: multi-dimensional LLM budget enforcement ─────────

    /// Wrap a budget config into the shared per-session state the executor
    /// holds. `now_ms = 0` is fine: the capability dimensions charged on this
    /// MCP surface (tool calls, config changes, MIDI out) are not time-windowed.
    fn budget_state(
        cfg: conductor_core::security::LlmBudgetConfig,
    ) -> Arc<Mutex<conductor_core::security::LlmBudgetState>> {
        Arc::new(Mutex::new(conductor_core::security::LlmBudgetState::new(
            cfg, 0,
        )))
    }

    #[tokio::test]
    async fn test_budget_halts_after_tool_call_quota() {
        // One tool call allowed for the whole session; the rest stay at ADR
        // defaults (high enough not to interfere with a 2-call test).
        let cfg = conductor_core::security::LlmBudgetConfig {
            max_tool_calls_per_session: 1,
            ..Default::default()
        };
        let mut executor = ToolExecutor::new(live_config_arc(create_test_config()));
        executor.set_budget_state(budget_state(cfg));

        // First ReadOnly call is admitted.
        let first = executor.execute("conductor_get_status", None, None).await;
        assert!(
            !matches!(&first, ExecutionResult::Error { message } if message.contains("budget")),
            "first call should be within budget, got {:?}",
            first
        );

        // Second call exhausts the per-session tool-call quota → halt.
        let second = executor.execute("conductor_get_status", None, None).await;
        match second {
            ExecutionResult::Error { message } => {
                assert!(message.contains("budget exceeded"), "got: {message}");
                assert!(
                    message.contains("max_tool_calls_per_session"),
                    "got: {message}"
                );
            }
            other => panic!("Expected budget halt Error, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_budget_charges_config_change_dimension() {
        // ConfigChange-tier calls have their own per-session quota independent
        // of the total tool-call count.
        let cfg = conductor_core::security::LlmBudgetConfig {
            max_config_changes_per_session: 1,
            ..Default::default()
        };
        let mut executor = ToolExecutor::new(live_config_arc(create_test_config()));
        executor.set_budget_state(budget_state(cfg));

        let mapping = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 60, "channel": 0 },
            "action": { "type": "Keystroke", "keys": ["a"] }
        });

        // First ConfigChange tool call is admitted (charges the dimension to 1).
        let first = executor
            .execute("conductor_create_mapping", Some(mapping.clone()), None)
            .await;
        assert!(
            !matches!(&first, ExecutionResult::Error { message } if message.contains("budget")),
            "first config change should be within budget, got {:?}",
            first
        );

        // Second ConfigChange call trips the config-change quota.
        let second = executor
            .execute("conductor_create_mapping", Some(mapping), None)
            .await;
        match second {
            ExecutionResult::Error { message } => {
                assert!(
                    message.contains("max_config_changes_per_session"),
                    "got: {message}"
                );
            }
            other => panic!("Expected config-change budget halt, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_no_budget_state_means_no_enforcement() {
        // Without set_budget_state, the executor never charges — the historical
        // behaviour every existing constructor preserves.
        let executor = ToolExecutor::new(live_config_arc(create_test_config()));
        for _ in 0..5 {
            let r = executor.execute("conductor_get_status", None, None).await;
            assert!(
                !matches!(&r, ExecutionResult::Error { message } if message.contains("budget")),
                "unbudgeted executor must not enforce, got {:?}",
                r
            );
        }
    }

    // ─── ADR-025 Phase 2.H: conductor_set_context_mapping (#854) ──────

    #[tokio::test]
    async fn test_set_context_mapping_pc_creates_plan() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // Use Shell inner actions to keep the test focused on the
        // context-switch shape — SendMidi takes flat fields
        // (`controller`, `value`), not a `params` object, so any
        // SendMidi fixture here would need to match that exactly to
        // avoid silently testing against serde defaults.
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "CC", "cc": 7, "channel": 0 },
            "action": {
                "type": "PcContextSwitch",
                "channel": 0,
                "device": "fcb1010",
                "mappings": {
                    "0": { "type": "Shell", "command": "echo preset-0" },
                    "12": { "type": "Shell", "command": "echo preset-12" }
                }
            },
            "description": "FCB1010 volume pedal → PC-routed"
        });

        let result = executor
            .execute("conductor_set_context_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 1);
                assert!(plan.description.contains("context"));
            }
            other => panic!("Expected PlanCreated for PcContextSwitch, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_context_mapping_cc_creates_plan() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "mode": "Default",
            "trigger": { "type": "CC", "cc": 7, "channel": 0 },
            "action": {
                "type": "CcContextSwitch",
                "cc": 64,
                "channel": 0,
                "device": "keyboard",
                "ranges": [
                    { "min": 0, "max": 63, "action": { "type": "Shell", "command": "echo low" } },
                    { "min": 64, "max": 127, "action": { "type": "Shell", "command": "echo high" } }
                ]
            }
        });

        let result = executor
            .execute("conductor_set_context_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::PlanCreated { plan } => {
                assert_eq!(plan.changes.len(), 1);
            }
            other => panic!("Expected PlanCreated for CcContextSwitch, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_context_mapping_rejects_non_context_switch_action() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        // A Keystroke action is valid ActionConfig but not a context-
        // switch; this tool should reject it and point the LLM back
        // at the generic create tool.
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 36 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
        });

        let result = executor
            .execute("conductor_set_context_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.contains("PcContextSwitch") && message.contains("CcContextSwitch"),
                    "error should name the accepted action types, got: {}",
                    message
                );
            }
            other => panic!(
                "Expected Error for non-context-switch action, got: {:?}",
                other
            ),
        }
    }

    #[tokio::test]
    async fn test_set_context_mapping_mode_not_found() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "mode": "NonExistent",
            "trigger": { "type": "CC", "cc": 7, "channel": 0 },
            "action": {
                "type": "PcContextSwitch",
                "channel": 0,
                "device": "fcb1010",
                "mappings": {
                    "0": { "type": "Shell", "command": "echo a" }
                }
            }
        });

        let result = executor
            .execute("conductor_set_context_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.contains("NonExistent") || message.contains("mode"),
                    "expected mode-not-found error, got: {}",
                    message
                );
            }
            other => panic!("Expected Error for mode-not-found, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_context_mapping_missing_mode_argument() {
        // Distinct from `mode_not_found`: this covers the path where
        // the caller omits `mode` entirely. The error should surface
        // at arg-parsing time, before any mode lookup.
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "trigger": { "type": "CC", "cc": 7, "channel": 0 },
            "action": {
                "type": "PcContextSwitch",
                "channel": 0,
                "device": "fcb1010",
                "mappings": {
                    "0": { "type": "Shell", "command": "echo a" }
                }
            }
        });

        let result = executor
            .execute("conductor_set_context_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.contains("mode"),
                    "expected missing-mode-argument error, got: {}",
                    message
                );
            }
            other => panic!("Expected Error for missing mode argument, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_context_mapping_invalid_trigger() {
        let config = create_test_config();
        let executor = ToolExecutor::new(live_config_arc(config));

        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Bogus" },
            "action": {
                "type": "PcContextSwitch",
                "channel": 0,
                "device": "fcb1010",
                "mappings": { "0": { "type": "Shell", "command": "echo a" } }
            }
        });

        let result = executor
            .execute("conductor_set_context_mapping", Some(args), None)
            .await;

        match result {
            ExecutionResult::Error { message } => {
                assert!(
                    message.to_lowercase().contains("trigger"),
                    "expected trigger error, got: {}",
                    message
                );
            }
            other => panic!("Expected Error for invalid trigger, got: {:?}", other),
        }
    }

    // ── #1053: daemon-side MIDI Learn timeout enforcement ───────────────
    //
    // Before this fix, `conductor_start_learn` was fire-and-forget: it
    // flipped `midi_learn_active` to true and returned `timeout_seconds`
    // as informational metadata, but the daemon had no timer. The implicit
    // contract was "the LLM remembers to call conductor_stop_learn after
    // timeout_seconds" — structurally unfulfillable since LLM agent loops
    // are stateless across turns and have no async scheduling primitives.
    // In practice sessions stayed active forever.

    // Short real-time waits (~1.2s) instead of `start_paused` + `advance`
    // because the latter doesn't reliably propagate timer wakeups into
    // `tokio::spawn`-ed tasks under the current runtime: the spawned
    // timer's `sleep().await` parks but never resumes after `advance`
    // even with multiple `yield_now()` calls. The smallest timeout the
    // public start tool accepts is 1 second (timeout_seconds is u64),
    // so the test must wait at least that. Trade-off accepted — tests
    // run in ~1.5s each, deterministic, no flakiness observed.

    #[tokio::test]
    async fn test_start_learn_auto_stops_after_timeout() {
        use std::collections::VecDeque;
        use std::sync::atomic::{AtomicBool, Ordering};

        let config = create_test_config();
        let active = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(VecDeque::new()));
        let executor =
            ToolExecutor::with_midi_learn_state(live_config_arc(config), active.clone(), events);

        // timeout_seconds is u64 so the smallest public-API value is 1
        // second. We pass that and wait ~1.2s of real time for the
        // spawned tokio timer's sleep().await to resolve — see the
        // module-level note above the test fixtures explaining why
        // start_paused + advance() doesn't reliably propagate wakes
        // into spawned tasks under the current runtime.
        let args = json!({ "timeout_seconds": 1 });
        let _ = executor
            .execute_stateful("conductor_start_learn", Some(args))
            .await;
        assert!(
            active.load(Ordering::SeqCst),
            "Learn should be active after start"
        );

        // Wait past the deadline. Real time so the spawned timer's
        // `sleep().await` actually resolves.
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

        assert!(
            !active.load(Ordering::SeqCst),
            "Learn should be auto-stopped by daemon timeout"
        );
    }

    #[tokio::test]
    async fn test_explicit_stop_cancels_pending_timeout_timer() {
        // Explicit conductor_stop_learn should cancel the timer so a
        // subsequent start can install a fresh one without the old timer
        // firing late and stopping the new session.
        use std::collections::VecDeque;
        use std::sync::atomic::{AtomicBool, Ordering};

        let config = create_test_config();
        let active = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(VecDeque::new()));
        let executor =
            ToolExecutor::with_midi_learn_state(live_config_arc(config), active.clone(), events);

        // Start with a 1s timeout, immediately stop. The first timer
        // should be cancelled so it can't fire later.
        let args = json!({ "timeout_seconds": 1 });
        let _ = executor
            .execute_stateful("conductor_start_learn", Some(args))
            .await;
        let _ = executor
            .execute_stateful("conductor_stop_learn", None)
            .await;
        assert!(!active.load(Ordering::SeqCst), "stop sets active false");

        // Re-start with a longer timeout. Old (cancelled) timer must NOT
        // fire mid-session and prematurely stop us.
        let args = json!({ "timeout_seconds": 10 });
        let _ = executor
            .execute_stateful("conductor_start_learn", Some(args))
            .await;

        // Wait past the FIRST timer's would-be deadline.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        assert!(
            active.load(Ordering::SeqCst),
            "second session must still be active — old timer should have been cancelled"
        );
    }

    // #1059 Copilot review: race window between `active.store(true)` and
    // `prev.abort()` in start. If the prior timer's sleep wakes during
    // that window, its body runs `active.swap(false, ...)` against the
    // freshly-started session and silently stops it. Fix: each timer
    // captures a session generation; subsequent start bumps the
    // generation; the timer's body checks generation match before
    // calling swap. This test rapid-restarts WITHOUT an intervening
    // explicit stop (the existing test_explicit_stop_cancels_…
    // scenario goes through stop, which already invalidated active=false
    // so swap was a no-op even without the gen check).
    #[tokio::test]
    async fn test_subsequent_start_invalidates_prior_timer_via_session_generation() {
        use std::collections::VecDeque;
        use std::sync::atomic::{AtomicBool, Ordering};

        let config = create_test_config();
        let active = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(VecDeque::new()));
        let executor =
            ToolExecutor::with_midi_learn_state(live_config_arc(config), active.clone(), events);

        // Start with 1s timeout — T1 spawned.
        let _ = executor
            .execute_stateful(
                "conductor_start_learn",
                Some(json!({ "timeout_seconds": 1 })),
            )
            .await;
        // Immediately re-start with a 10s timeout — T2 replaces T1.
        // T1 may have been aborted before its sleep woke (most likely),
        // OR its body may run if abort lost the race. Either way, the
        // generation check in T1's body must prevent a swap that would
        // stop the freshly-started session.
        let _ = executor
            .execute_stateful(
                "conductor_start_learn",
                Some(json!({ "timeout_seconds": 10 })),
            )
            .await;

        // Wait past T1's would-be deadline.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        assert!(
            active.load(Ordering::SeqCst),
            "T2's session must still be active — T1's stale wake must not stop it"
        );
    }

    // #1053 follow-up: when the LLM rapid-restarts conductor_start_learn
    // (intentionally — user asked it to "run learn each time"), the LLM
    // had no signal that it was preempting an active session. The tool
    // result looked identical for fresh start vs restart, so the LLM
    // didn't acknowledge the restart to the user. Surface a
    // `was_already_active` flag in the response so the LLM can reason
    // about it and a `message` that explicitly says RESTARTED.
    #[tokio::test]
    async fn test_start_learn_response_indicates_restart_when_already_active() {
        use std::collections::VecDeque;
        use std::sync::atomic::AtomicBool;

        let config = create_test_config();
        let active = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(VecDeque::new()));
        let executor =
            ToolExecutor::with_midi_learn_state(live_config_arc(config), active.clone(), events);

        // First start: fresh session.
        let r1 = executor
            .execute_stateful(
                "conductor_start_learn",
                Some(json!({"timeout_seconds": 10})),
            )
            .await;
        let r1_text = match r1 {
            ExecutionResult::Logged { result, .. } => extract_text(&result),
            other => panic!("Expected Logged result, got: {:?}", other),
        };
        let r1_json: serde_json::Value = serde_json::from_str(&r1_text).unwrap();
        assert_eq!(
            r1_json.get("was_already_active").and_then(|v| v.as_bool()),
            Some(false),
            "first start: was_already_active should be false (fresh session)"
        );
        let r1_msg = r1_json
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            !r1_msg.to_lowercase().contains("restart"),
            "first start message should not mention restart: {}",
            r1_msg
        );

        // Second start: should be flagged as a restart.
        let r2 = executor
            .execute_stateful(
                "conductor_start_learn",
                Some(json!({"timeout_seconds": 10})),
            )
            .await;
        let r2_text = match r2 {
            ExecutionResult::Logged { result, .. } => extract_text(&result),
            other => panic!("Expected Logged result, got: {:?}", other),
        };
        let r2_json: serde_json::Value = serde_json::from_str(&r2_text).unwrap();
        assert_eq!(
            r2_json.get("was_already_active").and_then(|v| v.as_bool()),
            Some(true),
            "second start: was_already_active should be true (preempted active session)"
        );
        let r2_msg = r2_json
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            r2_msg.to_lowercase().contains("restart"),
            "second start message should explicitly say restarted: {}",
            r2_msg
        );
        // Copilot review (#1059): the daemon does NOT drain
        // midi_learn_events on restart — only conductor_stop_learn
        // drains. The message must NOT falsely claim events were
        // discarded; it should explain the buffer semantics instead.
        assert!(
            !r2_msg.to_lowercase().contains("discarded"),
            "restart message must NOT claim events were discarded (daemon does not drain on restart): {}",
            r2_msg
        );
        assert!(
            r2_msg.to_lowercase().contains("buffer"),
            "restart message should explain the events-buffer semantics: {}",
            r2_msg
        );
    }

    // Helper for the test above. Pulls plain text out of the
    // ToolCallResult content vec — the daemon's tools return JSON
    // serialised as the `text` field of a Text content block.
    fn extract_text(result: &crate::daemon::mcp_types::ToolCallResult) -> String {
        for c in &result.content {
            if let crate::daemon::mcp_types::ToolContent::Text { text } = c {
                return text.clone();
            }
        }
        panic!("No Text content block in result");
    }

    // ===== ADR-035 Slice 8: conductor_create_endpoint + deprecations =====

    #[tokio::test]
    async fn test_create_endpoint_matcher_produces_plan() {
        use crate::daemon::llm::ConfigChange;
        use conductor_core::config::types::EndpointKind;
        let executor = ToolExecutor::new(live_config_arc(create_test_config()));
        let args = json!({
            "alias": "pads",
            "direction": "Input",
            "type": "Matcher",
            "matchers": [{ "type": "NameContains", "value": "Mikro" }]
        });
        match executor
            .execute("conductor_create_endpoint", Some(args), None)
            .await
        {
            ExecutionResult::PlanCreated { plan } => {
                assert!(
                    plan.validation_errors.is_empty(),
                    "valid endpoint must apply+validate cleanly: {:?}",
                    plan.validation_errors
                );
                assert!(
                    plan.deprecation.is_none(),
                    "create_endpoint is not deprecated"
                );
                match &plan.changes[0] {
                    ConfigChange::CreateEndpoint {
                        alias,
                        direction,
                        kind,
                        ..
                    } => {
                        assert_eq!(alias, "pads");
                        assert_eq!(format!("{direction:?}"), "Input");
                        assert!(matches!(kind, EndpointKind::Matcher { .. }));
                    }
                    other => panic!("expected CreateEndpoint, got {other:?}"),
                }
            }
            other => panic!("expected PlanCreated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_create_endpoint_osc_roundtrips_kind_and_protocol() {
        use crate::daemon::llm::ConfigChange;
        use conductor_core::config::types::EndpointKind;
        let executor = ToolExecutor::new(live_config_arc(create_test_config()));
        let args = json!({
            "alias": "eos",
            "direction": "Output",
            "protocol": "Osc",
            "type": "OscEndpoint",
            "host": "127.0.0.1",
            "port": 9000
        });
        match executor
            .execute("conductor_create_endpoint", Some(args), None)
            .await
        {
            ExecutionResult::PlanCreated { plan } => match &plan.changes[0] {
                ConfigChange::CreateEndpoint {
                    alias,
                    protocol,
                    kind,
                    ..
                } => {
                    assert_eq!(alias, "eos");
                    assert_eq!(format!("{protocol:?}"), "Some(Osc)");
                    assert!(matches!(kind, EndpointKind::OscEndpoint { port: 9000, .. }));
                }
                other => panic!("expected CreateEndpoint, got {other:?}"),
            },
            other => panic!("expected PlanCreated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_create_endpoint_requires_direction() {
        // `direction` is REQUIRED for endpoints (ADR-035 §4.1 R2 P1 — no default).
        let executor = ToolExecutor::new(live_config_arc(create_test_config()));
        let args = json!({
            "alias": "nodir",
            "type": "Matcher",
            "matchers": [{ "type": "NameContains", "value": "X" }]
        });
        match executor
            .execute("conductor_create_endpoint", Some(args), None)
            .await
        {
            ExecutionResult::Error { message } => assert!(
                message.to_lowercase().contains("direction"),
                "error must name the missing field; got: {message}"
            ),
            other => panic!("expected Error for missing direction, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_create_endpoint_rejects_duplicate_alias() {
        use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
        let mut config = create_test_config();
        config.endpoints.push(EndpointConfig {
            alias: "taken".to_string(),
            direction: ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers: vec![conductor_core::identity::DeviceMatcher::NameContains {
                    value: "A".to_string(),
                }],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        });
        let executor = ToolExecutor::new(live_config_arc(config));
        let args = json!({
            "alias": "taken",
            "direction": "Input",
            "type": "Matcher",
            "matchers": [{ "type": "NameContains", "value": "B" }]
        });
        match executor
            .execute("conductor_create_endpoint", Some(args), None)
            .await
        {
            ExecutionResult::Error { message } => assert!(
                message.contains("taken") && message.to_lowercase().contains("exist"),
                "error must name the colliding alias and that it already exists; got: {message}"
            ),
            other => panic!("expected Error for duplicate alias, got {other:?}"),
        }
    }
}
