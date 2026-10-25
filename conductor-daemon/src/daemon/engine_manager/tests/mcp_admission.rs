// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 §D16/§D6 — admission checks on the `ExecuteMcpTool`
//! direct-dispatch arms.
//!
//! `conductor_switch_profile` and `conductor_probe_device_identity` are
//! handled directly in `ipc_llm.rs` (not through `ToolExecutor::execute`)
//! because the executor's `command_tx` round-trip cannot resolve while
//! `handle_ipc_request` runs inside the command_rx select arm. Bypassing
//! the executor for deadlock reasons must not also bypass its admission
//! sequence — security gate → rate limiter → LLM budget, each with
//! audit-on-deny. These tests pin the rate-limiter and budget halves
//! end-to-end through the IPC handler. The gate half cannot be observed
//! here (`SecurityPolicy::default()` is shadow-mode under `cfg(test)`);
//! it is pinned at the executor level in
//! `llm/executor/tests/admission.rs` with an explicit enforcing policy.
//!
//! Linux ignore on every test: `EngineManager::new` transitively
//! constructs an `ActionExecutor` → `Enigo::new()` panics on headless
//! Linux (project-wide constraint; see the e2e.rs module rationale).

use super::*;

#[cfg(feature = "llm-executor")]
fn admission_manager() -> EngineManager {
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

#[cfg(feature = "llm-executor")]
fn mcp_tool_request(
    id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> crate::daemon::types::IpcRequest {
    crate::daemon::types::IpcRequest {
        id: id.to_string(),
        command: IpcCommand::ExecuteMcpTool,
        args: serde_json::json!({
            "tool_name": tool_name,
            "arguments": arguments,
        }),
    }
}

/// Attach a tight LLM budget to the manager's executor. Only possible
/// while the executor `Arc` is unshared — i.e. right after `new()`,
/// which is exactly where production wiring would attach one.
#[cfg(feature = "llm-executor")]
fn attach_budget(manager: &mut EngineManager, cfg: conductor_core::security::LlmBudgetConfig) {
    std::sync::Arc::get_mut(&mut manager.tool_executor)
        .expect("tool_executor Arc must be unshared right after construction")
        .set_budget_state(std::sync::Arc::new(tokio::sync::Mutex::new(
            conductor_core::security::LlmBudgetState::new(cfg, 0),
        )));
}

#[cfg(feature = "llm-executor")]
async fn send(
    manager: &mut EngineManager,
    request: crate::daemon::types::IpcRequest,
) -> serde_json::Value {
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        manager.handle_ipc_request(request, None),
    )
    .await
    .expect("handle_ipc_request must return within 5 s");
    response.data.expect("ExecuteMcpTool response carries data")
}

/// The Stateful tier admits 30 requests per window (RateLimiter
/// defaults); the 31st `conductor_switch_profile` must come back
/// `RateLimited` instead of reaching `execute_profile_switch`.
#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn ipc_switch_profile_is_rate_limited_at_the_stateful_tier() {
    let mut manager = admission_manager();
    let args = serde_json::json!({
        "profile_name": "nope",
        "config_path": "/tmp/definitely-missing-profile.toml",
    });

    for i in 0..30 {
        let data = send(
            &mut manager,
            mcp_tool_request(
                &format!("switch-{i}"),
                "conductor_switch_profile",
                args.clone(),
            ),
        )
        .await;
        // The switch itself fails (nonexistent profile) — irrelevant
        // here; it must not fail by throttling yet.
        assert_ne!(
            data["type"], "RateLimited",
            "call {i} is within the Stateful window and must be admitted; got {data}"
        );
    }

    let data = send(
        &mut manager,
        mcp_tool_request("switch-31", "conductor_switch_profile", args),
    )
    .await;
    assert_eq!(
        data["type"], "RateLimited",
        "31st Stateful call in one window must be throttled; got {data}"
    );
}

/// With an LLM budget attached (`max_tool_calls_per_session = 1`), the
/// second `conductor_switch_profile` must be refused by the budget —
/// the direct-dispatch arm charges the same counters `execute()` does.
#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn ipc_switch_profile_charges_an_attached_llm_budget() {
    let mut manager = admission_manager();
    attach_budget(
        &mut manager,
        conductor_core::security::LlmBudgetConfig {
            max_tool_calls_per_session: 1,
            ..Default::default()
        },
    );
    let args = serde_json::json!({
        "profile_name": "nope",
        "config_path": "/tmp/definitely-missing-profile.toml",
    });

    let first = send(
        &mut manager,
        mcp_tool_request("budget-1", "conductor_switch_profile", args.clone()),
    )
    .await;
    assert_ne!(
        first["type"], "Error",
        "first call consumes the budget but is admitted; got {first}"
    );

    let second = send(
        &mut manager,
        mcp_tool_request("budget-2", "conductor_switch_profile", args),
    )
    .await;
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

/// The HardwareIO tier admits 5 requests per window (RateLimiter
/// defaults); the 6th `conductor_probe_device_identity` must come back
/// `RateLimited` from the executor's limiter — a per-window *count*
/// bound layered on the ProbeCoordinator's per-port wire throttle.
#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn ipc_probe_is_rate_limited_at_the_hardware_io_tier() {
    let mut manager = admission_manager();
    let args = serde_json::json!({ "port_name": "fake-port" });

    for i in 0..5 {
        let data = send(
            &mut manager,
            mcp_tool_request(
                &format!("probe-{i}"),
                "conductor_probe_device_identity",
                args.clone(),
            ),
        )
        .await;
        assert_ne!(
            data["type"], "RateLimited",
            "call {i} is within the HardwareIO window and must be admitted; got {data}"
        );
    }

    let data = send(
        &mut manager,
        mcp_tool_request("probe-6", "conductor_probe_device_identity", args),
    )
    .await;
    assert_eq!(
        data["type"], "RateLimited",
        "6th HardwareIO call in one window must be throttled; got {data}"
    );
}

/// With an LLM budget attached (`max_midi_out_per_session = 1`), the
/// second probe must be refused on the MIDI-out dimension — HardwareIO
/// direct dispatch charges `charge_midi_out(1)` exactly like
/// `execute()` does.
#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module rationale
async fn ipc_probe_charges_the_midi_out_budget() {
    let mut manager = admission_manager();
    attach_budget(
        &mut manager,
        conductor_core::security::LlmBudgetConfig {
            max_midi_out_per_session: 1,
            ..Default::default()
        },
    );
    let args = serde_json::json!({ "port_name": "fake-port" });

    let first = send(
        &mut manager,
        mcp_tool_request(
            "probe-budget-1",
            "conductor_probe_device_identity",
            args.clone(),
        ),
    )
    .await;
    assert_ne!(
        first["type"], "Error",
        "first probe consumes the MIDI-out budget but is admitted; got {first}"
    );

    let second = send(
        &mut manager,
        mcp_tool_request("probe-budget-2", "conductor_probe_device_identity", args),
    )
    .await;
    assert_eq!(
        second["type"], "Error",
        "second probe must be refused; got {second}"
    );
    assert!(
        second["message"]
            .as_str()
            .is_some_and(|m| m.contains("max_midi_out_per_session")),
        "refusal must name the exhausted dimension; got {second}"
    );
}
