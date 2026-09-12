// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Shared fixtures and imports for the validation test suite.

pub(crate) use super::*;
pub(crate) use crate::config::types::InterpreterPolicy;
pub(crate) use crate::config::types::{
    ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind, Mapping, Mode,
    RouteConfig, SignalFilter, SignalTransform,
};
pub(crate) use crate::identity::DeviceMatcher;

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

fn config_with_action_and_policy(action: ActionConfig, policy: InterpreterPolicy) -> Config {
    let mut config = config_with_action(action);
    config.advanced_settings.allow_interpreters = policy;
    config
}

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

fn midi_forward(target: &str) -> ActionConfig {
    ActionConfig::MidiForward {
        target: target.to_string(),
        transform: None,
    }
}

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

mod aliases;
mod conditions;
mod coverage;
mod endpoints;
mod led;
mod lints;
mod routes;
mod security;
mod structure;
