// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-039-B #1762 step 4b — `HidForward` action config-load validation.
//!
//! HidForward forwards the structured gamepad event that fired a mapping to a
//! MIDI output (V1: HidToMidi only). Validation enforces the Council safety
//! rules: the transform variant must match the target protocol, the target
//! must be a declared endpoint, and the action may only sit on an
//! exclusively-HID trigger (else the structured event is absent → silent
//! drop). Kept in its own small file to fit the LLM-Council review window.

use conductor_core::Config;
use conductor_core::config::validation::validate_config;

fn parse_or_panic(toml: &str) -> Config {
    toml::from_str(toml).expect("config parses")
}

fn assert_error_about(
    report: &conductor_core::config::validation::ValidationReport,
    fragment: &str,
) {
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.to_lowercase().contains(&fragment.to_lowercase())),
        "expected an error mentioning '{}'; got: {:#?}",
        fragment,
        report.errors
    );
}

/// A config with a single gamepad-triggered mapping whose action is the given
/// `[modes.mappings.action]` TOML body, plus a MIDI output endpoint "synth".
fn cfg_with_action(trigger_toml: &str, action_toml: &str) -> String {
    format!(
        r#"
[[endpoints]]
alias = "synth"
direction = "Output"
type = "MidiVirtualPort"
protocol = "Midi"
port_name = "Conductor: synth"

[[modes]]
name = "Default"

[[modes.mappings]]
trigger = {trigger_toml}
action = {action_toml}
"#
    )
}

const GAMEPAD_TRIGGER: &str = r#"{ type = "GamepadButton", button = 128 }"#;
const HID_TO_MIDI: &str = r#"{ type = "HidForward", target = "synth", transform = { type = "HidToMidi", trigger_to_cc = { south = 20 }, channel = 0 } }"#;

#[test]
fn hid_forward_to_midi_with_gamepad_trigger_validates() {
    let cfg = parse_or_panic(&cfg_with_action(GAMEPAD_TRIGGER, HID_TO_MIDI));
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "valid HidForward (HidToMidi → MIDI, gamepad trigger) must pass; got: {:#?}",
        report.errors
    );
}

#[test]
fn hid_forward_on_midi_trigger_is_rejected() {
    // A Note trigger is not exclusively-HID → no structured gamepad event →
    // silent drop. Must be rejected at load. Assert on the distinctive
    // "exclusively-HID" phrase so the test can't false-pass on an unrelated
    // HidForward/trigger error.
    let cfg = parse_or_panic(&cfg_with_action(
        r#"{ type = "Note", note = 60 }"#,
        HID_TO_MIDI,
    ));
    let report = validate_config(&cfg);
    assert_error_about(&report, "exclusively-HID");
}

#[test]
fn hid_forward_empty_target_is_rejected() {
    let action = r#"{ type = "HidForward", target = "", transform = { type = "HidToMidi", trigger_to_cc = { south = 20 }, channel = 0 } }"#;
    let cfg = parse_or_panic(&cfg_with_action(GAMEPAD_TRIGGER, action));
    let report = validate_config(&cfg);
    assert_error_about(&report, "target endpoint alias");
}

#[test]
fn hid_forward_nested_in_sequence_on_midi_trigger_is_rejected() {
    // The HID-trigger gate must walk nested actions: a HidForward inside a
    // Sequence on a MIDI trigger is still a silent-drop and must be rejected.
    let action = r#"{ type = "Sequence", actions = [ { type = "Text", text = "x" }, { type = "HidForward", target = "synth", transform = { type = "HidToMidi", trigger_to_cc = { south = 20 }, channel = 0 } } ] }"#;
    let cfg = parse_or_panic(&cfg_with_action(r#"{ type = "Note", note = 60 }"#, action));
    let report = validate_config(&cfg);
    assert_error_about(&report, "exclusively-HID");
}

#[test]
fn hid_forward_deeply_nested_walk_detects_and_does_not_panic() {
    // The nested-action walk is depth-bounded (MAX_ACTION_DEPTH) so it can't
    // stack-overflow config validation — defense-in-depth beyond the TOML
    // deserializer's own recursion cap. At a depth that parses, the walk must
    // still find the nested HidForward (and the gate reject it on a MIDI
    // trigger), and validation must complete without crashing.
    let mut action = r#"{ type = "HidForward", target = "synth", transform = { type = "HidToMidi", trigger_to_cc = { south = 20 }, channel = 0 } }"#.to_string();
    for _ in 0..15 {
        action = format!(r#"{{ type = "Sequence", actions = [ {action} ] }}"#);
    }
    let cfg = parse_or_panic(&cfg_with_action(r#"{ type = "Note", note = 60 }"#, &action));
    let report = validate_config(&cfg);
    assert_error_about(&report, "exclusively-HID");
}

#[test]
fn hid_forward_with_osc_transform_is_rejected() {
    let action = r#"{ type = "HidForward", target = "synth", transform = { type = "HidToOsc", trigger_to_address = { south = "/pad/a" } } }"#;
    let cfg = parse_or_panic(&cfg_with_action(GAMEPAD_TRIGGER, action));
    let report = validate_config(&cfg);
    // Rejected: V1 forwards to MIDI only — HID→OSC is route-only.
    assert_error_about(&report, "use a route");
}

#[test]
fn hid_forward_with_artnet_transform_is_rejected() {
    let action = r#"{ type = "HidForward", target = "synth", transform = { type = "HidToArtNet", trigger_to_channel = { south = 1 } } }"#;
    let cfg = parse_or_panic(&cfg_with_action(GAMEPAD_TRIGGER, action));
    let report = validate_config(&cfg);
    assert_error_about(&report, "use a route");
}

#[test]
fn hid_forward_unknown_target_is_rejected() {
    let action = r#"{ type = "HidForward", target = "ghost", transform = { type = "HidToMidi", trigger_to_cc = { south = 20 }, channel = 0 } }"#;
    let cfg = parse_or_panic(&cfg_with_action(GAMEPAD_TRIGGER, action));
    let report = validate_config(&cfg);
    assert_error_about(&report, "does not match any");
}

#[test]
fn hid_forward_hidtomidi_to_osc_target_is_rejected() {
    // Target is an OSC endpoint but the transform is HidToMidi — variant must
    // match the target protocol.
    let cfg = parse_or_panic(&format!(
        r#"
[[endpoints]]
alias = "lights"
direction = "Output"
type = "OscEndpoint"
protocol = "Osc"
host = "127.0.0.1"
port = 9000

[[modes]]
name = "Default"

[[modes.mappings]]
trigger = {GAMEPAD_TRIGGER}
action = {{ type = "HidForward", target = "lights", transform = {{ type = "HidToMidi", trigger_to_cc = {{ south = 20 }}, channel = 0 }} }}
"#
    ));
    let report = validate_config(&cfg);
    // Distinctive to the variant/target mismatch arm (not just "HidForward").
    assert_error_about(&report, "MIDI output target");
}

#[test]
fn hid_forward_channel_out_of_range_is_rejected() {
    let action = r#"{ type = "HidForward", target = "synth", transform = { type = "HidToMidi", trigger_to_cc = { south = 20 }, channel = 16 } }"#;
    let cfg = parse_or_panic(&cfg_with_action(GAMEPAD_TRIGGER, action));
    let report = validate_config(&cfg);
    assert_error_about(&report, "channel 16 out of range");
}

#[test]
fn hid_forward_cc_out_of_range_is_rejected() {
    let action = r#"{ type = "HidForward", target = "synth", transform = { type = "HidToMidi", trigger_to_cc = { south = 200 }, channel = 0 } }"#;
    let cfg = parse_or_panic(&cfg_with_action(GAMEPAD_TRIGGER, action));
    let report = validate_config(&cfg);
    assert_error_about(&report, "CC 200 for trigger 'south' out of range");
}
