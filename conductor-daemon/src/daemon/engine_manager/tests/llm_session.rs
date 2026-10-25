// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 §D6 — LLM budget session lifecycle
//! (`LlmSessionBegin` / `LlmTurnBegin`).
//!
//! The daemon's D6 budget enforcement (charged by `ToolExecutor::admit`
//! on every MCP tool call, including the direct-dispatch arms) is
//! INACTIVE until the trusted GUI backend signals a session boundary.
//! These tests pin the lifecycle: who may signal (GuiTrusted ONLY —
//! `CliTrusted` is excluded because a budgeted LLM with shell access
//! could invoke the signed CLI to reset its own caps), where the limits
//! come from (the operator-editable USER config file, never the
//! `live.toml` authority, which drops the `[security]` table), and the
//! reset semantics (session-begin = fresh state; turn-begin = per-turn
//! counters only).
//!
//! Linux ignore on every test: `EngineManager::new` transitively
//! constructs an `ActionExecutor` → `Enigo::new()` panics on headless
//! Linux (project-wide constraint; see the e2e.rs module rationale).

use super::*;

#[cfg(feature = "llm-executor")]
fn session_manager() -> EngineManager {
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _) = broadcast::channel(1);
    EngineManager::new(
        create_test_config(),
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap()
}

/// Manager whose user file is a temp config carrying the given
/// `[security.llm]` body. Returns the TempDir so the file outlives the
/// test body.
#[cfg(feature = "llm-executor")]
fn session_manager_with_security(llm_table: &str) -> (EngineManager, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let user_file = dir.path().join("config.toml");
    std::fs::write(&user_file, format!("[security.llm]\n{llm_table}\n")).expect("write user file");
    let mut manager = session_manager();
    manager.set_user_file_path(user_file);
    (manager, dir)
}

#[cfg(feature = "llm-executor")]
fn bare_request(id: &str, command: IpcCommand) -> crate::daemon::types::IpcRequest {
    crate::daemon::types::IpcRequest {
        id: id.to_string(),
        command,
        args: serde_json::json!({}),
    }
}

#[cfg(feature = "llm-executor")]
fn gui_ctx() -> crate::security::CallerContext {
    crate::security::CallerContext::new(crate::security::TrustLevel::GuiTrusted)
}

#[cfg(feature = "llm-executor")]
async fn send(
    manager: &mut EngineManager,
    request: crate::daemon::types::IpcRequest,
    ctx: Option<crate::security::CallerContext>,
) -> IpcResponse {
    tokio::time::timeout(
        Duration::from_secs(5),
        manager.handle_ipc_request(request, ctx),
    )
    .await
    .expect("handle_ipc_request must return within 5 s")
}

/// Drive one `conductor_switch_profile` MCP call (fails functionally on
/// the nonexistent profile — irrelevant; the budget charge in `admit`
/// is what these tests observe) and return its `data` payload.
#[cfg(feature = "llm-executor")]
async fn charged_tool_call(manager: &mut EngineManager, id: &str) -> serde_json::Value {
    let request = crate::daemon::types::IpcRequest {
        id: id.to_string(),
        command: IpcCommand::ExecuteMcpTool,
        args: serde_json::json!({
            "tool_name": "conductor_switch_profile",
            "arguments": {
                "profile_name": "nope",
                "config_path": "/tmp/definitely-missing-profile.toml",
            },
        }),
    };
    send(manager, request, None)
        .await
        .data
        .expect("ExecuteMcpTool response carries data")
}

// ── Trust gating ────────────────────────────────────────────────────

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn session_begin_denied_for_unpinned_peer() {
    let mut manager = session_manager();
    let response = send(
        &mut manager,
        bare_request("sb-unpinned", IpcCommand::LlmSessionBegin),
        None,
    )
    .await;
    assert!(matches!(response.status, ResponseStatus::Error));
    assert_eq!(
        response.error.expect("error details").code,
        IpcErrorCode::PermissionDenied.as_u16(),
        "unpinned peers must not be able to attach/reset the session budget"
    );
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn session_begin_denied_for_cli_peer() {
    // CliTrusted is deliberately NOT a session-boundary signaller: a
    // budgeted LLM with a shell-exec capability could invoke the signed
    // CLI binary and launder its own per-session caps.
    let mut manager = session_manager();
    let ctx = crate::security::CallerContext::new(crate::security::TrustLevel::CliTrusted);
    let response = send(
        &mut manager,
        bare_request("sb-cli", IpcCommand::LlmSessionBegin),
        Some(ctx),
    )
    .await;
    assert!(matches!(response.status, ResponseStatus::Error));
    assert_eq!(
        response.error.expect("error details").code,
        IpcErrorCode::PermissionDenied.as_u16()
    );
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn turn_begin_denied_for_unpinned_peer() {
    let mut manager = session_manager();
    let response = send(
        &mut manager,
        bare_request("tb-unpinned", IpcCommand::LlmTurnBegin),
        None,
    )
    .await;
    assert!(matches!(response.status, ResponseStatus::Error));
    assert_eq!(
        response.error.expect("error details").code,
        IpcErrorCode::PermissionDenied.as_u16(),
        "a turn reset launders the per-turn caps, so it needs the same trust"
    );
}

// ── Activation and limit source ─────────────────────────────────────

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn session_begin_activates_budget_from_user_file() {
    let (mut manager, _dir) = session_manager_with_security("max_tool_calls_per_session = 1");

    let response = send(
        &mut manager,
        bare_request("sb-activate", IpcCommand::LlmSessionBegin),
        Some(gui_ctx()),
    )
    .await;
    assert!(
        matches!(response.status, ResponseStatus::Success),
        "session begin from a verified GUI peer must succeed: {:?}",
        response.error
    );
    assert_eq!(response.data.expect("data")["active"], true);

    // The single budgeted tool call is admitted…
    let first = charged_tool_call(&mut manager, "sb-call-1").await;
    assert_ne!(
        first["type"], "Error",
        "first call is within budget; got {first}"
    );
    // …and the second is refused by the budget compiled from the USER file.
    let second = charged_tool_call(&mut manager, "sb-call-2").await;
    assert_eq!(
        second["type"], "Error",
        "second call must be refused; got {second}"
    );
    assert!(
        second["message"]
            .as_str()
            .is_some_and(|m| m.contains("LLM budget exceeded")),
        "refusal must name the budget; got {second}"
    );
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn session_begin_with_missing_user_file_uses_adr_defaults() {
    // No user file on disk: the ADR-027 §D6 defaults apply (all
    // dimensions enforced at their default limits), matching
    // `conductorctl llm budgets show` semantics.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut manager = session_manager();
    manager.set_user_file_path(dir.path().join("does-not-exist.toml"));

    let response = send(
        &mut manager,
        bare_request("sb-defaults", IpcCommand::LlmSessionBegin),
        Some(gui_ctx()),
    )
    .await;
    assert!(
        matches!(response.status, ResponseStatus::Success),
        "missing user file means defaults, not failure: {:?}",
        response.error
    );
    assert_eq!(response.data.expect("data")["active"], true);
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn session_begin_rejects_malformed_security_table() {
    // A malformed [security.llm] is a hard error (fail-closed on the
    // operator's intent — don't silently run defaults when they tried
    // to configure something), and the budget stays inactive.
    let (mut manager, _dir) =
        session_manager_with_security("max_tool_calls_per_session = \"not-a-number\"");

    let response = send(
        &mut manager,
        bare_request("sb-malformed", IpcCommand::LlmSessionBegin),
        Some(gui_ctx()),
    )
    .await;
    assert!(matches!(response.status, ResponseStatus::Error));
    assert_eq!(
        response.error.expect("error details").code,
        IpcErrorCode::ConfigParseFailed.as_u16()
    );

    // Budget must not have been attached: turn-begin reports inactive.
    let turn = send(
        &mut manager,
        bare_request("tb-after-malformed", IpcCommand::LlmTurnBegin),
        Some(gui_ctx()),
    )
    .await;
    assert!(matches!(turn.status, ResponseStatus::Success));
    assert_eq!(turn.data.expect("data")["active"], false);
}

// ── Reset semantics ─────────────────────────────────────────────────

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn turn_begin_resets_per_turn_counters_only() {
    let (mut manager, _dir) = session_manager_with_security("max_tool_calls_per_turn = 1");

    let begin = send(
        &mut manager,
        bare_request("tb-session", IpcCommand::LlmSessionBegin),
        Some(gui_ctx()),
    )
    .await;
    assert!(matches!(begin.status, ResponseStatus::Success));

    let first = charged_tool_call(&mut manager, "tb-call-1").await;
    assert_ne!(
        first["type"], "Error",
        "first call of the turn is admitted; got {first}"
    );
    let second = charged_tool_call(&mut manager, "tb-call-2").await;
    assert_eq!(
        second["type"], "Error",
        "second call in the same turn must trip max_tool_calls_per_turn; got {second}"
    );

    let turn = send(
        &mut manager,
        bare_request("tb-turn", IpcCommand::LlmTurnBegin),
        Some(gui_ctx()),
    )
    .await;
    assert!(matches!(turn.status, ResponseStatus::Success));
    assert_eq!(turn.data.expect("data")["active"], true);

    let third = charged_tool_call(&mut manager, "tb-call-3").await;
    assert_ne!(
        third["type"], "Error",
        "a new turn resets the per-turn counter; got {third}"
    );
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn session_begin_attaches_a_fresh_state() {
    let (mut manager, _dir) = session_manager_with_security("max_tool_calls_per_session = 1");

    let begin = send(
        &mut manager,
        bare_request("fresh-session-1", IpcCommand::LlmSessionBegin),
        Some(gui_ctx()),
    )
    .await;
    assert!(matches!(begin.status, ResponseStatus::Success));

    let first = charged_tool_call(&mut manager, "fresh-call-1").await;
    assert_ne!(first["type"], "Error");
    let second = charged_tool_call(&mut manager, "fresh-call-2").await;
    assert_eq!(second["type"], "Error", "session cap spent; got {second}");

    // A new session (the GUI opened a new conversation) starts clean.
    let begin2 = send(
        &mut manager,
        bare_request("fresh-session-2", IpcCommand::LlmSessionBegin),
        Some(gui_ctx()),
    )
    .await;
    assert!(matches!(begin2.status, ResponseStatus::Success));
    let third = charged_tool_call(&mut manager, "fresh-call-3").await;
    assert_ne!(
        third["type"], "Error",
        "a fresh session must reset the per-session counters; got {third}"
    );
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn turn_begin_without_session_reports_inactive() {
    // The GUI may fire turn-begin unconditionally; before any
    // session-begin there is nothing to reset and nothing is charged.
    let mut manager = session_manager();
    let response = send(
        &mut manager,
        bare_request("tb-no-session", IpcCommand::LlmTurnBegin),
        Some(gui_ctx()),
    )
    .await;
    assert!(matches!(response.status, ResponseStatus::Success));
    assert_eq!(response.data.expect("data")["active"], false);

    // And the budget really is inactive: tool calls are not charged.
    let call = charged_tool_call(&mut manager, "tb-uncharged").await;
    assert_ne!(
        call["type"], "Error",
        "no budget attached, nothing refused; got {call}"
    );
}
