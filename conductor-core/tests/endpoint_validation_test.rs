// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-035 — endpoint validation: non-empty-matchers invariant,
// direction legality, route alias resolution against the endpoint set, and
// the dropped `direction = Input` connector rejection.
//
// Acceptance: all-empty Matcher → error; input endpoint accepted.

use conductor_core::Config;
use conductor_core::config::validation::{Severity, validate_config};

fn parse(s: &str) -> Config {
    toml::from_str(s).expect("config parses")
}

#[test]
fn empty_matcher_endpoint_is_rejected() {
    // A Matcher with no matchers / input_matchers / output_matchers can never
    // resolve → hard error (the §4.1 non-empty invariant).
    // Alias deliberately distinct from any word in the error message, so the
    // assertion can't pass on an incidental substring match.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "blank_ep"
direction = "Input"
type = "Matcher"
"#,
    );
    let report = validate_config(&cfg);
    assert!(!report.is_valid(), "all-empty Matcher must be rejected");
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("no matchers") && e.message.contains("blank_ep")),
        "error states the reason ('no matchers') and names the endpoint: {:?}",
        report.errors
    );
}

#[test]
fn matcher_endpoint_with_matchers_is_valid() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "a Matcher endpoint with matchers is valid"
    );
}

#[test]
fn asymmetric_only_matcher_endpoint_is_valid() {
    // Empty symmetric `matchers` is fine as long as an asymmetric list is set.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "out_only"
direction = "Output"
type = "Matcher"
output_matchers = [{ type = "NameContains", value = "Synth" }]
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "output_matchers-only Matcher satisfies the non-empty invariant"
    );
}

#[test]
fn asymmetric_input_only_matcher_endpoint_is_valid() {
    // Mirror of the output_matchers-only case: input_matchers-only also
    // satisfies the non-empty invariant (guards against the check looking at
    // only one of the asymmetric lists).
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "in_only"
direction = "Input"
type = "Matcher"
input_matchers = [{ type = "NameContains", value = "Pad" }]
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "input_matchers-only Matcher satisfies the non-empty invariant"
    );
}

#[test]
fn input_direction_endpoint_is_accepted() {
    // ADR-035: input endpoints are first-class (no Input rejection).
    // ADR-042 Phase A: a network *listener* must bind loopback only, so the
    // host is `127.0.0.1` here (a non-loopback listener host is now a
    // config-load error — see network_listener_validation_test.rs).
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 9000
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "a loopback input-direction endpoint is accepted (ADR-039 listeners)"
    );
}

#[test]
fn midi_virtual_port_input_direction_warns_not_errors() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "vport"
direction = "Input"
type = "MidiVirtualPort"
port_name = "Conductor: In"
"#,
    );
    let report = validate_config(&cfg);
    assert!(
        report.is_valid(),
        "MidiVirtualPort+Input is a warning, not an error: {:?}",
        report.errors
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.severity == Severity::Warning
                && w.message.contains("MidiVirtualPort")
                && w.message.contains("Input")),
        "expected a MidiVirtualPort direction warning: {:?}",
        report.warnings
    );
}

#[test]
fn route_referencing_endpoint_aliases_resolves() {
    // Routes resolve `from`/`to` against the unified endpoint set,
    // so a route between two [[endpoints]] aliases is valid (no unknown-alias).
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "src"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "In" }]

[[endpoints]]
alias = "dst"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Out" }]

[[routes]]
from = "src"
to = "dst"
"#,
    );
    let report = validate_config(&cfg);
    assert!(
        report.is_valid(),
        "a route between two endpoint aliases resolves: {:?}",
        report.errors
    );
}

#[test]
fn route_referencing_unknown_alias_is_rejected() {
    // Negative: extending route resolution to include [[endpoints]] must NOT
    // make it accept *any* alias — an unknown `to` is still an error.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "src"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "In" }]

[[routes]]
from = "src"
to = "nope_does_not_exist"
"#,
    );
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid(),
        "a route to an unknown alias must be rejected"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("nope_does_not_exist")),
        "error names the unresolved alias: {:?}",
        report.errors
    );
}

#[test]
fn route_referencing_unknown_from_alias_is_rejected() {
    // Symmetry with the unknown-`to` case: an unknown `from` is equally invalid.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "dst"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Out" }]

[[routes]]
from = "ghost_source"
to = "dst"
"#,
    );
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid()
            && report
                .errors
                .iter()
                .any(|e| e.message.contains("ghost_source")),
        "a route from an unknown alias must be rejected: {:?}",
        report.errors
    );
}
