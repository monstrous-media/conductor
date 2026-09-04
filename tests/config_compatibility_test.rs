// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Configuration Backward Compatibility Test Suite
//!
//! This test suite validates that configuration files from v0.1.0 and later
//! remain compatible across all future versions of Conductor.
//!
//! Test Coverage:
//! - CFG-001 through CFG-010 from docs/config-compatibility.md
//! - All trigger types
//! - All action types
//! - All example configs from documentation
//!
//! Related Issues: AMI-125, AMI-123, AMI-126

// Integration test: diagnostic stdout/stderr output is legitimate here.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]

// Re-export config types for testing
use conductor::config::{ActionConfig, Config, Trigger};

/// Test ID: CFG-001
/// Basic config.toml loads without errors
#[test]
fn test_cfg_001_basic_config_loads() {
    let config_path = "config.toml";

    // Skip if config.toml doesn't exist (CI environment)
    if !std::path::Path::new(config_path).exists() {
        eprintln!("Skipping CFG-001: config.toml not found");
        return;
    }

    let result = Config::load(config_path);
    assert!(
        result.is_ok(),
        "CFG-001 FAILED: Basic config.toml should load without errors: {:?}",
        result.err()
    );

    let config = result.unwrap();
    // ADR-035: the legacy `[device]` block is silently ignored; the config
    // still loads and its modes parse.
    assert!(
        !config.modes.is_empty(),
        "config.toml should define at least one mode"
    );
}

/// Test ID: CFG-002
/// Enhanced config with advanced settings
#[test]
fn test_cfg_002_enhanced_config_loads() {
    let config_path = "config_enhanced.toml";

    // The sample is shipped at the repo root, so its ABSENCE is itself a
    // regression — skipping here would let an accidentally-unshipped sample pass
    // the suite green (and `Config::load` would silently create a default in its
    // place). Require it to exist before asserting it loads.
    assert!(
        std::path::Path::new(config_path).exists(),
        "CFG-002: config_enhanced.toml must be shipped at the repo root"
    );

    // The original "skip until v0.2.0" escape hatch is obsolete — every
    // advanced trigger/action type the enhanced config uses (VelocityRange,
    // LongPress, DoubleTap, EncoderTurn, VolumeControl, ModeChange) shipped years
    // ago. The enhanced config MUST load now; a warning-only skip would let a
    // real compatibility regression (e.g. the sample drifting out of sync with
    // the schema, or the loader rejecting a supported field) pass silently.
    let result = Config::load(config_path);
    let config = result.unwrap_or_else(|e| {
        panic!("CFG-002: config_enhanced.toml must load against the current schema: {e:?}")
    });
    assert!(
        config.modes.len() >= 3,
        "Enhanced config should have at least 3 modes (Default, Development, Media)"
    );
    assert!(
        !config.global_mappings.is_empty(),
        "Enhanced config should have global mappings"
    );
}

/// Test ID: CFG-003
/// Minimal config with device only
#[test]
fn test_cfg_003_minimal_device_only() {
    let minimal_toml = r#"
[device]
name = "TestDevice"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Test mapping"
[modes.mappings.trigger]
type = "Note"
note = 60
[modes.mappings.action]
type = "Keystroke"
keys = "space"
modifiers = []
"#;

    let result = toml::from_str::<Config>(minimal_toml);
    assert!(
        result.is_ok(),
        "CFG-003 FAILED: Minimal config should parse: {:?}",
        result.err()
    );

    let config = result.unwrap();
    // ADR-035: `[device]` is silently ignored; modes still parse.
    assert_eq!(config.modes.len(), 1);
}

/// Test ID: CFG-004
/// All trigger types parse correctly
#[test]
fn test_cfg_004_all_trigger_types() {
    let trigger_configs = vec![
        // Note trigger
        (
            "Note",
            r#"type = "Note"
note = 60
velocity_min = 1"#,
        ),
        // CC trigger
        (
            "CC",
            r#"type = "CC"
cc = 1
value_min = 1"#,
        ),
        // NoteChord trigger
        (
            "NoteChord",
            r#"type = "NoteChord"
notes = [60, 64, 67]"#,
        ),
    ];

    for (name, toml_str) in trigger_configs {
        let result = toml::from_str::<Trigger>(toml_str);
        assert!(
            result.is_ok(),
            "CFG-004 FAILED: {} trigger should parse: {:?}",
            name,
            result.err()
        );
    }
}

/// Test ID: CFG-005
/// All action types parse correctly
#[test]
fn test_cfg_005_all_action_types() {
    let action_configs = vec![
        // Keystroke
        (
            "Keystroke",
            r#"type = "Keystroke"
keys = "space"
modifiers = ["cmd"]"#,
        ),
        // Text
        (
            "Text",
            r#"type = "Text"
text = "Hello, World!""#,
        ),
        // Launch
        (
            "Launch",
            r#"type = "Launch"
app = "Terminal""#,
        ),
        // Shell
        (
            "Shell",
            r#"type = "Shell"
command = "echo test""#,
        ),
        // Delay
        (
            "Delay",
            r#"type = "Delay"
ms = 1000"#,
        ),
        // MouseClick
        (
            "MouseClick",
            r#"type = "MouseClick"
button = "Left""#,
        ),
    ];

    for (name, toml_str) in action_configs {
        let result = toml::from_str::<ActionConfig>(toml_str);
        assert!(
            result.is_ok(),
            "CFG-005 FAILED: {} action should parse: {:?}",
            name,
            result.err()
        );
    }
}

/// Test ID: CFG-006
/// Complex sequence actions
#[test]
fn test_cfg_006_complex_sequences() {
    let sequence_toml = r#"
type = "Sequence"
actions = [
    { type = "Keystroke", keys = "c", modifiers = ["cmd"] },
    { type = "Delay", ms = 100 },
    { type = "Keystroke", keys = "v", modifiers = ["cmd"] }
]
"#;

    let result = toml::from_str::<ActionConfig>(sequence_toml);
    assert!(
        result.is_ok(),
        "CFG-006 FAILED: Sequence action should parse: {:?}",
        result.err()
    );

    if let Ok(ActionConfig::Sequence { actions }) = result {
        assert_eq!(actions.len(), 3, "Sequence should have 3 actions");
    } else {
        panic!("CFG-006 FAILED: Parsed action is not a Sequence");
    }
}

/// Test ID: CFG-007
/// Multiple modes configuration
#[test]
fn test_cfg_007_multiple_modes() {
    let multi_mode_toml = r#"
[device]
name = "TestDevice"
auto_connect = true

[[modes]]
name = "Mode1"
color = "blue"
[[modes.mappings]]
description = "Mapping 1"
[modes.mappings.trigger]
type = "Note"
note = 60
[modes.mappings.action]
type = "Keystroke"
keys = "a"
modifiers = []

[[modes]]
name = "Mode2"
color = "green"
[[modes.mappings]]
description = "Mapping 2"
[modes.mappings.trigger]
type = "Note"
note = 61
[modes.mappings.action]
type = "Keystroke"
keys = "b"
modifiers = []

[[global_mappings]]
description = "Global mapping"
[global_mappings.trigger]
type = "Note"
note = 1
[global_mappings.action]
type = "Keystroke"
keys = "escape"
modifiers = []
"#;

    let result = toml::from_str::<Config>(multi_mode_toml);
    assert!(
        result.is_ok(),
        "CFG-007 FAILED: Multi-mode config should parse: {:?}",
        result.err()
    );

    let config = result.unwrap();
    assert_eq!(config.modes.len(), 2, "Should have 2 modes");
    assert_eq!(config.modes[0].name, "Mode1");
    assert_eq!(config.modes[1].name, "Mode2");
    assert_eq!(
        config.global_mappings.len(),
        1,
        "Should have 1 global mapping"
    );
}

/// Test ID: CFG-008
/// Global mappings only (no modes)
#[test]
fn test_cfg_008_global_mappings_only() {
    let global_only_toml = r#"
[device]
name = "TestDevice"
auto_connect = true

[[modes]]
name = "Empty"

[[global_mappings]]
description = "Global action"
[global_mappings.trigger]
type = "Note"
note = 0
[global_mappings.action]
type = "Keystroke"
keys = "escape"
modifiers = []
"#;

    let result = toml::from_str::<Config>(global_only_toml);
    assert!(
        result.is_ok(),
        "CFG-008 FAILED: Global-only config should parse: {:?}",
        result.err()
    );

    let config = result.unwrap();
    assert_eq!(config.modes.len(), 1, "Should have 1 mode (even if empty)");
    assert!(
        config.modes[0].mappings.is_empty(),
        "Mode should have no mappings"
    );
    assert_eq!(
        config.global_mappings.len(),
        1,
        "Should have 1 global mapping"
    );
}

/// Test ID: CFG-009
/// Legacy v0.1.0 syntax (exact baseline)
#[test]
fn test_cfg_009_legacy_v0_1_0_syntax() {
    let legacy_toml = r#"
[device]
name = "Mikro"
auto_connect = true

[[modes]]
name = "Default"
color = "blue"

[[modes.mappings]]
description = "Spotlight Search"
[modes.mappings.trigger]
type = "Note"
note = 60
velocity_min = 1
[modes.mappings.action]
type = "Keystroke"
keys = "space"
modifiers = ["cmd"]

[[global_mappings]]
description = "Global Action Example"
[global_mappings.trigger]
type = "Note"
note = 0
[global_mappings.action]
type = "Keystroke"
keys = "a"
modifiers = ["cmd"]
"#;

    let result = toml::from_str::<Config>(legacy_toml);
    assert!(
        result.is_ok(),
        "CFG-009 FAILED: Legacy v0.1.0 syntax should parse without errors: {:?}",
        result.err()
    );

    let config = result.unwrap();
    // ADR-035: legacy `[device]` is silently ignored; the rest still parses.
    assert_eq!(config.modes.len(), 1);
    assert_eq!(config.modes[0].mappings.len(), 1);
    assert_eq!(config.global_mappings.len(), 1);
}

/// Test ID: CFG-010
/// Invalid syntax produces clear error
#[test]
fn test_cfg_010_invalid_syntax_clear_error() {
    let invalid_configs = vec![
        // Missing required field
        (
            "Missing note field",
            r#"
[device]
name = "Test"
auto_connect = true

[[modes]]
name = "Default"
[[modes.mappings]]
description = "Invalid"
[modes.mappings.trigger]
type = "Note"
[modes.mappings.action]
type = "Keystroke"
keys = "a"
modifiers = []
"#,
        ),
        // Invalid type
        (
            "Invalid trigger type",
            r#"
[device]
name = "Test"
auto_connect = true

[[modes]]
name = "Default"
[[modes.mappings]]
description = "Invalid"
[modes.mappings.trigger]
type = "InvalidType"
[modes.mappings.action]
type = "Keystroke"
keys = "a"
modifiers = []
"#,
        ),
    ];

    for (name, toml_str) in invalid_configs {
        let result = toml::from_str::<Config>(toml_str);
        assert!(
            result.is_err(),
            "CFG-010 FAILED: {} should produce an error",
            name
        );
    }
}

/// Regression test: every shipped config file must parse against the current
/// schema.
///
/// `config_enhanced.toml` used to be allowed to fail with only a warning
/// ("v0.2.0 features not yet available"). That rationale is obsolete — all those
/// features shipped — so it's now a REQUIRED parse alongside `config.toml`. A
/// warning-only check here couldn't catch the sample drifting out of sync with
/// the schema, which is exactly the compatibility regression this guards.
#[test]
fn test_regression_all_config_files_parse() {
    let required_configs = vec!["config.toml", "config_enhanced.toml"];

    for config_file in required_configs {
        // These are shipped at the repo root; a missing one is a regression in
        // its own right. Don't skip — `Config::load` would otherwise silently
        // create a default in place of an absent file and mask it.
        assert!(
            std::path::Path::new(config_file).exists(),
            "REGRESSION FAILED: shipped config {config_file} is missing from the repo root"
        );

        let result = Config::load(config_file);
        assert!(
            result.is_ok(),
            "REGRESSION FAILED: {} must parse against the current schema: {:?}",
            config_file,
            result.err()
        );
    }
}

/// Test optional fields have sensible defaults
#[test]
fn test_optional_fields_defaults() {
    // Config without velocity_min
    let no_velocity_min = r#"
type = "Note"
note = 60
"#;

    let result = toml::from_str::<Trigger>(no_velocity_min);
    assert!(
        result.is_ok(),
        "Optional velocity_min should have default: {:?}",
        result.err()
    );

    // Config without modifiers
    let no_modifiers = r#"
type = "Keystroke"
keys = "space"
"#;

    let result = toml::from_str::<ActionConfig>(no_modifiers);
    assert!(
        result.is_ok(),
        "Optional modifiers should have default (empty array): {:?}",
        result.err()
    );
}

/// Test forward compatibility: Unknown fields are ignored
#[test]
fn test_forward_compatibility_unknown_fields_ignored() {
    let future_config = r#"
[device]
name = "Mikro"
auto_connect = true
future_field = "ignored"

[[modes]]
name = "Default"
future_mode_field = "also ignored"

[[modes.mappings]]
description = "Test"
[modes.mappings.trigger]
type = "Note"
note = 60
future_trigger_field = "ignored too"
[modes.mappings.action]
type = "Keystroke"
keys = "space"
modifiers = []
future_action_field = "still ignored"
"#;

    // Forward compatibility — unknown fields MUST be ignored — is the
    // contract this test exists to protect, so assert it rather than warning.
    // A future `#[serde(deny_unknown_fields)]` on Config (or a nested type)
    // would silently break old/newer configs; this assertion catches it.
    let result = toml::from_str::<Config>(future_config);
    assert!(
        result.is_ok(),
        "Forward compatibility broken: unknown fields must be ignored, but parsing \
         failed (is `#[serde(deny_unknown_fields)]` set somewhere?): {:?}",
        result.err()
    );
}

/// Performance test: Large configs should parse quickly
#[test]
fn test_large_config_performance() {
    use std::time::Instant;

    // Generate a large config with 100 modes and 10 mappings each
    let mut large_config = String::from(
        r#"
[device]
name = "Mikro"
auto_connect = true
"#,
    );

    for mode_idx in 0..100 {
        large_config.push_str(&format!(
            r#"
[[modes]]
name = "Mode{}"
color = "blue"
"#,
            mode_idx
        ));

        for mapping_idx in 0..10 {
            large_config.push_str(&format!(
                r#"
[[modes.mappings]]
description = "Mapping {}"
[modes.mappings.trigger]
type = "Note"
note = {}
[modes.mappings.action]
type = "Keystroke"
keys = "a"
modifiers = []
"#,
                mapping_idx,
                60 + mapping_idx
            ));
        }
    }

    let start = Instant::now();
    let result = toml::from_str::<Config>(&large_config);
    let elapsed = start.elapsed();

    assert!(
        result.is_ok(),
        "Large config should parse: {:?}",
        result.err()
    );
    assert!(
        elapsed.as_millis() < 1000,
        "Large config (100 modes, 1000 mappings) should parse in <1 second, took {:?}",
        elapsed
    );

    let config = result.unwrap();
    assert_eq!(config.modes.len(), 100);
}

/// The v0.1.0 baseline config is committed as a static fixture under
/// `tests/fixtures/v0.1.0/baseline.toml`. It opens with a legacy `[device]`
/// block.
///
/// Policy: Conductor does **not** support old config formats. A config
/// file authored with a legacy I/O block (`[device]` / `[[devices]]` /
/// `[[bindings]]` / `[[connectors]]`) is now **rejected** at `Config::load`
/// with a migration hint — it is not silently dropped. This test pins that the
/// v0.1.0 baseline is rejected on current code, rather than its old shape
/// continuing to load.
#[test]
fn test_v0_1_0_baseline_fixture_is_rejected() {
    let fixture_path = "tests/fixtures/v0.1.0/baseline.toml";
    let err = Config::load(fixture_path)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_else(|| {
            panic!("v0.1.0 baseline uses a legacy [device] block and must be rejected (#2124)")
        });
    assert!(
        err.contains("[device]") && err.contains("[[endpoints]]"),
        "rejection should name the legacy block and the [[endpoints]] migration target; got: {err}"
    );
}
