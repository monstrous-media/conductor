// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ToolExecutor construction and configuration accessors.

use super::*;

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

    /// Create a new tool executor with MIDI Learn state and daemon state
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
}
