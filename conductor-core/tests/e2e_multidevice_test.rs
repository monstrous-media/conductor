// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! End-to-end tests for multi-device MIDI pipeline (ADR-009 Phase 6)
//!
//! Full pipeline: Config parse -> EventProcessor::process(MidiEvent) -> ProcessedEvent
//! -> CompiledRuleSet::match_event -> Action inspection.

use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
use conductor_core::event_processor::{MidiEvent, ProcessedEvent};
use conductor_core::identity::DeviceMatcher;
use conductor_core::{Action, ActionConfig, Config, EventProcessor, Mapping, Mode, Trigger};
use std::time::Instant;

/// Build a multi-device config for E2E tests
fn e2e_config() -> Config {
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
        modes: vec![
            Mode {
                name: "Mode A".to_string(),
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
                            app: "mode-a-pads".to_string(),
                        },
                        description: None,
                        let_through: false,
                    },
                    // CC 14 (any device) -> Text
                    Mapping {
                        trigger: Trigger::CC {
                            cc: 14,
                            value_min: None,
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Text {
                            text: "encoder turned".to_string(),
                        },
                        description: None,
                        let_through: false,
                    },
                    // Note 60 (no device) -> fallback
                    Mapping {
                        trigger: Trigger::Note {
                            note: 60,
                            velocity_min: None,
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Text {
                            text: "any-device-fallback".to_string(),
                        },
                        description: None,
                        let_through: false,
                    },
                ],
            },
            Mode {
                name: "Mode B".to_string(),
                color: Some("red".to_string()),
                mappings: vec![
                    // Same note 36 from "pads" -> Shell
                    Mapping {
                        trigger: Trigger::Note {
                            note: 36,
                            velocity_min: None,
                            channel: None,
                            device: Some("pads".to_string()),
                        },
                        action: ActionConfig::Shell {
                            sandbox: None,
                            command: "mode-b-cmd".to_string(),
                            args: None,
                            timeout_ms: None,
                        },
                        description: None,
                        let_through: false,
                    },
                ],
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
    }
}

/// Find the first PadPressed event from the processed events
fn find_pad_pressed(events: &[ProcessedEvent]) -> Option<&ProcessedEvent> {
    events
        .iter()
        .find(|e| matches!(e, ProcessedEvent::PadPressed { .. }))
}

#[test]
fn test_e2e_midi_input_to_action_output() {
    let config = e2e_config();
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    // Simulate NoteOn from "pads"
    let mut processor = EventProcessor::new();
    let midi_event = MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 0,
        time: Instant::now(),
    };

    let processed = processor.process(midi_event);
    let pad_pressed = find_pad_pressed(&processed);
    assert!(pad_pressed.is_some(), "Should produce PadPressed event");

    // Match with device_id = "pads"
    let action = rule_set.match_event(pad_pressed.unwrap(), 0, Some("pads"));
    assert!(action.is_some(), "Should match a rule");
    assert!(
        matches!(action.unwrap(), Action::Launch(_)),
        "Should be Launch action in Mode A"
    );
}

#[test]
fn test_e2e_cc_input_to_action() {
    let config = e2e_config();
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let mut processor = EventProcessor::new();
    let midi_event = MidiEvent::ControlChange {
        cc: 14,
        value: 65,
        channel: 0,
        time: Instant::now(),
    };

    let processed = processor.process(midi_event);

    // Find EncoderTurned or CCReceived in results
    let matchable = processed.iter().find(|e| {
        matches!(
            e,
            ProcessedEvent::EncoderTurned { .. } | ProcessedEvent::CCReceived { .. }
        )
    });
    assert!(matchable.is_some(), "Should produce CC-related event");

    let action = rule_set.match_event(matchable.unwrap(), 0, Some("pads"));
    assert!(action.is_some(), "Should match CC rule");
    assert!(matches!(action.unwrap(), Action::Text(_)));
}

#[test]
fn test_e2e_mode_switch_changes_action() {
    let config = e2e_config();
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let mut processor = EventProcessor::new();
    let midi_event = MidiEvent::NoteOn {
        note: 36,
        velocity: 80,
        channel: 0,
        time: Instant::now(),
    };

    let processed = processor.process(midi_event);
    let pad_pressed = find_pad_pressed(&processed).unwrap();

    // Mode 0 (Mode A) -> Launch
    let action_a = rule_set.match_event(pad_pressed, 0, Some("pads"));
    assert!(matches!(action_a, Some(Action::Launch(_))));

    // Mode 1 (Mode B) -> Shell
    let action_b = rule_set.match_event(pad_pressed, 1, Some("pads"));
    assert!(matches!(
        action_b,
        Some(Action::Shell {
            command: _,
            args: _,
            ..
        })
    ));
}

#[test]
fn test_e2e_unmatched_device_fallthrough() {
    let config = e2e_config();
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let mut processor = EventProcessor::new();
    let midi_event = MidiEvent::NoteOn {
        note: 60,
        velocity: 64,
        channel: 0,
        time: Instant::now(),
    };

    let processed = processor.process(midi_event);
    let pad_pressed = find_pad_pressed(&processed).unwrap();

    // Unknown device should fall through to any-device rule for note 60
    let action = rule_set.match_event(pad_pressed, 0, Some("unknown-device"));
    assert!(action.is_some());
    assert!(matches!(action.unwrap(), Action::Text(_)));
}

#[test]
fn test_e2e_no_match_returns_none() {
    let config = e2e_config();
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    let mut processor = EventProcessor::new();
    let midi_event = MidiEvent::NoteOn {
        note: 99, // No mapping for note 99
        velocity: 64,
        channel: 0,
        time: Instant::now(),
    };

    let processed = processor.process(midi_event);
    let pad_pressed = find_pad_pressed(&processed).unwrap();

    let action = rule_set.match_event(pad_pressed, 0, Some("pads"));
    assert!(action.is_none(), "Unmapped note should return None");
}

#[test]
fn test_e2e_device_name_resolves_to_alias_before_matching() {
    // The other E2E tests pass the resolved alias ("pads") straight into
    // match_event, so a regression in the real physical-name -> alias resolution
    // (the whole point of the `[[endpoints]]` identities this suite configures)
    // would go uncaught. This test drives a concrete MIDI source *name* through
    // the actual resolver (PortResolver::resolve over the endpoint set) and
    // matches with the RESOLVED alias.
    use conductor_core::resolver::{BindingResult, PortInfo, PortResolver};

    let config = e2e_config();
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    // Resolve a physical port name to its bound alias using the configured
    // `[[endpoints]]` set the resolver consumes at runtime.
    let endpoints = config.endpoints.clone();
    let resolve_alias = |port_name: &str| -> Option<String> {
        let ports = vec![PortInfo::new(port_name.to_string(), 0)];
        PortResolver::resolve(&ports, &endpoints)
            .into_iter()
            .find_map(|b| match b {
                BindingResult::Bound { device_id, .. } => Some(device_id.as_str().to_string()),
                _ => None,
            })
    };

    // "Maschine Mikro MK3" matches name_contains("Mikro") -> alias "pads".
    let pads_alias = resolve_alias("Maschine Mikro MK3")
        .expect("a 'Mikro' port must resolve to a configured alias");
    assert_eq!(
        pads_alias, "pads",
        "'Mikro' must resolve to the 'pads' identity"
    );

    let mut processor = EventProcessor::new();
    let processed = processor.process(MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 0,
        time: Instant::now(),
    });
    let pad_pressed = find_pad_pressed(&processed).unwrap();

    // Matching with the RESOLVED alias must fire the pads-only note-36 mapping.
    let action = rule_set.match_event(pad_pressed, 0, Some(pads_alias.as_str()));
    assert!(
        matches!(action, Some(Action::Launch(_))),
        "Mikro -> pads must fire the pads-only note 36 Launch mapping"
    );

    // "Launchpad Mini" matches name_contains("Launchpad") -> alias "keys".
    let keys_alias = resolve_alias("Launchpad Mini")
        .expect("a 'Launchpad' port must resolve to a configured alias");
    assert_eq!(
        keys_alias, "keys",
        "'Launchpad' must resolve to the 'keys' identity, not 'pads'"
    );

    // The note-36 mapping is pads-only and there is no any-device note-36 rule,
    // so the same event resolved from "keys" must NOT match.
    let action_keys = rule_set.match_event(pad_pressed, 0, Some(keys_alias.as_str()));
    assert!(
        action_keys.is_none(),
        "Launchpad -> keys must NOT fire the pads-only note 36 mapping"
    );
}
