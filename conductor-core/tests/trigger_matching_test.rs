// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! TDD tests for ADR-002 trigger matching gaps
//!
//! These tests verify that all trigger types in config/types.rs
//! correctly match their corresponding ProcessedEvent variants.
//!
//! Issue Tracking:
//! - Epic: #12 - Fix ADR-002 Trigger Matching Gaps in MappingEngine
//! - Sub-issues: #13-#19 (one per trigger type)
//!
//! These tests are written BEFORE implementation (TDD RED phase).
//! They should FAIL until the corresponding match cases are added to mapping.rs.

use conductor_core::config::types::Config;
use conductor_core::{EncoderDirection, MappingEngine, MidiEvent, ProcessedEvent, VelocityLevel};
use std::time::Instant;

// ============================================================
// TEST GROUP 1: DoubleTap -> ProcessedEvent::DoubleTap
// Issue: #13
// ============================================================

#[test]
fn test_double_tap_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Double tap action"

[modes.mappings.trigger]
type = "DoubleTap"
note = 44

[modes.mappings.action]
type = "Shell"
command = "echo double_tap"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::DoubleTap {
        note: 44,
        first_velocity: 100,
        second_velocity: 100,
        interval_ms: 150,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "DoubleTap trigger should match DoubleTap event"
    );
}

#[test]
fn test_double_tap_wrong_note_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Double tap action"

[modes.mappings.trigger]
type = "DoubleTap"
note = 44

[modes.mappings.action]
type = "Shell"
command = "echo double_tap"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Different note should not match
    let event = ProcessedEvent::DoubleTap {
        note: 45,
        first_velocity: 100,
        second_velocity: 100,
        interval_ms: 150,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_none(),
        "DoubleTap trigger should NOT match different note"
    );
}

// ============================================================
// TEST GROUP 2: LongPress -> ProcessedEvent::LongPress
// Issue: #14
// ============================================================

#[test]
fn test_long_press_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Long press action"

[modes.mappings.trigger]
type = "LongPress"
note = 40
duration_ms = 2000

[modes.mappings.action]
type = "Shell"
command = "echo long_press"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Duration exceeds threshold
    let event = ProcessedEvent::LongPress {
        note: 40,
        duration_ms: 2500,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "LongPress trigger should match when duration exceeds threshold"
    );
}

#[test]
fn test_long_press_short_duration_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Long press action"

[modes.mappings.trigger]
type = "LongPress"
note = 40
duration_ms = 2000

[modes.mappings.action]
type = "Shell"
command = "echo long_press"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Duration below threshold
    let event = ProcessedEvent::LongPress {
        note: 40,
        duration_ms: 1000,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_none(),
        "LongPress trigger should NOT match when duration is below threshold"
    );
}

#[test]
fn test_long_press_wrong_note_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Long press action"

[modes.mappings.trigger]
type = "LongPress"
note = 40
duration_ms = 2000

[modes.mappings.action]
type = "Shell"
command = "echo long_press"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Different note should not match
    let event = ProcessedEvent::LongPress {
        note: 41,
        duration_ms: 3000,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_none(),
        "LongPress trigger should NOT match different note"
    );
}

// ============================================================
// TEST GROUP 3: Aftertouch -> ProcessedEvent::AftertouchChanged
// Issue: #15
// ============================================================

#[test]
fn test_aftertouch_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Aftertouch action"

[modes.mappings.trigger]
type = "Aftertouch"
pressure_min = 64

[modes.mappings.action]
type = "Shell"
command = "echo aftertouch"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::AftertouchChanged {
        pressure: 100,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "Aftertouch trigger should match when pressure >= pressure_min"
    );
}

#[test]
fn test_aftertouch_below_threshold_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Aftertouch action"

[modes.mappings.trigger]
type = "Aftertouch"
pressure_min = 64

[modes.mappings.action]
type = "Shell"
command = "echo aftertouch"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::AftertouchChanged {
        pressure: 50,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_none(),
        "Aftertouch trigger should NOT match when pressure < pressure_min"
    );
}

// ============================================================
// TEST GROUP 3b: PolyAftertouch -> ProcessedEvent::PolyAftertouchChanged
// Empty match arm in event_processor → trigger never fires
// ============================================================

#[test]
fn test_poly_aftertouch_matches_specific_note() {
    // PolyAftertouch trigger MUST match only the configured note,
    // not any note. This is the key behavioural difference from
    // channel-wide Aftertouch.
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Pad 60 poly aftertouch"

[modes.mappings.trigger]
type = "PolyAftertouch"
note = 60
pressure_min = 64

[modes.mappings.action]
type = "Shell"
command = "echo poly"
"#;
    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PolyAftertouchChanged {
        note: 60,
        pressure: 100,
        channel: Some(0),
    };
    assert!(
        engine.get_action_for_processed(&event, 0).is_some(),
        "PolyAftertouch should match same note + pressure >= min"
    );
}

#[test]
fn test_poly_aftertouch_no_match_other_note() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Pad 60 only"

[modes.mappings.trigger]
type = "PolyAftertouch"
note = 60
pressure_min = 1

[modes.mappings.action]
type = "Shell"
command = "echo poly"
"#;
    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PolyAftertouchChanged {
        note: 61, // wrong note
        pressure: 100,
        channel: Some(0),
    };
    assert!(
        engine.get_action_for_processed(&event, 0).is_none(),
        "PolyAftertouch must NOT fire for a different note"
    );
}

#[test]
fn test_poly_aftertouch_below_threshold_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Heavy press only"

[modes.mappings.trigger]
type = "PolyAftertouch"
note = 60
pressure_min = 80

[modes.mappings.action]
type = "Shell"
command = "echo poly"
"#;
    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PolyAftertouchChanged {
        note: 60,
        pressure: 50, // below 80
        channel: Some(0),
    };
    assert!(
        engine.get_action_for_processed(&event, 0).is_none(),
        "PolyAftertouch must NOT fire when pressure < pressure_min"
    );
}

#[test]
fn test_poly_aftertouch_channel_filter() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Channel 9 only"

[modes.mappings.trigger]
type = "PolyAftertouch"
note = 60
pressure_min = 1
channel = 9

[modes.mappings.action]
type = "Shell"
command = "echo poly"
"#;
    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let on_channel = ProcessedEvent::PolyAftertouchChanged {
        note: 60,
        pressure: 100,
        channel: Some(9),
    };
    assert!(
        engine.get_action_for_processed(&on_channel, 0).is_some(),
        "Channel filter should accept matching channel"
    );

    let other_channel = ProcessedEvent::PolyAftertouchChanged {
        note: 60,
        pressure: 100,
        channel: Some(0),
    };
    assert!(
        engine.get_action_for_processed(&other_channel, 0).is_none(),
        "Channel filter should reject other channels"
    );
}

// ── PolyPressure → PolyAftertouchChanged conversion ──
// The EventProcessor entry-point arms for both MidiEvent and
// InputEvent now emit PolyAftertouchChanged but had no direct test coverage.

#[test]
fn test_event_processor_emits_poly_aftertouch_from_midi_event() {
    use conductor_core::EventProcessor;
    use conductor_core::events::MidiEvent;
    use std::time::Instant;

    let mut processor = EventProcessor::new();
    let event = MidiEvent::PolyPressure {
        note: 60,
        pressure: 90,
        channel: 4,
        time: Instant::now(),
    };
    let processed = processor.process(event);
    let found = processed.iter().any(|e| {
        matches!(
            e,
            ProcessedEvent::PolyAftertouchChanged {
                note: 60,
                pressure: 90,
                channel: Some(4),
            }
        )
    });
    assert!(
        found,
        "EventProcessor::process(MidiEvent::PolyPressure) must emit \
         PolyAftertouchChanged carrying note/pressure/channel; got {processed:?}"
    );
}

#[test]
fn test_event_processor_emits_poly_aftertouch_from_input_event() {
    use conductor_core::EventProcessor;
    use conductor_core::events::InputEvent;
    use std::time::Instant;

    let mut processor = EventProcessor::new();
    let event = InputEvent::PolyPressure {
        pad: 60,
        pressure: 90,
        channel: Some(4),
        time: Instant::now(),
    };
    let processed = processor.process_input(event);
    let found = processed.iter().any(|e| {
        matches!(
            e,
            ProcessedEvent::PolyAftertouchChanged {
                note: 60,
                pressure: 90,
                channel: Some(4),
            }
        )
    });
    assert!(
        found,
        "EventProcessor::process_input(InputEvent::PolyPressure) must emit \
         PolyAftertouchChanged carrying note/pressure/channel; got {processed:?}"
    );
}

// ============================================================
// TEST GROUP 4: PitchBend -> ProcessedEvent::PitchBendMoved
// Issue: #16
// ============================================================

#[test]
fn test_pitch_bend_in_range_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Pitch bend up action"

[modes.mappings.trigger]
type = "PitchBend"
value_min = 12000
value_max = 16383

[modes.mappings.action]
type = "Shell"
command = "echo pitch_up"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PitchBendMoved {
        value: 14000,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "PitchBend trigger should match when value is in range"
    );
}

#[test]
fn test_pitch_bend_outside_range_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Pitch bend up action"

[modes.mappings.trigger]
type = "PitchBend"
value_min = 12000
value_max = 16383

[modes.mappings.action]
type = "Shell"
command = "echo pitch_up"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PitchBendMoved {
        value: 8000,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_none(),
        "PitchBend trigger should NOT match when value is outside range"
    );
}

#[test]
fn test_pitch_bend_any_value_matches() {
    // When no range specified, any value should match
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Pitch bend any action"

[modes.mappings.trigger]
type = "PitchBend"

[modes.mappings.action]
type = "Shell"
command = "echo pitch_any"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PitchBendMoved {
        value: 5000,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "PitchBend trigger with no range should match any value"
    );
}

// ============================================================
// TEST GROUP 5: VelocityRange -> ProcessedEvent::PadPressed
// Issue: #17
// ============================================================

#[test]
fn test_velocity_range_soft_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Soft press action"

[modes.mappings.trigger]
type = "VelocityRange"
note = 36
soft_max = 40
medium_max = 80

[modes.mappings.action]
type = "Shell"
command = "echo soft"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 30,
        velocity_level: VelocityLevel::Soft,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "VelocityRange trigger should match soft velocity press"
    );
}

#[test]
fn test_velocity_range_medium_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Medium press action"

[modes.mappings.trigger]
type = "VelocityRange"
note = 36
soft_max = 40
medium_max = 80

[modes.mappings.action]
type = "Shell"
command = "echo medium"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 60,
        velocity_level: VelocityLevel::Medium,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "VelocityRange trigger should match medium velocity press"
    );
}

#[test]
fn test_velocity_range_hard_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Hard press action"

[modes.mappings.trigger]
type = "VelocityRange"
note = 36
soft_max = 40
medium_max = 80

[modes.mappings.action]
type = "Shell"
command = "echo hard"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "VelocityRange trigger should match hard velocity press"
    );
}

#[test]
fn test_velocity_range_wrong_note_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Velocity range action"

[modes.mappings.trigger]
type = "VelocityRange"
note = 36
soft_max = 40
medium_max = 80

[modes.mappings.action]
type = "Shell"
command = "echo velocity"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::PadPressed {
        note: 37, // Wrong note
        velocity: 60,
        velocity_level: VelocityLevel::Medium,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_none(),
        "VelocityRange trigger should NOT match different note"
    );
}

// ============================================================
// TEST GROUP 5b: VelocityRange Custom Thresholds
// Fix: VelocityRange config ignored
// ============================================================

#[test]
fn test_velocity_range_custom_soft_max_boundary() {
    // NOTE: VelocityRange matching is velocity-LEVEL-agnostic — it
    // matches the note for ANY velocity (in mapping.rs the config-classified
    // level is informational/traced, not a match gate). So this test only
    // proves that a VelocityRange mapping with custom thresholds still MATCHES.
    // The actual "custom soft_max/medium_max are respected" classification is
    // covered by the `classify_velocity` unit tests in `mapping.rs`.
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Custom soft boundary"

[modes.mappings.trigger]
type = "VelocityRange"
note = 36
soft_max = 30
medium_max = 70

[modes.mappings.action]
type = "Shell"
command = "echo custom_soft"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Velocity 25 <= 30 (soft_max) should match
    let event_soft = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 25,
        velocity_level: VelocityLevel::Soft,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event_soft, 0);
    assert!(
        action.is_some(),
        "VelocityRange should match velocity 25 with soft_max=30"
    );

    // Velocity 35 also matches — VelocityRange matches every velocity for the
    // note. (The prebuilt velocity_level here is ignored by matching; threshold
    // classification is asserted in mapping.rs::classify_velocity tests.)
    let event_medium = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 35,
        velocity_level: VelocityLevel::Medium,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event_medium, 0);
    assert!(action.is_some(), "VelocityRange should match velocity 35");
}

#[test]
fn test_velocity_range_custom_medium_max_boundary() {
    // As above: this only proves the VelocityRange mapping matches;
    // the medium_max threshold behaviour is asserted in the
    // `classify_velocity` unit tests in `mapping.rs`.
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Custom medium boundary"

[modes.mappings.trigger]
type = "VelocityRange"
note = 36
soft_max = 20
medium_max = 60

[modes.mappings.action]
type = "Shell"
command = "echo custom_medium"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Velocity 55 > 20 (soft_max) and <= 60 (medium_max) = medium
    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 55,
        velocity_level: VelocityLevel::Medium,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);
    assert!(
        action.is_some(),
        "VelocityRange should match velocity 55 with medium_max=60"
    );

    // Velocity 65 > 60 (medium_max) = hard
    let event_hard = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 65,
        velocity_level: VelocityLevel::Hard, // Would be medium with default 80, hard with custom 60
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event_hard, 0);
    assert!(
        action.is_some(),
        "VelocityRange should match velocity 65 (classified as hard)"
    );
}

#[test]
fn test_velocity_range_gamepad_id_not_matched() {
    // Ensure VelocityRange only matches MIDI range (0-127), not gamepad (128+)
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "MIDI velocity range"

[modes.mappings.trigger]
type = "VelocityRange"
note = 36
soft_max = 40
medium_max = 80

[modes.mappings.action]
type = "Shell"
command = "echo midi"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Gamepad button ID (128+) should NOT match VelocityRange
    let gamepad_event = ProcessedEvent::PadPressed {
        note: 128, // Gamepad South button
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: None, // Gamepad events have no MIDI channel
    };
    let action = engine.get_action_for_processed(&gamepad_event, 0);
    assert!(
        action.is_none(),
        "VelocityRange trigger should NOT match gamepad ID 128"
    );
}

// ============================================================
// TEST GROUP 6: CC -> ProcessedEvent::EncoderTurned
// Issue: #18
// ============================================================

#[test]
fn test_cc_trigger_matches_encoder_turned() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "CC action"

[modes.mappings.trigger]
type = "CC"
cc = 7
value_min = 64

[modes.mappings.action]
type = "Shell"
command = "echo cc"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // CC triggers should match EncoderTurned events
    let event = ProcessedEvent::EncoderTurned {
        cc: 7,
        value: 100,
        direction: EncoderDirection::Clockwise,
        delta: 36,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "CC trigger should match EncoderTurned when value >= value_min"
    );
}

#[test]
fn test_cc_trigger_below_threshold_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "CC action"

[modes.mappings.trigger]
type = "CC"
cc = 7
value_min = 64

[modes.mappings.action]
type = "Shell"
command = "echo cc"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::EncoderTurned {
        cc: 7,
        value: 50, // Below threshold
        direction: EncoderDirection::Clockwise,
        delta: 10,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_none(),
        "CC trigger should NOT match when value < value_min"
    );
}

#[test]
fn test_cc_trigger_wrong_cc_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "CC action"

[modes.mappings.trigger]
type = "CC"
cc = 7
value_min = 64

[modes.mappings.action]
type = "Shell"
command = "echo cc"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::EncoderTurned {
        cc: 8, // Wrong CC
        value: 100,
        direction: EncoderDirection::Clockwise,
        delta: 36,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_none(),
        "CC trigger should NOT match different CC number"
    );
}

// ============================================================
// TEST GROUP 7: EncoderTurn -> ProcessedEvent::EncoderTurned
// Issue: #19
// ============================================================

#[test]
fn test_encoder_turn_clockwise_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Clockwise encoder action"

[modes.mappings.trigger]
type = "EncoderTurn"
cc = 1
direction = "Clockwise"

[modes.mappings.action]
type = "Shell"
command = "echo clockwise"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::EncoderTurned {
        cc: 1,
        value: 65,
        direction: EncoderDirection::Clockwise,
        delta: 1,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "EncoderTurn Clockwise should match Clockwise event"
    );
}

#[test]
fn test_encoder_turn_counter_clockwise_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Counter-clockwise encoder action"

[modes.mappings.trigger]
type = "EncoderTurn"
cc = 1
direction = "CounterClockwise"

[modes.mappings.action]
type = "Shell"
command = "echo counter_clockwise"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::EncoderTurned {
        cc: 1,
        value: 63,
        direction: EncoderDirection::CounterClockwise,
        delta: 1,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_some(),
        "EncoderTurn CounterClockwise should match CounterClockwise event"
    );
}

#[test]
fn test_encoder_turn_any_direction_matches() {
    // When no direction specified, any direction should match
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Any direction encoder action"

[modes.mappings.trigger]
type = "EncoderTurn"
cc = 1

[modes.mappings.action]
type = "Shell"
command = "echo any_direction"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // Should match clockwise
    let event_cw = ProcessedEvent::EncoderTurned {
        cc: 1,
        value: 65,
        direction: EncoderDirection::Clockwise,
        delta: 1,
        channel: Some(0),
    };
    let action_cw = engine.get_action_for_processed(&event_cw, 0);
    assert!(
        action_cw.is_some(),
        "EncoderTurn with no direction should match Clockwise"
    );

    // Should match counter-clockwise
    let event_ccw = ProcessedEvent::EncoderTurned {
        cc: 1,
        value: 63,
        direction: EncoderDirection::CounterClockwise,
        delta: 1,
        channel: Some(0),
    };
    let action_ccw = engine.get_action_for_processed(&event_ccw, 0);
    assert!(
        action_ccw.is_some(),
        "EncoderTurn with no direction should match CounterClockwise"
    );
}

#[test]
fn test_encoder_turn_wrong_direction_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Clockwise only encoder action"

[modes.mappings.trigger]
type = "EncoderTurn"
cc = 1
direction = "Clockwise"

[modes.mappings.action]
type = "Shell"
command = "echo clockwise"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    // CounterClockwise should NOT match Clockwise trigger
    let event = ProcessedEvent::EncoderTurned {
        cc: 1,
        value: 63,
        direction: EncoderDirection::CounterClockwise,
        delta: 1,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_none(),
        "EncoderTurn Clockwise should NOT match CounterClockwise event"
    );
}

#[test]
fn test_encoder_turn_wrong_cc_no_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Encoder action"

[modes.mappings.trigger]
type = "EncoderTurn"
cc = 1

[modes.mappings.action]
type = "Shell"
command = "echo encoder"
"#;

    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);

    let event = ProcessedEvent::EncoderTurned {
        cc: 2, // Wrong CC
        value: 65,
        direction: EncoderDirection::Clockwise,
        delta: 1,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);

    assert!(
        action.is_none(),
        "EncoderTurn should NOT match different CC number"
    );
}

// ============================================================
// TEST GROUP: load_from_config stale mode fix
// ADR-002 verification
// ============================================================

#[test]
fn test_load_from_config_clears_stale_modes() {
    // Config with 3 modes
    let config_3_modes = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Mode0"
[[modes.mappings]]
trigger = { type = "Note", note = 36 }
action = { type = "Shell", command = "echo mode0" }

[[modes]]
name = "Mode1"
[[modes.mappings]]
trigger = { type = "Note", note = 37 }
action = { type = "Shell", command = "echo mode1" }

[[modes]]
name = "Mode2"
[[modes.mappings]]
trigger = { type = "Note", note = 38 }
action = { type = "Shell", command = "echo mode2" }
"#;

    // Config with only 1 mode
    let config_1_mode = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "OnlyMode"
[[modes.mappings]]
trigger = { type = "Note", note = 60 }
action = { type = "Shell", command = "echo only" }
"#;

    let config3: Config = toml::from_str(config_3_modes).expect("Failed to parse config");
    let config1: Config = toml::from_str(config_1_mode).expect("Failed to parse config");

    let mut engine = MappingEngine::new();

    // Load 3 modes
    engine.load_from_config(&config3);
    assert_eq!(
        engine.mode_count(),
        3,
        "Should have 3 modes after first load"
    );

    // Reload with 1 mode - stale modes should be cleared
    engine.load_from_config(&config1);
    assert_eq!(
        engine.mode_count(),
        1,
        "Should have 1 mode after reload (stale modes cleared)"
    );

    // Verify old mode mappings don't trigger
    let event_old = ProcessedEvent::PadPressed {
        note: 36, // Was in Mode0 before reload
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };

    // Mode 0 now has different mapping, old note 36 should not match
    let action = engine.get_action_for_processed(&event_old, 0);
    assert!(
        action.is_none(),
        "Old mode 0 mapping should not persist after reload"
    );

    // But new mode 0 mapping should work
    let event_new = ProcessedEvent::PadPressed {
        note: 60, // New Mode 0 mapping
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event_new, 0);
    assert!(action.is_some(), "New mode 0 mapping should work");
}

#[test]
fn test_load_from_config_old_mode_indices_not_accessible() {
    // Config with 5 modes
    let config_5_modes = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Mode0"
[[modes]]
name = "Mode1"
[[modes]]
name = "Mode2"
[[modes]]
name = "Mode3"
[[modes]]
name = "Mode4"
"#;

    // Config with 2 modes
    let config_2_modes = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Mode0"
[[modes]]
name = "Mode1"
"#;

    let config5: Config = toml::from_str(config_5_modes).expect("Failed to parse config");
    let config2: Config = toml::from_str(config_2_modes).expect("Failed to parse config");

    let mut engine = MappingEngine::new();

    // Load 5 modes
    engine.load_from_config(&config5);
    assert_eq!(engine.mode_count(), 5, "Should have 5 modes");

    // Reload with 2 modes
    engine.load_from_config(&config2);
    assert_eq!(engine.mode_count(), 2, "Should have 2 modes after reload");

    // Modes 2, 3, 4 should no longer exist
    // This is tested via mode_count(), as mappings to non-existent modes
    // would still be empty (no panic, just no matches)
}

// ============================================================
// TEST GROUP: Direct MappingEngine::get_action(&MidiEvent) raw path
//
// The raw matcher (`trigger_matches_raw`) previously only handled Note/CC,
// so direct `get_action(&MidiEvent)` lookups for ProgramChange, PitchBend,
// Aftertouch, and PolyPressure returned None even with a matching mapping
// configured. These tests pin the extended coverage; they fail on the old
// `_ => false` raw matcher.
// ============================================================

fn engine_from(config_toml: &str) -> MappingEngine {
    let config: Config = toml::from_str(config_toml).expect("Failed to parse config");
    let mut engine = MappingEngine::new();
    engine.load_from_config(&config);
    engine
}

#[test]
fn test_raw_aftertouch_matches() {
    let engine = engine_from(
        r#"
[device]
name = "Test"
auto_connect = false
[[modes]]
name = "Default"
[[modes.mappings]]
[modes.mappings.trigger]
type = "Aftertouch"
pressure_min = 64
[modes.mappings.action]
type = "Shell"
command = "echo at"
"#,
    );
    let at = MidiEvent::Aftertouch {
        pressure: 100,
        channel: 0,
        time: Instant::now(),
    };
    assert!(
        engine.get_action(&at, 0).is_some(),
        "raw Aftertouch over threshold must match (#1457)"
    );

    let weak = MidiEvent::Aftertouch {
        pressure: 10,
        channel: 0,
        time: Instant::now(),
    };
    assert!(
        engine.get_action(&weak, 0).is_none(),
        "raw Aftertouch below threshold must not match"
    );
}

#[test]
fn test_raw_poly_aftertouch_matches_specific_note() {
    let engine = engine_from(
        r#"
[device]
name = "Test"
auto_connect = false
[[modes]]
name = "Default"
[[modes.mappings]]
[modes.mappings.trigger]
type = "PolyAftertouch"
note = 60
pressure_min = 50
[modes.mappings.action]
type = "Shell"
command = "echo poly"
"#,
    );
    let hit = MidiEvent::PolyPressure {
        note: 60,
        pressure: 90,
        channel: 0,
        time: Instant::now(),
    };
    assert!(
        engine.get_action(&hit, 0).is_some(),
        "raw PolyPressure on matching note must match (#1457)"
    );

    let other = MidiEvent::PolyPressure {
        note: 61,
        pressure: 90,
        channel: 0,
        time: Instant::now(),
    };
    assert!(
        engine.get_action(&other, 0).is_none(),
        "raw PolyPressure on a different note must not match"
    );
}

#[test]
fn test_raw_pitch_bend_matches_in_range() {
    let engine = engine_from(
        r#"
[device]
name = "Test"
auto_connect = false
[[modes]]
name = "Default"
[[modes.mappings]]
[modes.mappings.trigger]
type = "PitchBend"
value_min = 10000
value_max = 14000
[modes.mappings.action]
type = "Shell"
command = "echo bend"
"#,
    );
    let in_range = MidiEvent::PitchBend {
        value: 12000,
        channel: 0,
        time: Instant::now(),
    };
    assert!(
        engine.get_action(&in_range, 0).is_some(),
        "raw PitchBend within range must match (#1457)"
    );

    let out = MidiEvent::PitchBend {
        value: 2000,
        channel: 0,
        time: Instant::now(),
    };
    assert!(
        engine.get_action(&out, 0).is_none(),
        "raw PitchBend outside range must not match"
    );
}

#[test]
fn test_raw_program_change_matches() {
    let engine = engine_from(
        r#"
[device]
name = "Test"
auto_connect = false
[[modes]]
name = "Default"
[[modes.mappings]]
[modes.mappings.trigger]
type = "ProgramChange"
pc = 5
[modes.mappings.action]
type = "Shell"
command = "echo pc"
"#,
    );
    let pc5 = MidiEvent::ProgramChange {
        program: 5,
        channel: 0,
        time: Instant::now(),
    };
    assert!(
        engine.get_action(&pc5, 0).is_some(),
        "raw ProgramChange matching pc must match (#1457)"
    );

    let pc6 = MidiEvent::ProgramChange {
        program: 6,
        channel: 0,
        time: Instant::now(),
    };
    assert!(
        engine.get_action(&pc6, 0).is_none(),
        "raw ProgramChange with a different pc must not match"
    );
}
