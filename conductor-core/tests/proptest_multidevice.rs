// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Property tests for multi-device support (v4.24.0 - ADR-009 Phase 6)
//!
//! Uses proptest to verify invariants of device matching, filtering, and resolution.

use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
use conductor_core::identity::DeviceMatcher;
use conductor_core::resolver::{BindingResult, PortInfo, PortResolver};
use conductor_core::{ActionConfig, Config, Mapping, Mode, Trigger};
use proptest::prelude::*;

/// ADR-035 removed the legacy `DeviceIdentityConfig` shape; the resolver now
/// consumes the unified `EndpointConfig` set directly. Build the lowered input
/// endpoint the resolver sees at runtime.
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

/// Strategy for generating arbitrary DeviceMatcher variants (name-based only)
fn arb_name_matcher() -> impl Strategy<Value = DeviceMatcher> {
    prop_oneof![
        any::<String>().prop_map(DeviceMatcher::exact_name),
        any::<String>().prop_map(DeviceMatcher::name_contains),
        // Use safe regex patterns (no invalid regex metacharacters)
        "[a-zA-Z0-9 _-]{0,50}".prop_map(DeviceMatcher::name_regex),
    ]
}

/// Build a config with N devices each having one mapping on the same note
fn build_determinism_config(device_alias: &str, note: u8) -> Config {
    Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![ep_input(
            device_alias,
            vec![DeviceMatcher::name_contains("Test")],
        )],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note,
                    velocity_min: None,
                    channel: None,
                    device: Some(device_alias.to_string()),
                },
                action: ActionConfig::Keystroke {
                    keys: "a".to_string(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    }
}

proptest! {
    /// A device-filtered mapping matches events from the configured device and
    /// REJECTS the same event from any other device (#1489). This pins the real
    /// multi-device filter SEMANTICS — not merely that two compilations agree on
    /// a result. Determinism is retained as a secondary invariant.
    #[test]
    fn prop_device_filter_matches_only_configured_device(
        note in 0u8..128,
        velocity in 1u8..128,
    ) {
        let config = build_determinism_config("pads", note);

        let rule_set_1 = conductor_core::rule_compiler::compile(&config, 1);
        let rule_set_2 = conductor_core::rule_compiler::compile(&config, 2);

        let event = conductor_core::events::ProcessedEvent::PadPressed {
            note,
            velocity,
            velocity_level: conductor_core::events::VelocityLevel::Medium,
            channel: Some(0),
        };

        // Semantics: the only mapping is filtered to device "pads".
        let from_configured = rule_set_1.match_event(&event, 0, Some("pads")).is_some();
        let from_other = rule_set_1.match_event(&event, 0, Some("other-device")).is_some();

        prop_assert!(
            from_configured,
            "event from the configured device 'pads' must match the device-filtered mapping"
        );
        prop_assert!(
            !from_other,
            "the same event from a different device must NOT match a device-filtered mapping"
        );

        // Note-sensitivity: the same configured device but a DIFFERENT note must
        // NOT match. This makes the generated `note` causally drive the outcome
        // (the match isn't merely "device filter passes for any note") (#1489).
        let wrong_note = if note == 0 { 1 } else { note - 1 };
        let wrong_event = conductor_core::events::ProcessedEvent::PadPressed {
            note: wrong_note,
            velocity,
            velocity_level: conductor_core::events::VelocityLevel::Medium,
            channel: Some(0),
        };
        prop_assert!(
            rule_set_1.match_event(&wrong_event, 0, Some("pads")).is_none(),
            "an event with note {} must not match the mapping configured for note {}",
            wrong_note,
            note
        );

        // Determinism: a second independent compilation yields the same decision
        // for BOTH directions — match and reject (Copilot, #1489).
        let from_configured_2 = rule_set_2.match_event(&event, 0, Some("pads")).is_some();
        let from_other_2 = rule_set_2.match_event(&event, 0, Some("other-device")).is_some();
        prop_assert_eq!(from_configured, from_configured_2);
        prop_assert_eq!(from_other, from_other_2);
    }

    /// DeviceMatcher specificity ordering is strict: Regex < Contains < ExactName
    /// (only testing name-based matchers that can actually match port names)
    #[test]
    fn prop_specificity_ordering_strict(
        _input in ".*" // just to make it a proptest
    ) {
        let regex = DeviceMatcher::name_regex(".*");
        let contains = DeviceMatcher::name_contains("test");
        let exact = DeviceMatcher::exact_name("test");
        let platform_id = DeviceMatcher::platform_id("abc");
        let usb_topo = DeviceMatcher::usb_topology("1-2");
        let usb_id = DeviceMatcher::usb_identifier(0x1234, 0x5678);
        let core_midi = DeviceMatcher::core_midi_unique_id(42);

        // Verify strict total ordering
        prop_assert!(regex.specificity() < contains.specificity());
        prop_assert!(contains.specificity() < platform_id.specificity());
        prop_assert!(platform_id.specificity() < exact.specificity());
        prop_assert!(exact.specificity() < usb_topo.specificity());
        prop_assert!(usb_topo.specificity() < usb_id.specificity());
        prop_assert!(usb_id.specificity() < core_midi.specificity());
    }

    /// ExactName, NameContains, NameRegex never panic on arbitrary string inputs
    #[test]
    fn prop_matcher_matches_no_panic(
        port_name in "\\PC{0,100}",
        matcher in arb_name_matcher(),
    ) {
        // Should never panic, regardless of input
        let _ = matcher.matches(&port_name);
    }

    /// Each port resolves to the device whose name matcher matches it (#1489):
    /// `Port i` binds to `device-i` (matcher `name_contains("Port i")`) for
    /// `i < num_identities`, and is `Unbound` otherwise. This pins the real
    /// resolution SEMANTICS — which port maps to which device — not merely that
    /// two `resolve()` calls agree. Determinism is retained as a secondary
    /// invariant. (`Port 0..4` and single-digit matchers give a clean 1:1
    /// mapping with no cross-matches or specificity ties.)
    #[test]
    fn prop_resolver_binds_each_port_to_its_matching_device(
        // Bounded to single-digit names on purpose: `name_contains("Port 1")`
        // would cross-match "Port 10"/"Port 11" if this were widened past 9,
        // breaking the 1:1 mapping the assertions below rely on (Copilot, #1489).
        num_ports in 1usize..5,
        num_identities in 1usize..4,
    ) {
        let ports: Vec<PortInfo> = (0..num_ports)
            .map(|i| PortInfo::new(format!("Port {}", i), i))
            .collect();

        let endpoints: Vec<EndpointConfig> = (0..num_identities)
            .map(|i| {
                ep_input(
                    &format!("device-{}", i),
                    vec![DeviceMatcher::name_contains(format!("Port {}", i))],
                )
            })
            .collect();

        let result_1 = PortResolver::resolve(&ports, &endpoints);
        let result_2 = PortResolver::resolve(&ports, &endpoints);

        // One binding result per port.
        prop_assert_eq!(result_1.len(), num_ports);

        // Cardinality: with a clean 1:1 name mapping, exactly the first
        // `min(num_ports, num_identities)` ports bind and the rest are Unbound;
        // none are Ambiguous.
        let expected_bound = num_ports.min(num_identities);
        let bound = result_1
            .iter()
            .filter(|b| matches!(b, BindingResult::Bound { .. }))
            .count();
        let unbound = result_1
            .iter()
            .filter(|b| matches!(b, BindingResult::Unbound { .. }))
            .count();
        let ambiguous = result_1
            .iter()
            .filter(|b| matches!(b, BindingResult::Ambiguous { .. }))
            .count();
        prop_assert_eq!(bound, expected_bound);
        prop_assert_eq!(unbound, num_ports - expected_bound);
        prop_assert_eq!(ambiguous, 0);

        // Per-port semantics: Port i ↔ device-i when an identity exists, else Unbound.
        for binding in &result_1 {
            match binding {
                BindingResult::Bound {
                    device_id,
                    port_name,
                    port_index,
                } => {
                    prop_assert!(
                        *port_index < num_identities,
                        "only ports with a matching identity should bind"
                    );
                    prop_assert_eq!(port_name, &format!("Port {}", port_index));
                    prop_assert_eq!(device_id.as_str(), format!("device-{}", port_index));
                }
                BindingResult::Unbound { port_index, .. } => {
                    prop_assert!(
                        *port_index >= num_identities,
                        "a port with a matching identity must bind, not be left Unbound"
                    );
                }
                BindingResult::Ambiguous { .. } => {
                    prop_assert!(false, "1:1 name matchers must not produce Ambiguous bindings");
                }
            }
        }

        // Determinism: a repeated resolve produces identical bindings. Compares
        // `BindingResult` directly via its derived `PartialEq` (not a `Debug`
        // string surrogate) (Council, #1489).
        prop_assert_eq!(&result_1, &result_2);
    }
}
