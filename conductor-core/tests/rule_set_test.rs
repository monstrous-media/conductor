// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! TDD tests for ADR-009 Phase 3: CompiledRuleSet + RuleCompiler
//!
//! Tests the lock-free rule engine types (rule_set.rs) and compilation (rule_compiler.rs).

use conductor_core::actions::{Action, KeyCode};
use conductor_core::config::types::Config;
use conductor_core::event_processor::{EncoderDirection, ProcessedEvent, VelocityLevel};
use conductor_core::rule_compiler;
use conductor_core::rule_set::{CompiledRuleSet, ModeState};
use std::sync::Arc;

// Helper: parse a TOML config string into a CompiledRuleSet
fn compile_config(toml: &str, version: u64) -> CompiledRuleSet {
    let config: Config = toml::from_str(toml).expect("Failed to parse config");
    rule_compiler::compile(&config, version)
}

/// Returns the single Unicode character that a `Keystroke` action would type
/// (e.g. `keys = "a"` → `Some('a')`), or `None` for any other action shape.
/// Priority/first-match tests use this to assert WHICH candidate rule
/// won, not merely that *some* action matched — a rule engine that returned the
/// wrong same-trigger rule would otherwise pass.
fn keystroke_char(action: &Action) -> Option<char> {
    match action {
        Action::Keystroke { keys, .. } => match keys.as_slice() {
            [KeyCode::Unicode(c)] => Some(*c),
            _ => None,
        },
        _ => None,
    }
}

// Minimal config with a single Note trigger
const BASIC_NOTE_CONFIG: &str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Note 36 action"

[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "Keystroke"
keys = "space"
"#;

// ============================================================
// TEST GROUP 1: Empty / Basic CompiledRuleSet
// ============================================================

#[test]
fn test_empty_rule_set_matches_nothing() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Empty"
"#;
    let rules = compile_config(config_toml, 1);
    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&event, 0, None).is_none());
}

#[test]
fn test_note_trigger_matches_pad_pressed() {
    let rules = compile_config(BASIC_NOTE_CONFIG, 1);
    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&event, 0, None).is_some());
}

#[test]
fn test_note_trigger_velocity_min_filtering() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "High velocity only"

[modes.mappings.trigger]
type = "Note"
note = 36
velocity_min = 80

[modes.mappings.action]
type = "Keystroke"
keys = "space"
"#;
    let rules = compile_config(config_toml, 1);

    // Below threshold → no match
    let soft = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 50,
        velocity_level: VelocityLevel::Soft,
        channel: Some(0),
    };
    assert!(rules.match_event(&soft, 0, None).is_none());

    // At threshold → match
    let hard = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 80,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&hard, 0, None).is_some());
}

#[test]
fn test_cc_trigger_matches_cc_received() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "CC"
cc = 7
value_min = 64

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let event = ProcessedEvent::CCReceived {
        cc: 7,
        value: 100,
        channel: Some(0),
    };
    assert!(rules.match_event(&event, 0, None).is_some());

    let below = ProcessedEvent::CCReceived {
        cc: 7,
        value: 30,
        channel: Some(0),
    };
    assert!(rules.match_event(&below, 0, None).is_none());
}

#[test]
fn test_chord_trigger_matches_chord_detected() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "NoteChord"
notes = [36, 37, 38]

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let event = ProcessedEvent::ChordDetected {
        notes: vec![38, 36, 37], // Different order — should still match
        velocities: vec![100, 100, 100],
        channel: Some(0),
    };
    assert!(rules.match_event(&event, 0, None).is_some());

    // Subset should NOT match
    let partial = ProcessedEvent::ChordDetected {
        notes: vec![36, 37],
        velocities: vec![100, 100],
        channel: Some(0),
    };
    assert!(rules.match_event(&partial, 0, None).is_none());
}

#[test]
fn test_long_press_trigger_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "LongPress"
note = 36
duration_ms = 1000

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let long_enough = ProcessedEvent::LongPress {
        note: 36,
        duration_ms: 1500,
        channel: Some(0),
    };
    assert!(rules.match_event(&long_enough, 0, None).is_some());

    let too_short = ProcessedEvent::LongPress {
        note: 36,
        duration_ms: 500,
        channel: Some(0),
    };
    assert!(rules.match_event(&too_short, 0, None).is_none());
}

#[test]
fn test_double_tap_trigger_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "DoubleTap"
note = 44

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let event = ProcessedEvent::DoubleTap {
        note: 44,
        first_velocity: 100,
        second_velocity: 100,
        interval_ms: 150,
        channel: Some(0),
    };
    assert!(rules.match_event(&event, 0, None).is_some());

    let wrong_note = ProcessedEvent::DoubleTap {
        note: 45,
        first_velocity: 100,
        second_velocity: 100,
        interval_ms: 150,
        channel: Some(0),
    };
    assert!(rules.match_event(&wrong_note, 0, None).is_none());
}

#[test]
fn test_encoder_trigger_matches_direction() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "EncoderTurn"
cc = 16
direction = "Clockwise"

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let cw = ProcessedEvent::EncoderTurned {
        cc: 16,
        value: 65,
        direction: EncoderDirection::Clockwise,
        delta: 1,
        channel: Some(0),
    };
    assert!(rules.match_event(&cw, 0, None).is_some());

    let ccw = ProcessedEvent::EncoderTurned {
        cc: 16,
        value: 63,
        direction: EncoderDirection::CounterClockwise,
        delta: 1,
        channel: Some(0),
    };
    assert!(rules.match_event(&ccw, 0, None).is_none());
}

#[test]
fn test_aftertouch_trigger_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Aftertouch"
pressure_min = 50

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let above = ProcessedEvent::AftertouchChanged {
        pressure: 80,
        channel: Some(0),
    };
    assert!(rules.match_event(&above, 0, None).is_some());

    let below = ProcessedEvent::AftertouchChanged {
        pressure: 30,
        channel: Some(0),
    };
    assert!(rules.match_event(&below, 0, None).is_none());
}

#[test]
fn test_pitch_bend_trigger_matches_range() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "PitchBend"
value_min = 4000
value_max = 12000

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let in_range = ProcessedEvent::PitchBendMoved {
        value: 8000,
        channel: Some(0),
    };
    assert!(rules.match_event(&in_range, 0, None).is_some());

    let below = ProcessedEvent::PitchBendMoved {
        value: 2000,
        channel: Some(0),
    };
    assert!(rules.match_event(&below, 0, None).is_none());

    let above = ProcessedEvent::PitchBendMoved {
        value: 14000,
        channel: Some(0),
    };
    assert!(rules.match_event(&above, 0, None).is_none());
}

#[test]
fn test_velocity_range_trigger_soft_medium_hard() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "VelocityRange"
note = 36
soft_max = 40
medium_max = 80

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    // All velocity levels should match VelocityRange (it triggers for any velocity)
    let soft = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 20,
        velocity_level: VelocityLevel::Soft,
        channel: Some(0),
    };
    assert!(rules.match_event(&soft, 0, None).is_some());

    let hard = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&hard, 0, None).is_some());
}

// ============================================================
// TEST GROUP 2: Gamepad triggers
// ============================================================

#[test]
fn test_gamepad_button_trigger_matches() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "GamepadButton"
button = 128

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let event = ProcessedEvent::PadPressed {
        note: 128,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: None, // Gamepad events have no MIDI channel
    };
    assert!(rules.match_event(&event, 0, None).is_some());

    // MIDI note should NOT match gamepad trigger
    let midi = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&midi, 0, None).is_none());
}

#[test]
fn test_gamepad_stick_trigger_direction() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "GamepadAnalogStick"
axis = 128
direction = "Clockwise"

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let cw = ProcessedEvent::EncoderTurned {
        cc: 128,
        value: 200,
        direction: EncoderDirection::Clockwise,
        delta: 10,
        channel: None, // Gamepad events have no MIDI channel
    };
    assert!(rules.match_event(&cw, 0, None).is_some());

    let ccw = ProcessedEvent::EncoderTurned {
        cc: 128,
        value: 50,
        direction: EncoderDirection::CounterClockwise,
        delta: 10,
        channel: None, // Gamepad events have no MIDI channel
    };
    assert!(rules.match_event(&ccw, 0, None).is_none());
}

// ============================================================
// TEST GROUP 3: Device filtering
// ============================================================

#[test]
fn test_device_specific_rule_matches_correct_device() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36
device = "mikro"

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&event, 0, Some("mikro")).is_some());
}

#[test]
fn test_device_specific_rule_skips_wrong_device() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36
device = "mikro"

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    // Wrong device → no match
    assert!(rules.match_event(&event, 0, Some("launchpad")).is_none());
    // No device → no match (device-filtered rules require a matching device)
    assert!(rules.match_event(&event, 0, None).is_none());
}

#[test]
fn test_any_device_rule_matches_all_devices() {
    let rules = compile_config(BASIC_NOTE_CONFIG, 1);

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    // Any-device rule matches regardless of device_id
    assert!(rules.match_event(&event, 0, None).is_some());
    assert!(rules.match_event(&event, 0, Some("mikro")).is_some());
    assert!(rules.match_event(&event, 0, Some("launchpad")).is_some());
}

#[test]
fn test_device_rules_checked_before_any_device() {
    // Device-specific rule should take priority over any-device rule
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "mikro-specific"

[modes.mappings.trigger]
type = "Note"
note = 36
device = "mikro"

[modes.mappings.action]
type = "Keystroke"
keys = "a"

[[modes.mappings]]
description = "any-device"

[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "Keystroke"
keys = "b"
"#;
    let rules = compile_config(config_toml, 1);

    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };

    // With mikro device → the device-SPECIFIC rule (keys "a") must win over
    // the any-device rule (keys "b"). Asserting identity catches an engine that
    // checks any-device rules first.
    let action = rules
        .match_event(&event, 0, Some("mikro"))
        .expect("note 36 on mikro must match");
    assert_eq!(
        keystroke_char(&action),
        Some('a'),
        "device-specific (mikro, keys=a) rule must win over the any-device rule (keys=b)"
    );

    // With a different device → the any-device rule (keys "b") is the match
    // (the mikro rule must NOT fire for launchpad).
    let action = rules
        .match_event(&event, 0, Some("launchpad"))
        .expect("note 36 on launchpad must match the any-device rule");
    assert_eq!(
        keystroke_char(&action),
        Some('b'),
        "a non-mikro device must fall back to the any-device rule (keys=b), not the mikro rule"
    );
}

// ============================================================
// TEST GROUP 4: Global rules and priority
// ============================================================

#[test]
fn test_global_rules_checked_after_mode_rules() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "mode rule"

[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "Keystroke"
keys = "a"

[[global_mappings]]
description = "global rule"

[global_mappings.trigger]
type = "Note"
note = 37

[global_mappings.action]
type = "Keystroke"
keys = "b"

[[global_mappings]]
description = "conflicting global rule — same trigger (note 36) as the mode rule"

[global_mappings.trigger]
type = "Note"
note = 36

[global_mappings.action]
type = "Keystroke"
keys = "z"
"#;
    let rules = compile_config(config_toml, 1);

    // Note 36 COLLIDES: a mode rule (keys "a") and a global rule (keys "z")
    // both match it. The priority contract — mode rules checked before global
    // rules — means the MODE action must win. Asserting identity (not just
    // `is_some`) is what actually exercises that contract; with the prior
    // distinct-trigger-only config a global-first engine would also have passed.
    let mode_event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let action = rules
        .match_event(&mode_event, 0, None)
        .expect("note 36 must match the mode rule");
    assert_eq!(
        keystroke_char(&action),
        Some('a'),
        "on a mode/global trigger collision, the mode rule (keys=a) must win over \
         the global rule (keys=z)"
    );

    // Note 37 matches global rule
    let global_event = ProcessedEvent::PadPressed {
        note: 37,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&global_event, 0, None).is_some());

    // Note 99 matches nothing
    let no_match = ProcessedEvent::PadPressed {
        note: 99,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&no_match, 0, None).is_none());
}

#[test]
fn test_mode_index_out_of_range_returns_none() {
    let rules = compile_config(BASIC_NOTE_CONFIG, 1);
    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    // Mode index 99 doesn't exist — should fall through to global (which has nothing)
    assert!(rules.match_event(&event, 99, None).is_none());
}

#[test]
fn test_match_event_returns_first_match() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "first match"

[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "Keystroke"
keys = "a"

[[modes.mappings]]
description = "second match"

[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "Keystroke"
keys = "b"
"#;
    let rules = compile_config(config_toml, 1);
    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    // First match wins: two mappings share trigger note 36; the FIRST (keys "a")
    // must be returned, not the second (keys "b"). Asserting identity catches an
    // engine that returns the last same-trigger mapping instead of the first.
    let action = rules
        .match_event(&event, 0, None)
        .expect("note 36 must match the first mapping");
    assert_eq!(
        keystroke_char(&action),
        Some('a'),
        "first matching mapping (keys=a) must win over the later duplicate (keys=b)"
    );
}

// ============================================================
// TEST GROUP 5: Compiler tests
// ============================================================

#[test]
fn test_compile_empty_config() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Empty"
"#;
    let rules = compile_config(config_toml, 1);
    assert_eq!(rules.mode_count(), 1);
    assert_eq!(rules.version(), 1);
    assert_eq!(rules.mode_name(0), Some("Empty"));
}

#[test]
fn test_compile_single_mode_single_mapping() {
    let rules = compile_config(BASIC_NOTE_CONFIG, 42);
    assert_eq!(rules.mode_count(), 1);
    assert_eq!(rules.version(), 42);
    assert_eq!(rules.mode_name(0), Some("Default"));
}

#[test]
fn test_compile_multiple_modes() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Mode A"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "Keystroke"
keys = "a"

[[modes]]
name = "Mode B"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 37

[modes.mappings.action]
type = "Keystroke"
keys = "b"

[[modes]]
name = "Mode C"
"#;
    let rules = compile_config(config_toml, 1);
    assert_eq!(rules.mode_count(), 3);
    assert_eq!(rules.mode_name(0), Some("Mode A"));
    assert_eq!(rules.mode_name(1), Some("Mode B"));
    assert_eq!(rules.mode_name(2), Some("Mode C"));

    // Mode A: note 36 matches, note 37 doesn't
    let note36 = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let note37 = ProcessedEvent::PadPressed {
        note: 37,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };

    assert!(rules.match_event(&note36, 0, None).is_some());
    assert!(rules.match_event(&note37, 0, None).is_none());

    // Mode B: note 37 matches, note 36 doesn't
    assert!(rules.match_event(&note37, 1, None).is_some());
    assert!(rules.match_event(&note36, 1, None).is_none());

    // Mode C: empty, nothing matches
    assert!(rules.match_event(&note36, 2, None).is_none());
}

#[test]
fn test_compile_global_mappings() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[global_mappings]]
[global_mappings.trigger]
type = "Note"
note = 50

[global_mappings.action]
type = "Keystroke"
keys = "g"
"#;
    let rules = compile_config(config_toml, 1);

    let event = ProcessedEvent::PadPressed {
        note: 50,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };

    // Global rule matches in any mode
    assert!(rules.match_event(&event, 0, None).is_some());
    // Global rule matches even for non-existent mode (falls through mode check)
    assert!(rules.match_event(&event, 99, None).is_some());
}

#[test]
fn test_compile_device_filtered_mappings_indexed() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36
device = "mikro"

[modes.mappings.action]
type = "Keystroke"
keys = "a"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 37
device = "launchpad"

[modes.mappings.action]
type = "Keystroke"
keys = "b"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 38

[modes.mappings.action]
type = "Keystroke"
keys = "c"
"#;
    let rules = compile_config(config_toml, 1);

    // mikro device: note 36 matches (device-specific), note 38 matches (any-device)
    let note36 = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let note37 = ProcessedEvent::PadPressed {
        note: 37,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let note38 = ProcessedEvent::PadPressed {
        note: 38,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };

    assert!(rules.match_event(&note36, 0, Some("mikro")).is_some());
    assert!(rules.match_event(&note37, 0, Some("mikro")).is_none());
    assert!(rules.match_event(&note38, 0, Some("mikro")).is_some());

    // launchpad device: note 37 matches (device-specific), note 38 matches (any-device)
    assert!(rules.match_event(&note36, 0, Some("launchpad")).is_none());
    assert!(rules.match_event(&note37, 0, Some("launchpad")).is_some());
    assert!(rules.match_event(&note38, 0, Some("launchpad")).is_some());
}

#[test]
fn test_compile_preserves_trigger_parameters() {
    // Verify that compilation preserves parameters like velocity_min, duration_ms, etc.
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36
velocity_min = 80

[modes.mappings.action]
type = "Keystroke"
keys = "a"
"#;
    let rules = compile_config(config_toml, 1);

    // velocity 79 (below min 80) → no match
    let below = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 79,
        velocity_level: VelocityLevel::Medium,
        channel: Some(0),
    };
    assert!(rules.match_event(&below, 0, None).is_none());

    // velocity 80 (at min) → match
    let at = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 80,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&at, 0, None).is_some());
}

#[test]
fn test_compile_version_increments() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"
"#;
    let rules_v1 = compile_config(config_toml, 1);
    let rules_v2 = compile_config(config_toml, 2);
    assert_eq!(rules_v1.version(), 1);
    assert_eq!(rules_v2.version(), 2);
}

// ============================================================
// TEST GROUP 6: Integration tests
// ============================================================

#[test]
fn test_full_config_toml_compile_and_match() {
    let config_toml = r#"
[device]
name = "Full Integration"
auto_connect = false

[[modes]]
name = "Production"

[[modes.mappings]]
description = "Volume up"

[modes.mappings.trigger]
type = "Note"
note = 40

[modes.mappings.action]
type = "VolumeControl"
operation = "Up"

[[modes.mappings]]
description = "CC fader"

[modes.mappings.trigger]
type = "CC"
cc = 1

[modes.mappings.action]
type = "Keystroke"
keys = "up"

[[modes]]
name = "DJ Mode"

[[modes.mappings]]
description = "Crossfade"

[modes.mappings.trigger]
type = "EncoderTurn"
cc = 20

[modes.mappings.action]
type = "Keystroke"
keys = "left"

[[global_mappings]]
description = "Emergency stop"

[global_mappings.trigger]
type = "Note"
note = 99

[global_mappings.action]
type = "Keystroke"
keys = "escape"
"#;
    let rules = compile_config(config_toml, 1);

    assert_eq!(rules.mode_count(), 2);
    assert_eq!(rules.mode_name(0), Some("Production"));
    assert_eq!(rules.mode_name(1), Some("DJ Mode"));

    // Production mode: note 40 matches
    let note40 = ProcessedEvent::PadPressed {
        note: 40,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&note40, 0, None).is_some());

    // DJ Mode: encoder 20 matches
    let encoder20 = ProcessedEvent::EncoderTurned {
        cc: 20,
        value: 65,
        direction: EncoderDirection::Clockwise,
        delta: 1,
        channel: Some(0),
    };
    assert!(rules.match_event(&encoder20, 1, None).is_some());

    // Global: note 99 matches in any mode
    let note99 = ProcessedEvent::PadPressed {
        note: 99,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    assert!(rules.match_event(&note99, 0, None).is_some());
    assert!(rules.match_event(&note99, 1, None).is_some());
}

#[test]
fn test_config_reload_produces_new_version() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"
"#;
    let config: Config = toml::from_str(config_toml).expect("parse");
    let v1 = rule_compiler::compile(&config, 1);
    let v2 = rule_compiler::compile(&config, 2);

    assert_eq!(v1.version(), 1);
    assert_eq!(v2.version(), 2);
    assert_eq!(v1.mode_count(), v2.mode_count());
}

#[test]
fn test_compiled_rule_set_is_send_sync() {
    // Required for ArcSwap — CompiledRuleSet must be Send + Sync
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CompiledRuleSet>();
    assert_send_sync::<ModeState>();
    assert_send_sync::<Arc<CompiledRuleSet>>();
}

#[test]
fn test_mode_name_out_of_range_returns_none() {
    let rules = compile_config(BASIC_NOTE_CONFIG, 1);
    assert_eq!(rules.mode_name(99), None);
}

#[test]
fn test_global_device_filtered_rules() {
    let config_toml = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[global_mappings]]
[global_mappings.trigger]
type = "Note"
note = 50
device = "mikro"

[global_mappings.action]
type = "Keystroke"
keys = "g"

[[global_mappings]]
[global_mappings.trigger]
type = "Note"
note = 51

[global_mappings.action]
type = "Keystroke"
keys = "h"
"#;
    let rules = compile_config(config_toml, 1);

    let note50 = ProcessedEvent::PadPressed {
        note: 50,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let note51 = ProcessedEvent::PadPressed {
        note: 51,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };

    // Global device-filtered rule: matches only for correct device
    assert!(rules.match_event(&note50, 0, Some("mikro")).is_some());
    assert!(rules.match_event(&note50, 0, Some("launchpad")).is_none());
    assert!(rules.match_event(&note50, 0, None).is_none());

    // Global any-device rule: matches for any device
    assert!(rules.match_event(&note51, 0, None).is_some());
    assert!(rules.match_event(&note51, 0, Some("mikro")).is_some());
}

// ========== find_mode_index tests (ADR-009 Gap 1) ==========

#[test]
fn test_find_mode_index_exists() {
    let config_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes]]
name = "DJ"

[[modes]]
name = "Live"
"#;
    let rules = compile_config(config_str, 1);
    assert_eq!(rules.find_mode_index("Default"), Some(0));
    assert_eq!(rules.find_mode_index("DJ"), Some(1));
    assert_eq!(rules.find_mode_index("Live"), Some(2));
}

#[test]
fn test_find_mode_index_not_found() {
    let config_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"
"#;
    let rules = compile_config(config_str, 1);
    assert_eq!(rules.find_mode_index("Nonexistent"), None);
    assert_eq!(rules.find_mode_index(""), None);
}

#[test]
fn test_find_mode_index_case_sensitive() {
    let config_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"
"#;
    let rules = compile_config(config_str, 1);
    assert_eq!(rules.find_mode_index("Default"), Some(0));
    assert_eq!(rules.find_mode_index("default"), None);
    assert_eq!(rules.find_mode_index("DEFAULT"), None);
}

// ========== match_event_with_provenance tests (ADR-009 Gap K) ==========

#[test]
fn test_provenance_returns_envelope_with_mode_name() {
    let rules = compile_config(BASIC_NOTE_CONFIG, 1);
    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let envelope = rules.match_event_with_provenance(&event, 0, None);
    assert!(envelope.is_some());
    let env = envelope.unwrap();
    assert_eq!(env.mode_name.as_deref(), Some("Default"));
    assert!(env.device_id.is_none());
}

#[test]
fn test_provenance_returns_device_id() {
    let config_str = r#"
[device]
name = "Test"
auto_connect = false

[[devices]]
alias = "mikro"
port_name = "Mikro MK3"

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36
device = "mikro"

[modes.mappings.action]
type = "Text"
text = "hello"
"#;
    let rules = compile_config(config_str, 1);
    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let envelope = rules.match_event_with_provenance(&event, 0, Some("mikro"));
    assert!(envelope.is_some());
    let env = envelope.unwrap();
    assert_eq!(env.device_id.as_deref(), Some("mikro"));
    assert_eq!(env.mode_name.as_deref(), Some("Default"));
}

#[test]
fn test_provenance_no_match_returns_none() {
    let rules = compile_config(BASIC_NOTE_CONFIG, 1);
    let event = ProcessedEvent::PadPressed {
        note: 99,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };
    let envelope = rules.match_event_with_provenance(&event, 0, None);
    assert!(envelope.is_none());
}

#[test]
fn test_provenance_backward_compat_with_match_event() {
    let rules = compile_config(BASIC_NOTE_CONFIG, 1);
    let event = ProcessedEvent::PadPressed {
        note: 36,
        velocity: 100,
        velocity_level: VelocityLevel::Hard,
        channel: Some(0),
    };

    // Both methods should agree on matching
    let action = rules.match_event(&event, 0, None);
    let envelope = rules.match_event_with_provenance(&event, 0, None);

    assert!(action.is_some());
    assert!(envelope.is_some());
    // The action from match_event should equal the action from the envelope
    let env = envelope.unwrap();
    assert_eq!(
        format!("{:?}", action.unwrap()),
        format!("{:?}", env.action)
    );
}
