// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Integration tests for multi-device rule routing via CompiledRuleSet
//! (v4.24.0 - ADR-009 Phase 6)
//!
//! Tests that CompiledRuleSet::match_event correctly routes events based
//! on device_id, mode index, and priority ordering.

use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
use conductor_core::events::{ProcessedEvent, VelocityLevel};
use conductor_core::identity::DeviceMatcher;
use conductor_core::{Action, ActionConfig, Config, Mapping, Mode, Trigger};

/// Build a two-device config with "pads" and "keys", same note but different actions
fn two_device_config() -> Config {
    Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![
            EndpointConfig {
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
            },
            EndpointConfig {
                alias: "keys".to_string(),
                direction: ConnectorDirection::Input,
                protocol: None,
                description: None,
                enabled: true,
                channels: vec![],
                kind: EndpointKind::Matcher {
                    matchers: vec![DeviceMatcher::name_contains("Launchpad")],
                    input_matchers: vec![],
                    output_matchers: vec![],
                    no_probe: false,
                },
            },
        ],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: Some("blue".to_string()),
            mappings: vec![
                // Note 36 from "pads" -> Launch
                Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: None,
                        channel: None,
                        device: Some("pads".to_string()),
                    },
                    action: ActionConfig::Launch {
                        app: "/Applications/Finder.app".to_string(),
                    },
                    description: Some("Pads launch".to_string()),
                    let_through: false,
                },
                // Note 36 from "keys" -> Shell
                Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: None,
                        channel: None,
                        device: Some("keys".to_string()),
                    },
                    action: ActionConfig::Shell {
                        sandbox: None,
                        command: "echo hello".to_string(),
                        args: None,
                        timeout_ms: None,
                    },
                    description: Some("Keys shell".to_string()),
                    let_through: false,
                },
                // Note 60 (no device filter) -> Text
                Mapping {
                    trigger: Trigger::Note {
                        note: 60,
                        velocity_min: None,
                        channel: None,
                        device: None,
                    },
                    action: ActionConfig::Text {
                        text: "any device".to_string(),
                    },
                    description: Some("Any device text".to_string()),
                    let_through: false,
                },
            ],
        }],
        global_mappings: vec![
            // Global mapping with device filter
            Mapping {
                trigger: Trigger::Note {
                    note: 80,
                    velocity_min: None,
                    channel: None,
                    device: Some("pads".to_string()),
                },
                action: ActionConfig::Text {
                    text: "global pads".to_string(),
                },
                description: Some("Global pads".to_string()),
                let_through: false,
            },
        ],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    }
}

fn pad_pressed(note: u8) -> ProcessedEvent {
    ProcessedEvent::PadPressed {
        note,
        velocity: 64,
        velocity_level: VelocityLevel::Medium,
        channel: Some(0),
    }
}

#[test]
fn test_two_devices_same_note_different_actions() {
    let config = two_device_config();
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let event = pad_pressed(36);

    // Note 36 from "pads" -> Launch
    let action = rule_set.match_event(&event, 0, Some("pads"));
    assert!(action.is_some(), "Should match for pads");
    assert!(matches!(action.unwrap(), Action::Launch(_)));

    // Note 36 from "keys" -> Shell
    let action = rule_set.match_event(&event, 0, Some("keys"));
    assert!(action.is_some(), "Should match for keys");
    assert!(matches!(
        action.unwrap(),
        Action::Shell {
            command: _,
            args: _,
            ..
        }
    ));
}

#[test]
fn test_any_device_rule_matches_both() {
    let config = two_device_config();
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let event = pad_pressed(60);

    // Note 60 has no device filter, should match from either device
    let action_pads = rule_set.match_event(&event, 0, Some("pads"));
    let action_keys = rule_set.match_event(&event, 0, Some("keys"));

    assert!(action_pads.is_some());
    assert!(action_keys.is_some());
    assert!(matches!(action_pads.unwrap(), Action::Text(_)));
    assert!(matches!(action_keys.unwrap(), Action::Text(_)));
}

#[test]
fn test_unknown_device_falls_to_any_device() {
    let config = two_device_config();
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let event = pad_pressed(60);

    // Unknown device should still match any-device rules
    let action = rule_set.match_event(&event, 0, Some("unknown-device"));
    assert!(action.is_some());
    assert!(matches!(action.unwrap(), Action::Text(_)));

    // Note 36 from unknown device should NOT match device-specific rules
    let event_36 = pad_pressed(36);
    let action = rule_set.match_event(&event_36, 0, Some("unknown-device"));
    assert!(
        action.is_none(),
        "Unknown device should not match device-specific rules"
    );
}

#[test]
fn test_device_specific_priority() {
    // Config where note 36 has both a device-specific and an any-device rule
    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![EndpointConfig {
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
        }],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![
                // Device-specific rule
                Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: None,
                        channel: None,
                        device: Some("pads".to_string()),
                    },
                    action: ActionConfig::Launch {
                        app: "specific".to_string(),
                    },
                    description: None,
                    let_through: false,
                },
                // Any-device rule (same note)
                Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: None,
                        channel: None,
                        device: None,
                    },
                    action: ActionConfig::Text {
                        text: "fallback".to_string(),
                    },
                    description: None,
                    let_through: false,
                },
            ],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let rule_set = conductor_core::rule_compiler::compile(&config, 1);
    let event = pad_pressed(36);

    // Device-specific should win
    let action = rule_set.match_event(&event, 0, Some("pads"));
    assert!(matches!(action, Some(Action::Launch(_))));

    // Unknown device falls through to any-device
    let action = rule_set.match_event(&event, 0, Some("other"));
    assert!(matches!(action, Some(Action::Text(_))));
}

#[test]
fn test_global_device_specific_rule() {
    let config = two_device_config();
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let event = pad_pressed(80);

    // Global rule with device="pads" should match only "pads"
    let action = rule_set.match_event(&event, 0, Some("pads"));
    assert!(action.is_some());
    assert!(matches!(action.unwrap(), Action::Text(_)));

    // Should NOT match for "keys"
    let action = rule_set.match_event(&event, 0, Some("keys"));
    assert!(action.is_none());
}

#[test]
fn test_multi_mode_device_routing() {
    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![EndpointConfig {
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
        }],
        modes: vec![
            Mode {
                name: "Mode A".to_string(),
                color: None,
                mappings: vec![Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: None,
                        channel: None,
                        device: Some("pads".to_string()),
                    },
                    action: ActionConfig::Launch {
                        app: "mode-a".to_string(),
                    },
                    description: None,
                    let_through: false,
                }],
            },
            Mode {
                name: "Mode B".to_string(),
                color: None,
                mappings: vec![Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: None,
                        channel: None,
                        device: Some("pads".to_string()),
                    },
                    action: ActionConfig::Shell {
                        sandbox: None,
                        command: "mode-b".to_string(),
                        args: None,
                        timeout_ms: None,
                    },
                    description: None,
                    let_through: false,
                }],
            },
        ],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };

    let rule_set = conductor_core::rule_compiler::compile(&config, 1);
    let event = pad_pressed(36);

    // Mode 0 -> Launch
    let action_0 = rule_set.match_event(&event, 0, Some("pads"));
    assert!(matches!(action_0, Some(Action::Launch(_))));

    // Mode 1 -> Shell
    let action_1 = rule_set.match_event(&event, 1, Some("pads"));
    assert!(matches!(
        action_1,
        Some(Action::Shell {
            command: _,
            args: _,
            ..
        })
    ));
}

// #751: Channel scope filtering in rule matching
#[test]
fn test_channel_scope_filters_events() {
    // Device "drums" only responds to channel 9 (drum channel)
    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![EndpointConfig {
            alias: "drums".to_string(),
            direction: ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![9], // Only channel 9 (0-indexed, i.e., MIDI channel 10)
            kind: EndpointKind::Matcher {
                matchers: vec![DeviceMatcher::name_contains("Drums")],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 36,
                    velocity_min: None,
                    channel: None, // Trigger matches any channel
                    device: Some("drums".to_string()),
                },
                action: ActionConfig::Keystroke {
                    keys: "d".to_string(),
                    modifiers: vec![],
                },
                description: Some("Drum hit".to_string()),
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        advanced_settings: Default::default(),
        led: None,
        event_console: None,
        routes: vec![],
        default_mode: None,
        last_selected_mode: None,
        per_app_modes: None,
        logging: None,
    };

    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    // Event on channel 9 → should match (in scope)
    let event_ch9 = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(9),
    };
    let action = rule_set.match_event(&event_ch9, 0, Some("drums"));
    assert!(action.is_some(), "Channel 9 should match (in scope)");

    // Event on channel 0 → should NOT match (out of scope)
    let event_ch0 = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let action = rule_set.match_event(&event_ch0, 0, Some("drums"));
    assert!(
        action.is_none(),
        "Channel 0 should NOT match (out of scope)"
    );
}

#[test]
fn test_empty_channel_scope_matches_all() {
    // Device with no channel scope → matches all channels
    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![EndpointConfig {
            alias: "pads".to_string(),
            direction: ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![], // Empty = all channels
            kind: EndpointKind::Matcher {
                matchers: vec![DeviceMatcher::name_contains("Pads")],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: None,
                    device: Some("pads".to_string()),
                },
                action: ActionConfig::Keystroke {
                    keys: "p".to_string(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        advanced_settings: Default::default(),
        led: None,
        event_console: None,
        routes: vec![],
        default_mode: None,
        last_selected_mode: None,
        per_app_modes: None,
        logging: None,
    };

    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    // Any channel should match
    let event = ProcessedEvent::PadPressed {
        note: 60,
        velocity: 80,
        velocity_level: VelocityLevel::Medium,
        channel: Some(5),
    };
    assert!(rule_set.match_event(&event, 0, Some("pads")).is_some());
}

#[test]
fn test_channel_out_of_scope_still_matches_any_device_rules() {
    // Device "drums" scoped to channel 9, but there's an any-device rule for note 36.
    // An event on channel 0 should skip the device-specific rule but still match
    // the any-device rule.
    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![EndpointConfig {
            alias: "drums".to_string(),
            direction: ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![9],
            kind: EndpointKind::Matcher {
                matchers: vec![DeviceMatcher::name_contains("Drums")],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![
                // Device-specific rule — should NOT fire for ch.0
                Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: None,
                        channel: None,
                        device: Some("drums".to_string()),
                    },
                    action: ActionConfig::Shell {
                        sandbox: None,
                        command: "device-action".to_string(),
                        args: None,
                        timeout_ms: None,
                    },
                    description: Some("Device rule".to_string()),
                    let_through: false,
                },
                // Any-device rule — SHOULD fire for ch.0
                Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: None,
                        channel: None,
                        device: None, // No device filter
                    },
                    action: ActionConfig::Keystroke {
                        keys: "fallback".to_string(),
                        modifiers: vec![],
                    },
                    description: Some("Any-device rule".to_string()),
                    let_through: false,
                },
            ],
        }],
        global_mappings: vec![],
        advanced_settings: Default::default(),
        led: None,
        event_console: None,
        routes: vec![],
        default_mode: None,
        last_selected_mode: None,
        per_app_modes: None,
        logging: None,
    };

    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    // Event on channel 0 (out of scope for "drums")
    let event_ch0 = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };

    // Should match the any-device rule (Keystroke), NOT the device-specific rule (Shell)
    let action = rule_set.match_event(&event_ch0, 0, Some("drums"));
    assert!(action.is_some(), "Any-device rule should still match");
    assert!(
        matches!(action, Some(Action::Keystroke { .. })),
        "Should be Keystroke (any-device), not Shell (device-specific)"
    );
}
