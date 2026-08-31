// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Tests for ADR-038 Slice 1 — `let_through` flag and `Tap` action schema.
//!
//! Covers:
//! - `Mapping.let_through` parses when present, defaults to `false` when omitted,
//!   and round-trips through serialize → deserialize.
//! - `ActionConfig::Tap { message }` parses from TOML.
//! - `Tap` lowers (compiles) to the runtime `Action::Tap { message }`.
//!
//! Spec: docs/let-through/ADR-038-implementation-spec.md §4.1, §5 Slice 1.

use conductor_core::ActionConfig;
use conductor_core::actions::Action;
use conductor_core::config::compile::lower_action;
use conductor_core::config::types::Config;

/// Helper: parse a single-mapping config and return the first mapping.
fn first_mapping(toml_str: &str) -> conductor_core::Mapping {
    let config: Config = toml::from_str(toml_str).expect("config should parse");
    config
        .modes
        .into_iter()
        .next()
        .expect("one mode")
        .mappings
        .into_iter()
        .next()
        .expect("one mapping")
}

#[test]
fn let_through_present_parses_true() {
    let toml_str = r#"
[[modes]]
name = "Default"

[[modes.mappings]]
let_through = true

[modes.mappings.trigger]
type = "Note"
note = 60

[modes.mappings.action]
type = "Keystroke"
keys = "space"
"#;
    let mapping = first_mapping(toml_str);
    assert!(
        mapping.let_through,
        "let_through = true should parse as true"
    );
}

#[test]
fn let_through_omitted_defaults_false() {
    let toml_str = r#"
[[modes]]
name = "Default"

[[modes.mappings]]
description = "no let_through key"

[modes.mappings.trigger]
type = "Note"
note = 60

[modes.mappings.action]
type = "Keystroke"
keys = "space"
"#;
    let mapping = first_mapping(toml_str);
    assert!(
        !mapping.let_through,
        "omitted let_through should default to false (pre-ADR-038 swallow behaviour)"
    );
}

#[test]
fn let_through_round_trips_stable() {
    let toml_str = r#"
[[modes]]
name = "Default"

[[modes.mappings]]
let_through = true

[modes.mappings.trigger]
type = "Note"
note = 60

[modes.mappings.action]
type = "Keystroke"
keys = "space"
"#;
    let config: Config = toml::from_str(toml_str).expect("config should parse");
    let serialized = toml::to_string(&config).expect("config should serialize");
    let reparsed: Config = toml::from_str(&serialized).expect("serialized config should reparse");
    assert!(
        reparsed.modes[0].mappings[0].let_through,
        "let_through should survive a serialize → deserialize round-trip"
    );
}

#[test]
fn let_through_false_is_omitted_from_serialization() {
    // ADR-038 is purely additive: a mapping with the default `let_through = false`
    // must serialize byte-identically to a pre-ADR-038 config (no `let_through`
    // key). This keeps existing configs unchanged on re-save and preserves the
    // canonical-serialise golden contract. Deviation from the spec's literal
    // `#[serde(default)]` (which would always emit the flag) — see
    // docs/let-through/ADR-038-implementation-spec.md §4.1 annotation.
    let toml_str = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "Note"
note = 60

[modes.mappings.action]
type = "Keystroke"
keys = "space"
"#;
    let config: Config = toml::from_str(toml_str).expect("config should parse");
    let serialized = toml::to_string(&config).expect("config should serialize");
    assert!(
        !serialized.contains("let_through"),
        "let_through = false must be omitted from serialized output, got:\n{serialized}"
    );
}

#[test]
fn tap_action_parses() {
    let toml_str = r#"
[[modes]]
name = "Default"

[[modes.mappings]]
let_through = true

[modes.mappings.trigger]
type = "Note"
note = 60

[modes.mappings.action]
type = "Tap"
message = "note {note} velocity {velocity}"
"#;
    let mapping = first_mapping(toml_str);
    match mapping.action {
        ActionConfig::Tap { message } => {
            assert_eq!(message, "note {note} velocity {velocity}");
        }
        other => panic!("expected ActionConfig::Tap, got {other:?}"),
    }
}

#[test]
fn tap_action_compiles_to_runtime_tap() {
    let cfg = ActionConfig::Tap {
        message: "observed {value}".to_string(),
    };
    let compiled = lower_action(cfg);
    match compiled {
        Action::Tap { message } => assert_eq!(message, "observed {value}"),
        other => panic!("expected Action::Tap, got {other:?}"),
    }
}
