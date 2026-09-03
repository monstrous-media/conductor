// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ── resolve_device_io tests (GAP-B3) ──

#[test]
fn resolve_device_io_unconfigured_returns_defaults() {
    let output_map = std::collections::HashMap::new();
    let io = resolve_device_io("some_port", false, &[], &output_map);
    assert_eq!(io.direction, DeviceDirection::Input);
    assert!(io.output_port_name.is_none());
    assert!(!io.output_connected);
    assert!(!io.output_auto_paired);
}

#[test]
fn resolve_device_io_configured_no_output_stays_input() {
    use conductor_core::config::types::ConnectorDirection;
    let endpoints = vec![test_endpoint("pads", ConnectorDirection::Input)];
    let output_map = std::collections::HashMap::new();
    let io = resolve_device_io("pads", true, &endpoints, &output_map);
    assert_eq!(io.direction, DeviceDirection::Input);
    assert!(io.output_port_name.is_none());
    assert!(!io.output_connected);
}

#[test]
fn resolve_device_io_auto_paired_upgrades_to_bidirectional() {
    use conductor_core::config::types::ConnectorDirection;
    let endpoints = vec![test_endpoint("pads", ConnectorDirection::Input)];
    let output_map = make_output_map(vec![("pads", "Mikro Output", true)]);
    let io = resolve_device_io("pads", true, &endpoints, &output_map);
    assert_eq!(io.direction, DeviceDirection::Bidirectional);
    assert_eq!(io.output_port_name.as_deref(), Some("Mikro Output"));
    assert!(io.output_connected);
    assert!(io.output_auto_paired);
}

#[test]
fn resolve_device_io_explicit_output_no_direction_upgrade() {
    use conductor_core::config::types::ConnectorDirection;
    // Explicit output endpoint, not auto-paired — direction stays Output.
    let endpoints = vec![test_endpoint("synth", ConnectorDirection::Output)];
    let output_map = make_output_map(vec![("synth", "Synth Port", false)]);
    let io = resolve_device_io("synth", true, &endpoints, &output_map);
    assert_eq!(io.direction, DeviceDirection::Output);
    assert_eq!(io.output_port_name.as_deref(), Some("Synth Port"));
    assert!(io.output_connected);
    assert!(!io.output_auto_paired);
}

// ── resolve_probe_output_port tests (ADR-026 Phase 2) ──

#[test]
fn resolve_probe_output_port_unknown_port_returns_no_paired_output() {
    // No bindings at all — the input port is not known to the daemon.
    // Mirrors the "no input_manager / port not bound" early return in
    // `run_probe_device_identity`. Maps to ProbeOutcome::NoPairedOutput.
    let bindings: Vec<(DeviceId, String, bool, bool)> = vec![];
    let output_map = HashMap::new();
    let resolved = resolve_probe_output_port("Mikro IN", &bindings, &output_map);
    assert_eq!(resolved, Err(ProbeResolveError::NoPairedOutput));
}

#[test]
fn resolve_probe_output_port_unconfigured_binding_returns_no_paired_output() {
    // Port exists in the bindings list but `is_configured == false` —
    // it's an opportunistic open without a `[[bindings]]` identity, so
    // we have no alias to look up an output for.
    let bindings = vec![make_binding("mikro", "Mikro IN", true, false)];
    let mut output_map = HashMap::new();
    output_map.insert("mikro".to_string(), "Mikro OUT".to_string());
    let resolved = resolve_probe_output_port("Mikro IN", &bindings, &output_map);
    assert_eq!(resolved, Err(ProbeResolveError::NoPairedOutput));
}

#[test]
fn resolve_probe_output_port_alias_missing_in_output_map_returns_no_paired_output() {
    // Port resolves to alias "mikro", but no output is paired —
    // Phase 1.B requires an output port to send the SysEx Identity Request.
    let bindings = vec![make_binding("mikro", "Mikro IN", true, true)];
    let output_map = HashMap::new();
    let resolved = resolve_probe_output_port("Mikro IN", &bindings, &output_map);
    assert_eq!(resolved, Err(ProbeResolveError::NoPairedOutput));
}

#[test]
fn resolve_probe_output_port_happy_path_returns_paired_output() {
    // Port "Mikro IN" → alias "mikro" → output "Mikro OUT".
    // Confirms the alias indirection works as documented in the
    // ADR-021 / ADR-026 cross-reference comment in run_probe_device_identity.
    let bindings = vec![make_binding("mikro", "Mikro IN", true, true)];
    let mut output_map = HashMap::new();
    output_map.insert("mikro".to_string(), "Mikro OUT".to_string());
    let resolved = resolve_probe_output_port("Mikro IN", &bindings, &output_map);
    assert_eq!(resolved.as_deref(), Ok("Mikro OUT"));
}

#[test]
fn resolve_probe_output_port_picks_correct_alias_among_many() {
    // Multiple configured bindings — only the one whose port_name matches
    // should drive the output lookup. Guards against accidental
    // first-match / wrong-row bugs in the find() predicate.
    let bindings = vec![
        make_binding("pads", "Pads IN", true, true),
        make_binding("keys", "Keys IN", true, true),
        make_binding("fcb", "FCB1010 IN", true, true),
    ];
    let mut output_map = HashMap::new();
    output_map.insert("pads".to_string(), "Pads OUT".to_string());
    output_map.insert("keys".to_string(), "Keys OUT".to_string());
    // No FCB output — pedalboard is input-only.

    assert_eq!(
        resolve_probe_output_port("Keys IN", &bindings, &output_map).as_deref(),
        Ok("Keys OUT")
    );
    assert_eq!(
        resolve_probe_output_port("Pads IN", &bindings, &output_map).as_deref(),
        Ok("Pads OUT")
    );
    // FCB matches a configured binding but has no output — NoPairedOutput.
    assert_eq!(
        resolve_probe_output_port("FCB1010 IN", &bindings, &output_map),
        Err(ProbeResolveError::NoPairedOutput)
    );
}

#[test]
fn resolve_probe_output_port_disconnected_returns_input_disconnected() {
    // Configured binding exists with a paired output, but the device
    // is currently disconnected. The helper must distinguish this
    // from "no paired output" so the caller can surface a SendFailed
    // outcome with a re-probe-after-reconnect hint instead of the
    // misleading NoPairedOutput — the connected check and the outcome
    // disambiguation rolled into one error variant.
    let bindings = vec![make_binding("mikro", "Mikro IN", false, true)];
    let mut output_map = HashMap::new();
    output_map.insert("mikro".to_string(), "Mikro OUT".to_string());
    let resolved = resolve_probe_output_port("Mikro IN", &bindings, &output_map);
    assert_eq!(resolved, Err(ProbeResolveError::InputDisconnected));
}
