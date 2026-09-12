// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ─── Mutation-gap tests (cargo-mutants findings) ──────────────────
//
// Each test below kills one or more mutants a sampled cargo-mutants
// run survived. Keep them behavior-anchored: they pin what the
// executor DOES, not how the lines are written.

/// Kills: `delete match arm ToolRiskTier::HardwareIO` in the budget
/// charge — without the arm, MIDI-out stops charging its budget
/// dimension and an LLM session can emit unbounded hardware output.
#[tokio::test]
async fn test_budget_charges_midi_out_dimension() {
    let cfg = conductor_core::security::LlmBudgetConfig {
        max_midi_out_per_session: 1,
        ..Default::default()
    };
    let mut executor = ToolExecutor::new(live_config_arc(create_test_config()));
    executor.set_budget_state(budget_state(cfg));

    // First HardwareIO-tier call charges midi_out 0 → 1 (whatever the
    // handler then does — confirmation, arg validation — the charge
    // lands first and must not be a budget halt).
    let first = executor.execute("conductor_send_midi", None, None).await;
    assert!(
        !matches!(&first, ExecutionResult::Error { message } if message.contains("budget")),
        "first send_midi should be within budget, got {:?}",
        first
    );

    // Second HardwareIO-tier call must trip the MIDI-out quota.
    let second = executor.execute("conductor_send_midi", None, None).await;
    match second {
        ExecutionResult::Error { message } => {
            assert!(message.contains("budget exceeded"), "got: {message}");
            assert!(
                message.contains("max_midi_out_per_session"),
                "the halt must name the MIDI-out dimension; got: {message}"
            );
        }
        other => panic!("Expected MIDI-out budget halt, got: {:?}", other),
    }
}

/// Kills: `fetch_devices_data -> None` / `-> Some(Default::default())`.
/// conductor_list_devices must return a payload carrying BOTH device
/// family keys, even with no hardware attached (empty arrays).
#[tokio::test]
async fn test_list_devices_payload_has_both_device_families() {
    let executor = ToolExecutor::new(live_config_arc(create_test_config()));
    let result = readonly_result(executor.execute("conductor_list_devices", None, None).await);
    assert_ne!(result.is_error, Some(true), "list_devices must not error");
    let text = match &result.content[0] {
        crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
        other => panic!("Expected text content, got {other:?}"),
    };
    let payload: Value = serde_json::from_str(&text).expect("payload parses as JSON");
    assert!(
        payload.get("midi_devices").is_some_and(Value::is_array),
        "payload must carry a midi_devices array; got: {payload}"
    );
    assert!(
        payload.get("hid_devices").is_some_and(Value::is_array),
        "payload must carry a hid_devices array; got: {payload}"
    );
    // The mcp-side None-fallback stamps a "message" marker; a real
    // enumeration (even with zero devices attached) never does.
    assert!(
        payload.get("message").is_none(),
        "payload must come from a real enumeration, not the \
         device-data-unavailable fallback; got: {payload}"
    );
}

/// Kills: `delete match arm "conductor_get_device_identity"` and
/// `delete match arm "conductor_list_device_identities"` — the two
/// identity tools must dispatch (an unknown-tool fallthrough is the
/// mutant's observable).
#[tokio::test]
async fn test_device_identity_tools_dispatch() {
    use arc_swap::ArcSwap;
    let live = live_config_arc(create_test_config());
    let refs = crate::daemon::engine_manager::SharedDaemonStateRefs::for_routing_tools_test(
        Arc::new(ArcSwap::from_pointee(
            crate::route_engine::RouteEngine::compile(&[]),
        )),
        Arc::new(crate::daemon::dispatch_trace::DispatchTraceRing::default()),
    );
    let executor = ToolExecutor::with_daemon_state(
        live,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        Arc::new(Mutex::new(std::collections::VecDeque::new())),
        refs,
    );

    let args = json!({ "port_name": "Virtual Test Port" });
    let result = readonly_result(
        executor
            .execute("conductor_get_device_identity", Some(args), None)
            .await,
    );
    assert_ne!(result.is_error, Some(true));
    let text = match &result.content[0] {
        crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
        other => panic!("Expected text content, got {other:?}"),
    };
    let payload: Value = serde_json::from_str(&text).expect("payload parses");
    assert_eq!(
        payload.get("port_name").and_then(Value::as_str),
        Some("Virtual Test Port"),
        "identity payload must echo the queried port; got: {payload}"
    );
    assert!(
        payload.get("identity").is_some(),
        "identity key must be present (null when unprobed); got: {payload}"
    );

    let result = readonly_result(
        executor
            .execute("conductor_list_device_identities", None, None)
            .await,
    );
    assert_ne!(result.is_error, Some(true));
    let text = match &result.content[0] {
        crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
        other => panic!("Expected text content, got {other:?}"),
    };
    let payload: Value = serde_json::from_str(&text).expect("payload parses");
    assert!(
        payload.get("identities").is_some_and(Value::is_array),
        "payload must carry an identities array; got: {payload}"
    );
}

/// Kills: `replace == with !=` on `result.is_error == Some(true)` in
/// the readonly audit branch — an errored tool result must be recorded
/// via log_tool_error, a clean one via log_tool_complete, never swapped.
#[tokio::test]
async fn test_readonly_audit_routes_error_vs_complete() {
    use arc_swap::ArcSwap;

    // Error path: no daemon refs → get_active_pc returns an is_error
    // result, which the control-state audit branch must record via
    // log_tool_error.
    let err_sink = Arc::new(RecordingSink::new());
    let mut executor = ToolExecutor::new(live_config_arc(create_test_config()));
    executor.set_audit_logger(err_sink.clone());
    let _ = readonly_result(
        executor
            .execute("conductor_get_active_pc", None, None)
            .await,
    );

    // Clean path: refs present → get_control_state succeeds and must be
    // recorded via log_tool_complete.
    let ok_sink = Arc::new(RecordingSink::new());
    let refs = crate::daemon::engine_manager::SharedDaemonStateRefs::for_routing_tools_test(
        Arc::new(ArcSwap::from_pointee(
            crate::route_engine::RouteEngine::compile(&[]),
        )),
        Arc::new(crate::daemon::dispatch_trace::DispatchTraceRing::default()),
    );
    let mut executor = ToolExecutor::with_daemon_state(
        live_config_arc(create_test_config()),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        Arc::new(Mutex::new(std::collections::VecDeque::new())),
        refs,
    );
    executor.set_audit_logger(ok_sink.clone());
    let _ = readonly_result(
        executor
            .execute("conductor_get_control_state", None, None)
            .await,
    );

    let errors = err_sink.errors.lock().unwrap().clone();
    let err_completes = err_sink.completes.lock().unwrap().clone();
    assert!(
        errors.iter().any(|t| t == "conductor_get_active_pc"),
        "errored result must be audited via log_tool_error; errors={errors:?} completes={err_completes:?}"
    );
    assert!(
        !err_completes.iter().any(|t| t == "conductor_get_active_pc"),
        "errored result must NOT hit log_tool_complete; completes={err_completes:?}"
    );
    let completes = ok_sink.completes.lock().unwrap().clone();
    let ok_errors = ok_sink.errors.lock().unwrap().clone();
    assert!(
        completes.iter().any(|t| t == "conductor_get_control_state"),
        "clean result must be audited via log_tool_complete; completes={completes:?} errors={ok_errors:?}"
    );
    assert!(
        !ok_errors.iter().any(|t| t == "conductor_get_control_state"),
        "clean result must NOT hit log_tool_error; errors={ok_errors:?}"
    );
}
