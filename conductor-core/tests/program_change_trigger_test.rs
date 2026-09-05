// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-025 Phase 1: ProgramChange trigger.
//!
//! Tests the new `Trigger::ProgramChange` variant, its compilation into
//! `CompiledTrigger`, and matching against `ProcessedEvent::ProgramChange`.
//! Validation covers pc/channel bounds and the "unknown DeviceRef in a
//! trigger is a warning (awaits reconnect)" rule from ADR-025 D5 v3.

use conductor_core::config::types::Config;
use conductor_core::config::validation::validate_config;
use conductor_core::event_processor::{EventProcessor, MidiEvent};
use conductor_core::{MappingEngine, ProcessedEvent};

fn parse(toml_str: &str) -> Config {
    toml::from_str(toml_str).expect("valid config")
}

// ────────────────────────────────────────────────────────────────
// Matching — exact pc/channel
// ────────────────────────────────────────────────────────────────

#[test]
fn program_change_matches_exact_pc_and_channel() {
    let cfg = parse(
        r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "PC 12 on channel 0"

[modes.mappings.trigger]
type = "ProgramChange"
pc = 12
channel = 0

[modes.mappings.action]
type = "Shell"
command = "echo pc-12"
"#,
    );
    let mut engine = MappingEngine::new();
    engine.load_from_config(&cfg);

    let event = ProcessedEvent::ProgramChange {
        program: 12,
        channel: Some(0),
    };
    let action = engine.get_action_for_processed(&event, 0);
    assert!(action.is_some(), "expected match for PC 12 ch0");
}

#[test]
fn program_change_no_match_wrong_pc() {
    let cfg = parse(
        r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "PC 12"

[modes.mappings.trigger]
type = "ProgramChange"
pc = 12

[modes.mappings.action]
type = "Shell"
command = "echo pc-12"
"#,
    );
    let mut engine = MappingEngine::new();
    engine.load_from_config(&cfg);

    let event = ProcessedEvent::ProgramChange {
        program: 7,
        channel: Some(0),
    };
    assert!(engine.get_action_for_processed(&event, 0).is_none());
}

#[test]
fn program_change_no_match_wrong_channel() {
    let cfg = parse(
        r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "PC 5 ch 0 only"

[modes.mappings.trigger]
type = "ProgramChange"
pc = 5
channel = 0

[modes.mappings.action]
type = "Shell"
command = "echo pc-5-ch0"
"#,
    );
    let mut engine = MappingEngine::new();
    engine.load_from_config(&cfg);

    let event = ProcessedEvent::ProgramChange {
        program: 5,
        channel: Some(3),
    };
    assert!(engine.get_action_for_processed(&event, 0).is_none());
}

// ────────────────────────────────────────────────────────────────
// Wildcards
// ────────────────────────────────────────────────────────────────

#[test]
fn program_change_wildcard_pc_matches_any_pc() {
    let cfg = parse(
        r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Any PC on ch 0"

[modes.mappings.trigger]
type = "ProgramChange"
channel = 0

[modes.mappings.action]
type = "Shell"
command = "echo any-pc"
"#,
    );
    let mut engine = MappingEngine::new();
    engine.load_from_config(&cfg);

    for pc in [0u8, 12, 42, 127] {
        let event = ProcessedEvent::ProgramChange {
            program: pc,
            channel: Some(0),
        };
        assert!(
            engine.get_action_for_processed(&event, 0).is_some(),
            "wildcard pc should match pc {}",
            pc
        );
    }
}

#[test]
fn program_change_wildcard_channel_matches_any_channel() {
    let cfg = parse(
        r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "PC 1 any channel"

[modes.mappings.trigger]
type = "ProgramChange"
pc = 1

[modes.mappings.action]
type = "Shell"
command = "echo pc-1"
"#,
    );
    let mut engine = MappingEngine::new();
    engine.load_from_config(&cfg);

    for ch in [0u8, 5, 15] {
        let event = ProcessedEvent::ProgramChange {
            program: 1,
            channel: Some(ch),
        };
        assert!(
            engine.get_action_for_processed(&event, 0).is_some(),
            "wildcard channel should match ch {}",
            ch
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Validation
// ────────────────────────────────────────────────────────────────

#[test]
fn validator_rejects_pc_out_of_range() {
    let cfg = parse(
        r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "invalid PC"

[modes.mappings.trigger]
type = "ProgramChange"
pc = 200

[modes.mappings.action]
type = "Shell"
command = "echo bad"
"#,
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("PC") || e.message.contains("Program")),
        "expected PC out-of-range error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_channel_out_of_range() {
    let cfg = parse(
        r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "invalid channel"

[modes.mappings.trigger]
type = "ProgramChange"
pc = 1
channel = 42

[modes.mappings.action]
type = "Shell"
command = "echo bad"
"#,
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("channel")),
        "expected channel out-of-range error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_warns_unknown_device_in_trigger() {
    // Per ADR-025 D5 v3: unknown DeviceRef in a trigger = warning
    // (awaits reconnect), not error. Only condition-bearing actions
    // escalate to error.
    let cfg = parse(
        r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "trigger refs unknown device"

[modes.mappings.trigger]
type = "ProgramChange"
pc = 1
device = "unknown_alias"

[modes.mappings.action]
type = "Shell"
command = "echo whatever"
"#,
    );
    let report = validate_config(&cfg);
    assert!(
        !report
            .errors
            .iter()
            .any(|e| e.message.contains("unknown_alias")),
        "unknown device in trigger should not be an error, got: {:?}",
        report.errors
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("unknown_alias")),
        "expected warning for unknown device, got: {:?}",
        report.warnings
    );
}

// ────────────────────────────────────────────────────────────────
// Device filter
// ────────────────────────────────────────────────────────────────

#[test]
fn program_change_with_device_filter_uses_helper() {
    // Trigger::device() and set_device() must cover the new variant,
    // otherwise alias cascading breaks.
    use conductor_core::config::types::Trigger;

    let mut t = Trigger::ProgramChange {
        pc: Some(1),
        channel: None,
        device: Some("fcb1010".into()),
    };
    assert_eq!(t.device().map(String::as_str), Some("fcb1010"));

    t.set_device(Some("mc6".into()));
    assert_eq!(t.device().map(String::as_str), Some("mc6"));

    t.set_device(None);
    assert!(t.device().is_none());
}

/// A device-filtered ProgramChange mapping must be enforced through the
/// real MappingEngine lookup — `get_action_for_processed_with_device` — not
/// merely at the `Trigger::device()` accessor level (above). It matches a
/// ProgramChange from the configured device and rejects the same event from a
/// different device. Catches a compilation / processed-event-matching
/// regression that drops the ProgramChange device filter.
#[test]
fn program_change_device_filter_matches_only_configured_device() {
    let cfg = parse(
        r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "PC 5 from fcb1010 only"

[modes.mappings.trigger]
type = "ProgramChange"
pc = 5
device = "fcb1010"

[modes.mappings.action]
type = "Shell"
command = "echo pc-5"
"#,
    );
    let mut engine = MappingEngine::new();
    engine.load_from_config(&cfg);

    let event = ProcessedEvent::ProgramChange {
        program: 5,
        channel: Some(0),
    };

    // From the configured device → matches.
    assert!(
        engine
            .get_action_for_processed_with_device(&event, 0, Some("fcb1010"))
            .is_some(),
        "PC 5 from the configured device 'fcb1010' must match"
    );
    // The same event from a different device → rejected by the device filter.
    assert!(
        engine
            .get_action_for_processed_with_device(&event, 0, Some("mc6"))
            .is_none(),
        "PC 5 from a different device must not match a device-filtered mapping"
    );
    // No device_id on the event → per `get_action_for_processed_with_device`,
    // only mappings WITHOUT a device filter are considered, so this
    // device-filtered ProgramChange mapping must not match. Pins the
    // None-arm of the device-filter path.
    assert!(
        engine
            .get_action_for_processed_with_device(&event, 0, None)
            .is_none(),
        "PC 5 with no device_id must not match a device-filtered mapping"
    );
}

// ────────────────────────────────────────────────────────────────
// The raw-MidiEvent `process` path must emit ProgramChange
// ────────────────────────────────────────────────────────────────

/// Regression test: `EventProcessor::process(MidiEvent)` dropped
/// `MidiEvent::ProgramChange` through its `_ => {}` arm, so callers parsing
/// raw MIDI into `MidiEvent` and using the public `process` path produced no
/// event for program/bank changes and `Trigger::ProgramChange` mappings could
/// not route. The processed stream must now contain a
/// `ProcessedEvent::ProgramChange { program, channel: Some(channel) }`.
#[test]
fn process_emits_program_change_from_raw_midi_event() {
    let mut processor = EventProcessor::new();
    let results = processor.process(MidiEvent::ProgramChange {
        program: 5,
        channel: 0,
        time: std::time::Instant::now(),
    });

    let pc = results.iter().find_map(|e| match e {
        ProcessedEvent::ProgramChange { program, channel } => Some((*program, *channel)),
        _ => None,
    });
    assert_eq!(
        pc,
        Some((5, Some(0))),
        "process() must emit ProcessedEvent::ProgramChange for a raw \
         MidiEvent::ProgramChange; got {results:?}"
    );
}

/// End-to-end: a ProgramChange parsed as a raw `MidiEvent` and run through
/// `process` must route to a compiled `Trigger::ProgramChange` mapping.
#[test]
fn process_program_change_routes_to_compiled_trigger() {
    let cfg = parse(
        r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "PC 5 on channel 0"

[modes.mappings.trigger]
type = "ProgramChange"
pc = 5
channel = 0

[modes.mappings.action]
type = "Shell"
command = "echo pc-5"
"#,
    );
    let mut engine = MappingEngine::new();
    engine.load_from_config(&cfg);

    let mut processor = EventProcessor::new();
    let results = processor.process(MidiEvent::ProgramChange {
        program: 5,
        channel: 0,
        time: std::time::Instant::now(),
    });

    let matched = results
        .iter()
        .any(|e| engine.get_action_for_processed(e, 0).is_some());
    assert!(
        matched,
        "a raw ProgramChange routed through process() must fire the compiled \
         Trigger::ProgramChange mapping; got {results:?}"
    );
}
