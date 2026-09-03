// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-009 Phase 2: Multi-device integration tests
//!
//! Tests for multi-device MIDI architecture: DeviceEvent channels, per-device
//! EventProcessors, port filtering, device mute, timer tick hold detection,
//! MidiLearnEvent device_id tagging, DevicePortStatus serialization, and
//! device-filtered mapping through the engine.

use conductor_core::actions::{Action, KeyCode, ModifierKey};
use conductor_core::event_processor::ProcessedEvent;
use conductor_core::events::InputEvent;
use conductor_core::identity::{DeviceEvent, DeviceId};
use conductor_core::resolver::PortInfo;
use conductor_core::{EventProcessor, EventType, MappingEngine};
use conductor_daemon::daemon::engine_manager::MidiLearnEvent;
use conductor_daemon::daemon::types::DevicePortStatus;
use conductor_daemon::input_manager::filter_ports;
use conductor_daemon::{DeviceStatus, IpcCommand, IpcRequest};
use dashmap::DashMap;
use serde_json::json;
use std::time::{Duration, Instant};

// ─── Test 1: DeviceEvent channel type ───────────────────────────────────────

#[tokio::test]
async fn test_device_event_channel_type() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DeviceEvent<InputEvent>>(16);

    let device_id = DeviceId::from_alias("pads");
    let event = InputEvent::PadPressed {
        pad: 36,
        velocity: 100,
        channel: None,
        time: Instant::now(),
    };

    let device_event = DeviceEvent::new(device_id.clone(), event);
    tx.send(device_event).await.unwrap();

    let received = rx.recv().await.unwrap();
    assert_eq!(received.device_id(), &device_id);
    assert_eq!(received.device_id().as_str(), "pads");

    match received.event() {
        InputEvent::PadPressed { pad, velocity, .. } => {
            assert_eq!(*pad, 36);
            assert_eq!(*velocity, 100);
        }
        _ => panic!("Expected PadPressed"),
    }
}

// ─── Test 2: Per-device EventProcessor isolation ────────────────────────────

#[test]
fn test_per_device_event_processor_isolation() {
    let processors: DashMap<DeviceId, EventProcessor> = DashMap::new();

    let pads_id = DeviceId::from_alias("pads");
    let keys_id = DeviceId::from_alias("keys");

    // Create separate processors with default settings
    processors.insert(pads_id.clone(), EventProcessor::new());
    processors.insert(keys_id.clone(), EventProcessor::new());

    // Send note 36 to "pads" processor
    let pads_event = InputEvent::PadPressed {
        pad: 36,
        velocity: 100,
        channel: None,
        time: Instant::now(),
    };
    let pads_results = processors
        .get_mut(&pads_id)
        .unwrap()
        .process_input(pads_event);

    // "pads" should produce results (at minimum a NoteOn)
    assert!(
        !pads_results.is_empty(),
        "pads processor should produce events"
    );

    // "keys" processor should have no state from the pads event
    let keys_event = InputEvent::PadPressed {
        pad: 60,
        velocity: 80,
        channel: None,
        time: Instant::now(),
    };
    let keys_results = processors
        .get_mut(&keys_id)
        .unwrap()
        .process_input(keys_event);

    assert!(
        !keys_results.is_empty(),
        "keys processor should produce events"
    );

    // Verify the events are independent — pads got note 36, keys got note 60
    let pads_has_36 = pads_results
        .iter()
        .any(|e| matches!(e, ProcessedEvent::PadPressed { note: 36, .. }));
    let keys_has_60 = keys_results
        .iter()
        .any(|e| matches!(e, ProcessedEvent::PadPressed { note: 60, .. }));
    assert!(pads_has_36, "pads should have note 36");
    assert!(keys_has_60, "keys should have note 60");
}

// ─── Test 3: listen_mode filtering logic (port filtering) ──────────────────

#[test]
fn test_listen_mode_all_opens_all_ports() {
    // With listen_mode=All, all ports including unmatched should be available
    // The filter_ports function handles ignore_ports and max cap, not listen_mode
    // listen_mode is handled by listen_to_all_ports() at the BindingResult level
    // Here we test that filter_ports passes through all ports when no filters
    let ports = vec![
        PortInfo::new("IAC Driver Bus 1".to_string(), 0),
        PortInfo::new("Mikro MK3".to_string(), 1),
        PortInfo::new("Launchpad Mini".to_string(), 2),
        PortInfo::new("MIDI Through".to_string(), 3),
    ];

    let result = filter_ports(ports, &[], 32);
    assert_eq!(
        result.len(),
        4,
        "All 4 ports should pass through with no filters"
    );
}

// ─── Test 4: ignore_ports filtering ─────────────────────────────────────────

#[test]
fn test_ignore_ports_filtering() {
    let ports = vec![
        PortInfo::new("IAC Driver Bus 1".to_string(), 0),
        PortInfo::new("Mikro MK3".to_string(), 1),
        PortInfo::new("MIDI Through".to_string(), 2),
    ];

    let result = filter_ports(ports, &["IAC Driver".to_string()], 32);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].name, "Mikro MK3");
    assert_eq!(result[1].name, "MIDI Through");
}

#[test]
fn test_ignore_ports_multiple_patterns() {
    let ports = vec![
        PortInfo::new("IAC Driver Bus 1".to_string(), 0),
        PortInfo::new("Mikro MK3".to_string(), 1),
        PortInfo::new("MIDI Through".to_string(), 2),
        PortInfo::new("IAC Driver Bus 2".to_string(), 3),
    ];

    let result = filter_ports(
        ports,
        &["IAC Driver".to_string(), "MIDI Through".to_string()],
        32,
    );
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "Mikro MK3");
}

// ─── Test 5: max_midi_ports cap ─────────────────────────────────────────────

#[test]
fn test_max_midi_ports_cap() {
    let ports = vec![
        PortInfo::new("Port A".to_string(), 0),
        PortInfo::new("Port B".to_string(), 1),
        PortInfo::new("Port C".to_string(), 2),
        PortInfo::new("Port D".to_string(), 3),
        PortInfo::new("Port E".to_string(), 4),
    ];

    let result = filter_ports(ports, &[], 3);
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].name, "Port A");
    assert_eq!(result[1].name, "Port B");
    assert_eq!(result[2].name, "Port C");
}

#[test]
fn test_max_midi_ports_with_ignore_applied_first() {
    // ignore_ports is applied before max cap
    let ports = vec![
        PortInfo::new("IAC Driver".to_string(), 0),
        PortInfo::new("Port A".to_string(), 1),
        PortInfo::new("Port B".to_string(), 2),
        PortInfo::new("Port C".to_string(), 3),
        PortInfo::new("Port D".to_string(), 4),
    ];

    // Ignore "IAC Driver", then cap at 3
    let result = filter_ports(ports, &["IAC Driver".to_string()], 3);
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].name, "Port A");
    assert_eq!(result[1].name, "Port B");
    assert_eq!(result[2].name, "Port C");
}

// ─── Test 6: Device mute drops events ───────────────────────────────────────

#[test]
fn test_device_mute_drops_events() {
    use conductor_daemon::InputManager;
    use conductor_daemon::input_manager::InputMode;

    let mut manager = InputManager::new(None, false, InputMode::MidiOnly);
    let pads_id = DeviceId::from_alias("pads");
    let keys_id = DeviceId::from_alias("keys");

    // Initially both enabled
    assert!(manager.is_device_enabled(&pads_id));
    assert!(manager.is_device_enabled(&keys_id));

    // Mute "pads"
    manager.set_device_enabled(&pads_id, false);

    // "pads" should be muted, "keys" should still be enabled
    assert!(!manager.is_device_enabled(&pads_id));
    assert!(manager.is_device_enabled(&keys_id));
}

// ─── Test 7: Device unmute restores events ──────────────────────────────────

#[test]
fn test_device_unmute_restores_events() {
    use conductor_daemon::InputManager;
    use conductor_daemon::input_manager::InputMode;

    let mut manager = InputManager::new(None, false, InputMode::MidiOnly);
    let device_id = DeviceId::from_alias("pads");

    // Mute
    manager.set_device_enabled(&device_id, false);
    assert!(!manager.is_device_enabled(&device_id));

    // Unmute
    manager.set_device_enabled(&device_id, true);
    assert!(manager.is_device_enabled(&device_id));
}

// ─── Test 8: Timer tick fires hold without input ────────────────────────────

#[test]
fn test_timer_tick_fires_hold_without_input() {
    let mut processor = EventProcessor::new();

    // Press a note
    let press_event = InputEvent::PadPressed {
        pad: 36,
        velocity: 100,
        channel: None,
        time: Instant::now(),
    };
    let _ = processor.process_input(press_event);

    // Negative case: with the default 2 s threshold, an immediate check must
    // NOT fire a hold (the note has been held for ~0 ms).
    let before = processor.check_holds();
    assert!(
        before.is_empty(),
        "hold must not fire immediately after press, got {before:?}"
    );

    // Positive case: drop the threshold to zero so the still-held note
    // is now past threshold, then assert check_holds() actually emits
    // HoldDetected — without this, an implementation that never emits a hold
    // would pass the negative-only test. (Deterministic: no sleeping.)
    processor.set_hold_threshold(Duration::ZERO);
    let after = processor.check_holds();

    let holds: Vec<_> = after
        .iter()
        .filter(|e| matches!(e, ProcessedEvent::HoldDetected { .. }))
        .collect();
    assert_eq!(
        holds.len(),
        1,
        "exactly one HoldDetected expected once past threshold, got {after:?}"
    );
    match holds[0] {
        ProcessedEvent::HoldDetected {
            note,
            press_velocity,
            channel,
            ..
        } => {
            assert_eq!(*note, 36, "hold must be for the pressed note");
            assert_eq!(*press_velocity, 100, "hold must carry the press velocity");
            assert_eq!(*channel, None, "hold must carry the press channel");
        }
        other => panic!("expected HoldDetected, got {other:?}"),
    }
}

// ─── Test 9: MidiLearnEvent has device_id ───────────────────────────────────

#[test]
fn test_midi_learn_event_has_device_id() {
    let event = MidiLearnEvent {
        event_type: EventType::NoteOn,
        device_id: Some("pads".to_string()),
        note: Some(36),
        velocity: Some(100),
        timestamp: 1234567890,
        ..Default::default()
    };

    assert_eq!(event.device_id, Some("pads".to_string()));
    assert_eq!(event.event_type, EventType::NoteOn);
    assert_eq!(event.note, Some(36));
}

#[test]
fn test_midi_learn_event_device_id_serialization() {
    let event = MidiLearnEvent {
        event_type: EventType::NoteOn,
        device_id: Some("pads".to_string()),
        note: Some(36),
        velocity: Some(100),
        timestamp: 1234567890,
        ..Default::default()
    };

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"device_id\":\"pads\""));

    // Deserialize back
    let parsed: MidiLearnEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.device_id, Some("pads".to_string()));
}

#[test]
fn test_midi_learn_event_no_device_id_omitted_in_json() {
    let event = MidiLearnEvent {
        event_type: EventType::NoteOn,
        device_id: None,
        note: Some(36),
        velocity: Some(100),
        ..Default::default()
    };

    let json = serde_json::to_string(&event).unwrap();
    // device_id should be omitted when None (skip_serializing_if)
    assert!(
        !json.contains("device_id"),
        "device_id should be omitted when None"
    );
}

// ─── Test 10: DeviceStatus multi-device serialization ───────────────────────

#[test]
fn test_device_status_multi_device_serialization() {
    let status = DeviceStatus {
        connected: true,
        name: Some("Mikro MK3".to_string()),
        port: Some(1),
        last_event_at: Some(1234567890),
        devices: vec![
            DevicePortStatus {
                device_id: "pads".to_string(),
                port_name: "Mikro MK3 MIDI".to_string(),
                port_index: 0,
                connected: true,
                enabled: true,
                last_event_at: Some(1234567890),
                is_configured: true,
                direction: conductor_core::config::DeviceDirection::Input,
                output_port_name: None,
                output_connected: false,
                output_auto_paired: false,
                protocol: "midi".to_string(),
            },
            DevicePortStatus {
                device_id: "keys".to_string(),
                port_name: "KeyStep Pro".to_string(),
                port_index: 1,
                connected: true,
                enabled: false,
                last_event_at: None,
                is_configured: true,
                direction: conductor_core::config::DeviceDirection::Input,
                output_port_name: None,
                output_connected: false,
                output_auto_paired: false,
                protocol: "midi".to_string(),
            },
        ],
    };

    // Serialize
    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"device_id\":\"pads\""));
    assert!(json.contains("\"device_id\":\"keys\""));
    assert!(json.contains("\"enabled\":true"));
    assert!(json.contains("\"enabled\":false"));

    // Deserialize
    let parsed: DeviceStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.devices.len(), 2);
    assert_eq!(parsed.devices[0].device_id, "pads");
    assert_eq!(parsed.devices[0].port_name, "Mikro MK3 MIDI");
    assert!(parsed.devices[0].enabled);
    assert_eq!(parsed.devices[1].device_id, "keys");
    assert!(!parsed.devices[1].enabled);
}

#[test]
fn test_device_status_empty_devices_not_serialized() {
    let status = DeviceStatus {
        connected: true,
        name: Some("Mikro MK3".to_string()),
        port: Some(1),
        last_event_at: None,
        devices: vec![], // empty
    };

    let json = serde_json::to_string(&status).unwrap();
    // Empty devices vec should be omitted (skip_serializing_if = "Vec::is_empty")
    assert!(!json.contains("devices"), "Empty devices should be omitted");
}

#[test]
fn test_device_status_backward_compat_deserialization() {
    // Legacy JSON without devices field should deserialize with empty vec
    let json = r#"{"connected":true,"name":"Mikro MK3","port":1}"#;
    let parsed: DeviceStatus = serde_json::from_str(json).unwrap();
    assert!(parsed.connected);
    assert_eq!(parsed.name, Some("Mikro MK3".to_string()));
    assert!(
        parsed.devices.is_empty(),
        "Missing devices field should default to empty"
    );
}

// ─── Test 11: Mapping with device filter via engine ─────────────────────────

#[test]
fn test_mapping_with_device_filter_via_engine() {
    use conductor_core::Config;

    // Create a config with device-filtered mappings. (ADR-035 removed the
    // `[device]` block; the device filter under test is the per-trigger
    // `device = "..."` field, which is unrelated to I/O endpoint config.)
    let toml_str = r#"
[[modes]]
name = "Default"
color = "blue"

# Mapping for "pads" device: note 36 → cmd+space
[[modes.mappings]]
trigger = { type = "Note", note = 36, device = "pads" }
action = { type = "Keystroke", keys = "space", modifiers = ["cmd"] }

# Mapping for "keys" device: note 36 → cmd+c
[[modes.mappings]]
trigger = { type = "Note", note = 36, device = "keys" }
action = { type = "Keystroke", keys = "c", modifiers = ["cmd"] }

# Mapping with no device filter: note 37 → cmd+v (matches any device)
[[modes.mappings]]
trigger = { type = "Note", note = 37 }
action = { type = "Keystroke", keys = "v", modifiers = ["cmd"] }
"#;

    let config: Config = toml::from_str(toml_str).unwrap();
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Process note 36 from "pads" → should match cmd+space
    let event_36 = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: conductor_core::event_processor::VelocityLevel::Hard,
        channel: Some(0),
    };

    // Inspect the SELECTED action, not just that *something* matched.
    // A regression that returns the pads mapping for the keys device (or vice
    // versa) while still matching a known device would pass an is_some() check.
    let action_pads = engine.get_action_for_processed_with_device(&event_36, 0, Some("pads"));
    match action_pads {
        Some(Action::Keystroke { keys, modifiers }) => {
            assert_eq!(
                keys,
                vec![KeyCode::Space],
                "note 36 from `pads` must map to Space"
            );
            assert_eq!(
                modifiers,
                vec![ModifierKey::Command],
                "note 36 from `pads` must use Cmd"
            );
        }
        other => panic!("note 36 from `pads` should be Keystroke(cmd+space), got {other:?}"),
    }

    let action_keys = engine.get_action_for_processed_with_device(&event_36, 0, Some("keys"));
    match action_keys {
        Some(Action::Keystroke { keys, modifiers }) => {
            assert_eq!(
                keys,
                vec![KeyCode::Unicode('c')],
                "note 36 from `keys` must map to 'c'"
            );
            assert_eq!(
                modifiers,
                vec![ModifierKey::Command],
                "note 36 from `keys` must use Cmd"
            );
        }
        other => panic!("note 36 from `keys` should be Keystroke(cmd+c), got {other:?}"),
    }

    // Note 36 from unknown device should NOT match device-filtered mappings
    let action_unknown = engine.get_action_for_processed_with_device(&event_36, 0, Some("unknown"));
    assert!(
        action_unknown.is_none(),
        "Note 36 from unknown device should not match device-filtered mappings"
    );

    // Note 37 (no device filter) should match from any device
    let event_37 = ProcessedEvent::PadPressed {
        note: 37,
        velocity: 100,
        velocity_level: conductor_core::event_processor::VelocityLevel::Hard,
        channel: Some(0),
    };
    // The unfiltered mapping must select cmd+v regardless of device.
    let action_any = engine.get_action_for_processed_with_device(&event_37, 0, Some("pads"));
    match action_any {
        Some(Action::Keystroke { keys, modifiers }) => {
            assert_eq!(keys, vec![KeyCode::Unicode('v')], "note 37 must map to 'v'");
            assert_eq!(
                modifiers,
                vec![ModifierKey::Command],
                "note 37 must use Cmd"
            );
        }
        other => panic!("note 37 from `pads` should be Keystroke(cmd+v), got {other:?}"),
    }

    let action_any_none = engine.get_action_for_processed_with_device(&event_37, 0, None);
    match action_any_none {
        Some(Action::Keystroke { keys, modifiers }) => {
            assert_eq!(
                keys,
                vec![KeyCode::Unicode('v')],
                "note 37 (no device) -> 'v'"
            );
            assert_eq!(modifiers, vec![ModifierKey::Command]);
        }
        other => panic!("note 37 with no device should be Keystroke(cmd+v), got {other:?}"),
    }
}

// ─── Test 12: SetDeviceEnabled IPC command ──────────────────────────────────

#[test]
fn test_set_device_enabled_ipc_command_serialization() {
    let request = IpcRequest {
        id: "device-enable-1".to_string(),
        command: IpcCommand::SetDeviceEnabled,
        args: json!({"device_id": "pads", "enabled": false}),
    };

    let json_str = serde_json::to_string(&request).unwrap();
    assert!(json_str.contains("SET_DEVICE_ENABLED"));
    assert!(json_str.contains("device-enable-1"));
    assert!(json_str.contains("pads"));

    // Verify deserialization
    let parsed: IpcRequest = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.id, "device-enable-1");
    assert!(matches!(parsed.command, IpcCommand::SetDeviceEnabled));
    assert_eq!(
        parsed.args.get("device_id").and_then(|v| v.as_str()),
        Some("pads")
    );
    assert_eq!(
        parsed.args.get("enabled").and_then(|v| v.as_bool()),
        Some(false)
    );
}

#[test]
fn test_set_device_enabled_ipc_deserialization() {
    let json = r#"{"id":"test","command":"SET_DEVICE_ENABLED","args":{"device_id":"keys","enabled":true}}"#;
    let request: IpcRequest = serde_json::from_str(json).unwrap();
    assert!(matches!(request.command, IpcCommand::SetDeviceEnabled));
    assert_eq!(
        request.args.get("device_id").and_then(|v| v.as_str()),
        Some("keys")
    );
}

// ─── Additional edge case tests ─────────────────────────────────────────────

#[test]
fn test_device_id_equality_and_display() {
    let id1 = DeviceId::from_alias("pads");
    let id2 = DeviceId::from_alias("pads");
    let id3 = DeviceId::raw("pads");

    assert_eq!(id1, id2);
    assert_eq!(
        id1, id3,
        "from_alias and raw with same string should be equal"
    );
    assert_eq!(format!("{}", id1), "pads");
}

#[test]
fn test_device_event_into_parts() {
    let device_id = DeviceId::from_alias("pads");
    let event = InputEvent::PadPressed {
        pad: 36,
        velocity: 100,
        channel: None,
        time: Instant::now(),
    };

    let device_event = DeviceEvent::new(device_id.clone(), event);
    let (extracted_id, extracted_event) = device_event.into_parts();

    assert_eq!(extracted_id, device_id);
    match extracted_event {
        InputEvent::PadPressed { pad, .. } => assert_eq!(pad, 36),
        _ => panic!("Expected PadPressed"),
    }
}

#[test]
fn test_device_port_status_round_trip() {
    let status = DevicePortStatus {
        device_id: "mikro".to_string(),
        port_name: "Maschine Mikro MK3 MIDI".to_string(),
        port_index: 2,
        connected: true,
        enabled: true,
        last_event_at: Some(1700000000),
        is_configured: true,
        direction: conductor_core::config::DeviceDirection::Input,
        output_port_name: None,
        output_connected: false,
        output_auto_paired: false,
        protocol: "midi".to_string(),
    };

    let json = serde_json::to_string(&status).unwrap();
    let parsed: DevicePortStatus = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.device_id, "mikro");
    assert_eq!(parsed.port_name, "Maschine Mikro MK3 MIDI");
    assert_eq!(parsed.port_index, 2);
    assert!(parsed.connected);
    assert!(parsed.enabled);
    assert_eq!(parsed.last_event_at, Some(1700000000));
    assert_eq!(
        parsed.direction,
        conductor_core::config::DeviceDirection::Input
    );
    assert_eq!(parsed.output_port_name, None);
    assert!(!parsed.output_connected);
    assert!(!parsed.output_auto_paired);
}

/// ADR-021: Verify backward compat — old JSON without new fields deserializes with defaults
#[test]
fn test_device_port_status_backward_compat_deserialization() {
    let old_json = r#"{
        "device_id": "pads",
        "port_name": "Mikro MK3 MIDI",
        "port_index": 0,
        "connected": true,
        "enabled": true,
        "last_event_at": null,
        "is_configured": true
    }"#;
    let parsed: DevicePortStatus = serde_json::from_str(old_json).unwrap();
    assert_eq!(parsed.device_id, "pads");
    assert_eq!(
        parsed.direction,
        conductor_core::config::DeviceDirection::Input
    );
    assert_eq!(parsed.output_port_name, None);
    assert!(!parsed.output_connected);
    assert!(!parsed.output_auto_paired);
}

/// ADR-021: Verify new fields serialize correctly
#[test]
fn test_device_port_status_with_output_fields() {
    let status = DevicePortStatus {
        device_id: "mikro".to_string(),
        port_name: "Mikro Input".to_string(),
        port_index: 0,
        connected: true,
        enabled: true,
        last_event_at: None,
        is_configured: true,
        direction: conductor_core::config::DeviceDirection::Bidirectional,
        output_port_name: Some("Mikro Output".to_string()),
        output_connected: true,
        output_auto_paired: true,
        protocol: "midi".to_string(),
    };
    let json = serde_json::to_string(&status).unwrap();
    assert!(json.contains("\"direction\":\"Bidirectional\""));
    assert!(json.contains("\"output_port_name\":\"Mikro Output\""));
    assert!(json.contains("\"output_connected\":true"));
    assert!(json.contains("\"output_auto_paired\":true"));

    let parsed: DevicePortStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(
        parsed.direction,
        conductor_core::config::DeviceDirection::Bidirectional
    );
    assert_eq!(parsed.output_port_name, Some("Mikro Output".to_string()));
    assert!(parsed.output_connected);
    assert!(parsed.output_auto_paired);
}

#[tokio::test]
async fn test_device_event_channel_multiple_devices() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<DeviceEvent<InputEvent>>(32);

    // Send events from two different devices
    let pads_id = DeviceId::from_alias("pads");
    let keys_id = DeviceId::from_alias("keys");

    let event1 = DeviceEvent::new(
        pads_id.clone(),
        InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: None,
            time: Instant::now(),
        },
    );
    let event2 = DeviceEvent::new(
        keys_id.clone(),
        InputEvent::PadPressed {
            pad: 60,
            velocity: 80,
            channel: None,
            time: Instant::now(),
        },
    );

    tx.send(event1).await.unwrap();
    tx.send(event2).await.unwrap();

    let received1 = rx.recv().await.unwrap();
    assert_eq!(received1.device_id(), &pads_id);

    let received2 = rx.recv().await.unwrap();
    assert_eq!(received2.device_id(), &keys_id);
}
