// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

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
