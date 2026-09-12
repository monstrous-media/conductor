// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

pub(crate) use super::*;

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

/// Unwrap a readonly tool outcome (Success without an audit logger,
/// Logged with one) to its ToolCallResult.
fn readonly_result(er: ExecutionResult) -> crate::daemon::mcp_types::ToolCallResult {
    match er {
        ExecutionResult::Success { result } | ExecutionResult::Logged { result, .. } => result,
        other => panic!("Expected Success/Logged readonly outcome, got: {other:?}"),
    }
}

/// Minimal recording AuditSink: counts which completion method fired.
struct RecordingSink {
    completes: std::sync::Mutex<Vec<String>>,
    errors: std::sync::Mutex<Vec<String>>,
    tx: tokio::sync::broadcast::Sender<crate::daemon::audit::AuditEntry>,
}
impl RecordingSink {
    fn new() -> Self {
        Self {
            completes: std::sync::Mutex::new(vec![]),
            errors: std::sync::Mutex::new(vec![]),
            tx: tokio::sync::broadcast::channel(4).0,
        }
    }
}
impl AuditSink for RecordingSink {
    fn log_tool_start(
        &self,
        _tool_name: &str,
        _risk_tier: AuditRiskTier,
        _arguments: Option<&str>,
        _user_context: Option<UserContext>,
    ) -> String {
        String::new()
    }
    fn log_tool_complete(
        &self,
        tool_name: &str,
        _risk_tier: AuditRiskTier,
        _arguments: Option<&str>,
        _result: Option<&str>,
        _execution_time: std::time::Duration,
        _user_context: Option<UserContext>,
    ) {
        self.completes.lock().unwrap().push(tool_name.to_string());
    }
    fn log_tool_error(
        &self,
        tool_name: &str,
        _risk_tier: AuditRiskTier,
        _arguments: Option<&str>,
        _error_message: &str,
        _execution_time: std::time::Duration,
        _user_context: Option<UserContext>,
    ) {
        self.errors.lock().unwrap().push(tool_name.to_string());
    }
    fn log_tool_denied(
        &self,
        _tool_name: &str,
        _risk_tier: AuditRiskTier,
        _reason: &str,
        _user_context: Option<UserContext>,
    ) {
    }
    fn log_llm_budget_exceeded(
        &self,
        _tool_name: &str,
        _risk_tier: AuditRiskTier,
        _dimension: &str,
        _limit: u64,
        _observed: u64,
        _user_context: Option<UserContext>,
    ) {
    }
    fn log_network_event(
        &self,
        _event_type: crate::daemon::audit::AuditEventType,
        _listener: &str,
        _ip: std::net::IpAddr,
        _summary: Option<&str>,
    ) -> String {
        String::new()
    }
    fn log_path_validation_failed(&self, _attempted_path: &str, _reason: &str) -> String {
        String::new()
    }
    fn log_pending_at_crash_batch(
        &self,
        _pending: &[crate::daemon::audit::ReconciledMutation],
    ) -> usize {
        0
    }
    fn log_plan_created(
        &self,
        _plan_id: &str,
        _changes_count: usize,
        _user_context: Option<UserContext>,
    ) {
    }
    fn log_plan_applied(
        &self,
        _plan_id: &str,
        _changes_applied: usize,
        _execution_time: std::time::Duration,
        _user_context: Option<UserContext>,
        _provenance: Option<conductor_core::config::Provenance>,
    ) {
    }
    fn log_plan_rejected(&self, _plan_id: &str, _user_context: Option<UserContext>) {}
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<crate::daemon::audit::AuditEntry> {
        self.tx.subscribe()
    }
}

mod audit;
mod batch;
mod construct;
mod core;
mod endpoint;
mod hardware;
mod mutation;
mod probe;
mod sessions;
mod undo;
