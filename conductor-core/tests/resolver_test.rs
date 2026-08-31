// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Tests for PortResolver (ADR-009 Phase 1)

use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
use conductor_core::identity::DeviceMatcher;
use conductor_core::resolver::{BindingResult, PortInfo, PortResolver};

// ===== Test-local EndpointConfig builders =====
//
// ADR-035 removed the legacy `DeviceIdentityConfig` shape and the
// `loader::lower_binding` normalize path. `PortResolver::resolve` consumes the
// unified `EndpointConfig` set directly. These helpers build the lowered
// `EndpointConfig` the resolver sees at runtime, preserving the exact matcher
// semantics the tests below pin (input-only, output-only, bidirectional,
// asymmetric, disabled-skip, USB/SysEx metadata).

/// Input endpoint whose symmetric `matchers` drive resolution.
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

/// Disabled variant of [`ep_input`] (resolver must skip it).
fn ep_input_disabled(alias: &str, matchers: Vec<DeviceMatcher>) -> EndpointConfig {
    EndpointConfig {
        enabled: false,
        ..ep_input(alias, matchers)
    }
}

/// Input endpoint with a description (carried over from the legacy fixture).
fn ep_input_described(
    alias: &str,
    matchers: Vec<DeviceMatcher>,
    description: &str,
) -> EndpointConfig {
    EndpointConfig {
        description: Some(description.to_string()),
        ..ep_input(alias, matchers)
    }
}

/// Bidirectional asymmetric endpoint: distinct input/output matcher sets.
/// Resolution (input side) keys off `input_matchers`.
fn ep_bidi_asymmetric(
    alias: &str,
    input_matchers: Vec<DeviceMatcher>,
    output_matchers: Vec<DeviceMatcher>,
) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Bidirectional,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![],
            input_matchers,
            output_matchers,
            no_probe: false,
        },
    }
}

// ===== PortResolver tests =====

#[test]
fn test_single_port_exact_match() {
    let ports = vec![PortInfo::new("Maschine Mikro MK3".to_string(), 0)];
    let endpoints = vec![ep_input(
        "pads",
        vec![DeviceMatcher::exact_name("Maschine Mikro MK3")],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    assert!(
        matches!(&results[0], BindingResult::Bound { device_id, .. } if device_id.as_str() == "pads")
    );
}

#[test]
fn test_unmatched_port() {
    let ports = vec![PortInfo::new("Unknown Device".to_string(), 0)];
    let endpoints = vec![ep_input(
        "pads",
        vec![DeviceMatcher::exact_name("Maschine Mikro MK3")],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    assert!(matches!(&results[0], BindingResult::Unbound { .. }));
}

#[test]
fn test_multiple_ports_and_identities() {
    let ports = vec![
        PortInfo::new("Maschine Mikro MK3".to_string(), 0),
        PortInfo::new("nanoKONTROL2".to_string(), 1),
    ];
    let endpoints = vec![
        ep_input("pads", vec![DeviceMatcher::name_contains("Mikro")]),
        ep_input("faders", vec![DeviceMatcher::name_contains("nano")]),
    ];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 2);

    // First port should bind to "pads"
    assert!(
        matches!(&results[0], BindingResult::Bound { device_id, .. } if device_id.as_str() == "pads")
    );
    // Second port should bind to "faders"
    assert!(
        matches!(&results[1], BindingResult::Bound { device_id, .. } if device_id.as_str() == "faders")
    );
}

#[test]
fn test_duplicate_ports_matching_same_alias_first_bound_second_ambiguous() {
    // D7: When two ports match the same identity, first is Bound, second is Ambiguous
    let ports = vec![
        PortInfo::new("nanoKONTROL2".to_string(), 0),
        PortInfo::new("nanoKONTROL2".to_string(), 1),
    ];
    let endpoints = vec![ep_input(
        "faders",
        vec![DeviceMatcher::name_contains("nano")],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 2);

    // First port should be Bound
    assert!(
        matches!(&results[0], BindingResult::Bound { device_id, .. } if device_id.as_str() == "faders"),
        "First duplicate port should be Bound"
    );
    // Second port should be Ambiguous
    assert!(
        matches!(&results[1], BindingResult::Ambiguous { .. }),
        "Second duplicate port should be Ambiguous"
    );
}

#[test]
fn test_specificity_ordering_higher_wins() {
    // ExactName has higher specificity than NameContains
    let ports = vec![PortInfo::new("Maschine Mikro MK3".to_string(), 0)];
    let endpoints = vec![
        ep_input("generic", vec![DeviceMatcher::name_contains("Mikro")]),
        ep_input(
            "specific",
            vec![DeviceMatcher::exact_name("Maschine Mikro MK3")],
        ),
    ];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    // Higher specificity (ExactName) should win
    assert!(
        matches!(&results[0], BindingResult::Bound { device_id, .. } if device_id.as_str() == "specific")
    );
}

#[test]
fn test_empty_ports() {
    let ports: Vec<PortInfo> = vec![];
    let endpoints = vec![ep_input("pads", vec![DeviceMatcher::exact_name("Mikro")])];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert!(results.is_empty());
}

#[test]
fn test_empty_identities() {
    let ports = vec![PortInfo::new("Maschine Mikro MK3".to_string(), 0)];
    let endpoints: Vec<EndpointConfig> = vec![];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    // No identities means all ports are Unbound
    assert!(matches!(&results[0], BindingResult::Unbound { .. }));
}

#[test]
fn test_both_empty() {
    let ports: Vec<PortInfo> = vec![];
    let endpoints: Vec<EndpointConfig> = vec![];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert!(results.is_empty());
}

#[test]
fn test_multiple_matchers_on_identity_any_matches() {
    // An identity with multiple matchers should bind if any matcher matches
    let ports = vec![PortInfo::new("Maschine Mikro MK3".to_string(), 0)];
    let endpoints = vec![ep_input(
        "pads",
        vec![
            DeviceMatcher::exact_name("Launchpad"), // won't match
            DeviceMatcher::name_contains("Mikro"),  // will match
        ],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    assert!(
        matches!(&results[0], BindingResult::Bound { device_id, .. } if device_id.as_str() == "pads")
    );
}

// ===== v4.25.0 - ADR-009 Gap 4: endpoint extra fields =====

#[test]
fn test_disabled_identity_skipped() {
    let ports = vec![PortInfo::new("Maschine Mikro MK3".to_string(), 0)];
    let endpoints = vec![ep_input_disabled(
        "pads",
        vec![DeviceMatcher::exact_name("Maschine Mikro MK3")],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    assert!(matches!(&results[0], BindingResult::Unbound { .. }));
}

#[test]
fn test_disabled_identity_does_not_block_others() {
    let ports = vec![PortInfo::new("Maschine Mikro MK3".to_string(), 0)];
    let endpoints = vec![
        ep_input_disabled(
            "disabled-pads",
            vec![DeviceMatcher::exact_name("Maschine Mikro MK3")],
        ),
        ep_input_described(
            "active-pads",
            vec![DeviceMatcher::name_contains("Mikro")],
            "Active controller",
        ),
    ];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    assert!(
        matches!(&results[0], BindingResult::Bound { device_id, .. } if device_id.as_str() == "active-pads")
    );
}

// #753: UsbIdentifier matcher works in PortResolver when USB metadata provided
#[test]
fn test_usb_identifier_matches_with_metadata() {
    let ports = vec![PortInfo::new_with_usb(
        "Generic MIDI Port",
        0,
        0x17CC,
        0x1620,
    )];
    let endpoints = vec![ep_input(
        "mikro",
        vec![DeviceMatcher::usb_identifier(0x17CC, 0x1620)],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    assert!(
        matches!(&results[0], BindingResult::Bound { device_id, .. } if device_id.as_str() == "mikro")
    );
}

#[test]
fn test_usb_identifier_no_match_without_metadata() {
    let ports = vec![PortInfo::new("Generic MIDI Port".to_string(), 0)];
    let endpoints = vec![ep_input(
        "mikro",
        vec![DeviceMatcher::usb_identifier(0x17CC, 0x1620)],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    // No USB metadata → UsbIdentifier can't match → Unbound
    assert!(matches!(&results[0], BindingResult::Unbound { .. }));
}

#[test]
fn test_name_matcher_still_works_with_usb_fields() {
    // Existing NameContains matchers should work unchanged
    let ports = vec![PortInfo::new_with_usb(
        "Maschine Mikro MK3",
        0,
        0x17CC,
        0x1620,
    )];
    let endpoints = vec![ep_input(
        "pads",
        vec![DeviceMatcher::name_contains("Mikro")],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    assert!(
        matches!(&results[0], BindingResult::Bound { device_id, .. } if device_id.as_str() == "pads")
    );
}

// #752: SysExIdentity matcher works in PortResolver when identity data provided
#[test]
fn test_sysex_identity_matches_with_metadata() {
    use conductor_core::device_intelligence::sysex_identity::SysExIdentity;

    let mut port = PortInfo::new("Generic MIDI Port", 0);
    port.sysex_identity = Some(SysExIdentity {
        manufacturer_id: vec![0x42], // KORG
        family: 0x0034,
        model: 0x0001,
        version: [1, 0, 0, 0],
    });
    let ports = vec![port];
    let endpoints = vec![ep_input(
        "korg",
        vec![DeviceMatcher::SysExIdentity {
            manufacturer_id: vec![0x42],
            family: Some(0x0034),
            model: None,
        }],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    assert!(
        matches!(&results[0], BindingResult::Bound { device_id, .. } if device_id.as_str() == "korg")
    );
}

#[test]
fn test_sysex_identity_no_match_without_metadata() {
    let ports = vec![PortInfo::new("Generic MIDI Port", 0)];
    let endpoints = vec![ep_input(
        "korg",
        vec![DeviceMatcher::SysExIdentity {
            manufacturer_id: vec![0x42],
            family: None,
            model: None,
        }],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    assert!(matches!(&results[0], BindingResult::Unbound { .. }));
}

// ─── ADR-022 input.matchers regression (Phase 2 follow-up) ────────────────
//
// PortResolver previously iterated only the legacy top-level matchers,
// silently dropping bindings that put their matchers under the input side.
// Symptom: `is_configured: false` for affected devices, events flowing
// through with port names instead of aliases, and
// `ActivePcIs { device: "<alias>" }` conditions never matching. Under
// ADR-035 the equivalent shape is an asymmetric Bidirectional endpoint whose
// `input_matchers` drive resolution. This test pins the contract that the
// input matcher set DOES bind ports.

#[test]
fn input_matchers_alone_resolves_binding() {
    let ports = vec![PortInfo::new("MPK Mini Mk II".to_string(), 0)];
    // Symmetric `matchers` EMPTY — everything is declared on the input side.
    let endpoints = vec![ep_bidi_asymmetric(
        "mpk",
        vec![DeviceMatcher::name_contains("MPK Mini Mk II")],
        vec![],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert_eq!(results.len(), 1);
    match &results[0] {
        BindingResult::Bound { device_id, .. } => {
            assert_eq!(
                device_id.as_str(),
                "mpk",
                "port should bind to alias via input matchers"
            );
        }
        other => panic!("expected Bound, got {:?}", other),
    }
}

#[test]
fn legacy_matchers_still_resolve_when_input_absent() {
    // Counterpart to the above: the original Komplete-style binding
    // (symmetric matchers populated, no asymmetric override) must keep working.
    let ports = vec![PortInfo::new("Komplete Audio 6 MK2".to_string(), 0)];
    let endpoints = vec![ep_input(
        "Komplete Audio Interface",
        vec![DeviceMatcher::name_contains("Komplete Audio 6 MK2")],
    )];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert!(matches!(
        &results[0],
        BindingResult::Bound { device_id, .. }
            if device_id.as_str() == "Komplete Audio Interface"
    ));
}

#[test]
fn input_matchers_take_precedence_over_legacy_when_both_present() {
    // When both the symmetric `matchers` and the input-side `input_matchers`
    // are populated and they DISAGREE, the input matchers win on the input
    // side (the resolution direction). Documented behaviour.
    let ports = vec![PortInfo::new("Real Port".to_string(), 0)];
    let endpoints = vec![EndpointConfig {
        alias: "test".to_string(),
        direction: ConnectorDirection::Bidirectional,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            // Symmetric set says match a different name — should be ignored on
            // the input side in favour of `input_matchers`.
            matchers: vec![DeviceMatcher::name_contains("Wrong Name")],
            input_matchers: vec![DeviceMatcher::name_contains("Real Port")],
            output_matchers: vec![],
            no_probe: false,
        },
    }];

    let results = PortResolver::resolve(&ports, &endpoints);
    assert!(matches!(
        &results[0],
        BindingResult::Bound { device_id, .. } if device_id.as_str() == "test"
    ));
}
