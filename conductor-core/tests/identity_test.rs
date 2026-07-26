// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Tests for DeviceId, DeviceEvent, DeviceMatcher (ADR-009 Phase 1)

use conductor_core::events::InputEvent;
use conductor_core::identity::{BindingState, DeviceEvent, DeviceId, DeviceMatcher};
use std::collections::HashSet;
use std::time::Instant;

// ===== DeviceId tests =====

#[test]
fn test_device_id_from_alias() {
    let id = DeviceId::from_alias("pads");
    assert_eq!(id.as_str(), "pads");
}

#[test]
fn test_device_id_from_port_instance_zero() {
    // First instance gets no suffix
    let id = DeviceId::from_port_instance("nano", 0);
    assert_eq!(id.as_str(), "nano");
}

#[test]
fn test_device_id_from_port_instance_nonzero() {
    // Second instance gets " #2" suffix
    let id = DeviceId::from_port_instance("nano", 1);
    assert_eq!(id.as_str(), "nano #2");
}

#[test]
fn test_device_id_from_port_instance_third() {
    let id = DeviceId::from_port_instance("nano", 2);
    assert_eq!(id.as_str(), "nano #3");
}

#[test]
fn test_device_id_equality() {
    let a = DeviceId::from_alias("pads");
    let b = DeviceId::from_alias("pads");
    assert_eq!(a, b);
}

#[test]
fn test_device_id_inequality() {
    let a = DeviceId::from_alias("pads");
    let b = DeviceId::from_alias("keys");
    assert_ne!(a, b);
}

#[test]
fn test_device_id_hashing() {
    let a = DeviceId::from_alias("pads");
    let b = DeviceId::from_alias("pads");
    let mut set = HashSet::new();
    set.insert(a);
    assert!(set.contains(&b));
}

#[test]
fn test_device_id_clone() {
    let a = DeviceId::from_alias("pads");
    let b = a.clone();
    assert_eq!(a, b);
}

// ===== DeviceEvent tests =====

#[test]
fn test_device_event_wraps_correctly() {
    let device_id = DeviceId::from_alias("pads");
    let event = InputEvent::PadPressed {
        pad: 36,
        velocity: 100,
        channel: None,
        time: Instant::now(),
    };
    let device_event = DeviceEvent::new(device_id.clone(), event.clone());
    assert_eq!(device_event.device_id(), &device_id);
    assert_eq!(device_event.event(), &event);
}

#[test]
fn test_device_event_into_parts() {
    let device_id = DeviceId::from_alias("keys");
    let event = InputEvent::PadReleased {
        pad: 60,
        channel: None,
        time: Instant::now(),
    };
    let device_event = DeviceEvent::new(device_id.clone(), event.clone());
    let (id, ev) = device_event.into_parts();
    assert_eq!(id, device_id);
    assert_eq!(ev, event);
}

// ===== DeviceMatcher tests =====

#[test]
fn test_matcher_exact_name_matches() {
    let matcher = DeviceMatcher::exact_name("Maschine Mikro MK3");
    assert!(matcher.matches("Maschine Mikro MK3"));
}

#[test]
fn test_matcher_exact_name_rejects_partial() {
    let matcher = DeviceMatcher::exact_name("Maschine Mikro MK3");
    assert!(!matcher.matches("Maschine Mikro"));
}

#[test]
fn test_matcher_exact_name_rejects_extra() {
    let matcher = DeviceMatcher::exact_name("Maschine Mikro MK3");
    assert!(!matcher.matches("Maschine Mikro MK3 Port 1"));
}

#[test]
fn test_matcher_name_contains_matches_substring() {
    let matcher = DeviceMatcher::name_contains("Mikro");
    assert!(matcher.matches("Maschine Mikro MK3"));
}

#[test]
fn test_matcher_name_contains_rejects_absent() {
    let matcher = DeviceMatcher::name_contains("Launchpad");
    assert!(!matcher.matches("Maschine Mikro MK3"));
}

#[test]
fn test_matcher_name_regex_matches() {
    let matcher = DeviceMatcher::name_regex("Mikro.*MK\\d+");
    assert!(matcher.matches("Maschine Mikro MK3"));
}

#[test]
fn test_matcher_name_regex_rejects_nonmatch() {
    let matcher = DeviceMatcher::name_regex("^Launchpad");
    assert!(!matcher.matches("Maschine Mikro MK3"));
}

#[test]
fn test_matcher_name_regex_rejects_long_pattern() {
    // D2: Regex patterns > 256 chars are rejected
    let long_pattern = "a".repeat(257);
    let matcher = DeviceMatcher::name_regex(long_pattern);
    // Should not match anything (pattern is invalid/rejected)
    assert!(!matcher.matches("anything"));
}

#[test]
fn test_matcher_specificity_total_ordering() {
    // D2: CoreMidiUniqueId > UsbIdentifier > UsbTopology > ExactName > PlatformId > NameContains > NameRegex
    let matchers = [
        DeviceMatcher::name_regex(".*"),
        DeviceMatcher::name_contains("foo"),
        DeviceMatcher::platform_id("id-1"),
        DeviceMatcher::exact_name("Foo Bar"),
        DeviceMatcher::usb_topology("1-2.3"),
        DeviceMatcher::usb_identifier(0x17cc, 0x1600),
        DeviceMatcher::core_midi_unique_id(12345),
    ];

    let specificities: Vec<u32> = matchers.iter().map(|m| m.specificity()).collect();

    // Each should be strictly greater than the previous
    for i in 1..specificities.len() {
        assert!(
            specificities[i] > specificities[i - 1],
            "specificity of {:?} ({}) should be > {:?} ({})",
            matchers[i],
            specificities[i],
            matchers[i - 1],
            specificities[i - 1]
        );
    }
}

// ===== BindingState tests =====

#[test]
fn test_binding_state_bound() {
    let state = BindingState::Bound {
        device_id: DeviceId::from_alias("pads"),
        port_name: "Maschine Mikro MK3".to_string(),
    };
    assert!(matches!(state, BindingState::Bound { .. }));
}

#[test]
fn test_binding_state_unbound() {
    let state = BindingState::Unbound {
        port_name: "Unknown Device".to_string(),
    };
    assert!(matches!(state, BindingState::Unbound { .. }));
}

#[test]
fn test_binding_state_ambiguous() {
    let state = BindingState::Ambiguous {
        port_name: "nanoKONTROL2".to_string(),
        candidates: vec![
            DeviceId::from_alias("faders-1"),
            DeviceId::from_alias("faders-2"),
        ],
    };
    assert!(matches!(state, BindingState::Ambiguous { .. }));
}

// #753: UsbIdentifier matcher should work when USB metadata is provided
#[test]
fn test_usb_identifier_matches_with_metadata() {
    let matcher = DeviceMatcher::usb_identifier(0x17CC, 0x1620); // NI Mikro MK3

    // matches_with_usb should return true when VID/PID match
    assert!(matcher.matches_with_usb("Mikro MK3", Some(0x17CC), Some(0x1620)));

    // Should return false when VID/PID don't match
    assert!(!matcher.matches_with_usb("Mikro MK3", Some(0x17CC), Some(0x9999)));
    assert!(!matcher.matches_with_usb("Mikro MK3", Some(0x0000), Some(0x1620)));

    // Should return false when no USB metadata provided
    assert!(!matcher.matches_with_usb("Mikro MK3", None, None));
}

#[test]
fn test_usb_identifier_matches_port_name_still_false() {
    // matches(port_name) without USB metadata should still return false
    let matcher = DeviceMatcher::usb_identifier(0x17CC, 0x1620);
    assert!(!matcher.matches("Mikro MK3"));
}

#[test]
fn test_non_usb_matcher_ignores_usb_metadata() {
    // Non-USB matchers should work the same with matches_with_usb
    let matcher = DeviceMatcher::name_contains("Mikro");
    assert!(matcher.matches_with_usb("Mikro MK3", Some(0x17CC), Some(0x1620)));
    assert!(matcher.matches_with_usb("Mikro MK3", None, None));
    assert!(!matcher.matches_with_usb("Launchpad", Some(0x17CC), Some(0x1620)));
}

// #752: SysExIdentity variant in DeviceMatcher
use conductor_core::device_intelligence::sysex_identity::SysExIdentity;

#[test]
fn test_sysex_identity_matcher_in_device_matcher() {
    let matcher = DeviceMatcher::SysExIdentity {
        manufacturer_id: vec![0x42], // KORG
        family: Some(0x0034),
        model: None,
    };

    let identity = SysExIdentity {
        manufacturer_id: vec![0x42],
        family: 0x0034,
        model: 0x0001,
        version: [1, 0, 0, 0],
    };

    assert!(matcher.matches_with_sysex("KORG Port", Some(&identity)));
    assert!(!matcher.matches_with_sysex("KORG Port", None));
    // Wrong manufacturer
    let wrong_mfr = SysExIdentity {
        manufacturer_id: vec![0x43], // Yamaha
        family: 0x0034,
        model: 0x0001,
        version: [1, 0, 0, 0],
    };
    assert!(!matcher.matches_with_sysex("KORG Port", Some(&wrong_mfr)));
}

#[test]
fn test_sysex_identity_matcher_specificity() {
    let matcher = DeviceMatcher::SysExIdentity {
        manufacturer_id: vec![0x42],
        family: None,
        model: None,
    };
    // SysExIdentity should have high specificity (65-70 per ADR-022 D6)
    assert!(matcher.specificity() >= 65);
}
