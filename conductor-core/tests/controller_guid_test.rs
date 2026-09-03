// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-047 §D2 — opaque SDL-GUID model matcher + generic resolver.
//!
//! Covers the `DeviceMatcher::ControllerGuid` matcher (specificity, GUID
//! equality, hex serde) and the generic `resolve_candidates` algorithm shared
//! by `PortResolver` (MIDI) and `GamepadResolver` (HID).

use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
use conductor_core::identity::DeviceMatcher;
use conductor_core::resolver::{
    BindingResult, GamepadInfo, GamepadResolver, PortInfo, PortResolver, resolve_candidates,
};

const GUID_A: [u8; 16] = [
    0x03, 0x00, 0x00, 0x00, 0x5e, 0x04, 0x00, 0x00, 0x13, 0x0b, 0x00, 0x00, 0x07, 0x05, 0x00, 0x00,
];
const GUID_B: [u8; 16] = [
    0x03, 0x00, 0x00, 0x00, 0xde, 0x28, 0x00, 0x00, 0xff, 0x11, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
];

fn ep_input(alias: &str, matchers: Vec<DeviceMatcher>) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers,
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    }
}

// ── specificity (ADR-047 §D2: 45, between ExactName 40 and UsbIdentifier 60) ──

#[test]
fn controller_guid_specificity_is_45_between_exactname_and_usb() {
    let guid = DeviceMatcher::controller_guid(GUID_A).specificity();
    assert_eq!(guid, 45);
    // Ordering relative to its neighbours (peers per the TDD plan).
    assert!(DeviceMatcher::name_contains("x").specificity() < guid); // 20 < 45
    assert!(DeviceMatcher::exact_name("x").specificity() < guid); // 40 < 45
    assert!(guid < DeviceMatcher::usb_topology("1-2").specificity()); // 45 < 50
    assert!(guid < DeviceMatcher::usb_identifier(1, 2).specificity()); // 45 < 60
}

// ── GUID equality ──

#[test]
fn controller_guid_matches_exact_bytes_only() {
    let m = DeviceMatcher::controller_guid(GUID_A);
    assert!(m.matches_with_guid(Some(GUID_A)));
    assert!(!m.matches_with_guid(Some(GUID_B)));
    assert!(!m.matches_with_guid(None));
    // A GUID matcher never matches via name/USB paths.
    assert!(!m.matches("anything"));
    assert!(!m.matches_with_usb("anything", Some(1), Some(2)));
    // Conversely, a name matcher never matches via the GUID path.
    assert!(!DeviceMatcher::exact_name("Pad").matches_with_guid(Some(GUID_A)));
}

// ── hex serde at the config/UI boundary (32-char lowercase string) ──

#[test]
fn controller_guid_serializes_as_hex_string_and_round_trips() {
    let m = DeviceMatcher::controller_guid(GUID_A);
    let json = serde_json::to_string(&m).unwrap();
    // Opaque hex string, not a 16-element byte array.
    assert!(
        json.contains("030000005e040000130b000007050000"),
        "expected lowercase hex GUID in {json}"
    );
    assert!(
        !json.contains('['),
        "GUID must serialize as a string, not an array: {json}"
    );
    let back: DeviceMatcher = serde_json::from_str(&json).unwrap();
    assert_eq!(back, m);
}

#[test]
fn controller_guid_rejects_non_ascii_value_without_panicking() {
    // A 32-BYTE value containing a multi-byte UTF-8 char would pass a naive
    // byte-length check, then panic when sliced on a non-char boundary. Must
    // deserialize-error instead of panicking.
    let value = format!("€{}", "a".repeat(29)); // 3 + 29 = 32 bytes, 30 chars
    assert_eq!(value.len(), 32);
    let json = format!(r#"{{"type":"ControllerGuid","value":"{value}"}}"#);
    assert!(serde_json::from_str::<DeviceMatcher>(&json).is_err());
}

#[test]
fn controller_guid_rejects_wrong_length_hex() {
    // 30 valid hex chars instead of 32 → deserialize error (not a panic).
    let thirty = "030000005e040000130b0000070500";
    assert_eq!(thirty.len(), 30);
    let bad = format!(r#"{{"type":"ControllerGuid","value":"{thirty}"}}"#);
    assert!(serde_json::from_str::<DeviceMatcher>(&bad).is_err());
}

// ── generic resolver over BOTH PortInfo and GamepadInfo (ADR-047 §D2) ──

#[test]
fn gamepad_resolver_binds_controller_guid_over_first_available() {
    let endpoints = vec![ep_input(
        "MyPad",
        vec![DeviceMatcher::controller_guid(GUID_B)],
    )];
    // Two connected controllers; only index 1 matches GUID_B.
    let gamepads = vec![
        GamepadInfo::new("Generic Pad", 0, GUID_A),
        GamepadInfo::new("Target Pad", 1, GUID_B),
    ];
    let results = GamepadResolver::resolve(&gamepads, &endpoints);
    assert_eq!(results.len(), 2);
    assert!(matches!(
        &results[0],
        BindingResult::Unbound { port_index: 0, .. }
    ));
    assert!(matches!(
        &results[1],
        BindingResult::Bound { port_index: 1, device_id, .. } if device_id.as_str() == "MyPad"
    ));
}

#[test]
fn controller_guid_endpoint_never_binds_a_midi_port() {
    // A MIDI port (no GUID) must never be claimed by a ControllerGuid matcher —
    // MIDI resolution is unchanged (ADR-047 §D2 acceptance #3).
    let endpoints = vec![ep_input(
        "MyPad",
        vec![DeviceMatcher::controller_guid(GUID_A)],
    )];
    let ports = vec![PortInfo::new("Some MIDI Port", 0)];
    let results = PortResolver::resolve(&ports, &endpoints);
    assert!(matches!(&results[0], BindingResult::Unbound { .. }));
}

#[test]
fn resolve_candidates_is_generic_over_name_and_guid() {
    // Same algorithm, two candidate types: a name matcher binds a MIDI port; a
    // GUID matcher binds a gamepad. Proves the trait extraction (no PortInfo
    // synthesis for gamepads).
    let name_ep = ep_input("Midi", vec![DeviceMatcher::name_contains("Mikro")]);
    let guid_ep = ep_input("Pad", vec![DeviceMatcher::controller_guid(GUID_A)]);
    let endpoints = vec![name_ep, guid_ep];

    let ports = vec![PortInfo::new("NI Mikro MK3", 0)];
    let port_results = resolve_candidates(&ports, &endpoints);
    assert!(matches!(
        &port_results[0],
        BindingResult::Bound { device_id, .. } if device_id.as_str() == "Midi"
    ));

    let gamepads = vec![GamepadInfo::new("Some Pad", 0, GUID_A)];
    let gp_results = resolve_candidates(&gamepads, &endpoints);
    assert!(matches!(
        &gp_results[0],
        BindingResult::Bound { device_id, .. } if device_id.as_str() == "Pad"
    ));
}
