// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-035 Slice 6 — `EndpointRegistry::from_config(&[EndpointConfig])` single-source.
//
// The runtime registry no longer lowers `[[bindings]]` itself. Lowering now
// happens once, in `conductor_core::config::loader::normalize_to_endpoints`,
// and the registry consumes the already-unified endpoint set. ADR-035 also
// removed the legacy `[[devices]]`/`[[bindings]]`/`[[connectors]]` authored
// blocks entirely — the sole authored I/O block is `[[endpoints]]`, so these
// fixtures author endpoints directly. These tests prove:
//   * byte-for-byte parity of the resulting `LiveConnector` config vs. the
//     canonical Input-Matcher shape, on equivalent configs (a regression-proof
//     acceptance criterion),
//   * authored `[[endpoints]]` of every kind land in the registry under their
//     alias,
//   * protocol inference for authored endpoints whose `protocol` is omitted.

use conductor_core::Config;
use conductor_core::config::loader::normalize_to_endpoints;
use conductor_core::config::types::{ConnectorDirection, ConnectorProtocol, EndpointKind};
use conductor_daemon::connector_registry::ConnectorRegistry;

/// Parse TOML → Config, then build the registry the way the daemon now does:
/// normalize the whole config into the unified endpoint set, then hand that
/// set to `from_config`.
fn registry_from_toml(toml_str: &str) -> ConnectorRegistry {
    let config: Config = toml::from_str(toml_str).expect("config parses");
    let (endpoints, _findings) =
        normalize_to_endpoints(&config).expect("config normalizes without collision");
    ConnectorRegistry::from_config(&endpoints)
}

#[test]
fn binding_lowers_into_registry_as_input_connector() {
    // An input `Matcher` endpoint must appear in the registry as an Input
    // connector whose `matchers` carry the endpoint's matchers. (Pre-ADR-035
    // this was authored as `[[devices]]`; the unified block is `[[endpoints]]`.)
    let reg = registry_from_toml(
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
    let lc = reg.get("pads").expect("endpoint present in registry");
    assert_eq!(lc.config.alias, "pads");
    assert_eq!(lc.config.direction, ConnectorDirection::Input);
    match &lc.config.endpoint {
        EndpointKind::Matcher {
            matchers,
            input_matchers,
            output_matchers,
            ..
        } => {
            assert_eq!(matchers.len(), 1, "binding matchers preserved");
            assert!(input_matchers.is_empty());
            assert!(output_matchers.is_empty());
        }
        other => panic!("expected Matcher kind, got {other:?}"),
    }
}

#[test]
fn explicit_connector_added_directly() {
    // An output `Matcher` endpoint is carried through verbatim. (Pre-ADR-035
    // authored as `[[connectors]]` with a nested `endpoint = { ... }`; the
    // unified block flattens that to top-level `type`/`matchers`.)
    let reg = registry_from_toml(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "synth_out"
direction = "Output"
protocol = "Midi"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Synth" }]
"#,
    );
    let lc = reg.get("synth_out").expect("connector present");
    assert_eq!(lc.config.direction, ConnectorDirection::Output);
    assert_eq!(lc.config.protocol, ConnectorProtocol::Midi);
}

#[test]
fn authored_endpoint_enters_registry() {
    // The unification (ADR-035): an authored `[[endpoints]]` block now lands in
    // the runtime registry — the old `from_config(devices, connectors)` never
    // saw authored endpoints at all.
    let reg = registry_from_toml(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "lights"
direction = "Output"
type = "ArtNetEndpoint"
universe = 3
host = "10.0.0.9"
port = 6454
"#,
    );
    let lc = reg
        .get("lights")
        .expect("authored endpoint present in registry");
    assert_eq!(lc.config.direction, ConnectorDirection::Output);
    assert_eq!(lc.config.protocol, ConnectorProtocol::ArtNet);
    assert!(matches!(
        lc.config.endpoint,
        EndpointKind::ArtNetEndpoint { universe: 3, .. }
    ));
}

#[test]
fn binding_connector_parity_serialized_byte_for_byte() {
    // The acceptance criterion: byte-for-byte parity of the produced connector
    // config vs. the canonical shape an input `Matcher` endpoint must take in
    // the registry:
    //   ConnectorConfig {
    //       alias, direction: Input, protocol: Midi,
    //       endpoint: Matcher { matchers, input/output: [] },
    //       description, enabled, channels,
    //   }
    // We replicate that shape as the EXPECTED value here — constructing every
    // field INDEPENDENTLY of `actual` (the matcher is re-typed from the TOML
    // fixture, NOT cloned from the parsed result) so this is a genuine
    // regression guard: if `normalize_to_endpoints` ever dropped or mutated the
    // matchers, the serialized comparison would diverge. Then compare the
    // canonical TOML serialization (ConnectorConfig has no PartialEq).
    use conductor_core::config::types::ConnectorConfig;
    use conductor_core::identity::DeviceMatcher;

    let reg = registry_from_toml(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "faders"
direction = "Input"
type = "Matcher"
enabled = true
channels = [0, 9]
matchers = [{ type = "NameContains", value = "nanoKONTROL" }]
"#,
    );
    let actual = &reg.get("faders").expect("endpoint present").config;

    let expected = ConnectorConfig {
        alias: "faders".to_string(),
        direction: ConnectorDirection::Input,
        protocol: ConnectorProtocol::Midi,
        endpoint: EndpointKind::Matcher {
            // Re-typed from the fixture above — independent of `actual`.
            matchers: vec![DeviceMatcher::NameContains {
                value: "nanoKONTROL".to_string(),
            }],
            input_matchers: Vec::new(),
            output_matchers: Vec::new(),
            no_probe: false,
        },
        description: None,
        enabled: true,
        channels: vec![0, 9],
    };

    assert_eq!(
        toml::to_string(actual).unwrap(),
        toml::to_string(&expected).unwrap(),
        "registry connector config must match the legacy binding-lowering output byte-for-byte"
    );
}

#[test]
fn binding_protocol_is_preserved_through_lowering() {
    // An endpoint that declares a non-MIDI protocol must keep it through
    // normalization (guards `EndpointConfig::effective_protocol` honouring the
    // explicit `protocol` override over the kind-inferred default).
    let reg = registry_from_toml(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_dev"
direction = "Input"
type = "Matcher"
protocol = "Osc"
matchers = [{ type = "NameContains", value = "TouchOSC" }]
"#,
    );
    let lc = reg.get("osc_dev").expect("endpoint present");
    assert_eq!(lc.config.protocol, ConnectorProtocol::Osc);
}

#[test]
fn multiple_endpoints_coexist_under_distinct_aliases() {
    // Post-ADR-035 there is a single authored block (`[[endpoints]]`). Several
    // endpoints with distinct aliases (mixing direction + kind) must all land
    // in the registry. (Pre-ADR-035 this exercised the now-removed
    // `[[devices]]`/`[[connectors]]` blocks coexisting with `[[endpoints]]`.)
    let reg = registry_from_toml(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "ep_input"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "A" }]

[[endpoints]]
alias = "ep_bidir"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "B" }]

[[endpoints]]
alias = "ep_output"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "C" }]
"#,
    );
    assert!(reg.contains("ep_input"));
    assert!(reg.contains("ep_bidir"));
    assert!(reg.contains("ep_output"));
    assert_eq!(reg.iter().count(), 3, "exactly the three aliases, no dupes");
}

#[test]
fn empty_config_yields_empty_registry() {
    let reg = ConnectorRegistry::from_config(&[]);
    assert_eq!(reg.iter().count(), 0);
    assert!(reg.get("anything").is_none());
}

#[test]
fn binding_channels_and_enabled_round_trip() {
    let reg = registry_from_toml(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "ch_dev"
direction = "Input"
type = "Matcher"
enabled = false
channels = [3, 4, 5]
matchers = [{ type = "NameContains", value = "Pad" }]
"#,
    );
    let lc = reg.get("ch_dev").expect("endpoint present");
    assert!(!lc.config.enabled, "enabled flag preserved");
    assert_eq!(lc.config.channels, vec![3, 4, 5], "channel scope preserved");
}

#[test]
fn authored_osc_endpoint_infers_osc_protocol_when_omitted() {
    // Protocol omitted on an authored OscEndpoint → inferred as Osc (not the
    // bare ConnectorProtocol default of Midi). The legacy path never had to
    // infer because connectors always carried an explicit protocol.
    let reg = registry_from_toml(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_sink"
direction = "Output"
type = "OscEndpoint"
host = "127.0.0.1"
port = 9000
"#,
    );
    let lc = reg.get("osc_sink").expect("authored endpoint present");
    assert_eq!(
        lc.config.protocol,
        ConnectorProtocol::Osc,
        "OscEndpoint with no explicit protocol must infer Osc"
    );
}
