// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// =========================================================================
// simulate_mapping tests (ADR-014 Phase 5A)
// =========================================================================

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_mapping_mode_not_found() {
    use conductor_core::dispatch::{SimulateError, SimulateOptions};
    let manager = create_simulate_manager(create_simulate_test_config()).await;

    let result = manager
        .simulate_mapping(SimulateOptions {
            mode: "NonExistent".to_string(),
            index: 0,
            execute: false,
            value: None,
        })
        .await;

    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err(),
        SimulateError::ModeNotFound("NonExistent".to_string())
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_mapping_index_out_of_bounds() {
    use conductor_core::dispatch::{SimulateError, SimulateOptions};
    let manager = create_simulate_manager(create_simulate_test_config()).await;

    let result = manager
        .simulate_mapping(SimulateOptions {
            mode: "Mix".to_string(),
            index: 99,
            execute: false,
            value: None,
        })
        .await;

    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err(),
        SimulateError::MappingIndexOutOfBounds {
            mode: "Mix".to_string(),
            index: 99,
            count: 2,
        }
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_mapping_dry_run() {
    use conductor_core::dispatch::SimulateOptions;
    let manager = create_simulate_manager(create_simulate_test_config()).await;

    let result = manager
        .simulate_mapping(SimulateOptions {
            mode: "Mix".to_string(),
            index: 0,
            execute: false,
            value: None,
        })
        .await;

    assert!(result.is_ok());
    let sim = result.unwrap();
    assert_eq!(sim.mode, "Mix");
    assert_eq!(sim.index, 0);
    assert_eq!(sim.action_summary, "Cmd+C");
    assert!(!sim.executed);
    assert!(sim.outcome.is_none());
    assert!(sim.error.is_none());
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_mapping_execute_keystroke() {
    use conductor_core::dispatch::SimulateOptions;
    let manager = create_simulate_manager(create_simulate_test_config()).await;

    let result = manager
        .simulate_mapping(SimulateOptions {
            mode: "Mix".to_string(),
            index: 0,
            execute: true,
            value: None,
        })
        .await;

    assert!(result.is_ok());
    let sim = result.unwrap();
    // ADR-015: executed=false because action is dispatched asynchronously (not yet completed)
    assert!(!sim.executed);
    assert_eq!(sim.action_summary, "Cmd+C");
    // Outcome indicates the action was dispatched to the executor thread
    assert!(
        sim.outcome
            .as_ref()
            .is_some_and(|o| o.starts_with("dispatched:")),
        "Expected dispatched outcome, got {:?}",
        sim.outcome
    );
}

// =========================================================================
// SimulateMapping IPC trust gate (ADR-027 §D1 Layer 2)
//
// `execute: true` dispatches the mapped action for real, so the IPC
// handler requires a verified peer (GuiTrusted / CliTrusted). The
// dry-run form stays open to every local caller. These tests drive
// `handle_ipc_request` (the only external entry to
// `simulate_mapping`) rather than the inner method, because the gate
// lives in the handler.
// =========================================================================

fn simulate_ipc_request(id: &str, execute: bool) -> crate::daemon::types::IpcRequest {
    crate::daemon::types::IpcRequest {
        id: id.to_string(),
        command: IpcCommand::SimulateMapping,
        args: serde_json::json!({
            "mode_name": "Mix",
            "mapping_index": 0,
            "execute": execute,
        }),
    }
}

/// An unpinned peer (`None` ctx — pinning failed at accept) must be
/// denied `execute: true` with PermissionDenied. Unlike break-glass
/// RollbackConfigForce, an unverified peer gets no benefit of the
/// doubt here: without an identity there is no basis to grant
/// execution.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_execute_denied_for_unpinned_peer() {
    let mut manager = create_simulate_manager(create_simulate_test_config()).await;
    let response = manager
        .handle_ipc_request(simulate_ipc_request("sim-gate-1", true), None)
        .await;
    assert!(matches!(response.status, ResponseStatus::Error));
    let err = response.error.expect("error details");
    assert_eq!(
        err.code,
        crate::daemon::error::IpcErrorCode::PermissionDenied.as_u16()
    );
    assert!(
        err.message.contains("execute"),
        "denial should name the gated flag; got: {}",
        err.message
    );
}

/// An Untrusted pinned peer must be denied the same way.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_execute_denied_for_untrusted_peer() {
    let mut manager = create_simulate_manager(create_simulate_test_config()).await;
    let ctx = crate::security::CallerContext::new(crate::security::TrustLevel::Untrusted);
    let response = manager
        .handle_ipc_request(simulate_ipc_request("sim-gate-2", true), Some(ctx))
        .await;
    assert!(matches!(response.status, ResponseStatus::Error));
    let err = response.error.expect("error details");
    assert_eq!(
        err.code,
        crate::daemon::error::IpcErrorCode::PermissionDenied.as_u16()
    );
}

/// A CliTrusted peer proceeds: the action is dispatched exactly as
/// before the gate landed.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_execute_allowed_for_cli_peer() {
    let mut manager = create_simulate_manager(create_simulate_test_config()).await;
    let ctx = crate::security::CallerContext::new(crate::security::TrustLevel::CliTrusted);
    let response = manager
        .handle_ipc_request(simulate_ipc_request("sim-gate-3", true), Some(ctx))
        .await;
    assert!(
        matches!(response.status, ResponseStatus::Success),
        "CliTrusted execute should succeed; got error: {:?}",
        response.error
    );
    let data = response.data.expect("data");
    assert!(
        data["outcome"]
            .as_str()
            .is_some_and(|o| o.starts_with("dispatched:")),
        "Expected dispatched outcome, got {:?}",
        data["outcome"]
    );
}

/// A GuiTrusted peer proceeds too — the trusted client is the
/// confirmation surface for its own explicit simulate action.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_execute_allowed_for_gui_peer() {
    let mut manager = create_simulate_manager(create_simulate_test_config()).await;
    let ctx = crate::security::CallerContext::new(crate::security::TrustLevel::GuiTrusted);
    let response = manager
        .handle_ipc_request(simulate_ipc_request("sim-gate-4", true), Some(ctx))
        .await;
    assert!(
        matches!(response.status, ResponseStatus::Success),
        "GuiTrusted execute should succeed; got error: {:?}",
        response.error
    );
}

/// Dry-run simulation is a read-only trace and stays open to every
/// local caller, pinned or not.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_dry_run_open_to_unpinned_peer() {
    let mut manager = create_simulate_manager(create_simulate_test_config()).await;
    let response = manager
        .handle_ipc_request(simulate_ipc_request("sim-gate-5", false), None)
        .await;
    assert!(
        matches!(response.status, ResponseStatus::Success),
        "dry-run from an unpinned peer should succeed; got error: {:?}",
        response.error
    );
    let data = response.data.expect("data");
    assert_eq!(data["executed"], false);
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_mapping_mode_change() {
    use conductor_core::dispatch::SimulateOptions;
    let manager = create_simulate_manager(create_simulate_test_config()).await;

    // Edit mode, mapping 0: ModeChange to "Mix"
    let result = manager
        .simulate_mapping(SimulateOptions {
            mode: "Edit".to_string(),
            index: 0,
            execute: true,
            value: None,
        })
        .await;

    assert!(result.is_ok());
    let sim = result.unwrap();
    // ADR-015: executed=false because action is dispatched asynchronously (not yet completed)
    assert!(!sim.executed);
    assert_eq!(sim.action_summary, "Switch to Mix");
    // ADR-015: simulate_mapping dispatches asynchronously — outcome is "dispatched:N"
    assert!(
        sim.outcome
            .as_ref()
            .is_some_and(|o| o.starts_with("dispatched:")),
        "Expected dispatched outcome, got {:?}",
        sim.outcome
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_mapping_global() {
    use conductor_core::dispatch::SimulateOptions;
    let manager = create_simulate_manager(create_simulate_test_config()).await;

    let result = manager
        .simulate_mapping(SimulateOptions {
            mode: "__global__".to_string(),
            index: 0,
            execute: false,
            value: None,
        })
        .await;

    assert!(result.is_ok());
    let sim = result.unwrap();
    assert_eq!(sim.mode, "__global__");
    assert_eq!(sim.index, 0);
    assert_eq!(sim.action_summary, "Cmd+A");
    assert!(!sim.executed);
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_mapping_global_index_out_of_bounds() {
    use conductor_core::dispatch::{SimulateError, SimulateOptions};
    let manager = create_simulate_manager(create_simulate_test_config()).await;

    let result = manager
        .simulate_mapping(SimulateOptions {
            mode: "__global__".to_string(),
            index: 5,
            execute: false,
            value: None,
        })
        .await;

    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err(),
        SimulateError::MappingIndexOutOfBounds {
            mode: "__global__".to_string(),
            index: 5,
            count: 1,
        }
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_simulate_mapping_emits_monitor_event() {
    use conductor_core::dispatch::SimulateOptions;
    let manager = create_simulate_manager(create_simulate_test_config()).await;

    // Enable event monitoring so mapping_fired events are emitted
    manager.event_monitor_active.store(true, Ordering::Relaxed);

    // Subscribe to broadcast channel before simulating
    let mut rx = manager.event_broadcast_tx.subscribe();

    let result = manager
        .simulate_mapping(SimulateOptions {
            mode: "Mix".to_string(),
            index: 0,
            execute: false,
            value: None,
        })
        .await;

    assert!(result.is_ok());

    // Should have received a mapping_fired event via push_monitor_event
    let event = rx.try_recv();
    assert!(event.is_ok(), "Expected a broadcast event");
    let event = event.unwrap();
    assert_eq!(event.event_type, "mapping_fired");
    // Detail is human-readable summary
    assert!(event.detail.is_some());
    let detail = event.detail.unwrap();
    assert!(detail.contains("Cmd+C"));
    // Payload is structured JSON (matching real firing path)
    assert!(event.payload.is_some());
    let payload = event.payload.unwrap();
    assert_eq!(payload["action"]["summary"], "Cmd+C");
    assert_eq!(payload["mapping_label"], "Copy shortcut");
    assert_eq!(payload["trigger"]["type"], "note");
    assert_eq!(payload["trigger"]["number"], 36);
    // trigger.value should be populated with default (100 for notes)
    assert_eq!(payload["trigger"]["value"], 100);
}

#[test]
fn test_trigger_info_from_trigger_note() {
    use conductor_core::config::types::Trigger;
    let trigger = Trigger::Note {
        note: 36,
        velocity_min: Some(1),
        channel: None,
        device: Some("Mikro".to_string()),
    };
    let info = EngineManager::trigger_info_from_trigger(&trigger);
    assert_eq!(info.trigger_type, "note");
    assert_eq!(info.number, Some(36));
    assert_eq!(info.device, Some("Mikro".to_string()));
}

#[test]
fn test_trigger_info_from_trigger_cc() {
    use conductor_core::config::types::Trigger;
    let trigger = Trigger::CC {
        cc: 7,
        value_min: Some(10),
        channel: None,
        device: None,
    };
    let info = EngineManager::trigger_info_from_trigger(&trigger);
    assert_eq!(info.trigger_type, "cc");
    assert_eq!(info.number, Some(7));
    assert!(info.device.is_none());
}

#[test]
fn test_trigger_info_from_trigger_gamepad_button() {
    use conductor_core::config::types::Trigger;
    let trigger = Trigger::GamepadButton {
        button: 128,
        velocity_min: None,
        device: None,
    };
    let info = EngineManager::trigger_info_from_trigger(&trigger);
    assert_eq!(info.trigger_type, "gamepad_button");
    assert_eq!(info.number, Some(128));
}

#[test]
fn test_default_value_for_note() {
    let info = FiredTriggerInfo {
        trigger_type: "note".to_string(),
        device: None,
        channel: None,
        number: Some(36),
        value: None,
    };
    assert_eq!(EngineManager::default_value_for(&info), Some(100));
}

#[test]
fn test_default_value_for_cc() {
    let info = FiredTriggerInfo {
        trigger_type: "cc".to_string(),
        device: None,
        channel: None,
        number: Some(7),
        value: None,
    };
    assert_eq!(EngineManager::default_value_for(&info), Some(64));
}

#[test]
fn test_default_value_for_pitch_bend() {
    let info = FiredTriggerInfo {
        trigger_type: "pitch_bend".to_string(),
        device: None,
        channel: None,
        number: None,
        value: None,
    };
    // pitch_bend returns None — simulate_mapping uses 14-bit center (8192) directly
    assert_eq!(EngineManager::default_value_for(&info), None);
}

#[test]
fn test_default_value_for_unknown() {
    let info = FiredTriggerInfo {
        trigger_type: "chord".to_string(),
        device: None,
        channel: None,
        number: None,
        value: None,
    };
    assert_eq!(EngineManager::default_value_for(&info), None);
}

#[test]
fn test_default_value_for_poly_aftertouch() {
    // simulate_mapping must produce a usable default
    // pressure for poly-aftertouch triggers, mirroring channel aftertouch.
    let info = FiredTriggerInfo {
        trigger_type: "poly_aftertouch".to_string(),
        device: None,
        channel: None,
        number: Some(60),
        value: None,
    };
    assert_eq!(EngineManager::default_value_for(&info), Some(64));
}

#[test]
fn test_synthesize_midi_bytes_note() {
    use conductor_core::config::types::Trigger;
    let trigger = Trigger::Note {
        note: 60,
        velocity_min: None,
        channel: None,
        device: None,
    };
    let bytes = EngineManager::synthesize_midi_bytes(&trigger, Some(100));
    assert_eq!(bytes, Some(vec![0x90, 60, 100]));
}

#[test]
fn test_synthesize_midi_bytes_cc() {
    use conductor_core::config::types::Trigger;
    let trigger = Trigger::CC {
        cc: 7,
        value_min: None,
        channel: None,
        device: None,
    };
    let bytes = EngineManager::synthesize_midi_bytes(&trigger, Some(64));
    assert_eq!(bytes, Some(vec![0xB0, 7, 64]));
}

#[test]
fn test_synthesize_midi_bytes_gamepad_returns_none() {
    use conductor_core::config::types::Trigger;
    let trigger = Trigger::GamepadButton {
        button: 128,
        velocity_min: None,
        device: None,
    };
    let bytes = EngineManager::synthesize_midi_bytes(&trigger, Some(100));
    assert!(bytes.is_none());
}
