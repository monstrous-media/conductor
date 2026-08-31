// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Tests for mapping engine device filter (ADR-009 Phase 1)

use conductor_core::actions::Action;
use conductor_core::events::ProcessedEvent;
use conductor_core::{ActionConfig, Config, Mapping, MappingEngine, Mode, Trigger};

/// Create a minimal config with device-specific mappings
fn config_with_device_mappings() -> Config {
    Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![
                Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: Some(1),
                        channel: None,
                        device: Some("pads".to_string()),
                    },
                    action: ActionConfig::Keystroke {
                        keys: "a".to_string(),
                        modifiers: vec![],
                    },
                    description: Some("Pads note 36".to_string()),
                    let_through: false,
                },
                Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: Some(1),
                        channel: None,
                        device: Some("keys".to_string()),
                    },
                    action: ActionConfig::Keystroke {
                        keys: "b".to_string(),
                        modifiers: vec![],
                    },
                    description: Some("Keys note 36".to_string()),
                    let_through: false,
                },
                Mapping {
                    trigger: Trigger::Note {
                        note: 60,
                        velocity_min: Some(1),
                        channel: None,
                        device: None, // No device filter = matches any
                    },
                    action: ActionConfig::Keystroke {
                        keys: "c".to_string(),
                        modifiers: vec![],
                    },
                    description: Some("Any device note 60".to_string()),
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
    }
}

#[test]
fn test_device_filter_matches_when_device_matches() {
    let config = config_with_device_mappings();
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: conductor_core::events::VelocityLevel::Hard,
        channel: Some(0),
    };

    // When device_id is "pads", should get action for "pads" mapping (keys = "a")
    let action = engine.get_action_for_processed_with_device(&event, 0, Some("pads"));
    assert!(action.is_some(), "Should match mapping with device='pads'");
    // Verify it's the correct action (keystroke "a")
    match action.unwrap() {
        Action::Keystroke { keys, .. } => {
            assert_eq!(keys, vec![conductor_core::actions::KeyCode::Unicode('a')])
        }
        other => panic!("Expected Keystroke 'a', got {:?}", other),
    }
}

#[test]
fn test_device_filter_rejects_when_device_differs() {
    let config = config_with_device_mappings();
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: conductor_core::events::VelocityLevel::Hard,
        channel: Some(0),
    };

    // When device_id is "keys", should get action for "keys" mapping (keys = "b"), not "pads"
    let action = engine.get_action_for_processed_with_device(&event, 0, Some("keys"));
    assert!(action.is_some(), "Should match mapping with device='keys'");
    match action.unwrap() {
        Action::Keystroke { keys, .. } => {
            assert_eq!(keys, vec![conductor_core::actions::KeyCode::Unicode('b')])
        }
        other => panic!("Expected Keystroke 'b', got {:?}", other),
    }
}

#[test]
fn test_device_filter_none_matches_any() {
    let config = config_with_device_mappings();
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PadPressed {
        note: 60,
        velocity: 100,
        velocity_level: conductor_core::events::VelocityLevel::Hard,
        channel: Some(0),
    };

    // Mapping with device=None should match regardless of device_id
    let action = engine.get_action_for_processed_with_device(&event, 0, Some("any-device"));
    assert!(
        action.is_some(),
        "Mapping with device=None should match any device"
    );
}

#[test]
fn test_device_filter_none_device_id_matches_unfiltered() {
    let config = config_with_device_mappings();
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PadPressed {
        note: 60,
        velocity: 100,
        velocity_level: conductor_core::events::VelocityLevel::Hard,
        channel: Some(0),
    };

    // When device_id is None, should still match unfiltered mappings
    let action = engine.get_action_for_processed_with_device(&event, 0, None);
    assert!(
        action.is_some(),
        "None device_id should match unfiltered mappings"
    );
}

#[test]
fn test_device_filter_none_device_id_rejects_device_specific() {
    let config = config_with_device_mappings();
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Note 36 has ONLY device-specific mappings ("pads" and "keys") — there is
    // no unfiltered note-36 mapping. An event from an unknown source
    // (device_id None) must therefore NOT match: None is not a wildcard that
    // satisfies an explicit device filter. The converse —
    // `test_device_filter_none_device_id_matches_unfiltered` — only pins that
    // None matches an *unfiltered* mapping; without this an implementation
    // treating None as match-all would fire device-bound actions for a stray
    // / unidentified source and still pass the suite (#1501).
    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: conductor_core::events::VelocityLevel::Hard,
        channel: Some(0),
    };

    let action = engine.get_action_for_processed_with_device(&event, 0, None);
    assert!(
        action.is_none(),
        "None device_id must not satisfy a device-specific mapping (note 36 is device-bound only)"
    );
}
