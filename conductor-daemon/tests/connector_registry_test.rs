// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-031 Phase 1 § 3.4 — `ConnectorRegistry` runtime construction + lookup.
//!
//! Runtime side of ADR-031 P1 (#1141): the registry indexes everything by
//! alias and supports lookup + port-binding lifecycle.
//!
//! ADR-035 Slice 6 (#1743): the registry NO LONGER lowers `[[bindings]]`
//! itself — lowering moved into `conductor_core::config::loader`
//! (`normalize_to_endpoints`). `from_config` now takes the already-unified
//! `&[EndpointConfig]`. ADR-035 also REMOVED the legacy `DeviceIdentityConfig`
//! /`DevicePortBinding`/`lower_binding`/`lower_connector` types and helpers
//! entirely — the only authored I/O block is `[[endpoints]]`. These tests
//! therefore build `EndpointConfig` fixtures directly (an input `Matcher`
//! endpoint stands in for a former `[[bindings]]` entry; an OSC output endpoint
//! stands in for a former `[[connectors]]` entry) and assert the registry
//! indexes them correctly — i.e. they pin that unified endpoints flow into the
//! runtime registry as the right `LiveConnector`s.
//!
//! Spec reference: `docs/signal-routing/ADR-031-implementation-spec.md` § 3.4,
//! `docs/endpoints-unification/ADR-035-implementation-spec.md` § 5 Slice 6.

use conductor_core::config::types::{
    ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
};
use conductor_core::identity::DeviceMatcher;
use conductor_daemon::connector_registry::ConnectorRegistry;

/// Collect the authored endpoint fixtures into the unified endpoint set that
/// `from_config` consumes after ADR-035. (Bindings + connectors no longer
/// exist as separate authored blocks; everything is an `EndpointConfig`.)
fn endpoints(
    bindings: Vec<EndpointConfig>,
    connectors: Vec<EndpointConfig>,
) -> Vec<EndpointConfig> {
    bindings.into_iter().chain(connectors).collect()
}

/// An input-side `Matcher` endpoint — the ADR-035 successor to a legacy
/// input-only `[[bindings]]` entry. No explicit protocol → inferred Midi.
fn binding(alias: &str) -> EndpointConfig {
    EndpointConfig {
        alias: alias.into(),
        direction: ConnectorDirection::Input,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    }
}

/// An OSC output endpoint — the ADR-035 successor to a legacy OSC
/// `[[connectors]]` entry.
fn osc_connector(alias: &str) -> EndpointConfig {
    EndpointConfig {
        alias: alias.into(),
        direction: ConnectorDirection::Output,
        protocol: Some(ConnectorProtocol::Osc),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::OscEndpoint {
            host: "127.0.0.1".into(),
            port: 9000,
            security: Default::default(),
        },
    }
}

#[test]
fn from_config_empty_inputs_creates_empty_registry() {
    let reg = ConnectorRegistry::from_config(&[]);
    assert!(!reg.contains("anything"));
    assert!(reg.get("anything").is_none());
}

#[test]
fn from_config_lowers_input_binding_to_input_connector() {
    // Per ADR-035 §4.4: an input-side binding lowers to a `direction = Input`
    // connector. The alias is preserved verbatim and the connector starts
    // un-bound once indexed by the registry.
    let reg = ConnectorRegistry::from_config(&endpoints(vec![binding("pads")], vec![]));

    let live = reg.get("pads").expect("binding lowered into registry");
    assert_eq!(live.config.alias, "pads");
    assert_eq!(live.config.direction, ConnectorDirection::Input);
    assert!(!live.connected, "starts disconnected until bind_port");
    assert!(live.bound_port.is_none());
}

#[test]
fn from_config_includes_explicit_connectors() {
    let reg = ConnectorRegistry::from_config(&endpoints(vec![], vec![osc_connector("eos")]));

    let live = reg.get("eos").expect("explicit connector indexed by alias");
    assert_eq!(live.config.direction, ConnectorDirection::Output);
    assert_eq!(live.config.protocol, ConnectorProtocol::Osc);
    assert!(!live.connected);
}

#[test]
fn contains_recognizes_both_bindings_and_connectors() {
    let reg = ConnectorRegistry::from_config(&endpoints(
        vec![binding("pads")],
        vec![osc_connector("eos")],
    ));
    assert!(reg.contains("pads"), "binding-derived connector visible");
    assert!(reg.contains("eos"), "explicit connector visible");
    assert!(!reg.contains("nope"));
}

#[test]
fn resolve_output_returns_none_until_port_bound() {
    let reg = ConnectorRegistry::from_config(&endpoints(vec![], vec![osc_connector("eos")]));
    assert!(reg.resolve_output("eos").is_none());
    assert!(reg.resolve_output("missing").is_none());
}

#[test]
fn bind_port_marks_connector_connected_and_resolves_port_name() {
    let mut reg = ConnectorRegistry::from_config(&endpoints(vec![], vec![osc_connector("eos")]));
    reg.bind_port("eos", "OSC: 127.0.0.1:9000".into(), 0, false);

    let live = reg.get("eos").unwrap();
    assert!(live.connected);
    let bound = live.bound_port.as_ref().expect("port bound");
    assert_eq!(bound.port_name, "OSC: 127.0.0.1:9000");
    assert_eq!(bound.port_index, 0);
    assert!(!bound.auto_paired);
    assert_eq!(reg.resolve_output("eos"), Some("OSC: 127.0.0.1:9000"));
}

#[test]
fn bind_port_on_unknown_alias_is_silent_noop() {
    // Per spec: bind_port accepts &mut self but tolerates unknown
    // aliases — the caller (config-reload loop) may race a removed
    // connector. No panic, no entry created.
    let mut reg = ConnectorRegistry::from_config(&[]);
    reg.bind_port("ghost", "x".into(), 0, false);
    assert!(!reg.contains("ghost"));
}

#[test]
fn disconnect_clears_bound_port_and_marks_disconnected() {
    // ADR-031 #1141 — `resolve_output` is documented as "returns None
    // when not connected"; disconnect must clear BOTH `bound_port` and
    // `connected` so that contract holds. This is also the lifecycle
    // hook the daemon will use on hot-plug + config-reload teardown
    // (spec § 3.4.1 / § 3.4.2 — the `MidiOutputManager::disconnect`
    // teardown vocabulary).
    let mut reg = ConnectorRegistry::from_config(&endpoints(vec![], vec![osc_connector("eos")]));
    reg.bind_port("eos", "OSC: 127.0.0.1:9000".into(), 0, false);
    assert!(reg.get("eos").unwrap().connected);
    assert!(reg.get("eos").unwrap().bound_port.is_some());

    reg.disconnect("eos");

    let live = reg.get("eos").unwrap();
    assert!(!live.connected, "disconnect must clear connected");
    assert!(
        live.bound_port.is_none(),
        "disconnect must clear bound_port"
    );
    assert!(
        reg.resolve_output("eos").is_none(),
        "resolve_output must return None for a disconnected connector"
    );
    // The connector itself remains in the registry (its config is
    // still valid; only its physical-port state is gone).
    assert!(reg.contains("eos"));
}

#[test]
fn disconnect_on_unknown_alias_is_silent_noop() {
    let mut reg = ConnectorRegistry::from_config(&[]);
    reg.disconnect("ghost");
    assert!(!reg.contains("ghost"));
}

#[test]
fn record_activity_increments_total_messages_and_sets_last_activity() {
    let mut reg = ConnectorRegistry::from_config(&endpoints(vec![], vec![osc_connector("eos")]));
    let before = reg.get("eos").unwrap().metrics.total_messages;
    assert_eq!(before, 0);
    assert!(reg.get("eos").unwrap().metrics.last_activity.is_none());

    reg.record_activity("eos");
    reg.record_activity("eos");

    let after = reg.get("eos").unwrap();
    assert_eq!(after.metrics.total_messages, 2);
    assert!(after.metrics.last_activity.is_some());
}

#[test]
fn record_activity_on_unknown_alias_is_silent_noop() {
    let mut reg = ConnectorRegistry::from_config(&[]);
    reg.record_activity("ghost");
    assert!(!reg.contains("ghost"));
}

#[test]
fn from_config_carries_input_matcher_endpoint() {
    // ADR-035: an input `Matcher` endpoint's port matchers must be carried
    // verbatim into the registry's `LiveConnector` config. (Pre-ADR-035 this
    // exercised the `[bindings.input].matchers` lowering precedence; with the
    // unified endpoint shape the matchers live directly on the endpoint.)
    let mut input_endpoint = binding("pads");
    if let EndpointKind::Matcher { matchers, .. } = &mut input_endpoint.kind {
        *matchers = vec![DeviceMatcher::NameContains {
            value: "Mikro".into(),
        }];
    }
    let reg = ConnectorRegistry::from_config(&endpoints(vec![input_endpoint], vec![]));
    let live = reg.get("pads").expect("input endpoint indexed");
    match &live.config.endpoint {
        EndpointKind::Matcher { matchers, .. } => {
            assert_eq!(matchers.len(), 1, "endpoint must carry its matchers");
            match &matchers[0] {
                DeviceMatcher::NameContains { value } => assert_eq!(value, "Mikro"),
                other => panic!("expected NameContains, got {:?}", other),
            }
        }
        other => panic!("expected Matcher endpoint, got {:?}", other),
    }
}

#[test]
fn from_config_carries_matchers_for_legacy_style_endpoint() {
    // An endpoint built via the bare `binding()` helper (matchers set on the
    // top-level `matchers` field of the Matcher kind) must still surface those
    // matchers in the registry.
    let mut legacy_binding = binding("legacy-pads");
    if let EndpointKind::Matcher { matchers, .. } = &mut legacy_binding.kind {
        *matchers = vec![DeviceMatcher::NameContains {
            value: "Vintage".into(),
        }];
    }
    let reg = ConnectorRegistry::from_config(&endpoints(vec![legacy_binding], vec![]));
    let live = reg.get("legacy-pads").unwrap();
    match &live.config.endpoint {
        EndpointKind::Matcher { matchers, .. } => {
            assert_eq!(matchers.len(), 1);
            match &matchers[0] {
                DeviceMatcher::NameContains { value } => assert_eq!(value, "Vintage"),
                other => panic!("expected NameContains, got {:?}", other),
            }
        }
        other => panic!("expected Matcher endpoint, got {:?}", other),
    }
}

#[test]
fn iter_returns_all_entries_in_arbitrary_order() {
    // ADR-031 P1B / Copilot review on PR #1156: `iter()` walks the
    // underlying HashMap, so order is intentionally unspecified.
    // Callers that need stable ordering (e.g. the
    // `conductor_get_resolved_routing_graph` MCP handler) must sort the result —
    // this test pins the contract that all entries are present even
    // though their order is not.
    let bindings = vec![binding("zeta"), binding("alpha"), binding("mu")];
    let connectors = vec![osc_connector("nu"), osc_connector("beta")];
    let reg = ConnectorRegistry::from_config(&endpoints(bindings, connectors));

    let mut aliases: Vec<&String> = reg.iter().map(|(a, _)| a).collect();
    aliases.sort();
    assert_eq!(
        aliases,
        vec![
            &"alpha".to_string(),
            &"beta".to_string(),
            &"mu".to_string(),
            &"nu".to_string(),
            &"zeta".to_string(),
        ],
        "iter() must yield every entry; ordering is the caller's problem"
    );
}

#[test]
fn from_config_preserves_all_protocol_variants_for_endpoints() {
    // Every explicit endpoint `ConnectorProtocol` must be carried into the
    // registry verbatim (via `EndpointConfig::effective_protocol`). Covering
    // only Hid + the default left Osc/ArtNet regressions (e.g. an explicit Osc
    // endpoint silently mapping to Midi) undetected by this integration
    // entrypoint — clawpatch #1559.
    let cases = [
        ("midi-alias", ConnectorProtocol::Midi),
        ("hid-alias", ConnectorProtocol::Hid),
        ("osc-alias", ConnectorProtocol::Osc),
        ("artnet-alias", ConnectorProtocol::ArtNet),
    ];

    let bindings: Vec<EndpointConfig> = cases
        .iter()
        .map(|(alias, protocol)| {
            let mut b = binding(alias);
            b.protocol = Some(*protocol);
            b
        })
        // Plus an endpoint with no explicit protocol — a Matcher kind must
        // default to Midi.
        .chain(std::iter::once(binding("default-alias")))
        .collect();

    let reg = ConnectorRegistry::from_config(&endpoints(bindings, vec![]));

    for (alias, expected) in cases {
        assert_eq!(
            reg.get(alias).unwrap().config.protocol,
            expected,
            "explicit endpoint protocol must be preserved as {expected:?}"
        );
    }
    assert_eq!(
        reg.get("default-alias").unwrap().config.protocol,
        ConnectorProtocol::Midi,
        "Matcher endpoint without explicit protocol defaults to Midi"
    );
}
