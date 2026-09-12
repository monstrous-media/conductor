// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ── ADR-039-A: OSC route validation (D8 + filter) ──

// ── ADR-039-A: OscToArtNet route validation ──

#[test]
fn osc_to_artnet_route_with_valid_template_passes() {
    let cfg = config_with_osc_route(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        osc_to_artnet_route("osc-in", "dmx-out", "/dmx/{dmx}"),
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "valid OscToArtNet route must pass: {:?}",
        report.errors
    );
}

#[test]
fn osc_to_artnet_template_without_placeholder_rejected() {
    let cfg = config_with_osc_route(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        osc_to_artnet_route("osc-in", "dmx-out", "/dmx/static"),
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("{dmx}")),
        "missing placeholder must be a load error: {:?}",
        report.errors
    );
}

#[test]
fn osc_to_artnet_template_without_leading_slash_rejected() {
    let cfg = config_with_osc_route(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        osc_to_artnet_route("osc-in", "dmx-out", "dmx/{dmx}"),
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("OscToArtNet")),
        "missing leading slash must be a load error: {:?}",
        report.errors
    );
}

#[test]
fn osc_to_artnet_template_with_two_placeholders_rejected() {
    let cfg = config_with_osc_route(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        osc_to_artnet_route("osc-in", "dmx-out", "/{dmx}/{dmx}"),
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("{dmx}")),
        "duplicate placeholder must be a load error: {:?}",
        report.errors
    );
}

#[test]
fn osc_to_artnet_route_without_transform_requires_one() {
    // (Osc, ArtNet) is now Required("OscToArtNet") in the matrix — a
    // transform-less route must be rejected, not silently passed.
    let mut route = osc_to_artnet_route("osc-in", "dmx-out", "/dmx/{dmx}");
    route.transform = None;
    let cfg = config_with_osc_route(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        route,
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("OscToArtNet")),
        "missing required transform must name OscToArtNet: {:?}",
        report.errors
    );
}

// ── ADR-039-A: OscForward action validation ──

#[test]
fn osc_forward_to_osc_output_passes() {
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in"), ep_osc_output("eos-out")],
        ActionConfig::OscForward {
            target: "eos-out".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "valid OscForward must pass: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_with_transform_rejected_in_v1() {
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in"), ep_osc_output("eos-out")],
        ActionConfig::OscForward {
            target: "eos-out".to_string(),
            transform: Some(SignalTransform::OscToMidi {
                address_to_cc: Some("/f/{cc}".to_string()),
                address_to_note: None,
                channel: None,
            }),
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("OscForward transform is not supported")),
        "a transform must be rejected in V1: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_to_non_osc_target_rejected() {
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
        ActionConfig::OscForward {
            target: "dmx-out".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("requires an OSC output endpoint")),
        "an Art-Net target must be rejected: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_to_unknown_target_rejected() {
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in")],
        ActionConfig::OscForward {
            target: "nope".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("does not match any [[endpoints]] alias")),
        "an unknown target alias must be rejected: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_to_input_only_osc_target_rejected() {
    // An OSC endpoint that is Input-only can't be sent to — the daemon's
    // runtime map would never contain it, so load must reject.
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in")],
        ActionConfig::OscForward {
            target: "osc-in".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("Input-only OSC endpoint")),
        "an Input-only OSC target must be rejected: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_to_disabled_osc_output_rejected() {
    // A disabled OSC output is excluded from the daemon's runtime map, so
    // load must reject it for clear UX rather than a silent no-op.
    let mut disabled = ep_osc_output("eos-out");
    disabled.enabled = false;
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in"), disabled],
        ActionConfig::OscForward {
            target: "eos-out".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("disabled endpoint")),
        "a disabled OSC output target must be rejected: {:?}",
        report.errors
    );
}

#[test]
fn osc_forward_to_bidirectional_osc_output_passes() {
    // Bidirectional OSC is a valid send target (mirrors the runtime map).
    let mut bidir = ep_osc_output("eos-io");
    bidir.direction = ConnectorDirection::Bidirectional;
    let cfg = config_with_osc_forward(
        vec![ep_osc_input("osc-in"), bidir],
        ActionConfig::OscForward {
            target: "eos-io".to_string(),
            transform: None,
        },
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "a Bidirectional OSC output target must pass: {:?}",
        report.errors
    );
}

#[test]
fn osc_route_to_bidirectional_midi_rejected() {
    // D8: a Bidirectional MIDI target is both output and input → self-loop.
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_midi(
                "synth",
                ConnectorDirection::Bidirectional,
                vec![DeviceMatcher::NameContains {
                    value: "Synth".into(),
                }],
            ),
        ],
        osc_to_midi_route("console", "synth", None),
    );
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid(),
        "OSC→bidirectional-MIDI must be rejected"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("feedback loop")),
        "error should name the feedback loop; got {:?}",
        report.errors
    );
}

#[test]
fn osc_route_to_self_ingested_midi_rejected() {
    // D8: distinct Output + Input MIDI endpoints with identical matchers =
    // same device wired in and out.
    let matchers = vec![DeviceMatcher::NameContains {
        value: "Mikro".into(),
    }];
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_midi("mikro_out", ConnectorDirection::Output, matchers.clone()),
            ep_midi("mikro_in", ConnectorDirection::Input, matchers),
        ],
        osc_to_midi_route("console", "mikro_out", None),
    );
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid(),
        "OSC→self-ingested-MIDI must be rejected"
    );
}

#[test]
fn osc_route_to_distinct_midi_output_ok() {
    // No MIDI input shares the output's device → no loop → valid.
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_midi(
                "fx_out",
                ConnectorDirection::Output,
                vec![DeviceMatcher::NameContains { value: "FX".into() }],
            ),
            ep_midi(
                "pad_in",
                ConnectorDirection::Input,
                vec![DeviceMatcher::NameContains {
                    value: "Pad".into(),
                }],
            ),
        ],
        osc_to_midi_route("console", "fx_out", None),
    );
    let report = validate_config(&cfg);
    assert!(
        report.is_valid(),
        "OSC→distinct-MIDI-output should validate; got {:?}",
        report.errors
    );
}

#[test]
fn osc_route_with_filter_rejected() {
    // OSC routes are currently catch-all only.
    let filter = SignalFilter {
        message_types: vec![],
        channels: vec![5],
        cc_range: None,
        note_range: None,
        osc_address_prefix: None,
    };
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_midi(
                "fx_out",
                ConnectorDirection::Output,
                vec![DeviceMatcher::NameContains { value: "FX".into() }],
            ),
        ],
        osc_to_midi_route("console", "fx_out", Some(filter)),
    );
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid(),
        "OSC-source route with a filter must be rejected"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("catch-all")),
        "error should mention catch-all; got {:?}",
        report.errors
    );
}

#[test]
fn osc_route_to_self_ingested_virtual_port_rejected() {
    // D8: a MidiVirtualPort output has no DeviceMatchers, so
    // the matcher-signature path misses it — but a Conductor-created virtual
    // port that an input endpoint also names is the classic loopback.
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_virtual("bus_out", ConnectorDirection::Output, "Conductor Bus"),
            ep_midi(
                "bus_in",
                ConnectorDirection::Input,
                vec![DeviceMatcher::ExactName {
                    value: "Conductor Bus".into(),
                }],
            ),
        ],
        osc_to_midi_route("console", "bus_out", None),
    );
    let report = validate_config(&cfg);
    assert!(
        !report.is_valid(),
        "OSC→virtual-port also ingested as input must be rejected"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("feedback loop")),
        "error should name the feedback loop; got {:?}",
        report.errors
    );
}

#[test]
fn osc_route_to_virtual_port_input_twin_rejected() {
    // The input twin is itself a MidiVirtualPort of the same name.
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_virtual("bus_out", ConnectorDirection::Output, "Bus"),
            ep_virtual("bus_in", ConnectorDirection::Input, "Bus"),
        ],
        osc_to_midi_route("console", "bus_out", None),
    );
    assert!(!validate_config(&cfg).is_valid());
}

#[test]
fn osc_route_to_unmonitored_virtual_port_ok() {
    // No input names this virtual port → no loop → valid.
    let cfg = config_with_osc_route(
        vec![
            ep_osc_input("console"),
            ep_virtual("bus_out", ConnectorDirection::Output, "Conductor Bus"),
            ep_midi(
                "pad_in",
                ConnectorDirection::Input,
                vec![DeviceMatcher::NameContains {
                    value: "Pad".into(),
                }],
            ),
        ],
        osc_to_midi_route("console", "bus_out", None),
    );
    let report = validate_config(&cfg);
    assert!(
        report.is_valid(),
        "OSC→unmonitored virtual port should validate; got {:?}",
        report.errors
    );
}
