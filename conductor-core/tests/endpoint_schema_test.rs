// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-035 Slice 1 — `[[endpoints]]` schema: hand-written strict `Deserialize`
// for `EndpointConfig` (wrapper) over the `EndpointKind` payload enum.
//
// Acceptance (issue #1738): parse each `type`; defaults; round-trip; a field
// typo (`prot =`) errors instead of being silently dropped; a stray field on
// `MidiVirtualPort` errors; `direction` is required.

use conductor_core::config::types::{
    ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
};

fn parse(s: &str) -> Result<EndpointConfig, toml::de::Error> {
    toml::from_str::<EndpointConfig>(s)
}

// ── Parse each type ────────────────────────────────────────────────

#[test]
fn parse_matcher_type() {
    let ep = parse(
        r#"
        alias = "pads"
        direction = "Input"
        type = "Matcher"
        matchers = [{ type = "NameContains", value = "Mikro" }]
    "#,
    )
    .expect("Matcher parses");
    assert_eq!(ep.alias, "pads");
    assert_eq!(ep.direction, ConnectorDirection::Input);
    match ep.kind {
        EndpointKind::Matcher {
            ref matchers,
            ref input_matchers,
            ref output_matchers,
            no_probe,
        } => {
            assert_eq!(matchers.len(), 1);
            assert!(input_matchers.is_empty());
            assert!(output_matchers.is_empty());
            assert!(!no_probe, "no_probe defaults to false");
        }
        other => panic!("expected Matcher, got {other:?}"),
    }
}

#[test]
fn parse_midi_virtual_port_type() {
    let ep = parse(
        r#"
        alias = "daw"
        direction = "Output"
        type = "MidiVirtualPort"
        port_name = "Conductor: DAW"
    "#,
    )
    .expect("MidiVirtualPort parses");
    assert_eq!(ep.direction, ConnectorDirection::Output);
    match ep.kind {
        EndpointKind::MidiVirtualPort { port_name } => assert_eq!(port_name, "Conductor: DAW"),
        other => panic!("expected MidiVirtualPort, got {other:?}"),
    }
}

#[test]
fn parse_osc_endpoint_type() {
    let ep = parse(
        r#"
        alias = "lights"
        direction = "Output"
        type = "OscEndpoint"
        host = "10.0.0.5"
        port = 9000
    "#,
    )
    .expect("OscEndpoint parses");
    match ep.kind {
        EndpointKind::OscEndpoint { host, port, .. } => {
            assert_eq!(host, "10.0.0.5");
            assert_eq!(port, 9000);
        }
        other => panic!("expected OscEndpoint, got {other:?}"),
    }
}

#[test]
fn parse_artnet_endpoint_type_uses_defaults() {
    let ep = parse(
        r#"
        alias = "dmx"
        direction = "Output"
        type = "ArtNetEndpoint"
        universe = 3
    "#,
    )
    .expect("ArtNetEndpoint parses");
    match ep.kind {
        EndpointKind::ArtNetEndpoint {
            universe,
            host,
            port,
            ..
        } => {
            assert_eq!(universe, 3);
            assert_eq!(host, "255.255.255.255", "default Art-Net host");
            assert_eq!(port, 6454, "default Art-Net port");
        }
        other => panic!("expected ArtNetEndpoint, got {other:?}"),
    }
}

// ── Defaults on the common (wrapper) fields ────────────────────────

#[test]
fn common_field_defaults() {
    let ep = parse(
        r#"
        alias = "pads"
        direction = "Bidirectional"
        type = "Matcher"
        matchers = [{ type = "NameContains", value = "Mikro" }]
    "#,
    )
    .expect("parses");
    assert!(ep.enabled, "enabled defaults to true");
    assert!(ep.protocol.is_none(), "protocol inferred (None) by default");
    assert!(ep.description.is_none());
    assert!(
        ep.channels.is_empty(),
        "channels default empty (all channels)"
    );
}

// ── Strictness: the whole reason the impl is hand-written ──────────

#[test]
fn field_typo_is_rejected_not_dropped() {
    // `prot` instead of `protocol` must ERROR, not be silently swallowed.
    let err = parse(
        r#"
        alias = "pads"
        direction = "Input"
        prot = "Midi"
        type = "Matcher"
        matchers = [{ type = "NameContains", value = "Mikro" }]
    "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("unknown field `prot`"),
        "expected unknown-field error, got: {err}"
    );
}

#[test]
fn stray_field_on_midi_virtual_port_is_rejected() {
    // `host` is meaningless for MidiVirtualPort → error.
    let err = parse(
        r#"
        alias = "daw"
        direction = "Output"
        type = "MidiVirtualPort"
        port_name = "Conductor: DAW"
        host = "127.0.0.1"
    "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("unknown field `host`"),
        "expected unknown-field error for stray host, got: {err}"
    );
}

#[test]
fn missing_direction_is_rejected() {
    let err = parse(
        r#"
        alias = "pads"
        type = "Matcher"
        matchers = [{ type = "NameContains", value = "Mikro" }]
    "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("missing field `direction`"),
        "direction is required for [[endpoints]], got: {err}"
    );
}

#[test]
fn unknown_type_is_rejected() {
    let err = parse(
        r#"
        alias = "pads"
        direction = "Input"
        type = "Bogus"
    "#,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("unknown endpoint `type`"),
        "expected unknown-type error, got: {err}"
    );
}

// ── Round-trip (Serialize → Deserialize) ───────────────────────────

#[test]
fn round_trips_through_serialize() {
    let src = r#"
        alias = "lights"
        direction = "Output"
        protocol = "Osc"
        type = "OscEndpoint"
        host = "10.0.0.5"
        port = 9000
    "#;
    let ep = parse(src).expect("parses");
    // Serialize (derived, via #[serde(flatten)]) then re-parse through the
    // strict manual impl — the two halves must agree.
    let serialized = toml::to_string(&ep).expect("serializes");
    let reparsed = parse(&serialized).expect("re-parses serialized form");
    assert_eq!(reparsed.alias, "lights");
    assert_eq!(reparsed.direction, ConnectorDirection::Output);
    match reparsed.kind {
        EndpointKind::OscEndpoint { host, port, .. } => {
            assert_eq!(host, "10.0.0.5");
            assert_eq!(port, 9000);
        }
        other => panic!("expected OscEndpoint after round-trip, got {other:?}"),
    }
}

#[test]
fn explicit_non_defaults_survive_round_trip() {
    // Council review (Slice 1): pin that non-default common fields are both
    // parsed AND survive serialize → reparse (the derived flatten Serialize
    // and the manual Deserialize must agree on every field, not just the tag).
    let src = r#"
        alias = "ctrl"
        direction = "Input"
        protocol = "Midi"
        enabled = false
        channels = [1, 2, 3]
        type = "Matcher"
        matchers = [{ type = "NameContains", value = "Mikro" }]
    "#;
    let ep = parse(src).expect("parses");
    assert_eq!(ep.protocol, Some(ConnectorProtocol::Midi));
    assert!(!ep.enabled, "explicit enabled = false is honoured");
    assert_eq!(ep.channels, vec![1, 2, 3]);

    let reparsed = parse(&toml::to_string(&ep).expect("serializes")).expect("round-trips");
    assert_eq!(reparsed.protocol, Some(ConnectorProtocol::Midi));
    assert!(!reparsed.enabled, "enabled survives round-trip");
    assert_eq!(
        reparsed.channels,
        vec![1, 2, 3],
        "channels survive round-trip"
    );
}

#[test]
fn explicit_empty_asymmetric_falls_back_to_symmetric() {
    // Documented behavior (ADR-035 §4.1): an explicit empty asymmetric list is
    // treated the same as absent — `effective_matchers` falls back to the
    // symmetric `matchers`. (Empty ≠ "block this direction"; an output-only
    // endpoint is expressed by populating the *other* list, not by emptying
    // this one.)
    let ep = parse(
        r#"
        alias = "fb"
        direction = "Bidirectional"
        type = "Matcher"
        matchers = [{ type = "NameContains", value = "Both" }]
        input_matchers = []
    "#,
    )
    .expect("parses");
    assert_eq!(
        ep.kind.effective_matchers(ConnectorDirection::Input).len(),
        1,
        "explicit empty input_matchers falls back to symmetric `matchers`"
    );
}

// ── effective_matchers + non-empty invariant ──────────────────────

#[test]
fn asymmetric_matchers_and_effective_selection() {
    let ep = parse(
        r#"
        alias = "split"
        direction = "Bidirectional"
        type = "Matcher"
        input_matchers = [{ type = "NameContains", value = "InPort" }]
        output_matchers = [{ type = "NameContains", value = "OutPort" }]
    "#,
    )
    .expect("asymmetric Matcher parses (empty `matchers` allowed here)");

    // Direction-aware selection picks the right asymmetric list.
    assert_eq!(
        ep.kind.effective_matchers(ConnectorDirection::Input).len(),
        1
    );
    assert_eq!(
        ep.kind.effective_matchers(ConnectorDirection::Output).len(),
        1
    );
    // The non-empty invariant holds (input/output populated even though
    // symmetric `matchers` is empty).
    assert!(ep.kind.has_any_matcher());
}

#[test]
fn effective_matchers_falls_back_to_symmetric() {
    let ep = parse(
        r#"
        alias = "sym"
        direction = "Bidirectional"
        type = "Matcher"
        matchers = [{ type = "NameContains", value = "Both" }]
    "#,
    )
    .expect("parses");
    // No asymmetric lists → every direction falls back to `matchers`.
    assert_eq!(
        ep.kind.effective_matchers(ConnectorDirection::Input).len(),
        1
    );
    assert_eq!(
        ep.kind.effective_matchers(ConnectorDirection::Output).len(),
        1
    );
    assert_eq!(
        ep.kind
            .effective_matchers(ConnectorDirection::Bidirectional)
            .len(),
        1
    );
}

#[test]
fn empty_matcher_fails_invariant() {
    // All three matcher lists empty → invariant violated (Slice 5 turns this
    // into a load error; here we assert the predicate the validator will use).
    let ep = parse(
        r#"
        alias = "empty"
        direction = "Input"
        type = "Matcher"
    "#,
    )
    .expect("parses structurally (validation is a separate slice)");
    assert!(
        !ep.kind.has_any_matcher(),
        "a Matcher with no matchers at all must fail the non-empty invariant"
    );
}

// ── Config-level: [[endpoints]] array parses ───────────────────────

#[test]
fn config_endpoints_array_parses() {
    let cfg: conductor_core::Config = toml::from_str(
        r#"
        [[modes]]
        name = "Default"

        [[endpoints]]
        alias = "pads"
        direction = "Input"
        type = "Matcher"
        matchers = [{ type = "NameContains", value = "Mikro" }]

        [[endpoints]]
        alias = "dmx"
        direction = "Output"
        type = "ArtNetEndpoint"
        universe = 1
    "#,
    )
    .expect("config with [[endpoints]] parses");
    assert_eq!(cfg.endpoints.len(), 2);
    assert_eq!(cfg.endpoints[0].alias, "pads");
    assert_eq!(cfg.endpoints[1].alias, "dmx");
}

// ── #2064: JSON `null` for optional fields → `None` (GUI save_config path) ──
//
// The GUI's `save_config` round-trips config through serde_json. TOML has no
// null, so the hand-written deserializer must tolerate a JSON `null` for an
// optional field (treat it as absent / `None`) rather than erroring with
// "invalid type: null". The strict TOML config-load path is unchanged.

fn parse_json(v: serde_json::Value) -> Result<EndpointConfig, serde_json::Error> {
    serde_json::from_value::<EndpointConfig>(v)
}

#[test]
fn json_null_optionals_deserialize_as_none() {
    let ep = parse_json(serde_json::json!({
        "alias": "daw",
        "direction": "Output",
        "type": "MidiVirtualPort",
        "port_name": "Conductor: DAW",
        "description": null,
        "protocol": null,
        "channels": null,
    }))
    .expect("#2064: JSON null optionals must deserialize, not error");
    assert_eq!(ep.alias, "daw");
    assert!(ep.description.is_none(), "null description → None");
    assert!(ep.protocol.is_none(), "null protocol → None");
    assert!(ep.channels.is_empty(), "null channels → default (empty)");
    match ep.kind {
        EndpointKind::MidiVirtualPort { port_name } => assert_eq!(port_name, "Conductor: DAW"),
        other => panic!("expected MidiVirtualPort, got {other:?}"),
    }
}

#[test]
fn json_present_optionals_still_deserialize() {
    // A non-null value on the JSON path still parses (no regression).
    let ep = parse_json(serde_json::json!({
        "alias": "pads",
        "direction": "Input",
        "type": "Matcher",
        "description": "my pads",
        "protocol": "Midi",
        "matchers": [{ "type": "NameContains", "value": "Mikro" }],
    }))
    .expect("present optionals parse on the JSON path");
    assert_eq!(ep.description.as_deref(), Some("my pads"));
    assert_eq!(ep.protocol, Some(ConnectorProtocol::Midi));
}

#[test]
fn json_unknown_field_still_errors() {
    // Strictness is preserved through the JSON path: a typo'd / stray key is
    // still rejected (not silently dropped), same as the TOML path.
    let err = parse_json(serde_json::json!({
        "alias": "daw",
        "direction": "Output",
        "type": "MidiVirtualPort",
        "port_name": "x",
        "prot": "Midi"
    }))
    .expect_err("an unknown field must error even via JSON");
    assert!(
        err.to_string().contains("unknown field `prot`"),
        "expected unknown-field error, got: {err}"
    );
}

#[test]
fn json_null_unknown_field_still_errors() {
    // Council #2179: a `null` value must NOT let an unknown/typo'd key slip
    // through. `prot: null` is still an unknown field, exactly like `prot: "x"`.
    let err = parse_json(serde_json::json!({
        "alias": "daw",
        "direction": "Output",
        "type": "MidiVirtualPort",
        "port_name": "x",
        "prot": null
    }))
    .expect_err("an unknown field with a null value must still error");
    assert!(
        err.to_string().contains("unknown field `prot`"),
        "null must not bypass the unknown-key check, got: {err}"
    );
}

#[test]
fn json_null_required_field_still_errors() {
    // A `null` for a REQUIRED field is absence → the missing-field error, not a
    // silent default. (`alias` is required.)
    let err = parse_json(serde_json::json!({
        "alias": null,
        "direction": "Output",
        "type": "MidiVirtualPort",
        "port_name": "x"
    }))
    .expect_err("null for a required field must error");
    assert!(
        err.to_string().contains("missing field `alias`"),
        "expected missing-field error, got: {err}"
    );
}
