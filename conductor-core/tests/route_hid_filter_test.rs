// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-039-B #1762 step 4a — byte-filters / no-transform on HID routes.
//!
//! §6.2.1: a HID source serializes to MIDI bytes lossily (gamepad button
//! 128 → `pad & 0x7F` = MIDI note 0). A byte-filter (channels / cc_range /
//! note_range / message_types / osc_address_prefix) evaluated against that
//! lossy serialization fires on non-deterministic ghost triggers, so V1
//! allows only catch-all (no-filter) HID routes; structured HID filters are
//! deferred. The no-transform lossy passthrough (guard #2) is already
//! rejected by the cross-protocol `Required(...)` gate (HID→{MIDI,OSC,
//! ArtNet} all require an explicit transform); the regression test pins it.
//!
//! Split into its own integration-test file (not `route_validation_test.rs`)
//! so the cohesive guard suite stays small enough for the LLM-Council review
//! window (ADR-034 char budget).

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

/// Build a HID→OSC route TOML with the given `[routes.filter]` body.
fn hid_route_with_filter(filter_toml: &str) -> String {
    format!(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "xbox"
direction = "Input"
type = "Matcher"
protocol = "Hid"
matchers = [{{ type = "NameContains", value = "Xbox" }}]

[[endpoints]]
alias = "lights-osc"
direction = "Output"
type = "OscEndpoint"
protocol = "Osc"
host = "127.0.0.1"
port = 9000

[[routes]]
from = "xbox"
to = "lights-osc"
[routes.transform]
type = "HidToOsc"
trigger_to_address = {{ south = "/pad/a" }}

[routes.filter]
{filter_toml}
"#
    )
}

// Each rejection test asserts on BOTH "filter" and "HID-source" — the two
// distinctive phrases of the Rule 4b error — so a test can't false-pass on an
// unrelated filter error (e.g. a `min > max` range error also mentions
// "filter"). `assert_error_about` lower-cases, so "HID-source" matches the
// message's "HID-source routes".

#[test]
fn hid_route_with_channel_filter_is_rejected() {
    let cfg = parse_or_panic(&hid_route_with_filter("channels = [0]"));
    let report = validate_config(&cfg);
    assert_error_about(&report, "filter");
    assert_error_about(&report, "HID-source");
}

#[test]
fn hid_route_with_cc_range_filter_is_rejected() {
    let cfg = parse_or_panic(&hid_route_with_filter("cc_range = [0, 63]"));
    let report = validate_config(&cfg);
    assert_error_about(&report, "filter");
    assert_error_about(&report, "HID-source");
}

#[test]
fn hid_route_with_note_range_filter_is_rejected() {
    let cfg = parse_or_panic(&hid_route_with_filter("note_range = [36, 48]"));
    let report = validate_config(&cfg);
    assert_error_about(&report, "filter");
    assert_error_about(&report, "HID-source");
}

#[test]
fn hid_route_with_message_type_filter_is_rejected() {
    let cfg = parse_or_panic(&hid_route_with_filter("message_types = [\"NoteOn\"]"));
    let report = validate_config(&cfg);
    assert_error_about(&report, "filter");
    assert_error_about(&report, "HID-source");
}

#[test]
fn hid_route_with_osc_address_prefix_filter_is_rejected() {
    let cfg = parse_or_panic(&hid_route_with_filter("osc_address_prefix = \"/pad\""));
    let report = validate_config(&cfg);
    assert_error_about(&report, "filter");
    assert_error_about(&report, "HID-source");
}

#[test]
fn hid_catch_all_route_with_transform_validates_cleanly() {
    // No `[routes.filter]` block at all — the only HID route shape V1 allows.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "xbox"
direction = "Input"
type = "Matcher"
protocol = "Hid"
matchers = [{ type = "NameContains", value = "Xbox" }]

[[endpoints]]
alias = "lights-osc"
direction = "Output"
type = "OscEndpoint"
protocol = "Osc"
host = "127.0.0.1"
port = 9000

[[routes]]
from = "xbox"
to = "lights-osc"
[routes.transform]
type = "HidToOsc"
trigger_to_address = { south = "/pad/a" }
"#,
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "catch-all HID route must validate cleanly; got: {:#?}",
        report.errors
    );
}

#[test]
fn midi_route_with_filter_is_still_allowed() {
    // The byte-filter ban is HID-only — a MIDI-source route with a filter
    // is the normal, supported case and must NOT be swept up by the guard.
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "pads-midi"
direction = "Input"
type = "Matcher"
protocol = "Midi"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "synth-midi"
direction = "Output"
type = "MidiVirtualPort"
protocol = "Midi"
port_name = "Conductor: synth"

[[routes]]
from = "pads-midi"
to = "synth-midi"

[routes.filter]
channels = [0]
cc_range = [0, 63]
"#,
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "MIDI-source route with a filter must remain allowed; got: {:#?}",
        report.errors
    );
}

#[test]
fn hid_route_without_transform_is_rejected_regression() {
    // Regression lock for §6.2.1 guard #2: a HID source with no transform
    // is the lossy passthrough and must be rejected (already enforced by
    // the cross-protocol Required(...) gate, which names the variant).
    let cfg = parse_or_panic(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "xbox"
direction = "Input"
type = "Matcher"
protocol = "Hid"
matchers = [{ type = "NameContains", value = "Xbox" }]

[[endpoints]]
alias = "synth-midi"
direction = "Output"
type = "MidiVirtualPort"
protocol = "Midi"
port_name = "Conductor: synth"

[[routes]]
from = "xbox"
to = "synth-midi"
"#,
    );
    let report = validate_config(&cfg);
    assert_error_about(&report, "HidToMidi");
}
