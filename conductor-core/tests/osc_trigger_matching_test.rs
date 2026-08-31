// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-039-A Slice 2 (#2325): typed OSC triggers through the compiled rule
//! engine — config TOML → `rule_compiler::compile` → `CompiledRuleSet::
//! match_event_with_provenance` against `ProcessedEvent::OscReceived`.
//!
//! The OSC address and args come off the wire (attacker-controlled); these
//! tests pin the matching semantics the security review relies on: exact
//! address compare, OSC 1.0 glob (part-bounded), fallible numeric arg
//! coercion, and that NO MIDI/gamepad trigger ever matches an OSC event.

use conductor_core::actions::OscArg;
use conductor_core::config::Config;
use conductor_core::event_processor::ProcessedEvent;
use conductor_core::rule_compiler;

fn config_with_trigger(trigger_toml: &str) -> Config {
    let toml = format!(
        r#"
[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
{}

[modes.mappings.action]
type = "ModeChange"
mode = "Default"
"#,
        trigger_toml
    );
    toml::from_str(&toml).expect("valid config TOML")
}

fn osc_event(address: &str, args: Vec<OscArg>) -> ProcessedEvent {
    ProcessedEvent::OscReceived {
        address: address.to_string(),
        args,
    }
}

fn matches(trigger_toml: &str, event: &ProcessedEvent) -> bool {
    let config = config_with_trigger(trigger_toml);
    let rules = rule_compiler::compile(&config, 1);
    rules
        .match_event_with_provenance(event, 0, Some("osc-in"))
        .is_some()
}

#[test]
fn osc_message_matches_exact_address_only() {
    let t = "type = \"OscMessage\"\naddress = \"/eos/go\"";
    assert!(matches(t, &osc_event("/eos/go", vec![])));
    assert!(!matches(t, &osc_event("/eos/go/1", vec![])));
    assert!(!matches(t, &osc_event("/eos", vec![])));
    assert!(!matches(t, &osc_event("", vec![])));
}

#[test]
fn osc_address_pattern_matches_glob_semantics() {
    let t = "type = \"OscAddressPattern\"\npattern = \"/eos/fader/*\"";
    assert!(matches(t, &osc_event("/eos/fader/1", vec![])));
    assert!(matches(t, &osc_event("/eos/fader/anything", vec![])));
    assert!(
        !matches(t, &osc_event("/eos/fader/1/fine", vec![])),
        "* must not cross a part boundary"
    );
    assert!(!matches(t, &osc_event("/other/fader/1", vec![])));
}

#[test]
fn osc_arg_range_coerces_numerics_fallibly() {
    let t = "type = \"OscArgRange\"\narg_index = 0\nmin = 0.5\nmax = 1.0";
    assert!(matches(t, &osc_event("/any", vec![OscArg::Float(0.75)])));
    assert!(matches(t, &osc_event("/any", vec![OscArg::Int(1)])));
    assert!(!matches(t, &osc_event("/any", vec![OscArg::Float(0.4)])));
    assert!(
        !matches(t, &osc_event("/any", vec![OscArg::Float(f32::NAN)])),
        "NaN never matches"
    );
    assert!(
        !matches(t, &osc_event("/any", vec![OscArg::String("1".into())])),
        "string args are not coerced"
    );
    assert!(
        !matches(t, &osc_event("/any", vec![])),
        "missing index never matches"
    );
}

#[test]
fn osc_arg_range_indexes_beyond_first_arg() {
    let t = "type = \"OscArgRange\"\narg_index = 1\nmin = 10\nmax = 20";
    assert!(matches(
        t,
        &osc_event("/x", vec![OscArg::Float(0.0), OscArg::Int(15)])
    ));
    assert!(!matches(t, &osc_event("/x", vec![OscArg::Int(15)])));
}

#[test]
fn midi_triggers_never_match_osc_events() {
    // A CC trigger must not fire on an OSC event (and vice versa the OSC
    // triggers must not fire on MIDI events — covered by the typed matcher
    // arms only pairing with OscReceived).
    let t = "type = \"CC\"\ncc = 7";
    assert!(!matches(
        t,
        &osc_event("/eos/fader/7", vec![OscArg::Int(7)])
    ));
}

#[test]
fn osc_triggers_never_match_midi_events() {
    let config = config_with_trigger("type = \"OscMessage\"\naddress = \"/eos/go\"");
    let rules = rule_compiler::compile(&config, 1);
    let midi_event = ProcessedEvent::CCReceived {
        cc: 7,
        value: 64,
        channel: Some(0),
    };
    assert!(
        rules
            .match_event_with_provenance(&midi_event, 0, Some("pads"))
            .is_none()
    );
}

#[test]
fn device_filter_scopes_osc_trigger_to_listener_alias() {
    let t = "type = \"OscMessage\"\naddress = \"/eos/go\"\ndevice = \"console-a\"";
    let config = config_with_trigger(t);
    let rules = rule_compiler::compile(&config, 1);
    let ev = osc_event("/eos/go", vec![]);
    assert!(
        rules
            .match_event_with_provenance(&ev, 0, Some("console-a"))
            .is_some(),
        "matching listener alias fires"
    );
    assert!(
        rules
            .match_event_with_provenance(&ev, 0, Some("console-b"))
            .is_none(),
        "a different listener must not fire a device-scoped trigger"
    );
}
