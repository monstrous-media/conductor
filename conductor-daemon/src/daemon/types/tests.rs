// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn validate_profile_path_accepts_uppercase_toml_extension() {
    // `validate_profile_path` must match the startup path's
    // case-INSENSITIVE `.toml` contract. A profile named `studio.TOML`
    // resolves at boot but was previously rejected here at runtime
    // (IPC/MCP/app-detection switch), an api-contract mismatch.
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();

    // Uppercase extension — must now be accepted.
    let upper = dir.path().join("studio.TOML");
    write!(std::fs::File::create(&upper).unwrap(), "# profile").unwrap();
    assert!(
        validate_profile_path(upper.to_str().unwrap()).is_ok(),
        "an uppercase `.TOML` extension must validate (matches startup)"
    );

    // Mixed case too.
    let mixed = dir.path().join("studio.ToMl");
    write!(std::fs::File::create(&mixed).unwrap(), "# profile").unwrap();
    assert!(validate_profile_path(mixed.to_str().unwrap()).is_ok());

    // Plain lowercase still works.
    let lower = dir.path().join("studio.toml");
    write!(std::fs::File::create(&lower).unwrap(), "# profile").unwrap();
    assert!(validate_profile_path(lower.to_str().unwrap()).is_ok());

    // A genuinely non-TOML extension is still rejected.
    let txt = dir.path().join("studio.txt");
    write!(std::fs::File::create(&txt).unwrap(), "# profile").unwrap();
    let err = validate_profile_path(txt.to_str().unwrap()).unwrap_err();
    assert!(
        err.contains(".toml"),
        "non-toml extension must be rejected, got: {err}"
    );
}

#[test]
fn test_lifecycle_state_transitions() {
    assert!(LifecycleState::Init.can_transition_to(LifecycleState::Starting));
    assert!(LifecycleState::Starting.can_transition_to(LifecycleState::Running));
    assert!(LifecycleState::Running.can_transition_to(LifecycleState::Reloading));
    assert!(LifecycleState::Running.can_transition_to(LifecycleState::Degraded));
    assert!(LifecycleState::Running.can_transition_to(LifecycleState::Stopping));
    // D4.B.3 — AwaitingConfig transitions
    assert!(LifecycleState::Init.can_transition_to(LifecycleState::AwaitingConfig));
    assert!(LifecycleState::AwaitingConfig.can_transition_to(LifecycleState::Starting));
    assert!(LifecycleState::AwaitingConfig.can_transition_to(LifecycleState::Stopping));

    // Invalid transitions
    assert!(!LifecycleState::Init.can_transition_to(LifecycleState::Running));
    assert!(!LifecycleState::Running.can_transition_to(LifecycleState::Starting));
    assert!(!LifecycleState::Stopped.can_transition_to(LifecycleState::Running));
}

// ────────────────────────────────────────────────────────────────
// ADR-034 §D4.2 / D4.B.3.B — IPC accept-list spec for AwaitingConfig.
// RESERVED: the idle mode is unreachable and the dispatch
// filter was removed, but these pin the accept-list truth table so the
// spec is preserved for an eventual reinstatement.
// ────────────────────────────────────────────────────────────────

#[test]
fn awaiting_config_accept_list_covers_spec_set() {
    // Spec §D4.2 accept-list: Init, Status, GetConfigSnapshot,
    // GetConfigBody (ReadOnly read that returns the
    // `config: null` sentinel during AwaitingConfig), ConfigDriftStatus
    // (D4.C.1), Ping (defensive aliveness probe; no side effects).
    for cmd in [
        IpcCommand::Init,
        IpcCommand::Status,
        IpcCommand::GetConfigSnapshot,
        IpcCommand::GetConfigBody,
        IpcCommand::ConfigDriftStatus,
        IpcCommand::GetConfigDiff,
        IpcCommand::Ping,
    ] {
        assert!(
            cmd.allowed_during_awaiting_config(),
            "spec accept-list command {cmd:?} must be allowed during AwaitingConfig"
        );
    }
}

#[test]
fn awaiting_config_rejects_mutation_commands() {
    // The whole point: mutating IPCs MUST reject with 5005
    // until the daemon has loaded a config. Reject-list sampling
    // covers the high-risk ConfigChange / HardwareIO tiers.
    for cmd in [
        IpcCommand::Reload,
        IpcCommand::Stop,
        IpcCommand::ApplyPlan,
        IpcCommand::RejectPlan,
        IpcCommand::ExecuteMcpTool,
        IpcCommand::SetLedScheme,
        IpcCommand::SetLedBrightness,
        IpcCommand::RollbackConfig,
        IpcCommand::RollbackConfigForce,
        // D4.C.1 — new strict-IPC mutation surface:
        // CAS-checked saves and reloads MUST also reject
        // during AwaitingConfig (there's no snapshot to
        // base_generation against).
        IpcCommand::SaveConfig,
        IpcCommand::ReloadFromDisk,
        IpcCommand::ImportConfig,
        // Overwrite writes the live config to disk; there's no live
        // config to persist before the initial load, so it must reject too.
        IpcCommand::OverwriteConfigFile,
        IpcCommand::SwitchProfile,
        IpcCommand::SwitchMode,
        IpcCommand::StartMidiLearn,
        IpcCommand::StopMidiLearn,
    ] {
        assert!(
            !cmd.allowed_during_awaiting_config(),
            "mutating command {cmd:?} MUST be rejected during AwaitingConfig"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// ADR-034 §D2 / D4.C.1 — strict IPC mutation surface
// wire-format tests. Pin SCREAMING_SNAKE_CASE so downstream
// consumers (GUI, CLI, MCP) can plug handlers without protocol
// drift across slices.
// ────────────────────────────────────────────────────────────────

#[test]
fn d4c1_save_config_command_serialises_to_save_config_screaming() {
    let request = IpcRequest {
        id: "save-1".to_string(),
        command: IpcCommand::SaveConfig,
        args: serde_json::json!({"base_generation": 7}),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(
        json.contains("\"command\":\"SAVE_CONFIG\""),
        "expected SAVE_CONFIG token, got: {json}"
    );
    assert!(json.contains("base_generation"));
}

#[test]
fn d4c1_reload_from_disk_command_serialises_screaming() {
    let request = IpcRequest {
        id: "reload-1".to_string(),
        command: IpcCommand::ReloadFromDisk,
        args: serde_json::json!({"base_generation": 7}),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(
        json.contains("\"command\":\"RELOAD_FROM_DISK\""),
        "expected RELOAD_FROM_DISK token, got: {json}"
    );
}

#[test]
fn d4c1_import_config_command_serialises_screaming() {
    let request = IpcRequest {
        id: "import-1".to_string(),
        command: IpcCommand::ImportConfig,
        args: serde_json::json!({
            "base_generation": 7,
            "path": "/abs/path/to/snapshot.toml"
        }),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(
        json.contains("\"command\":\"IMPORT_CONFIG\""),
        "expected IMPORT_CONFIG token, got: {json}"
    );
}

#[test]
fn d4c1_config_drift_status_command_serialises_screaming() {
    let request = IpcRequest {
        id: "drift-1".to_string(),
        command: IpcCommand::ConfigDriftStatus,
        args: serde_json::json!({}),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(
        json.contains("\"command\":\"CONFIG_DRIFT_STATUS\""),
        "expected CONFIG_DRIFT_STATUS token, got: {json}"
    );
}

#[test]
fn resume_audit_command_serialises_screaming() {
    // `conductorctl audit resume` → IpcCommand::ResumeAudit.
    let request = IpcRequest {
        id: "resume-1".to_string(),
        command: IpcCommand::ResumeAudit,
        args: serde_json::json!({}),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(
        json.contains("\"command\":\"RESUME_AUDIT\""),
        "expected RESUME_AUDIT token, got: {json}"
    );
}

#[test]
fn d4c1_strict_ipc_commands_round_trip_through_serde() {
    for (raw, expected_disc) in [
        (
            r#"{"id":"r1","command":"SAVE_CONFIG","args":{}}"#,
            "SaveConfig",
        ),
        (
            r#"{"id":"r2","command":"RELOAD_FROM_DISK","args":{}}"#,
            "ReloadFromDisk",
        ),
        (
            r#"{"id":"r3","command":"IMPORT_CONFIG","args":{}}"#,
            "ImportConfig",
        ),
        (
            r#"{"id":"r4","command":"CONFIG_DRIFT_STATUS","args":{}}"#,
            "ConfigDriftStatus",
        ),
        (
            r#"{"id":"r5","command":"RESUME_AUDIT","args":{}}"#,
            "ResumeAudit",
        ),
    ] {
        let req: IpcRequest =
            serde_json::from_str(raw).unwrap_or_else(|e| panic!("parse {raw}: {e}"));
        let disc = format!("{:?}", req.command);
        assert!(
            disc.starts_with(expected_disc),
            "expected {expected_disc}, got {disc}"
        );
    }
}

/// Test that Starting can transition to Degraded when device connection fails
#[test]
fn test_starting_can_transition_to_degraded() {
    // Starting → Degraded is valid when device connection fails at startup
    assert!(LifecycleState::Starting.can_transition_to(LifecycleState::Degraded));
}

/// Test that Starting can transition to Stopping for clean shutdown during startup
#[test]
fn test_starting_can_transition_to_stopping() {
    // Starting → Stopping is valid when shutdown is requested during startup
    assert!(LifecycleState::Starting.can_transition_to(LifecycleState::Stopping));
}

/// ADR-034 §D8.2 / D4.C.1: AuditDegraded transitions.
/// Running demotes on flush-failure threshold (subsequent slice
/// wires the threshold check); resumes after operator action.
/// Clean shutdown allowed from AuditDegraded directly.
#[test]
fn d4c1_audit_degraded_transitions_are_allowed() {
    assert!(
        LifecycleState::Running.can_transition_to(LifecycleState::AuditDegraded),
        "Running → AuditDegraded must be allowed (flush-failure demotion)"
    );
    assert!(
        LifecycleState::AuditDegraded.can_transition_to(LifecycleState::Running),
        "AuditDegraded → Running must be allowed (operator audit resume)"
    );
    assert!(
        LifecycleState::AuditDegraded.can_transition_to(LifecycleState::Stopping),
        "AuditDegraded → Stopping must be allowed (clean shutdown)"
    );
}

/// Negative pin: AuditDegraded is distinct from Degraded (device-
/// level). Don't accidentally allow transitions that would let
/// the daemon "heal" device problems by way of audit recovery.
#[test]
fn d4c1_audit_degraded_does_not_cross_into_device_states() {
    assert!(
        !LifecycleState::AuditDegraded.can_transition_to(LifecycleState::Degraded),
        "AuditDegraded must not transition directly to device Degraded"
    );
    assert!(
        !LifecycleState::AuditDegraded.can_transition_to(LifecycleState::Reconnecting),
        "AuditDegraded must not transition to Reconnecting"
    );
    assert!(
        !LifecycleState::Degraded.can_transition_to(LifecycleState::AuditDegraded),
        "Degraded must not promote to AuditDegraded (they're orthogonal)"
    );
}

/// Display impl must cover AuditDegraded — surfaced in IPC
/// Status responses and the menu-bar lifecycle indicator.
#[test]
fn d4c1_audit_degraded_display_is_camel_case() {
    assert_eq!(
        format!("{}", LifecycleState::AuditDegraded),
        "AuditDegraded"
    );
}

#[test]
fn test_ipc_request_serialization() {
    let request = IpcRequest {
        id: "test-123".to_string(),
        command: IpcCommand::Ping,
        args: serde_json::json!({}),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"command\":\"PING\""));
    assert!(json.contains("\"id\":\"test-123\""));
}

#[test]
fn test_ipc_response_serialization() {
    let response = IpcResponse {
        id: "test-456".to_string(),
        status: ResponseStatus::Success,
        data: Some(serde_json::json!({"message": "pong"})),
        error: None,
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"status\":\"success\""));
    assert!(!json.contains("error")); // Should be skipped when None
}

#[test]
fn test_error_entry_creation() {
    let entry = ErrorEntry::new("DeviceDisconnected", "MIDI device unplugged");
    assert_eq!(entry.kind, "DeviceDisconnected");
    assert_eq!(entry.message, "MIDI device unplugged");
    assert!(entry.timestamp > 0);
}

#[test]
fn test_reload_metrics_average_increases() {
    let mut stats = DaemonStatistics::default();

    // First reload: 10ms
    stats.update_reload_metrics(&ReloadMetrics {
        duration_ms: 10,
        modes_loaded: 1,
        mappings_loaded: 5,
        config_load_ms: 5,
        mapping_compile_ms: 3,
        swap_ms: 2,
    });
    assert_eq!(stats.avg_reload_ms, Some(10));

    // Second reload: 20ms (higher than avg)
    stats.update_reload_metrics(&ReloadMetrics {
        duration_ms: 20,
        modes_loaded: 1,
        mappings_loaded: 5,
        config_load_ms: 10,
        mapping_compile_ms: 5,
        swap_ms: 5,
    });
    // Average should increase: 10 + (20-10)/2 = 15
    assert_eq!(stats.avg_reload_ms, Some(15));
}

#[test]
fn test_reload_metrics_average_decreases() {
    let mut stats = DaemonStatistics::default();

    // First reload: 100ms
    stats.update_reload_metrics(&ReloadMetrics {
        duration_ms: 100,
        modes_loaded: 1,
        mappings_loaded: 5,
        config_load_ms: 50,
        mapping_compile_ms: 30,
        swap_ms: 20,
    });
    assert_eq!(stats.avg_reload_ms, Some(100));

    // Second reload: 10ms (lower than avg)
    stats.update_reload_metrics(&ReloadMetrics {
        duration_ms: 10,
        modes_loaded: 1,
        mappings_loaded: 5,
        config_load_ms: 5,
        mapping_compile_ms: 3,
        swap_ms: 2,
    });
    // Average should decrease: 100 - (100-10)/2 = 55
    assert_eq!(stats.avg_reload_ms, Some(55));
    // Verify it actually decreased
    assert!(
        stats.avg_reload_ms.unwrap() < 100,
        "Average should decrease when new value is lower"
    );
}

#[test]
fn test_reload_metrics_tracks_fastest_slowest() {
    let mut stats = DaemonStatistics::default();

    stats.update_reload_metrics(&ReloadMetrics {
        duration_ms: 50,
        modes_loaded: 1,
        mappings_loaded: 5,
        config_load_ms: 25,
        mapping_compile_ms: 15,
        swap_ms: 10,
    });
    assert_eq!(stats.fastest_reload_ms, Some(50));
    assert_eq!(stats.slowest_reload_ms, Some(50));

    // Faster reload
    stats.update_reload_metrics(&ReloadMetrics {
        duration_ms: 10,
        modes_loaded: 1,
        mappings_loaded: 5,
        config_load_ms: 5,
        mapping_compile_ms: 3,
        swap_ms: 2,
    });
    assert_eq!(stats.fastest_reload_ms, Some(10));
    assert_eq!(stats.slowest_reload_ms, Some(50));

    // Slower reload
    stats.update_reload_metrics(&ReloadMetrics {
        duration_ms: 200,
        modes_loaded: 1,
        mappings_loaded: 5,
        config_load_ms: 100,
        mapping_compile_ms: 60,
        swap_ms: 40,
    });
    assert_eq!(stats.fastest_reload_ms, Some(10));
    assert_eq!(stats.slowest_reload_ms, Some(200));
}

#[test]
fn test_start_midi_learn_command_serialization() {
    let request = IpcRequest {
        id: "midi-learn-1".to_string(),
        command: IpcCommand::StartMidiLearn,
        args: serde_json::json!({}),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"command\":\"START_MIDI_LEARN\""));
}

#[test]
fn test_stop_midi_learn_command_serialization() {
    let request = IpcRequest {
        id: "midi-learn-2".to_string(),
        command: IpcCommand::StopMidiLearn,
        args: serde_json::json!({}),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"command\":\"STOP_MIDI_LEARN\""));
}

#[test]
fn test_midi_learn_command_deserialization() {
    // Test deserializing START_MIDI_LEARN
    let json = r#"{"id":"test","command":"START_MIDI_LEARN","args":{}}"#;
    let request: IpcRequest = serde_json::from_str(json).unwrap();
    assert!(matches!(request.command, IpcCommand::StartMidiLearn));

    // Test deserializing STOP_MIDI_LEARN
    let json = r#"{"id":"test","command":"STOP_MIDI_LEARN","args":{}}"#;
    let request: IpcRequest = serde_json::from_str(json).unwrap();
    assert!(matches!(request.command, IpcCommand::StopMidiLearn));
}

#[test]
fn test_get_midi_learn_events_command_serialization() {
    let request = IpcRequest {
        id: "midi-events-1".to_string(),
        command: IpcCommand::GetMidiLearnEvents,
        args: serde_json::json!({}),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"command\":\"GET_MIDI_LEARN_EVENTS\""));

    // Also test deserialization
    let json = r#"{"id":"test","command":"GET_MIDI_LEARN_EVENTS","args":{}}"#;
    let request: IpcRequest = serde_json::from_str(json).unwrap();
    assert!(matches!(request.command, IpcCommand::GetMidiLearnEvents));
}

/// ADR-007 Phase 2: Test DaemonState to JSON conversion
#[test]
fn test_daemon_state_to_status_json() {
    let state = DaemonState {
        lifecycle_state: Some(LifecycleState::Running),
        device_status: Some(DeviceStatus {
            connected: true,
            name: Some("Maschine Mikro".to_string()),
            port: Some(2),
            last_event_at: Some(1234567890),
            ..Default::default()
        }),
        statistics: Some(DaemonStatistics {
            events_processed: 100,
            actions_executed: 50,
            errors_since_start: 2,
            config_reloads: 3,
            ..Default::default()
        }),
        input_mode: Some("MidiOnly".to_string()),
        hid_devices: vec![],
        uptime_secs: 3600,
        config_path: Some("/path/to/config.toml".to_string()),
        active_profile: None,
    };

    let json = state.to_status_json();
    assert_eq!(json["lifecycle_state"], "Running");
    assert_eq!(json["connected"], true);
    assert_eq!(json["uptime_secs"], 3600);
    assert_eq!(json["statistics"]["events_processed"], 100);
}

/// Test status JSON includes daemon_running when no device connected
#[test]
fn test_status_json_includes_daemon_running_field() {
    let state = DaemonState {
        lifecycle_state: Some(LifecycleState::Running),
        device_status: None, // No device connected
        statistics: None,
        input_mode: Some("MidiOnly".to_string()),
        hid_devices: vec![],
        uptime_secs: 100,
        config_path: None,
        active_profile: None,
    };

    let json = state.to_status_json();
    assert_eq!(json["daemon_running"], true);
    assert_eq!(json["connected"], false);
    assert_eq!(json["device_connected"], false);
}

/// Test status JSON daemon_running with device connected
#[test]
fn test_status_json_daemon_running_with_device() {
    let state = DaemonState {
        lifecycle_state: Some(LifecycleState::Running),
        device_status: Some(DeviceStatus {
            connected: true,
            name: Some("Maschine Mikro".to_string()),
            port: Some(2),
            last_event_at: Some(1234567890),
            ..Default::default()
        }),
        statistics: None,
        input_mode: None,
        hid_devices: vec![],
        uptime_secs: 3600,
        config_path: None,
        active_profile: None,
    };

    let json = state.to_status_json();
    assert_eq!(json["daemon_running"], true);
    assert_eq!(json["connected"], true);
    assert_eq!(json["device_connected"], true);
}

/// ADR-007 Phase 2: Test DaemonState devices JSON conversion
#[test]
fn test_daemon_state_to_devices_json() {
    let state = DaemonState {
        hid_devices: vec![json!({"id": 0, "name": "Xbox Controller"})],
        ..Default::default()
    };

    let midi_devices = vec![MidiDeviceInfo {
        port_index: 0,
        port_name: "Maschine Mikro".to_string(),
        manufacturer: Some("Native Instruments".to_string()),
        connected: true,
    }];

    let json = state.to_devices_json(midi_devices);
    assert_eq!(json["midi_devices"][0]["port_name"], "Maschine Mikro");
    assert_eq!(json["hid_devices"][0]["name"], "Xbox Controller");
}

/// ADR-007 Phase 2: Test ApplyPlan IPC command serialization
#[test]
fn test_apply_plan_command_serialization() {
    let request = IpcRequest {
        id: "plan-apply-1".to_string(),
        command: IpcCommand::ApplyPlan,
        args: serde_json::json!({"plan_id": "550e8400-e29b-41d4-a716-446655440000"}),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"command\":\"APPLY_PLAN\""));
    assert!(json.contains("plan_id"));
}

/// ADR-007 Phase 2: Test RejectPlan IPC command serialization
#[test]
fn test_reject_plan_command_serialization() {
    let request = IpcRequest {
        id: "plan-reject-1".to_string(),
        command: IpcCommand::RejectPlan,
        args: serde_json::json!({"plan_id": "550e8400-e29b-41d4-a716-446655440000"}),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"command\":\"REJECT_PLAN\""));
}

/// ADR-007 Phase 2: Test LLM IPC commands deserialization
#[test]
fn test_llm_ipc_commands_deserialization() {
    // ApplyPlan
    let json = r#"{"id":"test","command":"APPLY_PLAN","args":{"plan_id":"uuid"}}"#;
    let request: IpcRequest = serde_json::from_str(json).unwrap();
    assert!(matches!(request.command, IpcCommand::ApplyPlan));

    // RejectPlan
    let json = r#"{"id":"test","command":"REJECT_PLAN","args":{"plan_id":"uuid"}}"#;
    let request: IpcRequest = serde_json::from_str(json).unwrap();
    assert!(matches!(request.command, IpcCommand::RejectPlan));

    // ListPendingPlans
    let json = r#"{"id":"test","command":"LIST_PENDING_PLANS","args":{}}"#;
    let request: IpcRequest = serde_json::from_str(json).unwrap();
    assert!(matches!(request.command, IpcCommand::ListPendingPlans));

    // ExecuteMcpTool
    let json =
        r#"{"id":"test","command":"EXECUTE_MCP_TOOL","args":{"tool_name":"conductor_get_status"}}"#;
    let request: IpcRequest = serde_json::from_str(json).unwrap();
    assert!(matches!(request.command, IpcCommand::ExecuteMcpTool));
}

/// Test is_configured propagates correctly from DevicePortStatus to JSON (D19)
#[test]
fn test_is_configured_propagates_to_json() {
    let state = DaemonState {
        lifecycle_state: Some(LifecycleState::Running),
        device_status: Some(DeviceStatus {
            connected: true,
            devices: vec![
                DevicePortStatus {
                    device_id: "pads".to_string(),
                    port_name: "Mikro MK3 MIDI".to_string(),
                    port_index: 0,
                    connected: true,
                    enabled: true,
                    last_event_at: None,
                    is_configured: true,
                    direction: conductor_core::config::DeviceDirection::Input,
                    output_port_name: None,
                    output_connected: false,
                    output_auto_paired: false,
                    protocol: "midi".to_string(),
                },
                DevicePortStatus {
                    device_id: "Launchpad MIDI".to_string(),
                    port_name: "Launchpad MIDI".to_string(),
                    port_index: 1,
                    connected: true,
                    enabled: true,
                    last_event_at: None,
                    is_configured: false,
                    direction: conductor_core::config::DeviceDirection::Input,
                    output_port_name: None,
                    output_connected: false,
                    output_auto_paired: false,
                    protocol: "midi".to_string(),
                },
            ],
            ..Default::default()
        }),
        ..Default::default()
    };

    // Test to_status_json
    let status_json = state.to_status_json();
    let bindings = status_json["device_bindings"].as_array().unwrap();
    assert_eq!(bindings.len(), 2);
    assert_eq!(bindings[0]["device_id"], "pads");
    assert_eq!(bindings[0]["is_configured"], true);
    assert_eq!(bindings[1]["device_id"], "Launchpad MIDI");
    assert_eq!(bindings[1]["is_configured"], false);

    // Test to_devices_json
    let devices_json = state.to_devices_json(vec![]);
    let bindings = devices_json["device_bindings"].as_array().unwrap();
    assert_eq!(bindings.len(), 2);
    assert_eq!(bindings[0]["is_configured"], true);
    assert_eq!(bindings[1]["is_configured"], false);

    // Verify protocol field is serialized in JSON output
    let status_json = state.to_status_json();
    let bindings = status_json["device_bindings"].as_array().unwrap();
    assert_eq!(bindings[0]["protocol"], "midi");
    assert_eq!(bindings[1]["protocol"], "midi");
}

/// Test is_configured defaults to false for serde backward compat (D19)
#[test]
fn test_is_configured_serde_default() {
    // Old JSON without is_configured field should default to false
    let json = r#"{
        "device_id": "test",
        "port_name": "Test Port",
        "port_index": 0,
        "connected": true,
        "enabled": true,
        "last_event_at": null
    }"#;
    let status: DevicePortStatus = serde_json::from_str(json).unwrap();
    assert!(
        !status.is_configured,
        "Missing is_configured should default to false"
    );
}

/// Test protocol defaults to "midi" for serde backward compat
#[test]
fn test_protocol_serde_default() {
    let json = r#"{
        "device_id": "test",
        "port_name": "Test Port",
        "port_index": 0,
        "connected": true,
        "enabled": true,
        "last_event_at": null
    }"#;
    let status: DevicePortStatus = serde_json::from_str(json).unwrap();
    assert_eq!(
        status.protocol, "midi",
        "Missing protocol should default to \"midi\""
    );
}

/// Phase 1: Test ActiveProfileInfo serialization
#[test]
fn test_active_profile_info_serialization() {
    let info = ActiveProfileInfo {
        id: None,
        name: "Logic Pro".to_string(),
        config_path: "/Users/test/profiles/logic-pro.toml".to_string(),
    };
    let json = serde_json::to_string(&info).unwrap();
    assert!(json.contains("Logic Pro"));
    assert!(json.contains("logic-pro.toml"));

    let deserialized: ActiveProfileInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.name, "Logic Pro");
}

/// Phase 1: Test SwitchProfile IPC command serialization
#[test]
fn test_switch_profile_command_serialization() {
    let request = IpcRequest {
        id: "profile-1".to_string(),
        command: IpcCommand::SwitchProfile,
        args: serde_json::json!({
            "profile_name": "Logic Pro",
            "config_path": "/path/to/profile.toml"
        }),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"command\":\"SWITCH_PROFILE\""));
}

/// Phase 1: Test GetActiveProfile IPC command serialization
#[test]
fn test_get_active_profile_command_serialization() {
    let request = IpcRequest {
        id: "profile-2".to_string(),
        command: IpcCommand::GetActiveProfile,
        args: serde_json::json!({}),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"command\":\"GET_ACTIVE_PROFILE\""));
}

/// Phase 1: Test DaemonState with active profile in status JSON
#[test]
fn test_status_json_with_active_profile() {
    let state = DaemonState {
        lifecycle_state: Some(LifecycleState::Running),
        device_status: None,
        statistics: None,
        input_mode: None,
        hid_devices: vec![],
        uptime_secs: 100,
        config_path: None,
        active_profile: Some(ActiveProfileInfo {
            id: None,
            name: "Ableton".to_string(),
            config_path: "/profiles/ableton.toml".to_string(),
        }),
    };

    let json = state.to_status_json();
    assert_eq!(json["active_profile"]["name"], "Ableton");
    assert_eq!(
        json["active_profile"]["config_path"],
        "/profiles/ableton.toml"
    );
}

/// Phase 1: Test DaemonState without active profile (backward compat)
#[test]
fn test_status_json_without_active_profile() {
    let state = DaemonState {
        lifecycle_state: Some(LifecycleState::Running),
        device_status: None,
        statistics: None,
        input_mode: None,
        hid_devices: vec![],
        uptime_secs: 100,
        config_path: None,
        active_profile: None,
    };

    let json = state.to_status_json();
    assert!(json["active_profile"].is_null());
}

/// Phase 1: Test DaemonState active_profile defaults to None via serde
#[test]
fn test_active_profile_serde_default() {
    // Old JSON without active_profile field should default to None
    let json = r#"{
        "uptime_secs": 100,
        "hid_devices": []
    }"#;
    let state: DaemonState = serde_json::from_str(json).unwrap();
    assert!(state.active_profile.is_none());
}

#[test]
fn test_validate_plugin_name_valid() {
    assert!(validate_plugin_name("my-plugin").is_ok());
    assert!(validate_plugin_name("plugin_v2").is_ok());
    assert!(validate_plugin_name("com.example.plugin").is_ok());
    assert!(validate_plugin_name("a").is_ok());
}

#[test]
fn test_validate_plugin_name_empty() {
    assert!(validate_plugin_name("").is_err());
}

#[test]
fn test_validate_plugin_name_traversal() {
    assert!(validate_plugin_name("../etc/passwd").is_err());
    assert!(validate_plugin_name("../../secret").is_err());
    assert!(validate_plugin_name("foo/bar").is_err());
    assert!(validate_plugin_name("..").is_err());
    assert!(validate_plugin_name(".").is_err());
    assert!(validate_plugin_name("foo..bar").is_err());
}

#[test]
fn test_validate_plugin_name_special_chars() {
    assert!(validate_plugin_name("plugin name").is_err());
    assert!(validate_plugin_name("plugin;rm").is_err());
    assert!(validate_plugin_name("plugin$var").is_err());
    assert!(validate_plugin_name("plugin\0null").is_err());
}

/// Phase 2: ProfileSwitch with result_tx sends back success
#[tokio::test]
async fn test_profile_switch_result_channel_success() {
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let cmd = DaemonCommand::ProfileSwitch {
        profile_name: "Logic Pro".to_string(),
        config_path: "/profiles/logic.toml".to_string(),
        profile_id: None,
        result_tx: Some(result_tx),
    };

    // Simulate engine manager sending success
    if let DaemonCommand::ProfileSwitch {
        result_tx: Some(tx),
        profile_name,
        ..
    } = cmd
    {
        tx.send(Ok(profile_name)).unwrap();
    }

    let result = result_rx.await.unwrap();
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "Logic Pro");
}

/// Phase 2: ProfileSwitch with result_tx sends back failure
#[tokio::test]
async fn test_profile_switch_result_channel_failure() {
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let cmd = DaemonCommand::ProfileSwitch {
        profile_name: "Bad Profile".to_string(),
        config_path: "/nonexistent.toml".to_string(),
        profile_id: None,
        result_tx: Some(result_tx),
    };

    if let DaemonCommand::ProfileSwitch {
        result_tx: Some(tx),
        ..
    } = cmd
    {
        tx.send(Err("Config file not found".to_string())).unwrap();
    }

    let result = result_rx.await.unwrap();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

/// Phase 2: ProfileSwitch with None result_tx (fire-and-forget)
#[test]
fn test_profile_switch_fire_and_forget() {
    let _cmd = DaemonCommand::ProfileSwitch {
        profile_name: "Test".to_string(),
        config_path: "/test.toml".to_string(),
        profile_id: None,
        result_tx: None,
    };
    // Should compile and work without result_tx
}

// =========================================================================
// SwitchMode IPC command tests
// =========================================================================

#[test]
fn test_switch_mode_ipc_serialization() {
    let request = IpcRequest {
        id: "mode-test".to_string(),
        command: IpcCommand::SwitchMode,
        args: serde_json::json!({ "mode": "Edit" }),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"command\":\"SWITCH_MODE\""));

    // Round-trip deserialization
    let parsed: IpcRequest = serde_json::from_str(&json).unwrap();
    assert!(matches!(parsed.command, IpcCommand::SwitchMode));
    assert_eq!(parsed.args["mode"], "Edit");
}

#[test]
fn test_handshake_ipc_serialization() {
    let request = IpcRequest {
        id: "handshake-test".to_string(),
        command: IpcCommand::Handshake,
        args: serde_json::json!({ "nonce": "Zm9v" }),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("\"command\":\"HANDSHAKE\""));

    let parsed: IpcRequest = serde_json::from_str(&json).unwrap();
    assert!(matches!(parsed.command, IpcCommand::Handshake));
    assert_eq!(parsed.args["nonce"], "Zm9v");
}

// =========================================================================
// EventFilter tests
// =========================================================================

fn make_note_on(note: u8, vel: u8, channel: Option<u8>) -> MonitorEvent {
    MonitorEvent {
        timestamp_ms: 1000,
        event_type: "note_on".to_string(),
        note: Some(note),
        velocity: Some(vel),
        channel,
        ..Default::default()
    }
}

fn make_cc(cc: u8, value: u16, channel: Option<u8>) -> MonitorEvent {
    MonitorEvent {
        timestamp_ms: 1000,
        event_type: "cc".to_string(),
        cc: Some(cc),
        value: Some(value),
        channel,
        ..Default::default()
    }
}

#[test]
fn test_event_filter_empty_matches_all() {
    let filter = EventFilter::default();
    assert!(filter.matches(&make_note_on(60, 100, Some(0))));
    assert!(filter.matches(&make_cc(1, 64, Some(0))));
}

#[test]
fn test_event_filter_by_type() {
    let filter = EventFilter {
        event_type: Some("note_on".to_string()),
        ..Default::default()
    };
    assert!(filter.matches(&make_note_on(60, 100, Some(0))));
    assert!(!filter.matches(&make_cc(1, 64, Some(0))));
}

#[test]
fn test_event_filter_by_type_comma_separated() {
    let filter = EventFilter {
        event_type: Some("note_on,note_off".to_string()),
        ..Default::default()
    };
    assert!(filter.matches(&make_note_on(60, 100, None)));
    assert!(filter.matches(&MonitorEvent {
        event_type: "note_off".to_string(),
        note: Some(60),
        ..Default::default()
    }));
    assert!(!filter.matches(&make_cc(1, 64, None)));
}

#[test]
fn test_event_filter_by_channel() {
    let filter = EventFilter {
        channel: Some(0),
        ..Default::default()
    };
    assert!(filter.matches(&make_note_on(60, 100, Some(0))));
    assert!(!filter.matches(&make_note_on(60, 100, Some(1))));
    // Events without channel info pass through (forward compat)
    assert!(filter.matches(&make_note_on(60, 100, None)));
}

#[test]
fn test_event_filter_by_note_range() {
    let filter = EventFilter {
        note_min: Some(36),
        note_max: Some(51),
        ..Default::default()
    };
    assert!(filter.matches(&make_note_on(36, 100, None)));
    assert!(filter.matches(&make_note_on(51, 100, None)));
    assert!(filter.matches(&make_note_on(42, 100, None)));
    assert!(!filter.matches(&make_note_on(35, 100, None)));
    assert!(!filter.matches(&make_note_on(52, 100, None)));
    // CC events have no note — filtered out by note range
    assert!(!filter.matches(&make_cc(1, 64, None)));
}

#[test]
fn test_event_filter_by_note_min_only() {
    let filter = EventFilter {
        note_min: Some(60),
        ..Default::default()
    };
    assert!(filter.matches(&make_note_on(60, 100, None)));
    assert!(filter.matches(&make_note_on(127, 100, None)));
    assert!(!filter.matches(&make_note_on(59, 100, None)));
}

#[test]
fn test_event_filter_by_note_max_only() {
    let filter = EventFilter {
        note_max: Some(51),
        ..Default::default()
    };
    assert!(filter.matches(&make_note_on(0, 100, None)));
    assert!(filter.matches(&make_note_on(51, 100, None)));
    assert!(!filter.matches(&make_note_on(52, 100, None)));
}

#[test]
fn test_event_filter_by_device() {
    let filter = EventFilter {
        device_id: Some("launchpad-mini".to_string()),
        ..Default::default()
    };
    assert!(filter.matches(&MonitorEvent {
        event_type: "note_on".to_string(),
        device_id: Some("launchpad-mini".to_string()),
        note: Some(60),
        ..Default::default()
    }));
    assert!(!filter.matches(&MonitorEvent {
        event_type: "note_on".to_string(),
        device_id: Some("other-device".to_string()),
        note: Some(60),
        ..Default::default()
    }));
    // No device ID → filtered out
    assert!(!filter.matches(&make_note_on(60, 100, None)));
}

#[test]
fn test_event_filter_combined() {
    let filter = EventFilter {
        event_type: Some("note_on".to_string()),
        channel: Some(0),
        note_min: Some(36),
        note_max: Some(51),
        ..Default::default()
    };
    // Matches all criteria
    assert!(filter.matches(&make_note_on(42, 100, Some(0))));
    // Wrong type
    assert!(!filter.matches(&make_cc(1, 64, Some(0))));
    // Wrong channel
    assert!(!filter.matches(&make_note_on(42, 100, Some(1))));
    // Out of note range
    assert!(!filter.matches(&make_note_on(60, 100, Some(0))));
}

#[test]
fn test_event_filter_by_since() {
    let filter = EventFilter {
        since_ms: Some(5000),
        ..Default::default()
    };
    // Event at 6000ms — after since
    let mut event = make_note_on(60, 100, None);
    event.timestamp_ms = 6000;
    assert!(filter.matches(&event));

    // Event at 5000ms — exactly at since (inclusive)
    event.timestamp_ms = 5000;
    assert!(filter.matches(&event));

    // Event at 4999ms — before since
    event.timestamp_ms = 4999;
    assert!(!filter.matches(&event));
}

#[test]
fn test_event_stats_default() {
    let stats = EventStats::default();
    assert_eq!(stats.total_events, 0);
    assert_eq!(stats.events_per_second, 0.0);
    assert_eq!(stats.avg_velocity, 0.0);
    assert!(stats.most_active_note.is_none());
    assert_eq!(stats.error_count, 0);
}
