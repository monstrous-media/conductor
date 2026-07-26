// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-025 Phase 2.D — PcContextSwitch / CcContextSwitch `ActionConfig`
//! variants.
//!
//! This PR is the config-surface-only slice:
//!   (a) both variants deserialize from TOML,
//!   (b) both survive a JSON round-trip (used by MCP + chat persistence),
//!   (c) nested inner actions parse correctly,
//!   (d) the `default` branch is optional.
//!
//! Parse-time LOWERING to nested `Conditional` chains / `ContextSwitchTable`
//! is task #24. Runtime dispatch is task #25. Validation (overlapping CC
//! ranges, unknown device, etc.) is task #26. Those are intentionally
//! out of scope here — this PR is the typed surface only.

use conductor_core::config::types::{ActionConfig, CcRange, Config};
use indexmap::IndexMap;

fn parse(toml_str: &str) -> Config {
    toml::from_str(toml_str).expect("valid config")
}

// ────────────────────────────────────────────────────────────────
// PcContextSwitch — TOML parse
// ────────────────────────────────────────────────────────────────

#[test]
fn pc_context_switch_parses_from_toml() {
    // Canonical FCB1010-shaped config: one physical expression-pedal CC
    // (authored as the outer CC trigger — elsewhere in the mapping)
    // routes through a PcContextSwitch that selects different SendMidi
    // targets by active program change.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[modes.mappings]]
description = "Expression pedal → per-PC routing"

[modes.mappings.trigger]
type = "CC"
cc = 7
channel = 0
device = "fcb1010"

[modes.mappings.action]
type = "PcContextSwitch"
channel = 0
device = "fcb1010"

[modes.mappings.action.mappings.12]
type = "SendMidi"
port = "amp"
message_type = "CC"
channel = 0
controller = 7

[modes.mappings.action.mappings.13]
type = "SendMidi"
port = "wah"
message_type = "CC"
channel = 0
controller = 11

[modes.mappings.action.default]
type = "MidiForward"
target = "_source"
"#,
    );

    let action = &cfg.modes[0].mappings[0].action;
    match action {
        ActionConfig::PcContextSwitch {
            channel,
            device,
            mappings,
            default,
        } => {
            assert_eq!(*channel, 0);
            assert_eq!(device, "fcb1010");
            assert_eq!(mappings.len(), 2, "expected 2 PC branches");
            assert!(mappings.contains_key(&12));
            assert!(mappings.contains_key(&13));

            // Inner actions must parse as their concrete types.
            match &**mappings.get(&12).unwrap() {
                ActionConfig::SendMidi { port, .. } => assert_eq!(port, "amp"),
                other => panic!("expected SendMidi at PC 12, got {:?}", other),
            }
            match &**mappings.get(&13).unwrap() {
                ActionConfig::SendMidi { port, .. } => assert_eq!(port, "wah"),
                other => panic!("expected SendMidi at PC 13, got {:?}", other),
            }

            let default_action = default.as_ref().expect("default branch present");
            match &**default_action {
                ActionConfig::MidiForward { target, .. } => assert_eq!(target, "_source"),
                other => panic!("expected MidiForward default, got {:?}", other),
            }
        }
        other => panic!("expected PcContextSwitch, got {:?}", other),
    }
}

#[test]
fn pc_context_switch_default_is_optional() {
    // Minimal: just branches, no default. Authors who want "no-op"
    // when the active PC isn't in the table can just omit default.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "CC"
cc = 7
device = "fcb1010"

[modes.mappings.action]
type = "PcContextSwitch"
channel = 0
device = "fcb1010"

[modes.mappings.action.mappings.12]
type = "Shell"
command = "echo lead"
"#,
    );

    match &cfg.modes[0].mappings[0].action {
        ActionConfig::PcContextSwitch { default, .. } => {
            assert!(default.is_none(), "default should be None when omitted");
        }
        other => panic!("expected PcContextSwitch, got {:?}", other),
    }
}

#[test]
fn pc_context_switch_preserves_branch_ordering() {
    // IndexMap preserves insertion order. Lowering (task #24) will
    // iterate in reverse to emit a nested Conditional chain where the
    // FIRST declared branch is the OUTERMOST conditional — so authoring
    // order == eval priority. This test pins that ordering contract.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "CC"
cc = 7
device = "fcb1010"

[modes.mappings.action]
type = "PcContextSwitch"
channel = 0
device = "fcb1010"

[modes.mappings.action.mappings.42]
type = "Shell"
command = "echo first"

[modes.mappings.action.mappings.12]
type = "Shell"
command = "echo second"

[modes.mappings.action.mappings.99]
type = "Shell"
command = "echo third"
"#,
    );

    match &cfg.modes[0].mappings[0].action {
        ActionConfig::PcContextSwitch { mappings, .. } => {
            let keys: Vec<u8> = mappings.keys().copied().collect();
            assert_eq!(
                keys,
                vec![42, 12, 99],
                "IndexMap must preserve TOML authoring order"
            );
        }
        other => panic!("expected PcContextSwitch, got {:?}", other),
    }
}

#[test]
fn pc_context_switch_supports_nested_conditional_inner_action() {
    // Inner actions can themselves be any ActionConfig, including
    // another Conditional — the recursion ensures expressive power.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "CC"
cc = 7

[modes.mappings.action]
type = "PcContextSwitch"
channel = 0
device = "fcb1010"

[modes.mappings.action.mappings.12]
type = "Conditional"

[modes.mappings.action.mappings.12.condition]
type = "CcIsOn"
cc = 64
channel = 0
device = "keyboard"

[modes.mappings.action.mappings.12.then_action]
type = "Shell"
command = "echo sustain_on_and_pc12"

[modes.mappings.action.mappings.12.else_action]
type = "Shell"
command = "echo pc12_sustain_off"
"#,
    );

    match &cfg.modes[0].mappings[0].action {
        ActionConfig::PcContextSwitch { mappings, .. } => match &**mappings.get(&12).unwrap() {
            ActionConfig::Conditional { .. } => {}
            other => panic!("expected nested Conditional, got {:?}", other),
        },
        other => panic!("expected PcContextSwitch, got {:?}", other),
    }
}

// ────────────────────────────────────────────────────────────────
// CcContextSwitch — TOML parse
// ────────────────────────────────────────────────────────────────

#[test]
fn cc_context_switch_parses_from_toml() {
    // Modwheel-zones pattern: incoming CC value (from the trigger layer)
    // is routed by the CURRENT value of a separate CC (the one named in
    // this CcContextSwitch) via range matching.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "CC"
cc = 7
channel = 0
device = "keyboard"

[modes.mappings.action]
type = "CcContextSwitch"
cc = 1
channel = 0
device = "keyboard"

[[modes.mappings.action.ranges]]
min = 0
max = 31

[modes.mappings.action.ranges.action]
type = "Shell"
command = "echo low"

[[modes.mappings.action.ranges]]
min = 32
max = 95

[modes.mappings.action.ranges.action]
type = "Shell"
command = "echo mid"

[[modes.mappings.action.ranges]]
min = 96
max = 127

[modes.mappings.action.ranges.action]
type = "Shell"
command = "echo high"

[modes.mappings.action.default]
type = "Shell"
command = "echo fallback"
"#,
    );

    match &cfg.modes[0].mappings[0].action {
        ActionConfig::CcContextSwitch {
            cc,
            channel,
            device,
            ranges,
            default,
        } => {
            assert_eq!(*cc, 1);
            assert_eq!(*channel, 0);
            assert_eq!(device, "keyboard");
            assert_eq!(ranges.len(), 3);

            assert_eq!(ranges[0].min, 0);
            assert_eq!(ranges[0].max, 31);
            assert_eq!(ranges[1].min, 32);
            assert_eq!(ranges[1].max, 95);
            assert_eq!(ranges[2].min, 96);
            assert_eq!(ranges[2].max, 127);

            for (i, expected) in ["echo low", "echo mid", "echo high"].iter().enumerate() {
                match &*ranges[i].action {
                    ActionConfig::Shell { command, .. } => assert_eq!(command, *expected),
                    other => panic!("range[{}] expected Shell, got {:?}", i, other),
                }
            }

            assert!(default.is_some());
        }
        other => panic!("expected CcContextSwitch, got {:?}", other),
    }
}

#[test]
fn cc_context_switch_default_is_optional() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "CC"
cc = 7

[modes.mappings.action]
type = "CcContextSwitch"
cc = 1
channel = 0
device = "keyboard"

[[modes.mappings.action.ranges]]
min = 64
max = 127

[modes.mappings.action.ranges.action]
type = "Shell"
command = "echo loud"
"#,
    );

    match &cfg.modes[0].mappings[0].action {
        ActionConfig::CcContextSwitch { default, .. } => assert!(default.is_none()),
        other => panic!("expected CcContextSwitch, got {:?}", other),
    }
}

// ────────────────────────────────────────────────────────────────
// JSON round-trip (MCP + chat persistence path)
// ────────────────────────────────────────────────────────────────

#[test]
fn pc_context_switch_json_roundtrip() {
    // MCP tools and chat persistence use JSON, not TOML. The round-trip
    // proves serde handles both directions — important for the
    // forthcoming conductor_set_context_mapping MCP tool (task #27).
    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    mappings.insert(
        12,
        Box::new(ActionConfig::Shell {
            sandbox: None,
            command: "echo twelve".into(),
            args: None,
            timeout_ms: None,
        }),
    );
    mappings.insert(
        13,
        Box::new(ActionConfig::Shell {
            sandbox: None,
            command: "echo thirteen".into(),
            args: None,
            timeout_ms: None,
        }),
    );

    let original = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings,
        default: Some(Box::new(ActionConfig::Shell {
            sandbox: None,
            command: "echo fallback".into(),
            args: None,
            timeout_ms: None,
        })),
    };

    let json = serde_json::to_string(&original).expect("serialize");
    let decoded: ActionConfig = serde_json::from_str(&json).expect("deserialize");

    match decoded {
        ActionConfig::PcContextSwitch {
            channel,
            device,
            mappings,
            default,
        } => {
            assert_eq!(channel, 0);
            assert_eq!(device, "fcb1010");
            assert_eq!(mappings.len(), 2);
            // Ordering preserved through JSON round-trip.
            let keys: Vec<u8> = mappings.keys().copied().collect();
            assert_eq!(keys, vec![12, 13]);
            assert!(default.is_some());
        }
        other => panic!("expected PcContextSwitch, got {:?}", other),
    }
}

// ────────────────────────────────────────────────────────────────
// PR #849 review: reject out-of-range PC keys at the parse boundary
// ────────────────────────────────────────────────────────────────

#[test]
fn pc_context_switch_rejects_out_of_range_key() {
    // 200 is a valid u8 but NOT a valid MIDI Program Change (0-127).
    // Slipping through parse would leave the error latent until the
    // structural validator runs (task #26) — or never, if that path
    // misses it. Reject at the deserialisation boundary instead.
    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "CC"
cc = 7

[modes.mappings.action]
type = "PcContextSwitch"
channel = 0
device = "fcb1010"

[modes.mappings.action.mappings.200]
type = "Shell"
command = "echo out_of_range"
"#;

    let result: Result<Config, _> = toml::from_str(toml_input);
    assert!(
        result.is_err(),
        "PC key 200 should be rejected at parse, but parsed successfully"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("200") && (err.contains("0-127") || err.contains("range")),
        "error should mention the offending key and the valid range, got: {}",
        err
    );
}

#[test]
fn pc_context_switch_rejects_non_integer_key() {
    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "CC"
cc = 7

[modes.mappings.action]
type = "PcContextSwitch"
channel = 0
device = "fcb1010"

[modes.mappings.action.mappings.not_a_number]
type = "Shell"
command = "echo nope"
"#;

    let result: Result<Config, _> = toml::from_str(toml_input);
    assert!(result.is_err(), "non-integer PC key must be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not_a_number") || err.contains("integer"),
        "error should describe the malformed key, got: {}",
        err
    );
}

#[test]
fn pc_context_switch_accepts_boundary_values() {
    // Exactly 0 and exactly 127 must parse — they're the extreme
    // legal MIDI PC values.
    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "CC"
cc = 7

[modes.mappings.action]
type = "PcContextSwitch"
channel = 0
device = "fcb1010"

[modes.mappings.action.mappings.0]
type = "Shell"
command = "echo zero"

[modes.mappings.action.mappings.127]
type = "Shell"
command = "echo max"
"#;

    let cfg: Config = toml::from_str(toml_input).expect("boundary PC values should parse");
    match &cfg.modes[0].mappings[0].action {
        ActionConfig::PcContextSwitch { mappings, .. } => {
            assert!(mappings.contains_key(&0), "PC 0 should parse");
            assert!(mappings.contains_key(&127), "PC 127 should parse");
        }
        other => panic!("expected PcContextSwitch, got {:?}", other),
    }
}

// ────────────────────────────────────────────────────────────────
// PR #849 review round 2: validator field-level bounds
// ────────────────────────────────────────────────────────────────

use conductor_core::config::types::{
    ConnectorDirection, EndpointConfig, EndpointKind, Mapping, Mode, Trigger,
};
use conductor_core::config::validation::validate_config;
use conductor_core::identity::DeviceMatcher;

fn test_device(alias: &str) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![DeviceMatcher::NameContains {
                value: alias.to_string(),
            }],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    }
}

fn config_with_action(action: ActionConfig) -> Config {
    Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        // Seed the two aliases used across well-formed tests in this
        // file so ADR-025 Phase 2.G device-alias resolution doesn't
        // fire on otherwise-valid fixtures.
        endpoints: vec![test_device("fcb1010"), test_device("keyboard")],
        modes: vec![Mode {
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
fn validator_rejects_pc_context_switch_empty_device() {
    let cfg = config_with_action(ActionConfig::PcContextSwitch {
        channel: 0,
        device: "".into(),
        mappings: IndexMap::new(),
        default: None,
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("PcContextSwitch") && e.message.contains("device")),
        "expected device-required error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_pc_context_switch_bad_channel() {
    let cfg = config_with_action(ActionConfig::PcContextSwitch {
        channel: 42,
        device: "fcb1010".into(),
        mappings: IndexMap::new(),
        default: None,
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("PcContextSwitch") && e.message.contains("channel")),
        "expected channel out-of-range error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_context_switch_empty_device() {
    let cfg = config_with_action(ActionConfig::CcContextSwitch {
        cc: 1,
        channel: 0,
        device: "".into(),
        ranges: vec![],
        default: None,
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcContextSwitch") && e.message.contains("device")),
        "expected device-required error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_context_switch_bad_cc() {
    let cfg = config_with_action(ActionConfig::CcContextSwitch {
        cc: 200,
        channel: 0,
        device: "keyboard".into(),
        ranges: vec![],
        default: None,
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcContextSwitch") && e.message.contains("cc")),
        "expected cc out-of-range error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_context_switch_bad_channel() {
    let cfg = config_with_action(ActionConfig::CcContextSwitch {
        cc: 1,
        channel: 42,
        device: "keyboard".into(),
        ranges: vec![],
        default: None,
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcContextSwitch") && e.message.contains("channel")),
        "expected channel out-of-range error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_range_min_greater_than_max() {
    let cfg = config_with_action(ActionConfig::CcContextSwitch {
        cc: 1,
        channel: 0,
        device: "keyboard".into(),
        ranges: vec![CcRange {
            min: 80,
            max: 20, // backwards
            action: Box::new(ActionConfig::Shell {
                sandbox: None,
                command: "echo x".into(),
                args: None,
                timeout_ms: None,
            }),
        }],
        default: None,
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
fn validator_rejects_cc_range_max_out_of_range() {
    let cfg = config_with_action(ActionConfig::CcContextSwitch {
        cc: 1,
        channel: 0,
        device: "keyboard".into(),
        ranges: vec![CcRange {
            min: 0,
            max: 200, // MIDI max is 127
            action: Box::new(ActionConfig::Shell {
                sandbox: None,
                command: "echo x".into(),
                args: None,
                timeout_ms: None,
            }),
        }],
        default: None,
    });
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("max")),
        "expected max out-of-range error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_accepts_well_formed_pc_context_switch() {
    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    mappings.insert(
        12,
        Box::new(ActionConfig::Shell {
            sandbox: None,
            command: "echo lead".into(),
            args: None,
            timeout_ms: None,
        }),
    );
    let cfg = config_with_action(ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings,
        default: None,
    });
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "well-formed PcContextSwitch must pass, got errors: {:?}",
        report.errors
    );
}

#[test]
fn validator_accepts_well_formed_cc_context_switch() {
    let cfg = config_with_action(ActionConfig::CcContextSwitch {
        cc: 1,
        channel: 0,
        device: "keyboard".into(),
        ranges: vec![
            CcRange {
                min: 0,
                max: 63,
                action: Box::new(ActionConfig::Shell {
                    sandbox: None,
                    command: "echo low".into(),
                    args: None,
                    timeout_ms: None,
                }),
            },
            CcRange {
                min: 64,
                max: 127,
                action: Box::new(ActionConfig::Shell {
                    sandbox: None,
                    command: "echo high".into(),
                    args: None,
                    timeout_ms: None,
                }),
            },
        ],
        default: None,
    });
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "well-formed CcContextSwitch must pass, got errors: {:?}",
        report.errors
    );
}

#[test]
fn cc_context_switch_json_roundtrip() {
    let original = ActionConfig::CcContextSwitch {
        cc: 1,
        channel: 0,
        device: "keyboard".into(),
        ranges: vec![
            CcRange {
                min: 0,
                max: 63,
                action: Box::new(ActionConfig::Shell {
                    sandbox: None,
                    command: "echo low".into(),
                    args: None,
                    timeout_ms: None,
                }),
            },
            CcRange {
                min: 64,
                max: 127,
                action: Box::new(ActionConfig::Shell {
                    sandbox: None,
                    command: "echo high".into(),
                    args: None,
                    timeout_ms: None,
                }),
            },
        ],
        default: None,
    };

    let json = serde_json::to_string(&original).expect("serialize");
    let decoded: ActionConfig = serde_json::from_str(&json).expect("deserialize");

    match decoded {
        ActionConfig::CcContextSwitch { ranges, .. } => {
            assert_eq!(ranges.len(), 2);
            assert_eq!(ranges[0].min, 0);
            assert_eq!(ranges[0].max, 63);
            assert_eq!(ranges[1].min, 64);
            assert_eq!(ranges[1].max, 127);
        }
        other => panic!("expected CcContextSwitch, got {:?}", other),
    }
}

// ────────────────────────────────────────────────────────────────
// #1438: reject normalized-duplicate PC keys (`1` vs `01`)
// ────────────────────────────────────────────────────────────────

#[test]
fn pc_context_switch_rejects_normalized_duplicate_pc_key() {
    // `1` and `01` are DISTINCT TOML keys, so TOML's own duplicate-key check
    // does not fire — but both normalize to Program Change 1. A plain insert
    // would silently drop one authored branch; deserialization must fail loudly.
    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "CC"
cc = 7

[modes.mappings.action]
type = "PcContextSwitch"
channel = 0
device = "fcb1010"

[modes.mappings.action.mappings.1]
type = "Shell"
command = "echo one"

[modes.mappings.action.mappings.01]
type = "Shell"
command = "echo also_one"
"#;

    let result: Result<Config, _> = toml::from_str(toml_input);
    assert!(
        result.is_err(),
        "PC keys '1' and '01' both normalize to PC 1 and must be rejected, but parsed successfully"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("duplicate PC key") && err.contains("Program Change 1"),
        "error should be the duplicate-PC diagnostic naming normalized PC 1, got: {}",
        err
    );
}
