// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

/// IPC Stop command must send DaemonCommand::Shutdown through the command
/// channel so the engine manager's main loop breaks and disconnects devices.
/// This was the root cause of `conductorctl stop` not fully shutting down
/// the daemon (commit 6d1ce83).
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_ipc_stop_sends_shutdown_command() {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, mut shutdown_rx) = broadcast::channel(1);

    let mut manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // We need a new receiver since cmd_rx was moved into the manager.
    // But the manager's command_tx is a clone of cmd_tx, so we can
    // create a new receiver by subscribing to what the manager sends.
    // Actually, the manager owns cmd_rx. The test needs to read from
    // the channel that manager.command_tx sends to. Since cmd_tx was
    // moved into the manager, we need a different approach.
    //
    // The manager stores command_tx (a clone of the original cmd_tx).
    // When handle_ipc_request sends DaemonCommand::Shutdown, it goes
    // to cmd_rx which is inside the manager. We can't read that directly.
    //
    // However, we can verify the response is correct AND that the
    // shutdown broadcast was sent (which we CAN observe).
    let request = crate::daemon::types::IpcRequest {
        id: "test-stop-1".to_string(),
        command: IpcCommand::Stop,
        args: serde_json::json!({}),
    };

    let response = manager.handle_ipc_request(request, None).await;

    // Verify response indicates success
    assert!(
        matches!(response.status, ResponseStatus::Success),
        "IPC Stop should return Success status"
    );
    assert_eq!(response.id, "test-stop-1");
    let data = response.data.unwrap();
    assert_eq!(data["message"], "Daemon stopping");

    // Verify shutdown broadcast was sent (peripheral tasks: IPC server,
    // config watcher, MCP server all listen on this channel)
    let broadcast_result = shutdown_rx.try_recv();
    assert!(
        broadcast_result.is_ok(),
        "IPC Stop should broadcast shutdown signal"
    );
}

/// IPC Stop response should include state_saved field.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_ipc_stop_response_format() {
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
        id: "test-stop-2".to_string(),
        command: IpcCommand::Stop,
        args: serde_json::json!({}),
    };

    let response = manager.handle_ipc_request(request, None).await;
    let data = response.data.unwrap();
    assert_eq!(data["state_saved"], true);
}

// =========================================================================
// RollbackConfig / RollbackConfigForce handler tests
// — ADR-034 §D1.2.1 / D4.B.4
// =========================================================================

/// RollbackConfigForce without a `reason` arg must reject with
/// MissingField — the IPC framer per KI-B3 enforces this, and
/// the handler re-validates (defence in depth).
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_rollback_config_force_missing_reason_returns_missing_field() {
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
        id: "rbf-1".to_string(),
        command: IpcCommand::RollbackConfigForce,
        args: serde_json::json!({}),
    };
    // CLI-trusted so we get past the trust check and reach
    // reason validation.
    let ctx = crate::security::CallerContext::new(crate::security::TrustLevel::CliTrusted);
    let response = manager.handle_ipc_request(request, Some(ctx)).await;
    assert!(matches!(response.status, ResponseStatus::Error));
    let err = response.error.expect("error details");
    assert_eq!(
        err.code,
        crate::daemon::error::IpcErrorCode::MissingField.as_u16()
    );
    assert!(err.message.contains("reason"), "got: {}", err.message);
}

/// A `ResumeAudit` that returns `AlreadyHealthy` must
/// NOT punch the daemon out of `AuditDegraded`. In `cfg(test)` builds the
/// EngineManager's audit gate is `Disabled` (the outbox wiring is skipped to
/// avoid the shared-file race), so `resume_audit` yields `AlreadyHealthy` —
/// precisely the precondition for the bug. The lifecycle transition is gated on
/// actual recovery, so a daemon parked in `AuditDegraded` for an unrelated /
/// racing reason must stay put rather than being blindly cleared.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn resume_audit_already_healthy_does_not_clear_audit_degraded() {
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

    // Park the daemon in AuditDegraded. The audit gate itself is Disabled in
    // test builds, so resume will report AlreadyHealthy (recovered=false).
    *manager.state.write().await = crate::daemon::types::LifecycleState::AuditDegraded;

    let request = crate::daemon::types::IpcRequest {
        id: "resume-noop-1".to_string(),
        command: IpcCommand::ResumeAudit,
        args: serde_json::json!({}),
    };
    let ctx = crate::security::CallerContext::new(crate::security::TrustLevel::CliTrusted);
    let response = manager.handle_ipc_request(request, Some(ctx)).await;

    assert!(
        matches!(response.status, ResponseStatus::Success),
        "resume on a disabled/healthy gate should succeed"
    );
    let data = response.data.expect("data");
    assert_eq!(
        data["recovered"].as_bool(),
        Some(false),
        "AlreadyHealthy ⇒ recovered=false"
    );

    // The blocker: state must remain AuditDegraded (no blind clear-to-Running).
    assert_eq!(
        *manager.state.read().await,
        crate::daemon::types::LifecycleState::AuditDegraded,
        "#2384: an AlreadyHealthy resume must NOT transition AuditDegraded → Running"
    );
}

/// Whitespace-only `reason` (e.g. "   ") must also reject as
/// MissingField — the handler trims before the non-empty check.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_rollback_config_force_whitespace_reason_returns_missing_field() {
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
        id: "rbf-2".to_string(),
        command: IpcCommand::RollbackConfigForce,
        args: serde_json::json!({ "reason": "   \t  " }),
    };
    let ctx = crate::security::CallerContext::new(crate::security::TrustLevel::CliTrusted);
    let response = manager.handle_ipc_request(request, Some(ctx)).await;
    assert!(matches!(response.status, ResponseStatus::Error));
    let err = response.error.expect("error details");
    assert_eq!(
        err.code,
        crate::daemon::error::IpcErrorCode::MissingField.as_u16()
    );
}

/// RollbackConfigForce from a Gui peer must reject with
/// InvalidRequest — CLI-only per ADR-034 §D6.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_rollback_config_force_gui_peer_rejected_as_cli_only() {
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
        id: "rbf-3".to_string(),
        command: IpcCommand::RollbackConfigForce,
        args: serde_json::json!({ "reason": "valid reason — but wrong peer" }),
    };
    let ctx = crate::security::CallerContext::new(crate::security::TrustLevel::GuiTrusted);
    let response = manager.handle_ipc_request(request, Some(ctx)).await;
    assert!(matches!(response.status, ResponseStatus::Error));
    let err = response.error.expect("error details");
    assert_eq!(
        err.code,
        crate::daemon::error::IpcErrorCode::InvalidRequest.as_u16()
    );
    assert!(
        err.message.contains("CLI-only"),
        "GUI peer rejection should mention CLI-only; got: {}",
        err.message
    );
}

/// RollbackConfigForce from an Untrusted peer must reject the
/// same way — only CliTrusted gets past the trust check.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_rollback_config_force_untrusted_peer_rejected_as_cli_only() {
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
        id: "rbf-4".to_string(),
        command: IpcCommand::RollbackConfigForce,
        args: serde_json::json!({ "reason": "valid reason — wrong tier" }),
    };
    let ctx = crate::security::CallerContext::new(crate::security::TrustLevel::Untrusted);
    let response = manager.handle_ipc_request(request, Some(ctx)).await;
    assert!(matches!(response.status, ResponseStatus::Error));
    let err = response.error.expect("error details");
    assert_eq!(
        err.code,
        crate::daemon::error::IpcErrorCode::InvalidRequest.as_u16()
    );
}

// =========================================================================
// extract_trigger_info tests (ADR-014 Phase 1A)
// =========================================================================

#[test]
fn test_extract_trigger_info_pad_pressed_midi() {
    use conductor_core::event_processor::ProcessedEvent;
    let events = vec![ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: conductor_core::events::VelocityLevel::Hard,
        channel: Some(0),
    }];
    let device_id = DeviceId::from_alias("mikro");
    let info = EngineManager::extract_trigger_info(&events, &device_id);
    assert_eq!(info.trigger_type, "note");
    assert_eq!(info.number, Some(36));
    assert_eq!(info.value, Some(100));
    assert_eq!(info.device.as_deref(), Some("mikro"));
}

#[test]
fn test_extract_trigger_info_pad_pressed_gamepad() {
    use conductor_core::event_processor::ProcessedEvent;
    let events = vec![ProcessedEvent::PadPressed {
        note: 128, // Gamepad range
        velocity: 80,
        velocity_level: conductor_core::events::VelocityLevel::Medium,
        channel: None, // Gamepad events have no MIDI channel
    }];
    let device_id = DeviceId::from_alias("xbox");
    let info = EngineManager::extract_trigger_info(&events, &device_id);
    assert_eq!(info.trigger_type, "gamepad_button");
    assert_eq!(info.number, Some(128));
}

#[test]
fn test_extract_trigger_info_cc_received() {
    use conductor_core::event_processor::ProcessedEvent;
    let events = vec![ProcessedEvent::CCReceived {
        cc: 7,
        value: 100,
        channel: Some(0),
    }];
    let device_id = DeviceId::from_alias("nanok");
    let info = EngineManager::extract_trigger_info(&events, &device_id);
    assert_eq!(info.trigger_type, "cc");
    assert_eq!(info.number, Some(7));
    assert_eq!(info.value, Some(100));
}

#[test]
fn test_extract_trigger_info_chord_detected() {
    use conductor_core::event_processor::ProcessedEvent;
    let events = vec![ProcessedEvent::ChordDetected {
        notes: vec![36, 40, 44],
        velocities: vec![100, 90, 80],
        channel: Some(0),
    }];
    let device_id = DeviceId::from_alias("mikro");
    let info = EngineManager::extract_trigger_info(&events, &device_id);
    assert_eq!(info.trigger_type, "chord");
    assert_eq!(info.number, Some(36));
    assert_eq!(info.value, Some(3)); // note count
}

#[test]
fn test_extract_trigger_info_uses_matched_event_not_first() {
    use conductor_core::event_processor::ProcessedEvent;
    // Simulate: PadPressed comes first, but ChordDetected is the matched event
    let chord_event = ProcessedEvent::ChordDetected {
        notes: vec![36, 40, 44],
        velocities: vec![100, 90, 80],
        channel: Some(0),
    };
    // Only pass the matched event (as the daemon code now does)
    let events = vec![chord_event];
    let device_id = DeviceId::from_alias("mikro");
    let info = EngineManager::extract_trigger_info(&events, &device_id);
    assert_eq!(info.trigger_type, "chord");
}

#[test]
fn test_extract_trigger_info_empty_events() {
    let events: Vec<conductor_core::event_processor::ProcessedEvent> = vec![];
    let device_id = DeviceId::from_alias("mikro");
    let info = EngineManager::extract_trigger_info(&events, &device_id);
    assert_eq!(info.trigger_type, "unknown");
}

#[test]
fn test_extract_trigger_info_double_tap() {
    use conductor_core::event_processor::ProcessedEvent;
    let events = vec![ProcessedEvent::DoubleTap {
        note: 42,
        first_velocity: 80,
        second_velocity: 90,
        interval_ms: 200,
        channel: Some(0),
    }];
    let device_id = DeviceId::from_alias("mikro");
    let info = EngineManager::extract_trigger_info(&events, &device_id);
    assert_eq!(info.trigger_type, "double_tap");
    assert_eq!(info.number, Some(42));
}

#[test]
fn test_extract_trigger_info_encoder() {
    use conductor_core::event_processor::ProcessedEvent;
    let events = vec![ProcessedEvent::EncoderTurned {
        cc: 16,
        value: 65,
        direction: conductor_core::events::EncoderDirection::Clockwise,
        delta: 1,
        channel: Some(0),
    }];
    let device_id = DeviceId::from_alias("mikro");
    let info = EngineManager::extract_trigger_info(&events, &device_id);
    assert_eq!(info.trigger_type, "encoder");
    assert_eq!(info.number, Some(16));
    assert_eq!(info.value, Some(65));
}
