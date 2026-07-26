// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Config validation tests for multi-device support (v4.24.0 - ADR-009 Phase 6,
//! migrated to the unified `[[endpoints]]` model in ADR-035).
//!
//! Tests that config validation:
//! - enforces unique endpoint aliases (no duplicates) as hard errors — this
//!   check now lives in `normalize_to_endpoints` (the load-time collision gate),
//!   not in `Config::validate()`.
//! - WARNS (does not error) when a trigger or global-mapping device reference
//!   has no matching `[[endpoints]]` alias — the config still validates (Ok),
//!   the mapping simply won't match until such an alias exists. The
//!   `*_warns` tests below pin this warning-level contract.

use conductor_core::config::loader::normalize_to_endpoints;
use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
use conductor_core::identity::DeviceMatcher;
use conductor_core::{ActionConfig, Config, Mapping, Mode, Trigger};

/// Helper to create a minimal valid config with specified endpoints and modes
fn config_with_endpoints_and_modes(
    endpoints: Vec<EndpointConfig>,
    modes: Vec<Mode>,
    global_mappings: Vec<Mapping>,
) -> Config {
    Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints,
        modes,
        global_mappings,
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

fn simple_mode(name: &str, mappings: Vec<Mapping>) -> Mode {
    Mode {
        name: name.to_string(),
        color: Some("blue".to_string()),
        mappings,
    }
}

fn note_mapping(note: u8, device: Option<&str>) -> Mapping {
    Mapping {
        trigger: Trigger::Note {
            note,
            velocity_min: None,
            channel: None,
            device: device.map(String::from),
        },
        action: ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
        description: None,
        let_through: false,
    }
}

/// A minimal Input endpoint matching on port name (ADR-035 `EndpointKind::Matcher`).
fn endpoint(alias: &str, name_contains: &str) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![DeviceMatcher::name_contains(name_contains)],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    }
}

#[test]
fn test_validate_duplicate_endpoint_aliases_rejected() {
    // ADR-035: alias uniqueness is enforced at load by `normalize_to_endpoints`,
    // the collision gate, rather than by `Config::validate()`.
    let config = config_with_endpoints_and_modes(
        vec![endpoint("pads", "Mikro"), endpoint("pads", "Launchpad")],
        vec![simple_mode("Default", vec![])],
        vec![],
    );

    let err = normalize_to_endpoints(&config).unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("'pads'") && msg.contains("more than once"),
        "Expected duplicate endpoint alias error, got: {}",
        msg
    );
}

#[test]
fn test_validate_unique_endpoint_aliases_accepted() {
    let config = config_with_endpoints_and_modes(
        vec![endpoint("pads", "Mikro"), endpoint("keys", "Launchpad")],
        vec![simple_mode("Default", vec![])],
        vec![],
    );

    assert!(normalize_to_endpoints(&config).is_ok());
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_trigger_device_ref_valid() {
    let config = config_with_endpoints_and_modes(
        vec![endpoint("pads", "Mikro")],
        vec![simple_mode("Default", vec![note_mapping(36, Some("pads"))])],
        vec![],
    );

    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_trigger_device_ref_undefined_warns() {
    // Undefined device alias is a warning (not error) — config still loads,
    // and the mapping won't match until a matching [[endpoints]] alias exists.
    let config = config_with_endpoints_and_modes(
        vec![endpoint("pads", "Mikro")],
        vec![simple_mode(
            "Default",
            vec![note_mapping(36, Some("nonexistent"))],
        )],
        vec![],
    );

    assert!(
        config.validate().is_ok(),
        "Undefined device alias should be a warning, not an error"
    );
    let report = conductor_core::config::validation::validate_config(&config);
    assert!(
        report.warnings.iter().any(|w| w
            .message
            .contains("no [[endpoints]] entry defines this alias")),
        "Expected warning about missing alias, got: {:?}",
        report.warnings
    );
}

#[test]
fn test_validate_trigger_device_ref_none_valid() {
    let config = config_with_endpoints_and_modes(
        vec![endpoint("pads", "Mikro")],
        vec![simple_mode("Default", vec![note_mapping(36, None)])],
        vec![],
    );

    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_trigger_device_ref_no_endpoints_warns() {
    // Trigger references "pads" but no endpoints are defined — this is a warning,
    // not an error (ListenMode::All auto-discovers devices without [[endpoints]]
    // entries).
    let config = config_with_endpoints_and_modes(
        vec![],
        vec![simple_mode("Default", vec![note_mapping(36, Some("pads"))])],
        vec![],
    );

    assert!(
        config.validate().is_ok(),
        "Undefined device alias should be a warning, not an error"
    );
    let report = conductor_core::config::validation::validate_config(&config);
    assert!(
        report.warnings.iter().any(|w| w
            .message
            .contains("no [[endpoints]] entry defines this alias")),
        "Expected warning about missing alias for 'pads', got: {:?}",
        report.warnings
    );
}

#[test]
fn test_validate_global_mapping_undefined_device_ref_warns() {
    let config = config_with_endpoints_and_modes(
        vec![endpoint("pads", "Mikro")],
        vec![simple_mode("Default", vec![])],
        vec![note_mapping(36, Some("bad_ref"))],
    );

    assert!(
        config.validate().is_ok(),
        "Undefined device alias should be a warning, not an error"
    );
    let report = conductor_core::config::validation::validate_config(&config);
    assert!(
        report.warnings.iter().any(|w| w
            .message
            .contains("no [[endpoints]] entry defines this alias")),
        "Expected warning about missing alias for 'bad_ref', got: {:?}",
        report.warnings
    );
}

#[test]
fn test_validate_full_multidevice_config() {
    let config = config_with_endpoints_and_modes(
        vec![endpoint("pads", "Mikro"), endpoint("keys", "Launchpad")],
        vec![
            simple_mode(
                "Default",
                vec![
                    note_mapping(36, Some("pads")),
                    note_mapping(37, Some("keys")),
                    note_mapping(38, None),
                ],
            ),
            simple_mode(
                "Alt",
                vec![note_mapping(40, Some("pads")), note_mapping(41, None)],
            ),
        ],
        vec![note_mapping(60, Some("keys")), note_mapping(61, None)],
    );

    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_mode_change_case_mismatch_suggests_correct_name() {
    let config = config_with_endpoints_and_modes(
        vec![],
        vec![
            simple_mode(
                "Dev",
                vec![Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: None,
                        channel: None,
                        device: None,
                    },
                    action: ActionConfig::ModeChange {
                        mode: "dev".to_string(), // lowercase — should suggest "Dev"
                    },
                    description: None,
                    let_through: false,
                }],
            ),
            simple_mode("Media", vec![]),
        ],
        vec![],
    );

    let err = config.validate().unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("did you mean 'Dev'"),
        "Expected case-insensitive suggestion, got: {}",
        msg
    );
}

#[test]
fn test_validate_mode_change_no_match_no_suggestion() {
    let config = config_with_endpoints_and_modes(
        vec![],
        vec![simple_mode(
            "Default",
            vec![Mapping {
                trigger: Trigger::Note {
                    note: 36,
                    velocity_min: None,
                    channel: None,
                    device: None,
                },
                action: ActionConfig::ModeChange {
                    mode: "totally_wrong".to_string(),
                },
                description: None,
                let_through: false,
            }],
        )],
        vec![],
    );

    let err = config.validate().unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("non-existent mode 'totally_wrong'"),
        "Expected non-existent mode error, got: {}",
        msg
    );
    assert!(
        !msg.contains("did you mean"),
        "Should not suggest when no case match, got: {}",
        msg
    );
}

// ────────────────────────────────────────────────────────────────
// Channel validation (0-15 range)
// ────────────────────────────────────────────────────────────────

#[test]
fn test_validate_trigger_channel_15_allowed() {
    let config = config_with_endpoints_and_modes(
        vec![],
        vec![simple_mode(
            "Default",
            vec![Mapping {
                trigger: Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: Some(15),
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "a".to_string(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        )],
        vec![],
    );
    assert!(
        config.validate().is_ok(),
        "Channel 15 should be valid (max MIDI channel)"
    );
}

#[test]
fn test_validate_trigger_channel_16_rejected() {
    let config = config_with_endpoints_and_modes(
        vec![],
        vec![simple_mode(
            "Default",
            vec![Mapping {
                trigger: Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: Some(16),
                    device: None,
                },
                action: ActionConfig::Keystroke {
                    keys: "a".to_string(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        )],
        vec![],
    );
    let result = config.validate();
    assert!(
        result.is_err(),
        "Channel 16 should be rejected (valid range 0-15)"
    );
}

#[test]
fn test_validate_trigger_channel_none_allowed() {
    let config = config_with_endpoints_and_modes(
        vec![],
        vec![simple_mode("Default", vec![note_mapping(60, None)])],
        vec![],
    );
    assert!(
        config.validate().is_ok(),
        "Channel None (match any) should be valid"
    );
}

/// ADR-035: endpoint channel-scope is validated over `endpoints[N].channels`
/// (0-15 range). A channel above 15 is a hard error.
#[test]
fn test_validate_endpoint_channel_16_rejected() {
    let mut ep = endpoint("pads", "Mikro");
    ep.channels = vec![16];
    let config =
        config_with_endpoints_and_modes(vec![ep], vec![simple_mode("Default", vec![])], vec![]);
    let err = config.validate().unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("channel 16 is out of range"),
        "Expected endpoint channel-range error, got: {}",
        msg
    );
}

/// An endpoint channel within 0-15 validates cleanly.
#[test]
fn test_validate_endpoint_channel_15_allowed() {
    let mut ep = endpoint("pads", "Mikro");
    ep.channels = vec![15];
    let config =
        config_with_endpoints_and_modes(vec![ep], vec![simple_mode("Default", vec![])], vec![]);
    assert!(
        config.validate().is_ok(),
        "Endpoint channel 15 should be valid (max MIDI channel)"
    );
}

/// ADR-038: a Tap action whose message is only whitespace is effectively
/// blank and should be rejected, mirroring the alias/command `trim()` checks
/// elsewhere in the validator (Copilot review on PR #1854).
#[test]
fn test_validate_tap_whitespace_message_rejected() {
    let tap_mapping = Mapping {
        trigger: Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        action: ActionConfig::Tap {
            message: "   ".to_string(),
        },
        description: None,
        let_through: true,
    };
    let config = config_with_endpoints_and_modes(
        vec![],
        vec![simple_mode("Default", vec![tap_mapping])],
        vec![],
    );

    let err = config.validate().unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("Tap action requires a non-empty message"),
        "Expected whitespace-only Tap message to be rejected, got: {}",
        msg
    );
}

/// A Tap action with a real (non-blank) message validates cleanly.
#[test]
fn test_validate_tap_nonempty_message_ok() {
    let tap_mapping = Mapping {
        trigger: Trigger::Note {
            note: 60,
            velocity_min: None,
            channel: None,
            device: None,
        },
        action: ActionConfig::Tap {
            message: "note {note}".to_string(),
        },
        description: None,
        let_through: true,
    };
    let config = config_with_endpoints_and_modes(
        vec![],
        vec![simple_mode("Default", vec![tap_mapping])],
        vec![],
    );

    assert!(
        config.validate().is_ok(),
        "Tap with a non-empty message should validate"
    );
}
