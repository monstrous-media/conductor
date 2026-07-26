// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-021 Phase 5A: Output pipeline integration tests
//! (ADR-035 Slice 9.5: migrated to the unified `[[endpoints]]` set)
//!
//! Cross-layer tests verifying auto-pair → output map → alias resolution
//! for SendMidi and MidiForward actions.

use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
use conductor_core::identity::DeviceMatcher;
use conductor_daemon::daemon::output_resolver::build_output_map;
use std::collections::HashMap;

/// Build a test endpoint with optional input/output matchers. Direction is
/// derived from which sides are present (input-only → Input, output-only →
/// Output, both → Bidirectional), matching how authored endpoints declare I/O.
fn make_test_endpoint(
    alias: &str,
    input_matchers: Option<Vec<DeviceMatcher>>,
    output_matchers: Option<Vec<DeviceMatcher>>,
) -> EndpointConfig {
    let direction = match (input_matchers.is_some(), output_matchers.is_some()) {
        (true, true) => ConnectorDirection::Bidirectional,
        (false, true) => ConnectorDirection::Output,
        _ => ConnectorDirection::Input,
    };
    EndpointConfig {
        alias: alias.to_string(),
        direction,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![],
            input_matchers: input_matchers.unwrap_or_default(),
            output_matchers: output_matchers.unwrap_or_default(),
            no_probe: false,
        },
    }
}

// ─── Scenario 2: Auto-pair → Output Map ──────────────────────────────────────

#[test]
fn test_ni_naming_auto_pair_pipeline() {
    let ep = make_test_endpoint(
        "mikro",
        Some(vec![DeviceMatcher::name_contains("Mikro Input")]),
        None,
    );
    let outputs = vec![
        "Maschine Mikro MK3 Output".to_string(),
        "Other Device".to_string(),
    ];
    let input_bindings = vec![("mikro".to_string(), "Maschine Mikro MK3 Input".to_string())];
    let map = build_output_map(&[ep], &input_bindings, &outputs);
    assert_eq!(map.len(), 1);
    assert_eq!(map["mikro"].port_name, "Maschine Mikro MK3 Output");
    assert!(map["mikro"].auto_paired);
}

#[test]
fn test_novation_naming_auto_pair_pipeline() {
    let ep = make_test_endpoint(
        "launchpad",
        Some(vec![DeviceMatcher::name_contains("Launchpad")]),
        None,
    );
    let outputs = vec!["Launchpad MIDI Out".to_string()];
    let input_bindings = vec![("launchpad".to_string(), "Launchpad MIDI In".to_string())];
    let map = build_output_map(&[ep], &input_bindings, &outputs);
    assert_eq!(map.len(), 1);
    assert_eq!(map["launchpad"].port_name, "Launchpad MIDI Out");
    assert!(map["launchpad"].auto_paired);
}

#[test]
fn test_generic_usb_midi_auto_pair_pipeline() {
    let ep = make_test_endpoint(
        "usb",
        Some(vec![DeviceMatcher::name_contains("USB MIDI")]),
        None,
    );
    let outputs = vec!["USB MIDI Device".to_string()];
    let input_bindings = vec![("usb".to_string(), "USB MIDI Device".to_string())];
    let map = build_output_map(&[ep], &input_bindings, &outputs);
    assert_eq!(map.len(), 1);
    assert_eq!(map["usb"].port_name, "USB MIDI Device");
    assert!(map["usb"].auto_paired);
}

#[test]
fn test_ambiguous_auto_pair_returns_empty_map() {
    let ep = make_test_endpoint(
        "mikro",
        Some(vec![DeviceMatcher::name_contains("Mikro")]),
        None,
    );
    // Two output ports contain "Mikro" — ambiguous, auto-pair should fail
    let outputs = vec![
        "Mikro MK3 Pro Output".to_string(),
        "Mikro MK2 Pro Output".to_string(),
    ];
    let input_bindings = vec![("mikro".to_string(), "Mikro Input".to_string())];
    let map = build_output_map(&[ep], &input_bindings, &outputs);
    assert!(
        map.is_empty(),
        "ambiguous auto-pair should produce empty map"
    );
}

#[test]
fn test_explicit_output_wins_over_auto_pair() {
    let ep = make_test_endpoint(
        "mikro",
        Some(vec![DeviceMatcher::name_contains("Mikro")]),
        Some(vec![DeviceMatcher::name_contains("Explicit Out")]),
    );
    let outputs = vec![
        "Mikro Output".to_string(),      // would auto-pair
        "Explicit Out Port".to_string(), // explicit matcher hits this
    ];
    let input_bindings = vec![("mikro".to_string(), "Mikro Input".to_string())];
    let map = build_output_map(&[ep], &input_bindings, &outputs);
    assert_eq!(map.len(), 1);
    assert_eq!(map["mikro"].port_name, "Explicit Out Port");
    assert!(
        !map["mikro"].auto_paired,
        "explicit should win over auto-pair"
    );
}

// ─── Scenario 3: Output map → alias string map (as consumed by ActionExecutor) ──

#[test]
fn test_output_map_to_string_map_for_alias_lookup() {
    // Build output map, then convert to String map — the same transform engine_manager
    // applies before storing in ActionExecutor's ArcSwap<HashMap<String, String>>.
    // ActionExecutor::resolve_output_port (private) does: map.get(alias).unwrap_or(raw_name).
    let ep = make_test_endpoint(
        "mikro",
        Some(vec![DeviceMatcher::name_contains("Mikro")]),
        Some(vec![DeviceMatcher::name_contains("Mikro MK3 Output")]),
    );
    let outputs = vec!["Maschine Mikro MK3 Output".to_string()];
    let input_bindings = vec![("mikro".to_string(), "Mikro Input".to_string())];
    let map = build_output_map(&[ep], &input_bindings, &outputs);

    // Verify explicit matcher was used (not auto-pair fallback)
    assert!(
        !map["mikro"].auto_paired,
        "should resolve via explicit output matchers"
    );

    // Convert OutputResolution map → String map (same transform engine_manager applies)
    let string_map: HashMap<String, String> =
        map.into_iter().map(|(k, v)| (k, v.port_name)).collect();

    // Alias "mikro" resolves to physical port name
    assert_eq!(
        string_map.get("mikro").map(String::as_str),
        Some("Maschine Mikro MK3 Output")
    );
    // Raw port names not in map — ActionExecutor falls back to the raw name unchanged
    assert!(!string_map.contains_key("IAC Bus 1"));
}

// ─── Scenario 4: MidiForward _source resolution ─────────────────────────────

#[test]
fn test_bidirectional_device_present_in_output_map() {
    // Bidirectional endpoint → appears in output map (auto-paired here because
    // the output_matchers don't hit the discovered port, so the input-port
    // auto-pair fallback resolves it).
    let ep = make_test_endpoint(
        "mikro",
        Some(vec![DeviceMatcher::name_contains("Mikro Input")]),
        Some(vec![DeviceMatcher::name_contains("Mikro Output")]),
    );
    let outputs = vec!["Maschine Mikro MK3 Output".to_string()];
    let input_bindings = vec![("mikro".to_string(), "Mikro Input".to_string())];
    let map = build_output_map(&[ep], &input_bindings, &outputs);
    assert!(
        map.contains_key("mikro"),
        "bidirectional device should be in output map"
    );
    assert_eq!(map["mikro"].port_name, "Maschine Mikro MK3 Output");

    // In production, resolve_source_output("mikro") would find this entry
    let string_map: HashMap<String, String> =
        map.into_iter().map(|(k, v)| (k, v.port_name)).collect();
    assert_eq!(
        string_map.get("mikro").map(String::as_str),
        Some("Maschine Mikro MK3 Output")
    );
}

#[test]
fn test_unresolvable_device_absent_from_output_map() {
    // Endpoint with no output config and no auto-pair match → absent from output map.
    // This is the precondition for TargetNotBound at runtime: ActionExecutor's
    // resolve_source_output (private) returns DispatchError::TargetNotBound when
    // the alias is missing from the map.
    let ep = make_test_endpoint(
        "mikro",
        Some(vec![DeviceMatcher::name_contains("Mikro")]),
        None,
    );
    let outputs = vec!["Unrelated Port".to_string()];
    let input_bindings = vec![("mikro".to_string(), "Mikro Input".to_string())];
    let map = build_output_map(&[ep], &input_bindings, &outputs);

    // Verify device absent from map — this is the precondition that triggers TargetNotBound
    assert!(!map.contains_key("mikro"));
    // Also verify the map is completely empty (no other devices resolved)
    assert!(map.is_empty());
}

#[test]
fn test_output_map_skips_disabled_devices() {
    let mut ep = make_test_endpoint(
        "mikro",
        Some(vec![DeviceMatcher::name_contains("Mikro")]),
        Some(vec![DeviceMatcher::name_contains("Mikro Output")]),
    );
    ep.enabled = false;
    let outputs = vec!["Mikro Output".to_string()];
    let input_bindings = vec![("mikro".to_string(), "Mikro Input".to_string())];
    let map = build_output_map(&[ep], &input_bindings, &outputs);
    assert!(map.is_empty(), "disabled device should be excluded");
}

#[test]
fn test_output_map_resolves_output_only_device() {
    // Output-only endpoint (no input binding) with explicit output matchers
    let ep = make_test_endpoint(
        "leds",
        None,
        Some(vec![DeviceMatcher::name_contains("LED")]),
    );
    let outputs = vec!["LED Controller Output".to_string()];
    let input_bindings: Vec<(String, String)> = vec![]; // no input binding
    let map = build_output_map(&[ep], &input_bindings, &outputs);
    assert_eq!(map.len(), 1);
    assert_eq!(map["leds"].port_name, "LED Controller Output");
    assert!(!map["leds"].auto_paired);
}
