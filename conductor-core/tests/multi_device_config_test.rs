// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Tests for multi-device config parsing (ADR-009 Phase 1, ADR-035 endpoints).
//!
//! ADR-035 removed the legacy `[device]` singular block, the `[[devices]]` /
//! `[[bindings]]` blocks, and the `Config::device` / `Config::devices` /
//! `Config::primary_device()` accessors. The only authored I/O form is now
//! `[[endpoints]]`. Tests whose sole purpose was the removed legacy parsing /
//! migration / serialization-rename behavior were deleted; the rest were
//! migrated to `[[endpoints]]` preserving their assertion intent.

use conductor_core::config::ListenMode;
use conductor_core::{Config, Trigger};

// ===== Config parsing with [[endpoints]] array =====

#[test]
fn test_config_with_endpoints_array() {
    let toml_str = r#"
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "faders"
direction = "Input"
type = "Matcher"
matchers = [{ type = "ExactName", value = "nanoKONTROL2" }]

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

    let config: Config =
        toml::from_str(toml_str).expect("Failed to parse config with [[endpoints]]");
    assert_eq!(config.endpoints.len(), 2);
    assert_eq!(config.endpoints[0].alias, "pads");
    assert_eq!(config.endpoints[1].alias, "faders");
}

// ===== Trigger with device field =====

#[test]
fn test_trigger_with_device_field() {
    let toml_str = r#"
[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 60
device = "pads"

[modes.mappings.action]
type = "Keystroke"
keys = "space"
"#;

    let config: Config =
        toml::from_str(toml_str).expect("Failed to parse config with device trigger");
    let trigger = &config.modes[0].mappings[0].trigger;
    match trigger {
        Trigger::Note {
            channel: None,
            device,
            ..
        } => {
            assert_eq!(device.as_deref(), Some("pads"));
        }
        _ => panic!("Expected Note trigger"),
    }
}

#[test]
fn test_trigger_without_device_field() {
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

    let config: Config =
        toml::from_str(toml_str).expect("Failed to parse config without device trigger");
    let trigger = &config.modes[0].mappings[0].trigger;
    match trigger {
        Trigger::Note {
            channel: None,
            device,
            ..
        } => {
            assert!(device.is_none());
        }
        _ => panic!("Expected Note trigger"),
    }
}

// ===== Every device-bearing trigger variant RETAINS the device alias =====

/// Map a parsed `Trigger` to its variant name and the device alias it carries.
///
/// #1549: the device scope is the semantic value the multi-device work adds, so
/// the test must inspect the parsed value — not merely that the TOML parses. The
/// match is exhaustive over every variant, so adding or renaming a variant (or
/// dropping its `device` field) is a compile error here, keeping this guard in
/// step with the `Trigger` enum.
fn variant_name_and_device(trigger: &Trigger) -> (&'static str, Option<&str>) {
    match trigger {
        Trigger::Note { device, .. } => ("Note", device.as_deref()),
        Trigger::VelocityRange { device, .. } => ("VelocityRange", device.as_deref()),
        Trigger::LongPress { device, .. } => ("LongPress", device.as_deref()),
        Trigger::DoubleTap { device, .. } => ("DoubleTap", device.as_deref()),
        Trigger::NoteChord { device, .. } => ("NoteChord", device.as_deref()),
        Trigger::EncoderTurn { device, .. } => ("EncoderTurn", device.as_deref()),
        Trigger::Aftertouch { device, .. } => ("Aftertouch", device.as_deref()),
        Trigger::PolyAftertouch { device, .. } => ("PolyAftertouch", device.as_deref()),
        Trigger::PitchBend { device, .. } => ("PitchBend", device.as_deref()),
        Trigger::CC { device, .. } => ("CC", device.as_deref()),
        Trigger::ProgramChange { device, .. } => ("ProgramChange", device.as_deref()),
        Trigger::GamepadButton { device, .. } => ("GamepadButton", device.as_deref()),
        Trigger::GamepadButtonChord { device, .. } => ("GamepadButtonChord", device.as_deref()),
        Trigger::GamepadAnalogStick { device, .. } => ("GamepadAnalogStick", device.as_deref()),
        Trigger::GamepadTrigger { device, .. } => ("GamepadTrigger", device.as_deref()),
        Trigger::OscMessage { device, .. } => ("OscMessage", device.as_deref()),
        Trigger::OscAddressPattern { device, .. } => ("OscAddressPattern", device.as_deref()),
        Trigger::OscArgRange { device, .. } => ("OscArgRange", device.as_deref()),
    }
}

#[test]
fn test_all_trigger_variants_retain_device_field() {
    // (trigger TOML, expected variant, expected device alias). Covers every
    // device-bearing Trigger variant — including PolyAftertouch and
    // ProgramChange, which the parse-only version omitted.
    let cases: Vec<(&str, &str, &str)> = vec![
        (
            "type = \"Note\"\nnote = 60\ndevice = \"pads\"",
            "Note",
            "pads",
        ),
        (
            "type = \"VelocityRange\"\nnote = 60\ndevice = \"pads\"",
            "VelocityRange",
            "pads",
        ),
        (
            "type = \"LongPress\"\nnote = 60\ndevice = \"pads\"",
            "LongPress",
            "pads",
        ),
        (
            "type = \"DoubleTap\"\nnote = 60\ndevice = \"pads\"",
            "DoubleTap",
            "pads",
        ),
        (
            "type = \"NoteChord\"\nnotes = [60, 64]\ndevice = \"pads\"",
            "NoteChord",
            "pads",
        ),
        (
            "type = \"EncoderTurn\"\ncc = 1\ndevice = \"pads\"",
            "EncoderTurn",
            "pads",
        ),
        (
            "type = \"Aftertouch\"\ndevice = \"pads\"",
            "Aftertouch",
            "pads",
        ),
        (
            "type = \"PolyAftertouch\"\nnote = 60\ndevice = \"pads\"",
            "PolyAftertouch",
            "pads",
        ),
        (
            "type = \"PitchBend\"\ndevice = \"pads\"",
            "PitchBend",
            "pads",
        ),
        ("type = \"CC\"\ncc = 1\ndevice = \"pads\"", "CC", "pads"),
        (
            "type = \"ProgramChange\"\npc = 5\ndevice = \"pads\"",
            "ProgramChange",
            "pads",
        ),
        (
            "type = \"GamepadButton\"\nbutton = 128\ndevice = \"gamepad\"",
            "GamepadButton",
            "gamepad",
        ),
        (
            "type = \"GamepadButtonChord\"\nbuttons = [128, 129]\ndevice = \"gamepad\"",
            "GamepadButtonChord",
            "gamepad",
        ),
        (
            "type = \"GamepadAnalogStick\"\naxis = 128\ndevice = \"gamepad\"",
            "GamepadAnalogStick",
            "gamepad",
        ),
        (
            "type = \"GamepadTrigger\"\ntrigger = 132\ndevice = \"gamepad\"",
            "GamepadTrigger",
            "gamepad",
        ),
        // OSC triggers (ADR-039-A Slice 2, #2325)
        (
            "type = \"OscMessage\"\naddress = \"/eos/go\"\ndevice = \"osc-in\"",
            "OscMessage",
            "osc-in",
        ),
        (
            "type = \"OscAddressPattern\"\npattern = \"/eos/fader/*\"\ndevice = \"osc-in\"",
            "OscAddressPattern",
            "osc-in",
        ),
        (
            "type = \"OscArgRange\"\narg_index = 0\nmin = 0.5\nmax = 1.0\ndevice = \"osc-in\"",
            "OscArgRange",
            "osc-in",
        ),
    ];

    for (trigger_toml, expected_variant, expected_device) in &cases {
        let full_toml = format!(
            r#"
[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
{}

[modes.mappings.action]
type = "Keystroke"
keys = "space"
"#,
            trigger_toml
        );

        let config: Config = toml::from_str(&full_toml).unwrap_or_else(|e| {
            panic!("Trigger with device field should parse: {trigger_toml}; error: {e:?}")
        });

        let trigger = &config.modes[0].mappings[0].trigger;
        let (variant, device) = variant_name_and_device(trigger);

        // Variant identity: the TOML `type` produced the variant we expect.
        assert_eq!(
            variant, *expected_variant,
            "parsed the wrong Trigger variant for: {trigger_toml}"
        );
        // Device retention: the alias survived parsing onto THIS variant — the
        // actual multi-device contract, not merely TOML acceptance.
        assert_eq!(
            device,
            Some(*expected_device),
            "{expected_variant} dropped or misrouted its device alias for: {trigger_toml}"
        );
    }
}

// ===== AdvancedSettings new fields =====

#[test]
fn test_advanced_settings_listen_mode() {
    let toml_str = r#"
[advanced_settings]
listen_mode = "All"

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

    let config: Config = toml::from_str(toml_str).expect("Failed to parse config with listen_mode");
    assert_eq!(config.advanced_settings.listen_mode, ListenMode::All);
}

#[test]
fn test_advanced_settings_listen_mode_default() {
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

    let config: Config = toml::from_str(toml_str).expect("Failed to parse config");
    // Default is All — listen-first so all hardware is visible in GUI
    assert_eq!(config.advanced_settings.listen_mode, ListenMode::All);
}

#[test]
fn test_advanced_settings_ignore_ports() {
    let toml_str = r#"
[advanced_settings]
ignore_ports = ["IAC Driver Bus 1", "MIDI Through Port"]

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

    let config: Config =
        toml::from_str(toml_str).expect("Failed to parse config with ignore_ports");
    assert_eq!(config.advanced_settings.ignore_ports.len(), 2);
    assert_eq!(config.advanced_settings.ignore_ports[0], "IAC Driver Bus 1");
}

#[test]
fn test_advanced_settings_max_midi_ports() {
    let toml_str = r#"
[advanced_settings]
max_midi_ports = 16

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

    let config: Config =
        toml::from_str(toml_str).expect("Failed to parse config with max_midi_ports");
    assert_eq!(config.advanced_settings.max_midi_ports, 16);
}

#[test]
fn test_advanced_settings_max_midi_ports_default() {
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

    let config: Config = toml::from_str(toml_str).expect("Failed to parse config");
    // Default max_midi_ports should be 32
    assert_eq!(config.advanced_settings.max_midi_ports, 32);
}

// ===== Config.endpoints default =====

#[test]
fn test_config_endpoints_defaults_to_empty() {
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

    let config: Config = toml::from_str(toml_str).expect("Failed to parse config");
    assert!(
        config.endpoints.is_empty(),
        "endpoints should default to empty vec"
    );
}

// ===== [[endpoints]] alias parsing + round-trip =====

#[test]
fn test_endpoint_alias_parses() {
    let toml_str = r#"
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"
"#;

    let config: Config =
        toml::from_str(toml_str).expect("Failed to parse config with [[endpoints]]");
    assert_eq!(config.endpoints.len(), 1);
    assert_eq!(config.endpoints[0].alias, "pads");
}

#[test]
fn test_endpoints_round_trip() {
    let toml_str = r#"
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"
"#;

    let config: Config = toml::from_str(toml_str).expect("parse");
    let output = toml::to_string(&config).expect("serialize");
    // Serialized output uses [[endpoints]].
    assert!(
        output.contains("[[endpoints]]"),
        "Serialized config should use [[endpoints]]: {}",
        output
    );
    let config2: Config = toml::from_str(&output).expect("re-parse serialized output");
    assert_eq!(config2.endpoints.len(), 1);
    assert_eq!(config2.endpoints[0].alias, "pads");
}
