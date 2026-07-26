// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

use super::*;

// ── #576: ChannelAftertouch end-to-end validation ──────────────────
//
// The `Aftertouch` (0xD0) pipeline was tested at every isolated layer
// (MidiEvent parse, InputEvent conversion, ProcessedEvent emission,
// trigger matching), but the issue asked for a confirmed end-to-end
// path — bytes-equivalent input through `process_device_event` →
// dispatched action. The tests below feed an `InputEvent::Aftertouch`
// (the shape produced from raw `0xD0 <pressure>` MIDI bytes by
// `MidiEvent::from_midi_msg().into()`) through the daemon's unified
// hot path and assert the trigger fires above the configured
// `pressure_min` and is suppressed below it. They also confirm MIDI
// Learn capture, satisfying acceptance criteria 3, 4, and 5 of the
// issue via the simulator path. (Visual GUI verification is out of
// scope for an automated test.)

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_e2e_channel_aftertouch_fires_above_threshold() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_aftertouch_e2e_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    manager.capture_actions = true;
    manager.midi_learn_active.store(false, Ordering::SeqCst);

    let mut rx = manager.event_broadcast_tx.subscribe();

    // pressure 100 > pressure_min 64 → trigger must match
    let device_event = DeviceEvent::new(
        DeviceId::raw("test-aftertouch-port"),
        InputEvent::Aftertouch {
            pressure: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok");

    assert!(
        drain_for_mapping_matched(&mut rx),
        "Aftertouch with pressure=100 (>= pressure_min 64) MUST fire mapping_matched"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_e2e_channel_aftertouch_below_threshold_does_not_fire() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_aftertouch_e2e_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    manager.capture_actions = true;
    manager.midi_learn_active.store(false, Ordering::SeqCst);

    let mut rx = manager.event_broadcast_tx.subscribe();

    // pressure 30 < pressure_min 64 → must NOT match
    let device_event = DeviceEvent::new(
        DeviceId::raw("test-aftertouch-port"),
        InputEvent::Aftertouch {
            pressure: 30,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok");

    assert!(
        !drain_for_mapping_matched(&mut rx),
        "Aftertouch with pressure=30 (< pressure_min 64) must NOT fire mapping_matched"
    );
}

/// #1135 regression-lock: `mapping_matched` MonitorEvents must carry the
/// originating device's `device_id`, so the GUI EventRow can label the
/// source instead of falling back to the `'Unknown'` placeholder.
///
/// Pre-#885 the legacy single-device hot path hardcoded
/// `prov.device_id = None`, which propagated as `device_id: null` on the
/// wire and rendered as `'Unknown'`. #885 deleted that path; this test
/// pins the post-fix invariant so it can't regress silently.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_mapping_matched_carries_source_device_id() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_aftertouch_e2e_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    manager.capture_actions = true;
    manager.midi_learn_active.store(false, Ordering::SeqCst);

    let mut rx = manager.event_broadcast_tx.subscribe();

    let device_event = DeviceEvent::new(
        DeviceId::raw("Mikro"),
        InputEvent::Aftertouch {
            pressure: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok");

    let evt = drain_first_mapping_matched(&mut rx)
        .expect("mapping_matched must fire for above-threshold aftertouch");
    assert_eq!(
        evt.device_id.as_deref(),
        Some("Mikro"),
        "#1135: mapping_matched MonitorEvent must carry the source DeviceId, \
             not None (which renders as 'Unknown' in the GUI EventRow); got {:?}",
        evt.device_id
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_e2e_channel_aftertouch_captured_by_midi_learn() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_aftertouch_e2e_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    manager.capture_actions = true;
    // Learn ACTIVE — capture should occur, dispatch should be suppressed.
    manager.midi_learn_active.store(true, Ordering::SeqCst);

    let device_event = DeviceEvent::new(
        DeviceId::raw("test-aftertouch-port"),
        InputEvent::Aftertouch {
            pressure: 90,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok");

    let events = manager.midi_learn_events.lock().await;
    let captured: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == EventType::Aftertouch)
        .collect();
    assert_eq!(
        captured.len(),
        1,
        "MIDI Learn buffer must contain exactly one Aftertouch event, got {events:?}"
    );
    assert_eq!(
        captured[0].value,
        Some(90),
        "Captured event must carry pressure=90"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_mapping_value_override() {
    use conductor_core::dispatch::SimulateOptions;
    let manager = create_simulate_manager(create_simulate_test_config()).await;

    manager.event_monitor_active.store(true, Ordering::Relaxed);

    let mut rx = manager.event_broadcast_tx.subscribe();

    let result = manager
        .simulate_mapping(SimulateOptions {
            mode: "Mix".to_string(),
            index: 0,
            execute: false,
            value: Some(42),
        })
        .await;

    assert!(result.is_ok());

    let event = rx.try_recv().unwrap();
    let payload = event.payload.unwrap();
    // Value override should be reflected in trigger.value
    assert_eq!(payload["trigger"]["value"], 42);
}

// =========================================================================
// ADR-026 Phase 3.D.1 hotfix (#945) — IPC ExecuteMcpTool path for
// conductor_probe_device_identity must not deadlock
// =========================================================================
//
// Pre-fix: handle_ipc_request would call tool_executor.execute() which
// routes the probe through `state_refs.command_tx.send(DaemonCommand::
// ProbeDeviceIdentity)`. handle_ipc_request itself runs **inside the
// command_rx select arm**, so the new command sits in the mpsc buffer
// unprocessed → executor's oneshot await never resolves → 30 s timeout
// → IPC client times out at 5 s → Broken pipe cascade.
//
// The fix mirrors the existing `conductor_switch_profile` direct-handler
// workaround: short-circuit the IPC dispatch at the top of the
// `ExecuteMcpTool` arm and call `run_probe_device_identity` directly,
// bypassing the tool_executor + command_tx round-trip.
//
// These tests exercise the handler synchronously (no command loop
// running). The deadlock would manifest as an indefinite hang, not a
// wrong return value, so we wrap each call in `tokio::time::timeout`
// to fail loudly on regression rather than hang the whole test binary.
//
// Linux ignore: `EngineManager::new` transitively constructs an
// `ActionExecutor` which calls `Enigo::new()`. That panics on
// headless Linux without a display server (project-wide constraint
// — every test that touches `EngineManager::new` carries the same
// ignore; see `test_ipc_stop_sends_shutdown_command`,
// `test_state_transitions`, etc.). The deadlock pattern itself is
// pure async control flow, identical across platforms; macOS CI
// catches the regression. Linux CI gets implicit coverage of the
// surrounding control flow via every non-Enigo-touching test that
// exercises handle_ipc_request. Removing the ignore would break
// ubuntu-latest CI without adding regression coverage we don't
// already have on macOS.

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module-level rationale above
async fn ipc_execute_mcp_tool_probe_returns_no_paired_output_without_deadlock() {
    // Fresh EngineManager — no InputManager attached, empty
    // device_output_map. The probe path resolves to
    // `Err(ProbeStartError::NoPairedOutput)` synchronously inside
    // run_probe_device_identity; the wire format wraps that as
    // `{status: "NoPairedOutput", port_name: ...}` inside the
    // HardwareIoConfirmation envelope. The test assertion that
    // matters most is *that the call returns at all* — pre-fix
    // it would hang waiting for the unprocessed DaemonCommand.
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _) = broadcast::channel(1);

    let mut manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    let request = crate::daemon::types::IpcRequest {
        id: "test-probe-deadlock".to_string(),
        command: IpcCommand::ExecuteMcpTool,
        args: serde_json::json!({
            "tool_name": "conductor_probe_device_identity",
            "arguments": { "port_name": "fake-port" },
        }),
    };

    // Bound the wait — pre-fix this call hangs indefinitely; the
    // 5 s cap surfaces the deadlock as a clean test failure rather
    // than a hung test binary.
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        manager.handle_ipc_request(request, None),
    )
    .await
    .expect("handle_ipc_request must return within 5s — pre-fix deadlock regression");

    assert!(matches!(response.status, ResponseStatus::Success));
    let data = response.data.expect("ExecuteMcpTool response carries data");

    // Outer envelope: ExecutionResult::HardwareIoConfirmation. The
    // `extract_probe_outcome_from_execution_result` GUI helper
    // (Phase 3.D.1) parses exactly this shape — wire-format
    // compatibility with the merged GUI code is mandatory.
    assert_eq!(data["type"], "HardwareIoConfirmation");
    assert_eq!(data["tool_name"], "conductor_probe_device_identity");

    // Inner ConfirmationStatus::Confirmed { result: <stringified ProbeOutcomeWire JSON> }
    let inner = &data["status"];
    assert_eq!(inner["status"], "confirmed");
    let result_str = inner["result"]
        .as_str()
        .expect("Confirmed.result is a JSON-encoded string");
    let probe_outcome: serde_json::Value =
        serde_json::from_str(result_str).expect("inner result must be valid JSON");

    // The probe target ("fake-port") has no paired output in this
    // bare-EngineManager setup → NoPairedOutput.
    assert_eq!(probe_outcome["status"], "NoPairedOutput");
    assert_eq!(probe_outcome["port_name"], "fake-port");
}

#[cfg(feature = "llm-executor")]
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo panics on headless Linux — see module-level rationale above
async fn ipc_execute_mcp_tool_probe_missing_port_name_returns_error() {
    // Missing required argument → ExecutionResult::Error envelope
    // with the same `Missing required argument: port_name`
    // message the executor would have produced. Same wire shape
    // the GUI's extract helper handles via its `"Error"` branch.
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _) = broadcast::channel(1);

    let mut manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    let request = crate::daemon::types::IpcRequest {
        id: "test-probe-missing-arg".to_string(),
        command: IpcCommand::ExecuteMcpTool,
        args: serde_json::json!({
            "tool_name": "conductor_probe_device_identity",
            "arguments": {},
        }),
    };

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        manager.handle_ipc_request(request, None),
    )
    .await
    .expect("must return within 5 s");

    assert!(matches!(response.status, ResponseStatus::Success));
    let data = response.data.unwrap();
    assert_eq!(data["type"], "Error");
    assert!(
        data["message"]
            .as_str()
            .is_some_and(|m| m.contains("port_name")),
        "missing-port_name error message must mention the field; got: {}",
        data["message"]
    );
}

// ─── ADR-032 P4 — `IpcCommand::SetUiMode` + `ui_mode` in Status (#1089) ───

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new triggers Enigo init which panics on headless Linux (#1096 round 2)
async fn status_omits_ui_mode_when_unset() {
    // Default: GUI hasn't reported. Status response must NOT carry the
    // `ui_mode` key (so consumers without a connected GUI see no shape
    // change — ADR D7 contract).
    let mut manager = make_engine_for_ui_mode_test();
    let response = manager
        .handle_ipc_request(
            crate::daemon::types::IpcRequest {
                id: "t-status-no-ui-mode".to_string(),
                command: IpcCommand::Status,
                args: serde_json::Value::Null,
            },
            None,
        )
        .await;
    assert!(matches!(response.status, ResponseStatus::Success));
    let data = response.data.expect("Status returns data");
    assert!(
        data.get("ui_mode").is_none(),
        "ui_mode must be omitted when unset; got: {:?}",
        data.get("ui_mode")
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn set_ui_mode_then_status_includes_ui_mode() {
    let mut manager = make_engine_for_ui_mode_test();
    // Publish "llm"
    let set_resp = manager
        .handle_ipc_request(
            crate::daemon::types::IpcRequest {
                id: "t-set-llm".to_string(),
                command: IpcCommand::SetUiMode,
                args: serde_json::json!({ "mode": "llm" }),
            },
            None,
        )
        .await;
    assert!(matches!(set_resp.status, ResponseStatus::Success));
    // Subsequent Status should include "ui_mode": "llm"
    let status = manager
        .handle_ipc_request(
            crate::daemon::types::IpcRequest {
                id: "t-status-llm".to_string(),
                command: IpcCommand::Status,
                args: serde_json::Value::Null,
            },
            None,
        )
        .await;
    assert_eq!(status.data.unwrap()["ui_mode"], serde_json::json!("llm"));
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn set_ui_mode_studio_round_trips() {
    let mut manager = make_engine_for_ui_mode_test();
    manager
        .handle_ipc_request(
            crate::daemon::types::IpcRequest {
                id: "t-set-studio".to_string(),
                command: IpcCommand::SetUiMode,
                args: serde_json::json!({ "mode": "studio" }),
            },
            None,
        )
        .await;
    let status = manager
        .handle_ipc_request(
            crate::daemon::types::IpcRequest {
                id: "t-status-studio".to_string(),
                command: IpcCommand::Status,
                args: serde_json::Value::Null,
            },
            None,
        )
        .await;
    assert_eq!(status.data.unwrap()["ui_mode"], serde_json::json!("studio"));
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn set_ui_mode_rejects_invalid_value() {
    let mut manager = make_engine_for_ui_mode_test();
    let resp = manager
        .handle_ipc_request(
            crate::daemon::types::IpcRequest {
                id: "t-set-invalid".to_string(),
                command: IpcCommand::SetUiMode,
                args: serde_json::json!({ "mode": "spreadsheet" }),
            },
            None,
        )
        .await;
    assert!(matches!(resp.status, ResponseStatus::Error));
    assert!(
        resp.error.unwrap().message.contains("Invalid ui_mode"),
        "rejection message must mention the field"
    );
    // State must remain unchanged → Status still omits ui_mode
    let status = manager
        .handle_ipc_request(
            crate::daemon::types::IpcRequest {
                id: "t-status-after-invalid".to_string(),
                command: IpcCommand::Status,
                args: serde_json::Value::Null,
            },
            None,
        )
        .await;
    assert!(status.data.unwrap().get("ui_mode").is_none());
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn set_ui_mode_rejects_missing_mode_argument() {
    let mut manager = make_engine_for_ui_mode_test();
    let resp = manager
        .handle_ipc_request(
            crate::daemon::types::IpcRequest {
                id: "t-set-missing".to_string(),
                command: IpcCommand::SetUiMode,
                args: serde_json::json!({}),
            },
            None,
        )
        .await;
    assert!(matches!(resp.status, ResponseStatus::Error));
    assert!(
        resp.error
            .unwrap()
            .message
            .contains("Missing required argument"),
        "missing-mode error must mention the missing argument"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn set_ui_mode_can_switch_back_and_forth() {
    let mut manager = make_engine_for_ui_mode_test();
    for mode in &["llm", "studio", "llm", "studio"] {
        manager
            .handle_ipc_request(
                crate::daemon::types::IpcRequest {
                    id: format!("t-toggle-{}", mode),
                    command: IpcCommand::SetUiMode,
                    args: serde_json::json!({ "mode": *mode }),
                },
                None,
            )
            .await;
        let status = manager
            .handle_ipc_request(
                crate::daemon::types::IpcRequest {
                    id: format!("t-status-{}", mode),
                    command: IpcCommand::Status,
                    args: serde_json::Value::Null,
                },
                None,
            )
            .await;
        assert_eq!(
            status.data.unwrap()["ui_mode"],
            serde_json::json!(*mode),
            "ui_mode must reflect the latest published value"
        );
    }
}
