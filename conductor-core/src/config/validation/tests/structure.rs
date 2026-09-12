// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

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
