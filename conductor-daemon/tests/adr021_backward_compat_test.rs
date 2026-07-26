// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-021 Phase 5A: Backward compatibility integration tests (daemon layer)
//!
//! This integration suite verifies, at the daemon layer, that legacy configs
//! with no available output ports produce an empty output map
//! (`build_output_map`), and that `DevicePortStatus` (de)serialization handles
//! all ADR-021 fields plus old field-less JSON.
//!
//! Raw output-port fallback — an unmapped SendMidi target passing through as a
//! literal port name — is NOT exercised here. It is covered by the lower-level
//! `action_executor` unit tests (`test_resolve_output_port_raw_fallback`,
//! `test_output_map_update`, `test_existing_raw_port_usage_unchanged`), because
//! the resolving method `ActionExecutor::resolve_output_port` is private and
//! not reachable from this integration crate. Not duplicated here to avoid
//! widening production API surface for a redundant test (#1511).

use conductor_core::config::port_binding::DeviceDirection;
use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
use conductor_core::identity::DeviceMatcher;
use conductor_daemon::daemon::output_resolver::build_output_map;
use conductor_daemon::daemon::types::DevicePortStatus;

// ─── Legacy config backward compat ───────────────────────────────────────────

#[test]
fn test_legacy_config_output_map_empty() {
    // Input endpoint with no available output ports → empty map
    let ep = EndpointConfig {
        alias: "pads".to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![DeviceMatcher::name_contains("Mikro")],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    };
    let input_bindings = vec![("pads".to_string(), "Mikro Input".to_string())];
    // No output ports available at all
    let outputs: Vec<String> = vec![];
    let map = build_output_map(&[ep], &input_bindings, &outputs);
    assert!(
        map.is_empty(),
        "no outputs available should produce empty map"
    );
}

// ─── DevicePortStatus serialization ──────────────────────────────────────────

#[test]
fn test_device_port_status_backward_compat_json() {
    // Old JSON without ADR-021 fields should deserialize with defaults
    let old_json = serde_json::json!({
        "device_id": "mikro",
        "port_name": "Mikro Input",
        "port_index": 0,
        "connected": true,
        "enabled": true,
        "last_event_at": null
    });
    let status: DevicePortStatus =
        serde_json::from_value(old_json).expect("old JSON should deserialize");
    assert_eq!(status.device_id, "mikro");
    assert!(status.connected);
    // ADR-021 fields should have defaults
    assert_eq!(status.direction, DeviceDirection::Input);
    assert!(status.output_port_name.is_none());
    assert!(!status.output_connected);
    assert!(!status.output_auto_paired);
}

#[test]
fn test_device_port_status_all_directions_roundtrip() {
    let statuses = vec![
        DevicePortStatus {
            device_id: "pads".to_string(),
            port_name: "Mikro Input".to_string(),
            port_index: 0,
            connected: true,
            enabled: true,
            last_event_at: Some(1234567890),
            is_configured: true,
            direction: DeviceDirection::Input,
            output_port_name: None,
            output_connected: false,
            output_auto_paired: false,
            protocol: "midi".to_string(),
        },
        DevicePortStatus {
            device_id: "leds".to_string(),
            port_name: "LED Controller".to_string(),
            port_index: 1,
            connected: true,
            enabled: true,
            last_event_at: None,
            is_configured: true,
            direction: DeviceDirection::Output,
            output_port_name: Some("LED Controller Output".to_string()),
            output_connected: true,
            output_auto_paired: false,
            protocol: "midi".to_string(),
        },
        DevicePortStatus {
            device_id: "mikro".to_string(),
            port_name: "Mikro Input".to_string(),
            port_index: 2,
            connected: true,
            enabled: true,
            last_event_at: Some(999),
            is_configured: true,
            direction: DeviceDirection::Bidirectional,
            output_port_name: Some("Mikro Output".to_string()),
            output_connected: true,
            output_auto_paired: true,
            protocol: "midi".to_string(),
        },
    ];

    for status in &statuses {
        let json = serde_json::to_string(status).expect("serialize failed");
        let parsed: DevicePortStatus = serde_json::from_str(&json).expect("deserialize failed");
        assert_eq!(parsed.device_id, status.device_id);
        assert_eq!(parsed.direction, status.direction);
        assert_eq!(parsed.output_port_name, status.output_port_name);
        assert_eq!(parsed.output_connected, status.output_connected);
        assert_eq!(parsed.output_auto_paired, status.output_auto_paired);
    }
}

#[test]
fn test_device_port_status_output_only_serialization() {
    let status = DevicePortStatus {
        device_id: "synth".to_string(),
        port_name: "Synth Out".to_string(),
        port_index: 0,
        connected: false,
        enabled: true,
        last_event_at: None,
        is_configured: true,
        direction: DeviceDirection::Output,
        output_port_name: Some("Synth Out".to_string()),
        output_connected: false,
        output_auto_paired: false,
        protocol: "midi".to_string(),
    };

    let json = serde_json::to_string(&status).expect("serialize failed");
    let parsed: DevicePortStatus = serde_json::from_str(&json).expect("deserialize failed");
    assert_eq!(parsed.direction, DeviceDirection::Output);
    assert_eq!(parsed.output_port_name, Some("Synth Out".to_string()));
    assert!(!parsed.connected, "output-only device input not connected");
    assert!(!parsed.output_connected, "output port not connected");
}
