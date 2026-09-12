// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;
use crate::config::types::{
    ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind, Mapping, Mode,
    RouteConfig, SignalFilter, SignalTransform,
};
use crate::identity::DeviceMatcher;

fn default_config() -> Config {
    Config::default_config()
}

/// Build an Input-direction `Matcher` endpoint carrying `matchers` (the
/// ADR-035 replacement for the removed `DeviceIdentityConfig` matchers-only
/// fixture — `lower_binding` mapped `(input=None, output=None)` with
/// non-empty top-level `matchers` to an `Input` Matcher).
fn ep_input(alias: &str, matchers: Vec<DeviceMatcher>) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers,
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    }
}

fn config_with_mapping(trigger: Trigger, action: ActionConfig) -> Config {
    Config {
        config_meta: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger,
                action,
                description: None,
                let_through: false,
            }],
        }],
        ..default_config()
    }
}

fn config_with_action(action: ActionConfig) -> Config {
    config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: Some(1),
            channel: None,
            device: None,
        },
        action,
    )
}

// ── ADR-039-A: OSC route validation (D8 + filter) ──

fn ep_osc_input(alias: &str) -> EndpointConfig {
    use crate::config::types::NetworkSecurityConfig;
    EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Input,
        protocol: None, // inferred Osc from the OscEndpoint kind
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::OscEndpoint {
            host: "127.0.0.1".to_string(),
            port: 9000,
            security: NetworkSecurityConfig::default(),
        },
    }
}

fn ep_midi(alias: &str, dir: ConnectorDirection, matchers: Vec<DeviceMatcher>) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
        direction: dir,
        protocol: None, // Matcher kind defaults to Midi
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers,
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    }
}

fn osc_to_midi_route(from: &str, to: &str, filter: Option<SignalFilter>) -> RouteConfig {
    RouteConfig {
        from: from.to_string(),
        to: to.to_string(),
        transform: Some(SignalTransform::OscToMidi {
            address_to_cc: Some("/eos/fader/{cc}".to_string()),
            address_to_note: None,
            channel: Some(0),
        }),
        filter,
        enabled: true,
        description: None,
        modes: vec![],
    }
}

fn config_with_osc_route(endpoints: Vec<EndpointConfig>, route: RouteConfig) -> Config {
    Config {
        config_meta: Default::default(),
        endpoints,
        routes: vec![route],
        ..default_config()
    }
}

// ── ADR-039-A: OscToArtNet route validation ──

fn ep_artnet_output(alias: &str) -> EndpointConfig {
    use crate::config::types::NetworkSecurityConfig;
    EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Output,
        protocol: None, // inferred ArtNet from the ArtNetEndpoint kind
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::ArtNetEndpoint {
            universe: 0,
            host: "127.0.0.1".to_string(),
            port: 6454,
            allow_broadcast: false,
            security: NetworkSecurityConfig::default(),
        },
    }
}

fn osc_to_artnet_route(from: &str, to: &str, template: &str) -> RouteConfig {
    RouteConfig {
        from: from.to_string(),
        to: to.to_string(),
        transform: Some(SignalTransform::OscToArtNet {
            address_to_dmx: template.to_string(),
        }),
        filter: None,
        enabled: true,
        description: None,
        modes: vec![],
    }
}

#[test]
fn osc_to_artnet_route_with_valid_template_passes() {
    let cfg = config_with_osc_route(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        osc_to_artnet_route("osc-in", "dmx-out", "/dmx/{dmx}"),
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "valid OscToArtNet route must pass: {:?}",
        report.errors
    );
}

#[test]
fn osc_to_artnet_template_without_placeholder_rejected() {
    let cfg = config_with_osc_route(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        osc_to_artnet_route("osc-in", "dmx-out", "/dmx/static"),
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("{dmx}")),
        "missing placeholder must be a load error: {:?}",
        report.errors
    );
}

#[test]
fn osc_to_artnet_template_without_leading_slash_rejected() {
    let cfg = config_with_osc_route(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        osc_to_artnet_route("osc-in", "dmx-out", "dmx/{dmx}"),
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("OscToArtNet")),
        "missing leading slash must be a load error: {:?}",
        report.errors
    );
}

#[test]
fn osc_to_artnet_template_with_two_placeholders_rejected() {
    let cfg = config_with_osc_route(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        osc_to_artnet_route("osc-in", "dmx-out", "/{dmx}/{dmx}"),
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("{dmx}")),
        "duplicate placeholder must be a load error: {:?}",
        report.errors
    );
}

#[test]
fn osc_to_artnet_route_without_transform_requires_one() {
    // (Osc, ArtNet) is now Required("OscToArtNet") in the matrix — a
    // transform-less route must be rejected, not silently passed.
    let mut route = osc_to_artnet_route("osc-in", "dmx-out", "/dmx/{dmx}");
    route.transform = None;
    let cfg = config_with_osc_route(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        route,
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("OscToArtNet")),
        "missing required transform must name OscToArtNet: {:?}",
        report.errors
    );
}

// ── ADR-039-A: OscForward action validation ──

fn ep_osc_output(alias: &str) -> EndpointConfig {
    use crate::config::types::NetworkSecurityConfig;
    EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Output,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::OscEndpoint {
            host: "127.0.0.1".to_string(),
            port: 9100,
            security: NetworkSecurityConfig::default(),
        },
    }
}

fn config_with_osc_forward(endpoints: Vec<EndpointConfig>, action: ActionConfig) -> Config {
    Config {
        config_meta: Default::default(),
        endpoints,
        modes: vec![Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::OscMessage {
                    address: "/eos/go".to_string(),
                    device: Some("osc-in".to_string()),
                },
                action,
                description: None,
                let_through: false,
            }],
        }],
        ..default_config()
    }
}

#[test]
fn osc_forward_to_osc_output_passes() {
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in"), ep_osc_output("eos-out")],
        ActionConfig::OscForward {
            target: "eos-out".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "valid OscForward must pass: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_with_transform_rejected_in_v1() {
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in"), ep_osc_output("eos-out")],
        ActionConfig::OscForward {
            target: "eos-out".to_string(),
            transform: Some(SignalTransform::OscToMidi {
                address_to_cc: Some("/f/{cc}".to_string()),
                address_to_note: None,
                channel: None,
            }),
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("OscForward transform is not supported")),
        "a transform must be rejected in V1: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_to_non_osc_target_rejected() {
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        ActionConfig::OscForward {
            target: "dmx-out".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("requires an OSC output endpoint")),
        "an Art-Net target must be rejected: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_to_unknown_target_rejected() {
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in")],
        ActionConfig::OscForward {
            target: "nope".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("does not match any [[endpoints]] alias")),
        "an unknown target alias must be rejected: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_to_input_only_osc_target_rejected() {
    // An OSC endpoint that is Input-only can't be sent to — the daemon's
    // runtime map would never contain it, so load must reject.
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in")],
        ActionConfig::OscForward {
            target: "osc-in".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("Input-only OSC endpoint")),
        "an Input-only OSC target must be rejected: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_to_disabled_osc_output_rejected() {
    // A disabled OSC output is excluded from the daemon's runtime map, so
    // load must reject it for clear UX rather than a silent no-op.
    let mut disabled = ep_osc_output("eos-out");
    disabled.enabled = false;
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in"), disabled],
        ActionConfig::OscForward {
            target: "eos-out".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("disabled endpoint")),
        "a disabled OSC output target must be rejected: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_to_bidirectional_osc_output_passes() {
    // Bidirectional OSC is a valid send target (mirrors the runtime map).
    let mut bidir = ep_osc_output("eos-io");
    bidir.direction = ConnectorDirection::Bidirectional;
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in"), bidir],
        ActionConfig::OscForward {
            target: "eos-io".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "a Bidirectional OSC output target must pass: {:?}",
        report.errors
    );
}

#[test]
fn osc_route_to_bidirectional_midi_rejected() {
    // D8: a Bidirectional MIDI target is both output and input → self-loop.
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_midi(
                "synth",
                ConnectorDirection::Bidirectional,
                vec![DeviceMatcher::NameContains {
                    value: "Synth".into(),
                }],
            ),
        ],
        osc_to_midi_route("console", "synth", None),
    );
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid(),
        "OSC→bidirectional-MIDI must be rejected"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("feedback loop")),
        "error should name the feedback loop; got {:?}",
        report.errors
    );
}

#[test]
fn osc_route_to_self_ingested_midi_rejected() {
    // D8: distinct Output + Input MIDI endpoints with identical matchers =
    // same device wired in and out.
    let matchers = vec![DeviceMatcher::NameContains {
        value: "Mikro".into(),
    }];
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_midi("mikro_out", ConnectorDirection::Output, matchers.clone()),
            ep_midi("mikro_in", ConnectorDirection::Input, matchers),
        ],
        osc_to_midi_route("console", "mikro_out", None),
    );
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid(),
        "OSC→self-ingested-MIDI must be rejected"
    );
}

#[test]
fn osc_route_to_distinct_midi_output_ok() {
    // No MIDI input shares the output's device → no loop → valid.
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_midi(
                "fx_out",
                ConnectorDirection::Output,
                vec![DeviceMatcher::NameContains { value: "FX".into() }],
            ),
            ep_midi(
                "pad_in",
                ConnectorDirection::Input,
                vec![DeviceMatcher::NameContains {
                    value: "Pad".into(),
                }],
            ),
        ],
        osc_to_midi_route("console", "fx_out", None),
    );
    let report = validate_config(&cfg);
    assert!(
        report.is_valid(),
        "OSC→distinct-MIDI-output should validate; got {:?}",
        report.errors
    );
}

#[test]
fn osc_route_with_filter_rejected() {
    // OSC routes are currently catch-all only.
    let filter = SignalFilter {
        message_types: vec![],
        channels: vec![5],
        cc_range: None,
        note_range: None,
        osc_address_prefix: None,
    };
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_midi(
                "fx_out",
                ConnectorDirection::Output,
                vec![DeviceMatcher::NameContains { value: "FX".into() }],
            ),
        ],
        osc_to_midi_route("console", "fx_out", Some(filter)),
    );
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid(),
        "OSC-source route with a filter must be rejected"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("catch-all")),
        "error should mention catch-all; got {:?}",
        report.errors
    );
}

fn ep_virtual(alias: &str, dir: ConnectorDirection, port_name: &str) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
        direction: dir,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::MidiVirtualPort {
            port_name: port_name.to_string(),
        },
    }
}

#[test]
fn osc_route_to_self_ingested_virtual_port_rejected() {
    // D8: a MidiVirtualPort output has no DeviceMatchers, so
    // the matcher-signature path misses it — but a Conductor-created virtual
    // port that an input endpoint also names is the classic loopback.
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_virtual("bus_out", ConnectorDirection::Output, "Conductor Bus"),
            ep_midi(
                "bus_in",
                ConnectorDirection::Input,
                vec![DeviceMatcher::ExactName {
                    value: "Conductor Bus".into(),
                }],
            ),
        ],
        osc_to_midi_route("console", "bus_out", None),
    );
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid(),
        "OSC→virtual-port also ingested as input must be rejected"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("feedback loop")),
        "error should name the feedback loop; got {:?}",
        report.errors
    );
}

#[test]
fn osc_route_to_virtual_port_input_twin_rejected() {
    // The input twin is itself a MidiVirtualPort of the same name.
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_virtual("bus_out", ConnectorDirection::Output, "Bus"),
            ep_virtual("bus_in", ConnectorDirection::Input, "Bus"),
        ],
        osc_to_midi_route("console", "bus_out", None),
    );
    assert!(!validate_config(&cfg).is_valid());
}

#[test]
fn osc_route_to_unmonitored_virtual_port_ok() {
    // No input names this virtual port → no loop → valid.
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_virtual("bus_out", ConnectorDirection::Output, "Conductor Bus"),
            ep_midi(
                "pad_in",
                ConnectorDirection::Input,
                vec![DeviceMatcher::NameContains {
                    value: "Pad".into(),
                }],
            ),
        ],
        osc_to_midi_route("console", "bus_out", None),
    );
    let report = validate_config(&cfg);
    assert!(
        report.is_valid(),
        "OSC→unmonitored virtual port should validate; got {:?}",
        report.errors
    );
}

// ── Structural tests (from former loader.rs) ─────────────

#[test]
fn test_validate_valid_config() {
    let config = default_config();
    let report = validate_config(&config);
    assert!(report.is_valid());
}

#[test]
fn test_trace_buffer_size_zero_rejected() {
    let mut config = default_config();
    config.advanced_settings.trace_buffer_size = 0;
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.path == "advanced_settings.trace_buffer_size"
                && e.message.contains("at least 1")),
        "0 must be rejected with a clear message"
    );
}

#[test]
fn test_trace_buffer_size_too_large_rejected() {
    use crate::config::types::MAX_TRACE_BUFFER_SIZE;
    let mut config = default_config();
    config.advanced_settings.trace_buffer_size = MAX_TRACE_BUFFER_SIZE + 1;
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.path == "advanced_settings.trace_buffer_size"
                && e.message.contains("exceeds the maximum")),
        "values above the cap must be rejected"
    );
}

#[test]
fn test_trace_buffer_size_in_range_accepted() {
    let mut config = default_config();
    config.advanced_settings.trace_buffer_size = 5000;
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "a sane in-range buffer size must validate: {:?}",
        report.errors
    );
}

#[test]
fn test_validate_duplicate_mode_names() {
    let mut config = default_config();
    config.modes.push(Mode {
        name: "Default".to_string(),
        color: None,
        mappings: vec![],
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("Duplicate mode name"))
    );
}

#[test]
fn test_validate_invalid_note_number() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 128,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("out of range"))
    );
}

#[test]
fn test_validate_invalid_modifier() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec!["invalid_mod".to_string()],
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("Unknown modifier"))
    );
}

#[test]
fn test_validate_invalid_direction() {
    let config = config_with_mapping(
        Trigger::EncoderTurn {
            cc: 1,
            direction: Some("Invalid".to_string()),
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("Invalid direction"))
    );
}

#[test]
fn test_validate_empty_keystroke_keys() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: String::new(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_validate_sequence_with_empty_actions() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Sequence { actions: vec![] },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_validate_encoder_direction_clockwise() {
    let config = config_with_mapping(
        Trigger::EncoderTurn {
            cc: 1,
            direction: Some("Clockwise".to_string()),
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(report.is_valid());
}

#[test]
fn test_validate_encoder_direction_counter_clockwise() {
    let config = config_with_mapping(
        Trigger::EncoderTurn {
            cc: 1,
            direction: Some("CounterClockwise".to_string()),
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(report.is_valid());
}

#[test]
fn test_validate_note_chord_with_empty_notes() {
    let config = config_with_mapping(
        Trigger::NoteChord {
            notes: vec![],
            timeout_ms: None,
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_validate_invalid_mouse_button() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::MouseClick {
            button: "invalid".to_string(),
            x: None,
            y: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_validate_volume_control_set_without_value() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::VolumeControl {
            operation: "Set".to_string(),
            value: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

// ── Security tests (from former loader.rs) ──────────────

#[test]
fn test_shell_injection_semicolon_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "echo test; rm -rf /".to_string(),
            args: None,
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("command chaining with semicolon"))
    );
}

#[test]
fn test_shell_injection_and_operator_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "ls && malicious_command".to_string(),
            args: None,
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("command chaining with AND"))
    );
}

#[test]
fn test_shell_injection_or_operator_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "false || evil_fallback".to_string(),
            args: None,
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("command chaining with OR"))
    );
}

#[test]
fn test_shell_injection_pipe_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "cat /etc/passwd | grep root".to_string(),
            args: None,
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(report.errors.iter().any(|e| e.message.contains("piping")));
}

#[test]
fn test_shell_injection_backtick_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "echo `whoami`".to_string(),
            args: None,
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("backtick command substitution"))
    );
}

#[test]
fn test_shell_injection_dollar_paren_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "echo $(whoami)".to_string(),
            args: None,
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("dollar-paren command substitution"))
    );
}

#[test]
fn test_shell_injection_variable_expansion_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "echo ${DANGEROUS_VAR}".to_string(),
            args: None,
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("variable expansion"))
    );
}

#[test]
fn test_shell_injection_output_redirect_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "echo data > /etc/important_file".to_string(),
            args: None,
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_shell_injection_background_execution_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "malicious_daemon &".to_string(),
            args: None,
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("background execution"))
    );
}

#[test]
fn test_shell_safe_commands_allowed() {
    let safe_commands = [
        "git status",
        "cargo build",
        "ls -la",
        "echo hello world",
        "pwd",
    ];
    for cmd in &safe_commands {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: cmd.to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "Safe command '{}' should be allowed",
            cmd
        );
    }
}

// ───────────────────────────────────────────────────────────
// ADR-027 D3 §3.1 — argv-form `args` also
// get the metacharacter blocklist applied, so users can't
// smuggle redirects / pipes / chains past the validator by
// moving them into argv.
// ───────────────────────────────────────────────────────────

#[test]
fn test_shell_argv_form_args_metacharacters_blocked() {
    // The exact bypass class — `>` redirect smuggled via argv-form args.
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "/bin/sh".to_string(),
            args: Some(vec!["-c".to_string(), "env > /tmp/leak".to_string()]),
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "argv-form args containing `>` must be rejected"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("output redirection")),
        "error should attribute the rejection to the `>` redirect — got: {:?}",
        report.errors
    );
    // Wording: when the failure is in argv-form `.args[i]`, the
    // diagnostic must say "Shell argument" not "Shell command" —
    // otherwise users see a misleading "Shell command contains
    // '>'" error pointing at a path that ends in `.args[1]`.
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.starts_with("Shell argument")),
        "argv-form arg-blocklist error must use 'Shell argument' wording — got: {:?}",
        report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_shell_argv_form_args_chain_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "/usr/bin/env".to_string(),
            args: Some(vec!["FOO=bar; rm -rf /".to_string()]),
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "argv-form args containing `;` must be rejected"
    );
}

#[test]
fn test_shell_argv_form_safe_args_allowed() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "/usr/bin/osascript".to_string(),
            args: Some(vec![
                "-e".to_string(),
                "display notification \"MIDI triggered\"".to_string(),
            ]),
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "argv-form with safe args should be allowed — got errors: {:?}",
        report.errors
    );
}

#[test]
fn test_shell_whitespace_only_command_rejected() {
    // A whitespace-only `command` used to pass validation and
    // become a runtime no-op (the executor trims and aborts
    // silently). The validator's `command.trim().is_empty()`
    // check now rejects it at load with the standard "Shell
    // action requires command" error.
    for whitespace in &[" ", "   ", "\t", "\n", " \t \n "] {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: whitespace.to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(
            !report.is_valid(),
            "whitespace-only command {:?} should be rejected",
            whitespace
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("requires command")),
            "error should explain the empty command — got: {:?}",
            report.errors
        );
    }
}

#[test]
fn test_shell_quote_only_legacy_command_rejected() {
    // Legacy commands made up entirely of whitespace and the
    // `'`/`"` quote characters tokenise to zero argv parts, so
    // without this guard they'd pass validation only to no-op at
    // runtime (the executor logs "Failed to parse shell command"
    // and aborts). The `command_has_runnable_token` helper
    // rejects them at load instead.
    for cmd in &["'", "''", "\"", "\"\"", " ' ", "\t ' '", "'  \"  '"] {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: cmd.to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(
            !report.is_valid(),
            "quote-only legacy command {:?} should be rejected",
            cmd
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("requires command")),
            "error should diagnose as missing command — got: {:?}",
            report.errors
        );
    }
}

#[test]
fn test_shell_argv_form_quote_only_command_with_args_allowed() {
    // A legacy quote-only `command` is rejected because the
    // tokeniser yields nothing — but an argv-form invocation
    // with `command = "'"` and explicit `args` would spawn
    // (and fail at the OS level with "no such file or
    // directory: '"). That's a clearer user-facing failure
    // than the legacy silent no-op, so the validator allows
    // it through (the metacharacter blocklist still applies
    // and would reject any actually-dangerous patterns).
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "'".to_string(),
            args: Some(vec!["arg".to_string()]),
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "argv-form lets weird `command` through — got: {:?}",
        report.errors
    );
}

#[test]
fn test_shell_argv_form_args_path_includes_index() {
    // Error path should pinpoint WHICH arg failed, not just say
    // "Shell action somewhere broke". This helps users debug
    // multi-arg argv configs.
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "/usr/bin/env".to_string(),
            args: Some(vec!["SAFE=1".to_string(), "BAD$(rm -rf /)".to_string()]),
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report.errors.iter().any(|e| e.path.contains(".args[1]")),
        "error path should pinpoint args[1] — got: {:?}",
        report.errors.iter().map(|e| &e.path).collect::<Vec<_>>()
    );
}

#[test]
fn test_launch_injection_special_chars_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Launch {
            app: "Terminal; rm -rf /".to_string(),
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("invalid characters"))
    );
}

#[test]
fn test_launch_path_traversal_blocked() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Launch {
            app: "../../malicious".to_string(),
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("path traversal"))
    );
}

#[test]
fn test_launch_safe_app_names_allowed() {
    let safe_apps = [
        "Terminal",
        "VS Code",
        "Google Chrome",
        "/Applications/Safari.app",
        "my-app_v2.0",
    ];
    for app in &safe_apps {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Launch {
                app: app.to_string(),
            },
        );
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "Safe app name '{}' should be allowed",
            app
        );
    }
}

// ── Protocol coverage tests (from former validator.rs) ───

#[test]
fn test_midi_note_range_valid() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "c".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(report.is_valid());
    assert!(report.coverage.midi.used.contains(&"Note".to_string()));
}

#[test]
fn test_hid_button_range_valid() {
    let config = config_with_mapping(
        Trigger::GamepadButton {
            button: 128,
            velocity_min: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(report.is_valid());
    assert!(
        report
            .coverage
            .hid
            .used
            .contains(&"GamepadButton".to_string())
    );
}

#[test]
fn test_hid_button_in_midi_range_errors() {
    let config = config_with_mapping(
        Trigger::GamepadButton {
            button: 50,
            velocity_min: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    // Unified: now an error (was warning in validator.rs, error in loader.rs)
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("MIDI conflicts"))
    );
}

#[test]
fn test_shell_injection_warning_in_report() {
    // The unified system now treats shell injection as ERROR, not warning
    let config = config_with_mapping(
        Trigger::Note {
            note: 36,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "echo $USER | tee /tmp/out".to_string(),
            args: None,
            timeout_ms: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_coverage_calculation() {
    let config = Config {
        config_meta: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Test".to_string(),
            color: None,
            mappings: vec![
                Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: None,
                        channel: None,
                        device: None,
                    },
                    action: ActionConfig::Keystroke {
                        keys: "c".to_string(),
                        modifiers: vec![],
                    },
                    description: None,
                    let_through: false,
                },
                Mapping {
                    trigger: Trigger::CC {
                        cc: 1,
                        value_min: None,
                        channel: None,
                        device: None,
                    },
                    action: ActionConfig::Keystroke {
                        keys: "v".to_string(),
                        modifiers: vec![],
                    },
                    description: None,
                    let_through: false,
                },
            ],
        }],
        ..default_config()
    };
    let report = validate_config(&config);
    assert_eq!(report.coverage.midi.used.len(), 2);
    // 2 used / 11 available = 18.18% (Raw removed from the available set,
    // ADR-036 Phase 2).
    assert!(report.coverage.midi.percentage > 18.0);
    assert!(report.coverage.midi.percentage < 19.0);
}

#[test]
fn test_send_midi_channel_out_of_range() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 36,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::SendMidi {
            port: "Virtual Output".to_string(),
            channel: 16,
            note: Some(60),
            velocity: Some(100),
            message_type: "NoteOn".to_string(),
            controller: None,
            value: None,
            program: None,
            pitch: None,
            pressure: None,
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(report.errors.iter().any(|e| e.message.contains("channel")));
}

// ── validate_for_loading adapter tests ───────────────────

#[test]
fn test_validate_for_loading_ok() {
    let config = default_config();
    assert!(validate_for_loading(&config).is_ok());
}

#[test]
fn test_validate_for_loading_error() {
    let mut config = default_config();
    config.modes.push(Mode {
        name: "Default".to_string(),
        color: None,
        mappings: vec![],
    });
    let err = validate_for_loading(&config).unwrap_err();
    assert!(err.to_string().contains("Duplicate mode name"));
}

// ── Cross-field validation tests (NEW) ───────────────────

#[test]
fn test_mode_change_references_existing_mode() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::ModeChange {
            mode: "Test".to_string(),
        },
    );
    let report = validate_config(&config);
    assert!(report.is_valid());
}

#[test]
fn test_mode_change_references_nonexistent_mode() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        ActionConfig::ModeChange {
            mode: "NonExistent".to_string(),
        },
    );
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("non-existent mode"))
    );
}

#[test]
fn test_device_reference_undefined_alias() {
    let config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: Some("missing_device".to_string()),
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    // With no devices defined, device refs are allowed (backward compat)
    // But with devices defined and ref not matching, it's an error
    let mut config_with_devices = config;
    config_with_devices.endpoints = vec![ep_input(
        "my_device",
        vec![DeviceMatcher::NameContains {
            value: "Device".to_string(),
        }],
    )];
    let report = validate_config(&config_with_devices);
    // Undefined device alias is a warning (not error) since ListenMode::All
    // auto-discovers devices without needing [[devices]] entries
    assert!(report.is_valid());
    // ADR-035: warning must mention `[[endpoints]]` (the config
    // section that resolves the alias) and the alternative remediation
    // (remove the device filter). Both phrasings must be present so the
    // operator doesn't read the message as a connectivity hint.
    let warning = report
        .warnings
        .iter()
        .find(|w| w.message.contains("Trigger references device alias"))
        .expect("undefined-device-alias warning should fire");
    assert!(
        warning.message.contains("[[endpoints]]"),
        "warning must mention the [[endpoints]] section; got: {}",
        warning.message
    );
    assert!(
        warning.message.contains("remove the `device` filter"),
        "warning must mention the remove-filter remediation; got: {}",
        warning.message
    );
    assert!(
        !warning
            .message
            .contains("will only match if this device connects"),
        "warning must NOT imply this is a connectivity issue; got: {}",
        warning.message
    );
}

#[test]
fn test_device_reference_valid_alias() {
    let mut config = config_with_mapping(
        Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: Some("my_device".to_string()),
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    config.endpoints = vec![ep_input(
        "my_device",
        vec![DeviceMatcher::NameContains {
            value: "Device".to_string(),
        }],
    )];
    let report = validate_config(&config);
    assert!(report.is_valid());
}

// ── Velocity zone overlap warning (from former validator.rs) ──

#[test]
fn test_velocity_zone_overlap_warning() {
    let config = config_with_mapping(
        Trigger::VelocityRange {
            note: 60,
            soft_max: Some(100),
            medium_max: Some(80),
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(report.is_valid()); // warnings don't make it invalid
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("velocity zones overlap"))
    );
}

#[test]
fn test_note_chord_size_warning() {
    let config = config_with_mapping(
        Trigger::NoteChord {
            notes: vec![60],
            timeout_ms: None,
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(report.is_valid()); // single-note chord is valid but warned
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("fewer than 2 notes"))
    );
}

// ── LED config validation tests ─────────────

#[test]
fn test_led_config_valid() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        enabled: true,
        brightness: 100,
        scheme: "reactive".to_string(),
        idle_timeout_secs: 0,
        mode_colors: std::collections::BTreeMap::new(),
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
    });
    let report = validate_config(&config);
    assert!(report.is_valid());
}

#[test]
fn test_led_brightness_too_high() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        enabled: true,
        brightness: 200,
        scheme: "reactive".to_string(),
        idle_timeout_secs: 0,
        mode_colors: std::collections::BTreeMap::new(),
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("brightness"))
    );
}

#[test]
fn test_led_unknown_scheme() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        enabled: true,
        brightness: 100,
        scheme: "disco".to_string(),
        idle_timeout_secs: 0,
        mode_colors: std::collections::BTreeMap::new(),
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("Unknown LED scheme"))
    );
}

#[test]
fn test_led_mode_color_invalid_ref() {
    let mut config = default_config();
    let mut mode_colors = std::collections::BTreeMap::new();
    mode_colors.insert(
        "NonExistentMode".to_string(),
        crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
    );
    config.led = Some(crate::config::types::LedConfig {
        enabled: true,
        brightness: 100,
        scheme: "reactive".to_string(),
        idle_timeout_secs: 0,
        mode_colors,
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("non-existent mode"))
    );
}

#[test]
fn test_led_none_backward_compat() {
    let mut config = default_config();
    config.led = None;
    let report = validate_config(&config);
    assert!(report.is_valid());
}

#[test]
fn test_plugin_action_valid() {
    let config = config_with_action(ActionConfig::Plugin {
        plugin: "my-plugin".to_string(),
        params: serde_json::json!({"key": "value"}),
    });
    let report = validate_config(&config);
    assert!(report.is_valid(), "Valid plugin should pass: {:?}", report);
}

#[test]
fn test_plugin_action_empty_name() {
    let config = config_with_action(ActionConfig::Plugin {
        plugin: "".to_string(),
        params: serde_json::json!({}),
    });
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "Empty plugin name should fail validation"
    );
}

#[test]
fn test_plugin_action_invalid_chars() {
    let config = config_with_action(ActionConfig::Plugin {
        plugin: "my plugin!".to_string(),
        params: serde_json::json!({}),
    });
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "Plugin name with spaces/special chars should fail"
    );
}

#[test]
fn test_plugin_action_null_params_warns() {
    let config = config_with_action(ActionConfig::Plugin {
        plugin: "my-plugin".to_string(),
        params: serde_json::Value::Null,
    });
    let report = validate_config(&config);
    // Should be valid but have a warning
    assert!(report.is_valid());
    assert!(
        !report.warnings.is_empty(),
        "Null params should produce a warning"
    );
}

#[test]
fn test_plugin_action_dot_namespaced() {
    let config = config_with_action(ActionConfig::Plugin {
        plugin: "com.example.my-plugin".to_string(),
        params: serde_json::json!({"key": "value"}),
    });
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "Dot-namespaced plugin names should be valid: {:?}",
        report
    );
}

// ── MIDI LED Config Validation Tests ────────

#[test]
fn test_midi_led_config_valid() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        midi: Some(crate::config::types::MidiLedConfig::default()),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid(), "Valid MIDI LED config: {:?}", report);
}

#[test]
fn test_midi_led_channel_zero() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        midi: Some(crate::config::types::MidiLedConfig {
            channel: 0,
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(report.errors.iter().any(|e| e.message.contains("channel")));
}

#[test]
fn test_midi_led_channel_too_high() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        midi: Some(crate::config::types::MidiLedConfig {
            channel: 17,
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(report.errors.iter().any(|e| e.message.contains("channel")));
}

#[test]
fn test_midi_led_no_config_backward_compat() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid());
}

// ========== HID LED Validation Tests ==========

#[test]
fn test_hid_led_valid_profile() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            hid_profile: Some("mikro-mk3".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_hid_led_unknown_profile() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            hid_profile: Some("unknown-device".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(report.errors.iter().any(|e| e.message.contains("Unknown")));
}

#[test]
fn test_hid_led_no_vendor_no_profile_fails() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            product_id: Some(0x1234),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("vendor_id"))
    );
}

#[test]
fn test_hid_led_explicit_valid() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            vendor_id: Some(0x17CC),
            product_id: Some(0x1700),
            buffer_size: Some(80),
            pad_led_offset: Some(39),
            pad_count: Some(16),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_hid_led_buffer_overflow_fails() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            buffer_size: Some(10),
            pad_led_offset: Some(5),
            pad_count: Some(8),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_hid_led_pad_layout_mismatch() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            hid_profile: Some("mikro-mk3".to_string()),
            pad_count: Some(16),
            pad_layout: Some(vec![0, 1, 2]), // 3 != 16
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("pad_layout"))
    );
}

#[test]
fn test_hid_led_duplicate_pad_layout() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            buffer_size: Some(20),
            pad_led_offset: Some(0),
            pad_count: Some(4),
            pad_layout: Some(vec![0, 1, 1, 3]),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.to_lowercase().contains("duplicate"))
    );
}

#[test]
fn test_hid_led_no_config_backward_compat() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: None,
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid());
}

// ========== Velocity Color Map Validation ==========

#[test]
fn test_velocity_map_valid_default() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap::default()),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_velocity_map_overlapping_ranges() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap {
            ranges: vec![
                crate::config::types::VelocityRange {
                    min: 0,
                    max: 80,
                    color: crate::config::types::RgbColor { r: 0, g: 255, b: 0 },
                },
                crate::config::types::VelocityRange {
                    min: 60, // overlaps with 0-80
                    max: 127,
                    color: crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
                },
            ],
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("Overlapping"))
    );
}

#[test]
fn test_velocity_map_inverted_range() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap {
            ranges: vec![crate::config::types::VelocityRange {
                min: 80,
                max: 40, // inverted
                color: crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
            }],
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_velocity_map_gap_warns() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap {
            ranges: vec![
                crate::config::types::VelocityRange {
                    min: 0,
                    max: 30,
                    color: crate::config::types::RgbColor { r: 0, g: 255, b: 0 },
                },
                crate::config::types::VelocityRange {
                    min: 50, // gap: 31-49 unmapped
                    max: 127,
                    color: crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
                },
            ],
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid()); // gaps are warnings, not errors
    assert!(report.warnings.iter().any(|w| w.message.contains("Gap")));
}

#[test]
fn test_velocity_map_empty_ranges_fails() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap { ranges: vec![] }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_velocity_map_no_config_backward_compat() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: None,
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid());
}

// ── Mutation-gap tests: LED / velocity boundary pinning ──
//
// Each test kills specific cargo-mutants survivors: the existing
// LED-region tests assert only `!is_valid()` + message substrings,
// which lets boundary flips (`>`↔`>=`), early-`return` deletions,
// sort removal, and gap arithmetic mutate undetected. These assert
// exact paths and exact counts instead.

/// Exactly `n` errors whose path == `path`; total error count `total`.
#[track_caller]
fn assert_errors_at(report: &ValidationReport, path: &str, n: usize, total: usize) {
    let at: Vec<_> = report.errors.iter().filter(|e| e.path == path).collect();
    assert_eq!(
        at.len(),
        n,
        "expected {n} error(s) at exact path {path:?}; got {at:?} (all: {:?})",
        report.errors
    );
    assert_eq!(
        report.errors.len(),
        total,
        "expected {total} error(s) total; got {:?}",
        report.errors
    );
}

fn hid_led_config(hid: crate::config::types::HidLedConfig) -> Config {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(hid),
        ..Default::default()
    });
    config
}

fn velocity_config(ranges: Vec<crate::config::types::VelocityRange>) -> Config {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap { ranges }),
        ..Default::default()
    });
    config
}

fn vrange(min: u8, max: u8) -> crate::config::types::VelocityRange {
    crate::config::types::VelocityRange {
        min,
        max,
        color: crate::config::types::RgbColor { r: 0, g: 255, b: 0 },
    }
}

fn rgb() -> crate::config::types::RgbColor {
    crate::config::types::RgbColor { r: 1, g: 2, b: 3 }
}

#[test]
fn test_hid_led_zero_vendor_id_rejected_at_exact_path() {
    // vendor_id: Some(0) must be caught by the explicit `== 0` check —
    // NOT by the resolve_profile Err path (which fires only when the id
    // is absent entirely).
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0),
        product_id: Some(0x1700),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.vendor_id", 1, 1);

    // Boundary: 1 is the smallest valid id.
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(1),
        product_id: Some(0x1700),
        ..Default::default()
    }));
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_hid_led_zero_product_id_rejected_at_exact_path() {
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(0),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.product_id", 1, 1);

    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(1),
        ..Default::default()
    }));
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_hid_led_zero_pad_count_rejected_at_exact_path() {
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(0x1700),
        pad_count: Some(0),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.pad_count", 1, 1);

    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(0x1700),
        pad_count: Some(1),
        pad_layout: Some(vec![0]),
        ..Default::default()
    }));
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_hid_led_palette_64_boundary() {
    // 64 entries fit the 6-bit color index exactly; 65 do not. Pins the
    // `> 64` comparison and the interpolated len() in the message.
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(0x1700),
        color_palette: Some(vec![rgb(); 64]),
        ..Default::default()
    }));
    assert!(
        report.is_valid(),
        "64 entries must pass: {:?}",
        report.errors
    );

    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(0x1700),
        color_palette: Some(vec![rgb(); 65]),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.color_palette", 1, 1);
    assert!(
        report.errors[0].message.contains("65 entries"),
        "message must name the offending size: {:?}",
        report.errors[0].message
    );
}

#[test]
fn test_hid_led_unknown_profile_stops_further_validation() {
    // The unknown-profile error must be the ONLY finding — the early
    // `return` prevents resolve_profile from stacking a second error.
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        hid_profile: Some("no-such-device".to_string()),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.hid_profile", 1, 1);
}

#[test]
fn test_hid_led_checks_fire_independently() {
    // Each post-resolve check is an independent `if` — all four must
    // report on one pass.
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0),
        product_id: Some(0),
        pad_count: Some(0),
        color_palette: Some(vec![rgb(); 65]),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.vendor_id", 1, 4);
    assert_errors_at(&report, "led.hid.product_id", 1, 4);
    assert_errors_at(&report, "led.hid.pad_count", 1, 4);
    assert_errors_at(&report, "led.hid.color_palette", 1, 4);
}

#[test]
fn test_velocity_min_127_boundary() {
    // u8 means only 128..=255 can trip the `> 127` check.
    let report = validate_config(&velocity_config(vec![vrange(128, 130)]));
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.path == "led.velocity_colors.ranges[0]"
                && e.message.contains("min must be 0-127")),
        "min 128 must error at ranges[0]: {:?}",
        report.errors
    );

    let report = validate_config(&velocity_config(vec![vrange(127, 127)]));
    assert!(report.is_valid(), "min 127 is legal: {:?}", report.errors);
}

#[test]
fn test_velocity_max_127_boundary_and_index_interpolation() {
    // Second range bad → the error path must say ranges[1], pinning the
    // index interpolation.
    let report = validate_config(&velocity_config(vec![vrange(0, 100), vrange(101, 128)]));
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.path == "led.velocity_colors.ranges[1]"
                && e.message.contains("max must be 0-127")),
        "max 128 in second range must error at ranges[1]: {:?}",
        report.errors
    );

    let report = validate_config(&velocity_config(vec![vrange(0, 100), vrange(101, 127)]));
    assert!(report.is_valid(), "max 127 is legal: {:?}", report.errors);
}

#[test]
fn test_velocity_min_equals_max_is_legal() {
    // Pins `min > max` (not >=): a single-velocity range is valid.
    let report = validate_config(&velocity_config(vec![
        vrange(0, 3),
        vrange(4, 4),
        vrange(5, 127),
    ]));
    assert!(
        report.is_valid(),
        "min == max is legal: {:?}",
        report.errors
    );
    assert!(
        report.warnings.is_empty(),
        "no gaps here: {:?}",
        report.warnings
    );

    let report = validate_config(&velocity_config(vec![vrange(5, 4)]));
    assert_errors_at(&report, "led.velocity_colors.ranges[0]", 1, 1);
}

#[test]
fn test_velocity_touching_ranges_overlap_exactly_once() {
    // prev.max >= next.min: touching at 40 IS an overlap; adjacent
    // 39/40 is NOT (and produces no gap warning either). Pins `>=`
    // in both directions and the else-chain.
    let report = validate_config(&velocity_config(vec![vrange(0, 40), vrange(40, 127)]));
    assert_errors_at(&report, "led.velocity_colors.ranges", 1, 1);
    assert_eq!(
        report.errors[0].message, "Overlapping ranges: [0-40] and [40-127]",
        "exact overlap message pins the interpolated bounds"
    );
    assert!(
        report.warnings.is_empty(),
        "overlap must not also warn: {:?}",
        report.warnings
    );

    let report = validate_config(&velocity_config(vec![vrange(0, 39), vrange(40, 127)]));
    assert!(
        report.is_valid(),
        "adjacent ranges are clean: {:?}",
        report.errors
    );
    assert!(
        report.warnings.is_empty(),
        "adjacent ranges leave no gap: {:?}",
        report.warnings
    );
}

#[test]
fn test_velocity_overlap_detected_after_sorting() {
    // Input deliberately in REVERSE min-order: the overlap is only
    // visible after sort_by_key, and the message order proves the sort
    // ran (prev = the lower range).
    let report = validate_config(&velocity_config(vec![vrange(60, 127), vrange(0, 80)]));
    assert_errors_at(&report, "led.velocity_colors.ranges", 1, 1);
    assert_eq!(
        report.errors[0].message, "Overlapping ranges: [0-80] and [60-127]",
        "sorted order must put the lower range first"
    );
}

#[test]
fn test_velocity_gap_message_pins_arithmetic() {
    // Single-value gap: 31..31. The exact message pins prev.max+1 and
    // next.min-1; the empty-errors + is_valid asserts pin gap-as-warning.
    let report = validate_config(&velocity_config(vec![vrange(0, 30), vrange(32, 127)]));
    assert!(report.is_valid());
    assert!(
        report.errors.is_empty(),
        "gaps are warnings: {:?}",
        report.errors
    );
    assert_eq!(
        report.warnings.len(),
        1,
        "exactly one gap: {:?}",
        report.warnings
    );
    assert!(
        report.warnings[0]
            .message
            .contains("Gap in velocity coverage: 31-31 is unmapped"),
        "gap bounds must be exact: {:?}",
        report.warnings[0].message
    );

    // Off-by-one the other way: 31 meets 0-30 exactly — no gap.
    let report = validate_config(&velocity_config(vec![vrange(0, 30), vrange(31, 127)]));
    assert!(
        report.warnings.is_empty(),
        "no gap at exact adjacency: {:?}",
        report.warnings
    );
}

#[test]
fn test_velocity_two_gaps_two_warnings() {
    // Three ranges, two gaps — pins the windows(2) pairing.
    let report = validate_config(&velocity_config(vec![
        vrange(0, 10),
        vrange(20, 30),
        vrange(40, 50),
    ]));
    assert!(report.is_valid());
    assert_eq!(
        report.warnings.len(),
        2,
        "one warning per gap: {:?}",
        report.warnings
    );
}

#[test]
fn test_velocity_empty_ranges_single_error() {
    // Exactly one error (the early return prevents anything further).
    let report = validate_config(&velocity_config(vec![]));
    assert_errors_at(&report, "led.velocity_colors.ranges", 1, 1);
}

// ── Mutation-gap tests: MIDI LED boundaries (shard survivors) ──

fn midi_led_config(midi: crate::config::types::MidiLedConfig) -> Config {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        midi: Some(midi),
        ..Default::default()
    });
    config
}

fn note_on(note: u8, velocity: u8) -> crate::config::types::MidiLedMessage {
    crate::config::types::MidiLedMessage::NoteOn { note, velocity }
}

fn custom_mapping(
    pad: u8,
    led_on: crate::config::types::MidiLedMessage,
    led_off: crate::config::types::MidiLedMessage,
) -> crate::config::types::MidiLedCustomMapping {
    crate::config::types::MidiLedCustomMapping {
        pad,
        led_on,
        led_off,
    }
}

#[test]
fn test_midi_led_channel_16_boundary() {
    // Channel 16 is the last legal value; 17 trips `> 16`.
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        channel: 16,
        ..Default::default()
    }));
    assert!(report.is_valid(), "channel 16 legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        channel: 17,
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.channel", 1, 1);
    assert!(report.errors[0].message.contains("got 17"));
}

#[test]
fn test_midi_led_note_velocities_127_boundary() {
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        note_on_velocity: 127,
        note_off_velocity: 127,
        ..Default::default()
    }));
    assert!(report.is_valid(), "127 legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        note_on_velocity: 128,
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.note_on_velocity", 1, 1);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        note_off_velocity: 128,
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.note_off_velocity", 1, 1);
}

#[test]
fn test_midi_led_color_velocity_127_boundary_at_named_path() {
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        colors: crate::config::types::MidiLedColors {
            amber: 127,
            ..Default::default()
        },
        ..Default::default()
    }));
    assert!(report.is_valid(), "127 legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        colors: crate::config::types::MidiLedColors {
            amber: 128,
            ..Default::default()
        },
        ..Default::default()
    }));
    // Exact per-color path pins the loop's name interpolation.
    assert_errors_at(&report, "led.midi.colors.amber", 1, 1);
    assert!(report.errors[0].message.contains("got 128"));
}

#[test]
fn test_midi_led_custom_mapping_pad_127_boundary() {
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(127, note_on(1, 1), note_on(1, 0))],
        ..Default::default()
    }));
    assert!(report.is_valid(), "pad 127 legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(128, note_on(1, 1), note_on(1, 0))],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[0]", 1, 1);
    assert!(report.errors[0].message.contains("got 128"));
}

#[test]
fn test_midi_led_duplicate_pad_detection_polarity() {
    // Distinct pads → clean (kills `delete !`, which would flag every
    // insert as a duplicate); same pad twice → exactly one error, at
    // the SECOND occurrence's index.
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![
            custom_mapping(1, note_on(1, 1), note_on(1, 0)),
            custom_mapping(2, note_on(2, 1), note_on(2, 0)),
        ],
        ..Default::default()
    }));
    assert!(
        report.is_valid(),
        "distinct pads legal: {:?}",
        report.errors
    );

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![
            custom_mapping(7, note_on(1, 1), note_on(1, 0)),
            custom_mapping(7, note_on(2, 1), note_on(2, 0)),
        ],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[1]", 1, 1);
    assert!(
        report.errors[0]
            .message
            .contains("Duplicate custom mapping for pad 7")
    );
}

#[test]
fn test_midi_led_message_note_and_velocity_boundaries() {
    // NoteOn arm: note and velocity each pinned at 127/128, with the
    // led_on / led_off sub-path exact. Any error here also kills the
    // replace-whole-fn-with-() mutant on validate_midi_led_message.
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, note_on(127, 127), note_on(127, 127))],
        ..Default::default()
    }));
    assert!(report.is_valid(), "127s legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, note_on(128, 0), note_on(0, 0))],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[0].led_on", 1, 1);
    assert!(
        report.errors[0]
            .message
            .contains("Note must be 0-127, got 128")
    );

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, note_on(0, 0), note_on(0, 128))],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[0].led_off", 1, 1);
    assert!(
        report.errors[0]
            .message
            .contains("Velocity must be 0-127, got 128")
    );
}

#[test]
fn test_midi_led_message_cc_boundaries() {
    use crate::config::types::MidiLedMessage;
    let cc = |cc: u8, value: u8| MidiLedMessage::Cc { cc, value };

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, cc(127, 127), cc(0, 0))],
        ..Default::default()
    }));
    assert!(report.is_valid(), "127s legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, cc(128, 0), cc(0, 0))],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[0].led_on", 1, 1);
    assert!(
        report.errors[0]
            .message
            .contains("CC must be 0-127, got 128")
    );

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, cc(0, 0), cc(0, 128))],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[0].led_off", 1, 1);
    assert!(
        report.errors[0]
            .message
            .contains("Value must be 0-127, got 128")
    );
}

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

/// Helper: a `[[endpoints]]` Matcher endpoint with the given direction and
/// asymmetric matcher sets (ADR-035 direction↔matcher checks).
fn endpoint_with_matchers(
    alias: &str,
    direction: crate::config::types::ConnectorDirection,
    input_matchers: Vec<DeviceMatcher>,
    output_matchers: Vec<DeviceMatcher>,
) -> crate::config::types::EndpointConfig {
    crate::config::types::EndpointConfig {
        alias: alias.to_string(),
        direction,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: crate::config::types::EndpointKind::Matcher {
            matchers: vec![],
            input_matchers,
            output_matchers,
            no_probe: false,
        },
    }
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

/// Build a HID `Matcher` endpoint with the given direction.
fn hid_endpoint(
    alias: &str,
    direction: crate::config::types::ConnectorDirection,
) -> crate::config::types::EndpointConfig {
    crate::config::types::EndpointConfig {
        alias: alias.to_string(),
        direction,
        protocol: Some(crate::config::types::ConnectorProtocol::Hid),
        description: None,
        enabled: true,
        channels: vec![],
        kind: crate::config::types::EndpointKind::Matcher {
            matchers: vec![DeviceMatcher::name_contains("Xbox")],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    }
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

/// A MIDI **output** endpoint for HID→MIDI route tests (mirrors
/// `hid_endpoint`'s shape with `protocol = Midi`).
fn midi_output_endpoint(alias: &str) -> crate::config::types::EndpointConfig {
    crate::config::types::EndpointConfig {
        alias: alias.to_string(),
        direction: crate::config::types::ConnectorDirection::Output,
        protocol: Some(crate::config::types::ConnectorProtocol::Midi),
        description: None,
        enabled: true,
        channels: vec![],
        kind: crate::config::types::EndpointKind::Matcher {
            matchers: vec![DeviceMatcher::name_contains("Synth")],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    }
}

fn hid_to_midi_route(channel: u8, cc: u8) -> crate::config::types::RouteConfig {
    use std::collections::HashMap;
    let mut trigger_to_cc = HashMap::new();
    trigger_to_cc.insert("south".to_string(), cc);
    crate::config::types::RouteConfig {
        from: "xbox".to_string(),
        to: "synth".to_string(),
        transform: Some(crate::config::types::SignalTransform::HidToMidi {
            trigger_to_cc,
            channel,
        }),
        filter: None,
        enabled: true,
        description: None,
        modes: vec![],
    }
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

fn osc_output_endpoint(alias: &str) -> crate::config::types::EndpointConfig {
    crate::config::types::EndpointConfig {
        alias: alias.to_string(),
        direction: crate::config::types::ConnectorDirection::Output,
        protocol: None, // OscEndpoint kind ⇒ effective protocol = Osc
        description: None,
        enabled: true,
        channels: vec![],
        kind: crate::config::types::EndpointKind::OscEndpoint {
            host: "127.0.0.1".to_string(),
            port: 9000,
            security: Default::default(),
        },
    }
}

fn hid_to_osc_route(address: &str) -> crate::config::types::RouteConfig {
    use std::collections::HashMap;
    let mut trigger_to_address = HashMap::new();
    trigger_to_address.insert("south".to_string(), address.to_string());
    crate::config::types::RouteConfig {
        from: "xbox".to_string(),
        to: "osc_out".to_string(),
        transform: Some(crate::config::types::SignalTransform::HidToOsc {
            trigger_to_address,
            value_to_float: true,
        }),
        filter: None,
        enabled: true,
        description: None,
        modes: vec![],
    }
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

// ─── ADR-025 Phase 2: validate_condition ─────────

fn make_config_with_condition(condition: crate::actions::Condition) -> Config {
    use crate::config::types::{ActionConfig, Mapping, Mode, Trigger};
    let mut cfg = default_config();
    // Declare the "keyboard" alias the existing tests use so the
    // device-known check (added in ADR-025 Phase 2.G) doesn't
    // mask the bounds-error assertions these tests actually care
    // about. Device-unknown behaviour has dedicated coverage in
    // the 2.G test block below.
    cfg.endpoints = vec![ep_input(
        "keyboard",
        vec![DeviceMatcher::NameContains {
            value: "keyboard".to_string(),
        }],
    )];
    cfg.modes = vec![Mode {
        name: "Default".into(),
        color: None,
        mappings: vec![Mapping {
            trigger: Trigger::Note {
                note: 36,
                velocity_min: Some(1),
                channel: None,
                device: None,
            },
            action: ActionConfig::Conditional {
                condition,
                then_action: Box::new(ActionConfig::Shell {
                    sandbox: None,
                    command: "echo then".into(),
                    args: None,
                    timeout_ms: None,
                }),
                else_action: None,
            },
            description: None,
            let_through: false,
        }],
    }];
    cfg
}

#[test]
fn validator_rejects_cc_value_in_range_with_min_greater_than_max() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcValueInRange {
        cc: 1,
        channel: 0,
        min: 80,
        max: 20,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("min") && e.message.contains("max")),
        "expected min>max error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_value_in_range_with_out_of_range_bounds() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcValueInRange {
        cc: 1,
        channel: 0,
        min: 0,
        max: 200,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(report.errors.iter().any(|e| e.message.contains("max")));
}

#[test]
fn validator_rejects_active_pc_is_missing_device() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::ActivePcIs {
        pc: 12,
        channel: 0,
        device: "".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("device")),
        "expected device-required error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_note_held_out_of_range() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::NoteHeld {
        note: 200,
        channel: 0,
        device: "keyboard".into(),
        ttl_override_ms: None,
    });
    let report = validate_config(&cfg);
    assert!(report.errors.iter().any(|e| e.message.contains("note")));
}

#[test]
fn validator_recurses_into_and_or_not() {
    use crate::actions::Condition;
    // Nested broken condition inside And should still surface.
    let cfg = make_config_with_condition(Condition::And {
        conditions: vec![
            Condition::Always,
            Condition::CcValueInRange {
                cc: 1,
                channel: 0,
                min: 80,
                max: 20,
                device: "keyboard".into(),
            },
        ],
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("min") && e.message.contains("max")),
        "And should recurse into children; errors: {:?}",
        report.errors
    );
}

#[test]
fn validator_accepts_well_formed_state_conditions() {
    use crate::actions::Condition;
    // "keyboard" is the alias seeded by `make_config_with_condition`.
    let cfg = make_config_with_condition(Condition::ActivePcIs {
        pc: 12,
        channel: 0,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "well-formed condition must pass, got errors: {:?}",
        report.errors
    );
}

// ─── ADR-025 Phase 2.C: CcIsOn / CcIsOff sugar ──────────────────

#[test]
fn validator_accepts_well_formed_cc_is_on() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOn {
        cc: 64,
        channel: 0,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "CcIsOn must pass, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_accepts_well_formed_cc_is_off() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOff {
        cc: 64,
        channel: 0,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(report.errors.is_empty());
}

#[test]
fn validator_rejects_cc_is_on_missing_device() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOn {
        cc: 64,
        channel: 0,
        device: "".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOn") && e.message.contains("device")),
        "expected CcIsOn device error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_is_off_out_of_range() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOff {
        cc: 200, // out of range
        channel: 0,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOff") && e.message.contains("cc")),
        "expected CcIsOff cc bounds error, got: {:?}",
        report.errors
    );
}

// ─── Symmetric coverage ──────────────────────────
//
// The shared validator arm uses a `matches!(condition, CcIsOn {..})`
// check to pick which of the two error-message kinds to emit. These
// tests pin both branches of that selection so a copy-paste mistake
// — e.g. swapping `CcIsOn`/`CcIsOff` in the matches arm or in the
// emitted message prefix — surfaces immediately instead of leaking
// through unnoticed.

#[test]
fn validator_rejects_cc_is_off_missing_device() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOff {
        cc: 64,
        channel: 0,
        device: "".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOff") && e.message.contains("device")),
        "expected CcIsOff device error (not CcIsOn), got: {:?}",
        report.errors
    );
    // Explicitly guard against the wrong kind being emitted.
    assert!(
        !report.errors.iter().any(|e| e.message.contains("CcIsOn")),
        "CcIsOff error should not mention CcIsOn; got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_is_on_out_of_range() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOn {
        cc: 200, // out of range
        channel: 0,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOn") && e.message.contains("cc")),
        "expected CcIsOn cc bounds error, got: {:?}",
        report.errors
    );
    assert!(
        !report.errors.iter().any(|e| e.message.contains("CcIsOff")),
        "CcIsOn error should not mention CcIsOff; got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_is_on_bad_channel() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOn {
        cc: 64,
        channel: 42, // out of range
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOn") && e.message.contains("channel")),
        "expected CcIsOn channel error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_is_off_bad_channel() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOff {
        cc: 64,
        channel: 42, // out of range
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOff") && e.message.contains("channel")),
        "expected CcIsOff channel error, got: {:?}",
        report.errors
    );
}

// ─── ADR-025 Phase 2.G: context-switch validator ──────────
//
// Two new classes of check on top of the field-level bounds that
// already shipped in 2.D:
//
//   1. CcContextSwitch range overlap detection — first-match-wins
//      at runtime, so overlapping ranges silently mask later
//      branches. Detect pairwise, order-independent.
//   2. Device alias resolution — unknown device in a condition or
//      context-switch is an ERROR (stronger than trigger's WARNING,
//      because state conditions can't observe a device that isn't
//      bound to the store).

fn device_identity(alias: &str) -> EndpointConfig {
    ep_input(
        alias,
        vec![DeviceMatcher::NameContains {
            value: alias.to_string(),
        }],
    )
}

fn make_config_with_action_and_devices(
    action: ActionConfig,
    devices: Vec<EndpointConfig>,
) -> Config {
    let mut cfg = default_config();
    cfg.endpoints = devices;
    cfg.modes = vec![Mode {
        name: "Default".into(),
        color: None,
        mappings: vec![Mapping {
            trigger: Trigger::Note {
                note: 36,
                velocity_min: Some(1),
                channel: None,
                device: None,
            },
            action,
            description: None,
            let_through: false,
        }],
    }];
    cfg
}

fn make_config_with_condition_and_devices(
    condition: crate::actions::Condition,
    devices: Vec<EndpointConfig>,
) -> Config {
    make_config_with_action_and_devices(
        ActionConfig::Conditional {
            condition,
            then_action: Box::new(ActionConfig::Shell {
                sandbox: None,
                command: "echo ok".into(),
                args: None,
                timeout_ms: None,
            }),
            else_action: None,
        },
        devices,
    )
}

// ── Device alias resolution: context-switch actions ───────────────

#[test]
fn validator_rejects_unknown_device_in_pc_context_switch() {
    use indexmap::IndexMap;
    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    mappings.insert(
        0,
        Box::new(ActionConfig::Shell {
            sandbox: None,
            command: "echo a".into(),
            args: None,
            timeout_ms: None,
        }),
    );
    let cfg = make_config_with_action_and_devices(
        ActionConfig::PcContextSwitch {
            channel: 0,
            device: "unknown_alias".into(),
            mappings,
            default: None,
        },
        vec![], // no devices declared
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| {
            e.message.contains("PcContextSwitch")
                && e.message.contains("unknown device")
                && e.message.contains("unknown_alias")
        }),
        "expected unknown-device error, got errors: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_unknown_device_in_cc_context_switch() {
    use crate::config::types::CcRange;
    let cfg = make_config_with_action_and_devices(
        ActionConfig::CcContextSwitch {
            cc: 1,
            channel: 0,
            device: "ghost".into(),
            ranges: vec![CcRange {
                min: 0,
                max: 63,
                action: Box::new(ActionConfig::Shell {
                    sandbox: None,
                    command: "echo a".into(),
                    args: None,
                    timeout_ms: None,
                }),
            }],
            default: None,
        },
        vec![],
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| {
            e.message.contains("CcContextSwitch")
                && e.message.contains("unknown device")
                && e.message.contains("ghost")
        }),
        "expected unknown-device error, got errors: {:?}",
        report.errors
    );
}

#[test]
fn validator_accepts_known_device_in_context_switch_and_condition() {
    use crate::actions::Condition;
    use crate::config::types::CcRange;
    let cfg = make_config_with_action_and_devices(
        ActionConfig::Conditional {
            condition: Condition::CcIsOn {
                cc: 64,
                channel: 0,
                device: "fcb1010".into(),
            },
            then_action: Box::new(ActionConfig::CcContextSwitch {
                cc: 7,
                channel: 0,
                device: "fcb1010".into(),
                ranges: vec![CcRange {
                    min: 0,
                    max: 127,
                    action: Box::new(ActionConfig::Shell {
                        sandbox: None,
                        command: "echo a".into(),
                        args: None,
                        timeout_ms: None,
                    }),
                }],
                default: None,
            }),
            else_action: None,
        },
        vec![device_identity("fcb1010")],
    );
    let report = validate_config(&cfg);
    assert!(
        !report
            .errors
            .iter()
            .any(|e| e.message.contains("unknown device")),
        "expected no unknown-device error, got: {:?}",
        report.errors
    );
}

// ── Device alias resolution: state conditions ─────────────────────

#[test]
fn validator_rejects_unknown_device_in_active_pc_is() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition_and_devices(
        Condition::ActivePcIs {
            pc: 0,
            channel: 0,
            device: "phantom".into(),
        },
        vec![],
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("ActivePcIs")
                && e.message.contains("unknown device")
                && e.message.contains("phantom")),
        "expected ActivePcIs unknown-device error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_unknown_device_in_cc_value_in_range() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition_and_devices(
        Condition::CcValueInRange {
            cc: 1,
            channel: 0,
            min: 0,
            max: 63,
            device: "phantom".into(),
        },
        vec![],
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| {
            e.message.contains("CcValueInRange")
                && e.message.contains("unknown device")
                && e.message.contains("phantom")
        }),
        "expected CcValueInRange unknown-device error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_unknown_device_in_note_held() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition_and_devices(
        Condition::NoteHeld {
            note: 60,
            channel: 0,
            device: "phantom".into(),
            ttl_override_ms: None,
        },
        vec![],
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("NoteHeld")
            && e.message.contains("unknown device")
            && e.message.contains("phantom")),
        "expected NoteHeld unknown-device error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_unknown_device_in_cc_is_on_off() {
    use crate::actions::Condition;
    for cond in [
        Condition::CcIsOn {
            cc: 64,
            channel: 0,
            device: "phantom".into(),
        },
        Condition::CcIsOff {
            cc: 64,
            channel: 0,
            device: "phantom".into(),
        },
    ] {
        let name = match cond {
            Condition::CcIsOn { .. } => "CcIsOn",
            Condition::CcIsOff { .. } => "CcIsOff",
            _ => unreachable!(),
        };
        let cfg = make_config_with_condition_and_devices(cond, vec![]);
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| {
                e.message.contains(name)
                    && e.message.contains("unknown device")
                    && e.message.contains("phantom")
            }),
            "expected {} unknown-device error, got: {:?}",
            name,
            report.errors
        );
    }
}

// ── CcContextSwitch range overlap detection ───────────────────────

fn cc_switch_with_ranges(ranges: Vec<(u8, u8)>) -> Config {
    use crate::config::types::CcRange;
    make_config_with_action_and_devices(
        ActionConfig::CcContextSwitch {
            cc: 1,
            channel: 0,
            device: "fcb1010".into(),
            ranges: ranges
                .into_iter()
                .map(|(min, max)| CcRange {
                    min,
                    max,
                    action: Box::new(ActionConfig::Shell {
                        sandbox: None,
                        command: "echo a".into(),
                        args: None,
                        timeout_ms: None,
                    }),
                })
                .collect(),
            default: None,
        },
        vec![device_identity("fcb1010")],
    )
}

#[test]
fn validator_rejects_overlapping_cc_ranges_contained() {
    // [0, 100] fully contains [20, 40] — overlap error.
    let cfg = cc_switch_with_ranges(vec![(0, 100), (20, 40)]);
    let report = validate_config(&cfg);
    let overlap = report
        .errors
        .iter()
        .find(|e| {
            e.message.contains("CcContextSwitch")
                && e.message.contains("overlap")
                && e.message.contains("[0]")
                && e.message.contains("[1]")
        })
        .unwrap_or_else(|| {
            panic!(
                "expected overlap error naming both range indices, got: {:?}",
                report.errors
            )
        });
    // Anchored to the later (masked) range so UIs can point
    // directly at the branch that will never fire.
    assert!(
        overlap.path.ends_with(".ranges[1]"),
        "overlap error should anchor to the masked range path, got path: {:?}",
        overlap.path
    );
}

#[test]
fn validator_rejects_overlapping_cc_ranges_partial() {
    // [0, 50] partially overlaps [40, 80] at 40-50.
    let cfg = cc_switch_with_ranges(vec![(0, 50), (40, 80)]);
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("overlap")),
        "expected partial overlap error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_overlapping_cc_ranges_unsorted_pair() {
    // Non-adjacent overlap in an unsorted list:
    // [0,20], [60,127], [10,15] — (10,15) overlaps (0,20) even
    // though they aren't adjacent in the Vec. The spec's naive
    // "prev_max only" check would miss this; we detect pairwise.
    let cfg = cc_switch_with_ranges(vec![(0, 20), (60, 127), (10, 15)]);
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("overlap")),
        "expected non-adjacent overlap error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_overlapping_cc_ranges_identical() {
    let cfg = cc_switch_with_ranges(vec![(0, 50), (0, 50)]);
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("overlap")),
        "expected identical-range overlap error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_accepts_adjacent_non_overlapping_cc_ranges() {
    // [0,49] then [50,99] then [100,127] — touching at min-1/max
    // boundary but non-overlapping. Valid config.
    let cfg = cc_switch_with_ranges(vec![(0, 49), (50, 99), (100, 127)]);
    let report = validate_config(&cfg);
    assert!(
        !report.errors.iter().any(|e| e.message.contains("overlap")),
        "expected no overlap error for adjacent ranges, got: {:?}",
        report.errors
    );
}

// ═══════════════════════════════════════════════════════════════════
// ADR-027 D3 §3.2 — `allow_interpreters` policy
//
// The validator runs `resolve_effective_binary` against every Shell
// action; when the resolved binary is a known interpreter family
// and `advanced_settings.allow_interpreters` is `Deny` or `Warn`,
// emits a config-load diagnostic. `Allow` is the opt-in escape
// hatch for power users who deliberately rely on shell scripting.
// ═══════════════════════════════════════════════════════════════════

use crate::config::types::InterpreterPolicy;

fn config_with_action_and_policy(action: ActionConfig, policy: InterpreterPolicy) -> Config {
    let mut config = config_with_action(action);
    config.advanced_settings.allow_interpreters = policy;
    config
}

#[test]
fn allow_interpreters_default_is_warn() {
    // Default for new configs and `..default_config()` builders —
    // backwards-compat without users opting in, but the warning
    // surfaces the new gate so they're aware of the policy.
    let settings = crate::config::types::AdvancedSettings::default();
    assert_eq!(settings.allow_interpreters, InterpreterPolicy::Warn);
}

#[test]
fn allow_interpreters_warn_emits_warning_for_sh() {
    // Default policy (Warn): `/bin/sh -c …` should produce a
    // validation WARNING, not an error. Config still loads.
    let config = config_with_action_and_policy(
        ActionConfig::Shell {
            sandbox: None,
            command: "/bin/sh".to_string(),
            args: Some(vec!["-c".to_string(), "echo hi".to_string()]),
            timeout_ms: None,
        },
        InterpreterPolicy::Warn,
    );
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "Warn policy keeps the config valid — got errors: {:?}",
        report.errors
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("interpreter")
                && (w.message.contains("sh") || w.message.contains("Sh"))),
        "expected an interpreter-family warning — got warnings: {:?}",
        report
            .warnings
            .iter()
            .map(|w| &w.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn allow_interpreters_deny_emits_error_for_sh() {
    let config = config_with_action_and_policy(
        ActionConfig::Shell {
            sandbox: None,
            command: "/bin/sh".to_string(),
            args: Some(vec!["-c".to_string(), "echo hi".to_string()]),
            timeout_ms: None,
        },
        InterpreterPolicy::Deny,
    );
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "Deny policy must reject interpreter invocations"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("interpreter")),
        "expected an interpreter-family error — got: {:?}",
        report.errors
    );
}

#[test]
fn allow_interpreters_allow_emits_no_finding_for_sh() {
    // Explicit opt-in: users who know they want shell semantics.
    // No warning, no error.
    let config = config_with_action_and_policy(
        ActionConfig::Shell {
            sandbox: None,
            command: "/bin/sh".to_string(),
            args: Some(vec!["-c".to_string(), "echo hi".to_string()]),
            timeout_ms: None,
        },
        InterpreterPolicy::Allow,
    );
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "Allow policy keeps the config valid — got errors: {:?}",
        report.errors
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.message.contains("interpreter")),
        "Allow policy must NOT emit an interpreter warning — got: {:?}",
        report
            .warnings
            .iter()
            .map(|w| &w.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn allow_interpreters_warn_fires_on_env_wrapper() {
    // Wrapper unwinding: `env python -c …` resolves to python →
    // warn. Closes the canonical D3 bypass class.
    let config = config_with_action_and_policy(
        ActionConfig::Shell {
            sandbox: None,
            command: "/usr/bin/env".to_string(),
            args: Some(vec![
                "python".to_string(),
                "-c".to_string(),
                "print(1)".to_string(),
            ]),
            timeout_ms: None,
        },
        InterpreterPolicy::Warn,
    );
    let report = validate_config(&config);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("interpreter")
                && w.message.to_lowercase().contains("python")),
        "expected a Python interpreter warning — got: {:?}",
        report
            .warnings
            .iter()
            .map(|w| &w.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn allow_interpreters_does_not_fire_on_non_interpreter_binary() {
    // `/bin/ls` is not an interpreter — no finding regardless of
    // policy. Verifies the resolver classified correctly + the
    // policy is gated on `family.is_some()`.
    for policy in [
        InterpreterPolicy::Allow,
        InterpreterPolicy::Warn,
        InterpreterPolicy::Deny,
    ] {
        let config = config_with_action_and_policy(
            ActionConfig::Shell {
                sandbox: None,
                command: "/bin/ls -la".to_string(),
                args: None,
                timeout_ms: None,
            },
            policy,
        );
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "non-interpreter binary should never trip the policy ({:?}) — got: {:?}",
            policy,
            report.errors
        );
        assert!(
            !report
                .warnings
                .iter()
                .any(|w| w.message.contains("interpreter")),
            "non-interpreter binary should never trip the policy ({:?}) — got warnings: {:?}",
            policy,
            report
                .warnings
                .iter()
                .map(|w| &w.message)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn allow_interpreters_warn_message_includes_resolved_binary_path() {
    // Diagnostic UX: the warning should name the resolved binary
    // so users can find it (Phase 3 GUI editor will key its chip
    // off this same data).
    let config = config_with_action_and_policy(
        ActionConfig::Shell {
            sandbox: None,
            command: "/usr/bin/env".to_string(),
            args: Some(vec![
                "python".to_string(),
                "-c".to_string(),
                "1".to_string(),
            ]),
            timeout_ms: None,
        },
        InterpreterPolicy::Warn,
    );
    let report = validate_config(&config);
    let interpreter_warning = report
        .warnings
        .iter()
        .find(|w| w.message.contains("interpreter"))
        .expect("expected an interpreter warning");
    assert!(
        interpreter_warning.message.contains("python"),
        "warning should name `python` — got: {:?}",
        interpreter_warning.message
    );
}

// ── Shadowed-mapping detection ─────────────────────────────
//
// The rule engine matches first-match-wins. If two mappings in the
// same mode have overlapping triggers and the broader one appears
// first, the narrower one never fires. These tests pin the shadow
// detection on the four trigger types covered in v1: Note, CC,
// Aftertouch, PolyAftertouch. Cross-type pairs and uncovered
// variants must not produce false positives.

fn note_trigger(
    note: u8,
    velocity_min: Option<u8>,
    channel: Option<u8>,
    device: Option<&str>,
) -> Trigger {
    Trigger::Note {
        note,
        velocity_min,
        channel,
        device: device.map(String::from),
    }
}

fn config_with_mappings(triggers: Vec<Trigger>) -> Config {
    Config {
        config_meta: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Test".to_string(),
            color: None,
            mappings: triggers
                .into_iter()
                .enumerate()
                .map(|(i, t)| Mapping {
                    trigger: t,
                    action: ActionConfig::Keystroke {
                        keys: format!("k{i}"),
                        modifiers: vec![],
                    },
                    description: Some(format!("mapping-{i}")),
                    let_through: false,
                })
                .collect(),
        }],
        ..default_config()
    }
}

fn shadow_warnings(report: &ValidationReport) -> Vec<&str> {
    report
        .warnings
        .iter()
        .filter(|w| w.message.contains("shadowed by mapping #"))
        .map(|w| w.message.as_str())
        .collect()
}

#[test]
fn test_shadow_exact_duplicate_note_triggers_warns() {
    // The issue's primary example: two mappings with identical Note
    // triggers — the second never fires.
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, None),
        note_trigger(60, None, None, None),
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert_eq!(
        warnings.len(),
        1,
        "exact duplicate must produce exactly one shadow warning, got: {warnings:?}"
    );
    assert!(
        warnings[0].contains("shadowed by mapping #0"),
        "warning must point at the earlier shadowing mapping; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_velocity_min_subset_warns() {
    // Note{velocity_min:None} accepts any velocity; Note{velocity_min:80}
    // only accepts ≥80 — strict subset, so the second is shadowed.
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, None),
        note_trigger(60, Some(80), None, None),
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert_eq!(
        warnings.len(),
        1,
        "velocity-min subset must shadow; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_device_filter_subset_warns() {
    // Note without device filter (matches any device) covers Note that
    // requires device "mpk".
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, None),
        note_trigger(60, None, None, Some("mpk")),
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert_eq!(
        warnings.len(),
        1,
        "no-device-filter must shadow specific-device-filter; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_disjoint_devices_no_warn() {
    // Two specific-device-filter triggers on different devices are
    // disjoint — neither shadows the other.
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, Some("mpk")),
        note_trigger(60, None, None, Some("mikro")),
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert!(
        warnings.is_empty(),
        "disjoint device filters must NOT shadow; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_different_notes_no_warn() {
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, None),
        note_trigger(61, None, None, None),
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert!(
        warnings.is_empty(),
        "different note numbers must NOT shadow; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_cross_type_no_warn() {
    // A Note trigger and a CC trigger never overlap — different
    // ProcessedEvent types.
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, None),
        Trigger::CC {
            cc: 7,
            value_min: None,
            channel: None,
            device: None,
        },
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert!(
        warnings.is_empty(),
        "cross-type triggers must NOT shadow; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_cc_value_min_subset_warns() {
    let cfg = config_with_mappings(vec![
        Trigger::CC {
            cc: 7,
            value_min: None,
            channel: None,
            device: None,
        },
        Trigger::CC {
            cc: 7,
            value_min: Some(64),
            channel: None,
            device: None,
        },
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert_eq!(
        warnings.len(),
        1,
        "CC value_min subset must shadow; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_unanalyzed_variant_no_warn() {
    // LongPress is intentionally not analysed in v1 — the
    // conservative default returns false. Two identical LongPress
    // triggers must NOT raise a shadow warning until subset rules
    // for that variant are added.
    let cfg = config_with_mappings(vec![
        Trigger::LongPress {
            note: 60,
            duration_ms: Some(2000),
            channel: None,
            device: None,
        },
        Trigger::LongPress {
            note: 60,
            duration_ms: Some(2000),
            channel: None,
            device: None,
        },
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert!(
        warnings.is_empty(),
        "unanalyzed variant must NOT shadow in v1; got: {warnings:?}"
    );
}

// ── MidiForward raw-port-name target lint ─────────────────
//
// A `MidiForward.target` that matches a `[[bindings]]` alias gets
// hot-plug-aware output routing (the rescan loop refreshes the
// device output map for aliased outputs). A raw port-name target
// bypasses that map entirely — it still forwards, but receives no
// hot-plug liveness, status pill, mute affordance, or future
// ADR-031 connector treatment. The validator emits a non-blocking
// warning so the operator can choose to define a binding.

fn midi_forward(target: &str) -> ActionConfig {
    ActionConfig::MidiForward {
        target: target.to_string(),
        transform: None,
    }
}

// ── ADR-047 §D3a: frozen legacy gamepad sentinel (id 255) ──

#[test]
fn test_gamepad_analog_stick_accepts_dpad_axis_ids_d3b() {
    // ADR-047 §D3b: 147/148 (d-pad-as-axis) must validate, kept in sync with
    // the matcher in mapping.rs. Out-of-range still errors.
    for axis in [128u8, 131, 147, 148] {
        let cfg = config_with_mappings(vec![Trigger::GamepadAnalogStick {
            axis,
            direction: None,
            device: None,
        }]);
        let report = validate_config(&cfg);
        assert!(
            report.is_valid(),
            "axis {axis} should validate; errors: {:?}",
            report.errors
        );
    }
    let bad = config_with_mappings(vec![Trigger::GamepadAnalogStick {
        axis: 200,
        direction: None,
        device: None,
    }]);
    assert!(
        !validate_config(&bad).is_valid(),
        "axis 200 is out of range and must error"
    );
}

#[test]
fn test_gamepad_button_255_warns() {
    let cfg = config_with_mappings(vec![Trigger::GamepadButton {
        button: 255,
        velocity_min: None,
        device: None,
    }]);
    let report = validate_config(&cfg);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("255") && w.message.contains("ADR-047")),
        "button 255 must warn; got: {:?}",
        report.warnings
    );
}

#[test]
fn test_gamepad_chord_containing_255_warns() {
    let cfg = config_with_mappings(vec![Trigger::GamepadButtonChord {
        buttons: vec![128, 255],
        timeout_ms: None,
        device: None,
    }]);
    let report = validate_config(&cfg);
    assert!(
        report.warnings.iter().any(|w| w.message.contains("255")),
        "chord containing 255 must warn; got: {:?}",
        report.warnings
    );
}

#[test]
fn test_valid_gamepad_button_does_not_warn_255() {
    let cfg = config_with_mappings(vec![Trigger::GamepadButton {
        button: 128, // South — valid
        velocity_min: None,
        device: None,
    }]);
    let report = validate_config(&cfg);
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.message.contains("legacy") && w.message.contains("255")),
        "valid button must not warn about 255; got: {:?}",
        report.warnings
    );
}

#[test]
fn test_disable_legacy_sentinel_drops_only_255_binds() {
    let mut cfg = config_with_mappings(vec![
        Trigger::GamepadButton {
            button: 128,
            velocity_min: None,
            device: None,
        },
        Trigger::GamepadButton {
            button: 255,
            velocity_min: None,
            device: None,
        },
        Trigger::GamepadButtonChord {
            buttons: vec![129, 255],
            timeout_ms: None,
            device: None,
        },
    ]);
    let removed = disable_legacy_gamepad_sentinel_binds(&mut cfg);
    assert_eq!(
        removed, 2,
        "the 255 button and the 255-chord must be dropped"
    );
    let remaining: Vec<_> = cfg.modes[0].mappings.iter().map(|m| &m.trigger).collect();
    assert_eq!(remaining.len(), 1);
    assert!(
        matches!(remaining[0], Trigger::GamepadButton { button: 128, .. }),
        "only the valid button-128 mapping survives"
    );
}

#[test]
fn test_midi_forward_raw_port_name_warns() {
    // Target doesn't match any [[bindings]] alias → raw port name.
    let config = config_with_action(midi_forward("Komplete Audio 6 MK2"));
    let report = validate_config(&config);
    // Non-blocking: the config is still valid.
    assert!(
        report.is_valid(),
        "raw-port-name target must NOT be a hard error"
    );
    let warning = report
        .warnings
        .iter()
        .find(|w| w.message.contains("MidiForward target"))
        .expect("expected a MidiForward raw-port-name warning");
    assert!(
        warning.message.contains("Komplete Audio 6 MK2"),
        "warning must name the target; got: {}",
        warning.message
    );
    assert!(
        warning.message.contains("[[endpoints]]"),
        "warning must point at the [[endpoints]] remediation; got: {}",
        warning.message
    );
}

#[test]
fn test_midi_forward_aliased_target_no_warn() {
    // Target matches a defined [[bindings]] alias → no warning.
    let mut config = config_with_action(midi_forward("studio_out"));
    config.endpoints = vec![ep_input(
        "studio_out",
        vec![DeviceMatcher::NameContains {
            value: "Komplete".to_string(),
        }],
    )];
    let report = validate_config(&config);
    assert!(report.is_valid());
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.message.contains("MidiForward target")),
        "aliased target must NOT produce the raw-port-name warning"
    );
}

#[test]
fn test_midi_forward_empty_target_still_errors_not_warns() {
    // Empty target is a hard error (pre-existing behaviour); the
    // raw-port-name warning must not replace or suppress it.
    let config = config_with_action(midi_forward(""));
    let report = validate_config(&config);
    assert!(!report.is_valid(), "empty target must remain a hard error");
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("MidiForward requires target")),
        "empty target must keep the 'requires target port name' error"
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.message.contains("does not match any [[endpoints]]")),
        "empty target must NOT also emit the raw-port-name warning"
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

fn matcher_endpoint(name_contains: &str) -> EndpointKind {
    EndpointKind::Matcher {
        input_matchers: Vec::new(),
        output_matchers: Vec::new(),
        matchers: vec![DeviceMatcher::NameContains {
            value: name_contains.to_string(),
        }],
        no_probe: false,
    }
}

fn connector(alias: &str, direction: ConnectorDirection) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
        direction,
        protocol: Some(ConnectorProtocol::Midi),
        description: None,
        enabled: true,
        channels: vec![],
        kind: matcher_endpoint(alias),
    }
}

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
