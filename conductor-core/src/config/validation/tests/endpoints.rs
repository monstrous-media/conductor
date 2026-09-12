// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ── D10: Endpoint identity validation rules (ADR-035) ──
//
// Migrated from the removed `DeviceIdentityConfig`/`DevicePortBinding`
// lowering fixtures. The legacy binding model (separate `input`/`output`
// `DevicePortBinding`s + OSC host/port carried on a binding) no longer
// exists, so the tests that solely exercised that lowering — empty
// input/output `DevicePortBinding.matchers`, OSC-on-a-binding host/port
// completeness, and the matchers↔input coexistence warning — were deleted.
// The matchers-only / output-only / no-matchers invariants survive against
// the unified `[[endpoints]]` schema.

#[test]
fn test_endpoint_no_matchers_is_error() {
    let mut config = default_config();
    config.endpoints = vec![EndpointConfig {
        alias: "empty".to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    }];
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("no matchers"))
    );
}

#[test]
fn test_endpoint_with_only_matchers_is_valid() {
    let mut config = default_config();
    config.endpoints = vec![ep_input(
        "pads",
        vec![DeviceMatcher::NameContains {
            value: "Mikro".to_string(),
        }],
    )];
    let report = validate_config(&config);
    assert!(report.is_valid(), "Endpoint with matchers should be valid");
}

#[test]
fn test_endpoint_with_only_output_is_valid() {
    let mut config = default_config();
    config.endpoints = vec![EndpointConfig {
        alias: "synth-out".to_string(),
        direction: ConnectorDirection::Output,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![],
            input_matchers: vec![],
            output_matchers: vec![DeviceMatcher::NameContains {
                value: "IAC".to_string(),
            }],
            no_probe: false,
        },
    }];
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "Output-only endpoint should be valid: {:?}",
        report.errors
    );
}

#[test]
fn test_endpoint_input_direction_with_output_matchers_is_error() {
    use crate::config::types::ConnectorDirection;
    let mut config = default_config();
    config.endpoints = vec![endpoint_with_matchers(
        "mis-directed",
        ConnectorDirection::Input,
        vec![DeviceMatcher::name_contains("In")],
        vec![DeviceMatcher::name_contains("Out")], // ignored by effective_matchers(Input)
    )];
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report.errors.iter().any(|e| e
            .message
            .contains("direction = Input but defines `output_matchers`")),
        "Input endpoint with output_matchers must be a hard error, not a silent no-op"
    );
}

#[test]
fn test_endpoint_output_direction_with_input_matchers_is_error() {
    use crate::config::types::ConnectorDirection;
    let mut config = default_config();
    config.endpoints = vec![endpoint_with_matchers(
        "mis-directed",
        ConnectorDirection::Output,
        vec![DeviceMatcher::name_contains("In")], // ignored by effective_matchers(Output)
        vec![DeviceMatcher::name_contains("Out")],
    )];
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(report.errors.iter().any(|e| {
        e.message
            .contains("direction = Output but defines `input_matchers`")
    }));
}

#[test]
fn test_endpoint_bidirectional_with_both_matchers_is_valid() {
    use crate::config::types::ConnectorDirection;
    let mut config = default_config();
    config.endpoints = vec![endpoint_with_matchers(
        "split",
        ConnectorDirection::Bidirectional,
        vec![DeviceMatcher::name_contains("In")],
        vec![DeviceMatcher::name_contains("Out")],
    )];
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "Bidirectional legitimately uses both input_matchers and output_matchers: {:?}",
        report.errors
    );
}

#[test]
fn test_endpoint_hid_non_input_is_error() {
    use crate::config::types::ConnectorDirection;
    for dir in [
        ConnectorDirection::Output,
        ConnectorDirection::Bidirectional,
    ] {
        let mut config = default_config();
        config.endpoints = vec![hid_endpoint("xbox", dir)];
        let report = validate_config(&config);
        assert!(!report.is_valid(), "HID {dir:?} must be rejected");
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("HID is input-only")),
            "expected HID input-only error for {dir:?}, got {:?}",
            report.errors
        );
    }
}

#[test]
fn test_endpoint_hid_input_is_valid() {
    use crate::config::types::ConnectorDirection;
    let mut config = default_config();
    config.endpoints = vec![hid_endpoint("xbox", ConnectorDirection::Input)];
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "HID Input endpoint should be valid: {:?}",
        report.errors
    );
}

#[test]
fn test_hid_to_midi_out_of_range_is_rejected() {
    // ADR-039-B: out-of-range HidToMidi channel/CC
    // must be REJECTED at config-load, not silently masked at runtime.
    use crate::config::types::ConnectorDirection;
    let mut config = default_config();
    config.endpoints = vec![
        hid_endpoint("xbox", ConnectorDirection::Input),
        midi_output_endpoint("synth"),
    ];
    config.routes = vec![hid_to_midi_route(20, 200)]; // channel > 15, cc > 127
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "out-of-range HidToMidi must be rejected"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("channel 20 out of range")),
        "expected channel range error, got {:?}",
        report.errors
    );
    assert!(
        report.errors.iter().any(|e| e.message.contains("CC 200")),
        "expected CC range error, got {:?}",
        report.errors
    );
}

#[test]
fn test_hid_to_midi_valid_ranges_accepted() {
    use crate::config::types::ConnectorDirection;
    let mut config = default_config();
    config.endpoints = vec![
        hid_endpoint("xbox", ConnectorDirection::Input),
        midi_output_endpoint("synth"),
    ];
    config.routes = vec![hid_to_midi_route(5, 20)]; // in range
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "in-range HidToMidi route should validate: {:?}",
        report.errors
    );
}

#[test]
fn test_hid_to_osc_invalid_address_is_rejected() {
    // ADR-039-B: an OSC address not starting with '/' must be
    // rejected at config-load (else an invalid OSC packet is emitted).
    use crate::config::types::ConnectorDirection;
    let mut config = default_config();
    config.endpoints = vec![
        hid_endpoint("xbox", ConnectorDirection::Input),
        osc_output_endpoint("osc_out"),
    ];
    config.routes = vec![hid_to_osc_route("pad/a")]; // missing leading '/'
    let report = validate_config(&config);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("OSC addresses must start with '/'")),
        "expected OSC address error, got {:?}",
        report.errors
    );
}

#[test]
fn test_hid_to_osc_valid_address_accepted() {
    use crate::config::types::ConnectorDirection;
    let mut config = default_config();
    config.endpoints = vec![
        hid_endpoint("xbox", ConnectorDirection::Input),
        osc_output_endpoint("osc_out"),
    ];
    config.routes = vec![hid_to_osc_route("/pad/a")]; // valid
    let report = validate_config(&config);
    assert!(
        !report
            .errors
            .iter()
            .any(|e| e.message.contains("OSC addresses must start with '/'")),
        "valid OSC address must not trip the address check: {:?}",
        report.errors
    );
}

#[test]
fn test_hid_endpoint_let_through_is_error() {
    // ADR-035 Phase 2 regression: a non-gamepad trigger whose `device`
    // filter resolves to a HID *endpoint* must still trip the ADR-038 §4.3
    // let-through hard error. Pre-Phase-2 the protocol map was built only
    // from [[bindings]] (now empty for any loaded config), so an endpoint's
    // `protocol = "hid"` went unseen and the invalid `let_through` slipped
    // through. `device_protocols` now unions `[[endpoints]]`.
    use crate::config::types::ConnectorDirection;
    let mut config = default_config();
    config.endpoints = vec![hid_endpoint("xbox", ConnectorDirection::Input)];
    config.modes = vec![Mode {
        name: "Default".into(),
        color: None,
        mappings: vec![Mapping {
            trigger: Trigger::Note {
                note: 36,
                velocity_min: None,
                channel: None,
                device: Some("xbox".into()),
            },
            action: ActionConfig::Shell {
                sandbox: None,
                command: "echo hi".into(),
                args: None,
                timeout_ms: None,
            },
            description: None,
            let_through: true,
        }],
    }];
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "let_through on a HID-endpoint source must be rejected"
    );
    assert!(
        report.errors.iter().any(|e| e
            .message
            .contains("let_through = true on a HID-only source")),
        "expected the HID let-through hard error, got {:?}",
        report.errors
    );
}

#[test]
fn test_endpoint_channel_out_of_range_is_error() {
    let mut config = default_config();
    let mut ep = ep_input(
        "drums",
        vec![DeviceMatcher::NameContains {
            value: "Drums".to_string(),
        }],
    );
    ep.channels = vec![9, 16]; // 16 is out of range (valid: 0-15)
    config.endpoints = vec![ep];
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "Channel 16 should cause a validation error"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("channel 16")
                && e.message.contains("out of range")
                && e.message.contains("endpoint"))
    );
}

#[test]
fn test_endpoint_channel_valid_range_ok() {
    let mut config = default_config();
    let mut ep = ep_input(
        "drums",
        vec![DeviceMatcher::NameContains {
            value: "Drums".to_string(),
        }],
    );
    ep.channels = vec![0, 9, 15]; // All valid
    config.endpoints = vec![ep];
    let report = validate_config(&config);
    // No channel errors (there may be other warnings but channels should be fine)
    assert!(!report.errors.iter().any(|e| e.path.contains("channels")));
}

#[test]
fn test_hid_protocol_with_channels_warns() {
    // HID devices don't have MIDI channels — channels field is meaningless
    let mut config = default_config();
    let mut ep = ep_input(
        "gamepad",
        vec![DeviceMatcher::NameContains {
            value: "Xbox".to_string(),
        }],
    );
    ep.protocol = Some(ConnectorProtocol::Hid);
    ep.channels = vec![9];
    config.endpoints = vec![ep];
    let report = validate_config(&config);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("channels") && w.message.contains("Hid"))
    );
}

// ── Input-direction connectors are off-spec (ADR-031 §3.4) ──
//
// ADR-031 line 124/136/143 establishes that input identity is
// configured via [[bindings]] (ADR-022); [[connectors]] carries
// OUTPUT or BIDIRECTIONAL endpoints only. An input-direction
// connector is structurally dead — `PortResolver` only walks
// [[bindings]], so dispatched events never carry that alias and
// any routes keyed on it silently never fire.

#[test]
fn test_validate_accepts_input_direction_connector_adr035() {
    // ADR-035 REMOVED the `direction = Input` endpoint rejection —
    // input endpoints are now first-class (unblocks ADR-039 input listeners).
    let mut config = default_config();
    config
        .endpoints
        .push(connector("mpk_input", ConnectorDirection::Input));
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "input-direction endpoint is now accepted (ADR-035): {:?}",
        report.errors
    );
}

#[test]
fn test_validate_accepts_output_and_bidirectional_connectors() {
    let mut config = default_config();
    config
        .endpoints
        .push(connector("absynth_output", ConnectorDirection::Output));
    config
        .endpoints
        .push(connector("iac_bus", ConnectorDirection::Bidirectional));
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "output + bidirectional must be accepted: {:?}",
        report.errors
    );
}
