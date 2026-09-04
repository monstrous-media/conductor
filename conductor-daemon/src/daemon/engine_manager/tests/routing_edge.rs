// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ── Stage-9 edge cases ────────────────────────────
//
// The earlier tests pinned happy path + no-route + precedence. These cover
// the failure modes: disabled route, empty config, unknown
// destination alias. They pin the "stage 9 doesn't blow up under
// partial / malformed config" contract — a regression class easy
// to miss without explicit tests.

/// AC: a route with `enabled: false` MUST NOT forward, even if its
/// `from` matches and no trigger matches. `RouteEngine::compile()`
/// filters disabled routes at compile time per ADR-031 spec §3.4 —
/// this pins that filter through to the dispatch path.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_disabled_route_does_not_forward() {
    use conductor_core::config::types::RouteConfig;
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut config = create_route_only_test_config();
    // Disable the route. Per ADR-031 § 3.4, `enabled: false` routes
    // are filtered at `compile()` so `route_destinations()` returns
    // empty for the source alias — stage 9 finds nothing to dispatch.
    config.routes = vec![RouteConfig {
        from: "pads".to_string(),
        to: "absynth".to_string(),
        transform: None,
        filter: None,
        enabled: false, // <-- the test
        description: Some("disabled — must not forward".to_string()),
        modes: Vec::new(),
    }];

    let mut manager = create_simulate_manager(config).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);

    let mut rx = manager.event_broadcast_tx.subscribe();

    let device_event = DeviceEvent::new(
        DeviceId::raw("pads"),
        InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok");

    assert!(
        drain_for_event_type(&mut rx, "route_forwarded").is_none(),
        "route_forwarded MUST NOT fire when the matching route has enabled=false"
    );
}

/// AC: zero routes in config MUST be a no-op at stage 9 — the
/// matcher returns an empty Vec, the for loop iterates zero times,
/// no dispatches, no monitor events. Pins that the fast path for
/// the common case (no routes configured at all) doesn't accidentally
/// emit phantom events.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_empty_routes_config_is_noop() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut config = create_route_only_test_config();
    config.routes = vec![]; // <-- the test

    let mut manager = create_simulate_manager(config).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);

    let mut rx = manager.event_broadcast_tx.subscribe();

    let device_event = DeviceEvent::new(
        DeviceId::raw("pads"),
        InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok");

    assert!(
        drain_for_event_type(&mut rx, "route_forwarded").is_none(),
        "route_forwarded MUST NOT fire when routes config is empty — \
             stage 9 must short-circuit when route_destinations returns []"
    );
}

/// AC: a route to an alias not in the connector registry (e.g. a
/// typo'd or hot-removed destination) MUST NOT panic. The dispatch
/// fires (route_destinations returns the pair regardless of
/// destination existence), but the action executor's port-resolution
/// returns None and the send silently fails — same as `MidiForward`
/// to a bad target. Pins that the engine doesn't crash on partial
/// config.
///
/// Note: this test deliberately omits the "absynth" connector
/// declaration so the route's `to: "absynth"` has nothing to bind
/// to. The route still passes `compile()` because validation runs
/// against the connector list at config-load (different concern,
/// tracked separately) — at runtime the dispatch goes through and
/// fails gracefully at the action executor's port resolution.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_unknown_destination_alias_does_not_panic() {
    use conductor_core::config::types::RouteConfig;
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut config = create_route_only_test_config();
    // Drop the absynth endpoint — the route now points to nothing.
    config.endpoints.retain(|e| e.alias != "absynth");
    // Keep the route declaration pointing at the now-missing alias.
    config.routes = vec![RouteConfig {
        from: "pads".to_string(),
        to: "absynth_ghost".to_string(),
        transform: None,
        filter: None,
        enabled: true,
        description: Some("destination missing from registry".to_string()),
        modes: Vec::new(),
    }];

    let mut manager = create_simulate_manager(config).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);

    let mut rx = manager.event_broadcast_tx.subscribe();

    let device_event = DeviceEvent::new(
        DeviceId::raw("pads"),
        InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    // The contract is "doesn't panic". Whether route_forwarded
    // fires depends on whether `compile()` excluded the route or
    // not — both outcomes are acceptable for this test. The point
    // is: process_device_event must return Ok regardless.
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok even when route destination is unknown");

    // Drain to confirm no panic in the dispatch path (we'd see a
    // panic in test output rather than a clean drain).
    while rx.try_recv().is_ok() {}
}

// ── Metrics + payload enrichment ───────────────────
//
// Route dispatch emitted route_forwarded from the start; this addition
// closes a latent gap: every successful forward
// increments the destination connector's `total_messages` metric
// via `ConnectorRegistry::record_activity()`. The payload also
// grows to carry `bytes_out` + `bound_port` so the GUI
// badge has the data it needs without a second roundtrip.

/// AC: a successful stage-9 dispatch increments the destination
/// connector's `total_messages` counter. Previously
/// `record_activity()` was dead code — the method existed but
/// nothing in production called it, so every `ConnectorMetrics`
/// counter was stuck at zero. Pins the wiring fix.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_dispatch_increments_record_activity() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_route_only_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);

    // Snapshot the metric BEFORE — must be zero on a fresh registry.
    {
        let registry = manager.connector_registry.read().await;
        let connector = registry
            .get("absynth")
            .expect("absynth connector must exist in test config");
        assert_eq!(
            connector.metrics.total_messages, 0,
            "fresh registry must start at total_messages=0"
        );
    }

    // Fire 3 events on the source — stage 9 must record_activity 3 times.
    for _ in 0..3 {
        let device_event = DeviceEvent::new(
            DeviceId::raw("pads"),
            InputEvent::PadPressed {
                pad: 36,
                velocity: 100,
                channel: Some(0),
                time: StdInstant::now(),
            },
        );
        manager
            .process_device_event(device_event)
            .await
            .expect("process_device_event must return Ok");
    }

    // Snapshot AFTER — every stage-9 dispatch increments the counter.
    let registry = manager.connector_registry.read().await;
    let connector = registry
        .get("absynth")
        .expect("absynth connector must exist after dispatch");
    assert_eq!(
        connector.metrics.total_messages, 3,
        "every successful stage-9 dispatch must call record_activity \
             (got {} after 3 events; pre-slice this was always 0)",
        connector.metrics.total_messages
    );
    assert!(
        connector.metrics.last_activity.is_some(),
        "record_activity must populate last_activity"
    );
}

/// AC: the `route_forwarded` payload carries the enriched
/// fields (`bytes_out`, `bound_port`) in addition to the base
/// fields (`from`, `to`, `bytes_in`). Pins the contract the
/// GUI badge consumes.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_route_forwarded_payload_includes_enriched_fields() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_route_only_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);

    let mut rx = manager.event_broadcast_tx.subscribe();

    let device_event = DeviceEvent::new(
        DeviceId::raw("pads"),
        InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok");

    let route_event = drain_for_event_type(&mut rx, "route_forwarded")
        .expect("stage 9 must emit route_forwarded for the dispatch");
    let payload = route_event
        .payload
        .as_ref()
        .expect("route_forwarded must carry a payload");

    // Base fields preserved (backward compatibility).
    assert_eq!(
        payload["from"], "pads",
        "slice 1 field `from` must still be present"
    );
    assert_eq!(
        payload["to"], "absynth",
        "slice 1 field `to` must still be present"
    );
    assert!(
        payload["bytes_in"].as_u64().unwrap_or(0) >= 3,
        "slice 1 field `bytes_in` must still be present"
    );

    // Enrichment additions.
    assert!(
        payload["bytes_out"].is_u64(),
        "slice 3a field `bytes_out` must be present (got: {:?})",
        payload["bytes_out"]
    );
    // No transform on this route — bytes_in and bytes_out should match.
    assert_eq!(
        payload["bytes_out"].as_u64(),
        payload["bytes_in"].as_u64(),
        "with transform=None, bytes_out must equal bytes_in"
    );
    // `bound_port` is Some(String) when the connector has a bound
    // port, None otherwise. In `create_simulate_manager` no ports
    // are bound, so the field must serialize as JSON null but
    // still be PRESENT — the GUI consumer needs to distinguish
    // "field missing" from "explicitly unbound".
    assert!(
        payload.get("bound_port").is_some(),
        "slice 3a field `bound_port` must be present (even when null)"
    );
    assert!(
        payload["bound_port"].is_null(),
        "with no port bound in test fixture, bound_port must serialize \
             as null (got: {:?})",
        payload["bound_port"]
    );
}

// ── Cross-protocol exclusion runtime guard ────────
//
// `RouteEngine::compile()` originally excluded routes with cross-protocol
// transforms (MidiToOsc / OscToMidi / MidiToArtNet / HidToArtNet)
// as not yet executable, and this test pinned the runtime
// consequence: a declared MidiToOsc route MUST NOT fire stage-9
// dispatch even when its source receives MIDI events. Now that
// MidiToOsc has a runtime, this test
// FLIPS — MidiToOsc routes MUST dispatch as OSC. The replacement
// (`test_stage9_midi_to_osc_route_dispatches_as_osc_packet`)
// pins positive delivery via a real UDP receiver. A separate
// negative test still pins the contract for STILL-excluded
// variants (`OscToMidi`, `HidToArtNet`).

/// A route with a `MidiToOsc` transform now
/// dispatches as an OSC packet through the registry's
/// `send_osc` path. End-to-end via a real loopback UDP
/// receiver. Pins the cross-protocol dispatch wiring.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_midi_to_osc_route_dispatches_as_osc_packet() {
    use conductor_core::config::types::{
        ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind, RouteConfig,
        SignalTransform,
    };
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::net::UdpSocket;
    use std::time::Instant as StdInstant;

    // Bind a receiver socket on a free localhost port; the OSC
    // connector points at it.
    let receiver = UdpSocket::bind("127.0.0.1:0").expect("receiver bind");
    receiver
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .expect("set read timeout");
    let dest_port = receiver.local_addr().expect("local_addr").port();

    // Override the helper's "absynth" MIDI endpoint with an OSC
    // endpoint at the receiver address.
    let mut config = create_route_only_test_config();
    config.endpoints.retain(|e| e.alias != "absynth");
    config.endpoints.push(EndpointConfig {
        alias: "lighting".to_string(),
        direction: ConnectorDirection::Output,
        protocol: Some(ConnectorProtocol::Osc),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::OscEndpoint {
            host: "127.0.0.1".to_string(),
            port: dest_port,
            security: Default::default(),
        },
    });
    config.routes = vec![RouteConfig {
        from: "pads".to_string(),
        to: "lighting".to_string(),
        transform: Some(SignalTransform::MidiToOsc {
            cc_to_address: None,
            note_to_address: Some("/synth/note/{note}/vel/{velocity}".to_string()),
            value_to_float: false,
        }),
        filter: None,
        enabled: true,
        description: Some("cross-protocol — MIDI → OSC".to_string()),
        modes: Vec::new(),
    }];

    let mut manager = create_simulate_manager(config).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);

    let mut rx = manager.event_broadcast_tx.subscribe();

    // NoteOn channel 0, note 60 (middle C), velocity 100 — fires
    // the route, MidiToOsc transforms to a packet at
    // `/synth/note/60/vel/100` with arg Int(100).
    let device_event = DeviceEvent::new(
        DeviceId::raw("pads"),
        InputEvent::PadPressed {
            pad: 60,
            velocity: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok");

    // Receiver must have got the OSC packet.
    let mut buf = [0u8; 1024];
    let (n, _) = receiver
        .recv_from(&mut buf)
        .expect("receiver must observe an OSC packet from stage-9");
    let packet = rosc::decoder::decode_udp(&buf[..n])
        .expect("received bytes must decode as OSC")
        .1;
    match packet {
        rosc::OscPacket::Message(msg) => {
            assert_eq!(msg.addr, "/synth/note/60/vel/100");
            assert_eq!(msg.args, vec![rosc::OscType::Int(100)]);
        }
        _ => panic!("expected Message, got Bundle"),
    }

    // `route_forwarded` monitor event must fire, with the new
    // `protocol: "Osc"` payload field.
    let event = drain_for_event_type(&mut rx, "route_forwarded")
        .expect("route_forwarded must fire for cross-protocol dispatch");
    let payload = event.payload.expect("event must carry payload");
    assert_eq!(payload["protocol"], "Osc");
    assert_eq!(payload["to"], "lighting");
    assert!(
        payload["bound_port"].is_null(),
        "OSC connectors have no bound_port — must serialize as null"
    );

    // Metric write must have happened too.
    let registry = manager.connector_registry.read().await;
    let connector = registry
        .get("lighting")
        .expect("lighting connector must still exist in registry");
    assert_eq!(
        connector.metrics.total_messages, 1,
        "successful OSC send must increment total_messages"
    );
}

/// A route with a `MidiToArtNet` transform now
/// dispatches as an Art-Net OpDmx UDP packet via the registry's
/// `send_artnet` path. End-to-end via a real loopback UDP receiver.
/// Populated `cc_to_dmx` now survives
/// canonical TOML serialise via the `u8_string_map` serde helper.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_midi_to_artnet_route_dispatches_as_opdmx_packet() {
    use conductor_core::config::types::{
        ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind, RouteConfig,
        SignalTransform,
    };
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::net::UdpSocket;
    use std::time::Instant as StdInstant;

    let receiver = UdpSocket::bind("127.0.0.1:0").expect("receiver bind");
    receiver
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .expect("set read timeout");
    let dest_port = receiver.local_addr().expect("local_addr").port();

    let mut config = create_route_only_test_config();
    config.endpoints.retain(|e| e.alias != "absynth");
    config.endpoints.push(EndpointConfig {
        alias: "lights-artnet".to_string(),
        direction: ConnectorDirection::Output,
        protocol: Some(ConnectorProtocol::ArtNet),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::ArtNetEndpoint {
            universe: 0,
            host: "127.0.0.1".to_string(),
            port: dest_port,
            allow_broadcast: false,
            security: Default::default(),
        },
    });
    let mut cc_to_dmx = std::collections::HashMap::new();
    cc_to_dmx.insert(7u8, 50u16); // CC 7 → DMX channel 50
    config.routes = vec![RouteConfig {
        from: "pads".to_string(),
        to: "lights-artnet".to_string(),
        transform: Some(SignalTransform::MidiToArtNet {
            cc_to_dmx,
            note_to_dmx: std::collections::HashMap::new(),
        }),
        filter: None,
        enabled: true,
        description: Some("cross-protocol — MIDI → Art-Net".to_string()),
        modes: Vec::new(),
    }];

    let mut manager = create_simulate_manager(config).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    let mut rx = manager.event_broadcast_tx.subscribe();

    // CC 7 value 127 → DmxUpdate { channel: 50, value: 255 }.
    let device_event = DeviceEvent::new(
        DeviceId::raw("pads"),
        InputEvent::ControlChange {
            control: 7,
            value: 127,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok");

    // Receiver must observe one OpDmx packet — 530 bytes:
    // 18-byte header + 512-byte frame. Verify the header magic
    // + that DMX channel 50 (1-indexed → byte index 49 in the
    // frame body, which starts at byte 18 of the packet) holds
    // value 255.
    let mut buf = [0u8; 1024];
    let (n, _) = receiver
        .recv_from(&mut buf)
        .expect("receiver must observe an OpDmx packet from stage-9");
    assert_eq!(n, 530, "OpDmx packet should be 530 bytes");
    assert_eq!(&buf[0..8], b"Art-Net\0", "OpDmx header magic");
    assert_eq!(
        buf[18 + 49],
        255,
        "DMX channel 50 (frame byte 49) should hold 255 after scaling CC value 127"
    );

    let event = drain_for_event_type(&mut rx, "route_forwarded")
        .expect("route_forwarded must fire for Art-Net dispatch");
    let payload = event.payload.expect("event must carry payload");
    assert_eq!(payload["protocol"], "ArtNet");
    assert_eq!(payload["to"], "lights-artnet");
    assert!(
        payload["bound_port"].is_null(),
        "Art-Net connectors have no bound_port — must serialize as null"
    );

    let registry = manager.connector_registry.read().await;
    let connector = registry
        .get("lights-artnet")
        .expect("lights-artnet connector must still exist in registry");
    assert_eq!(
        connector.metrics.total_messages, 1,
        "successful Art-Net send must increment total_messages"
    );
}

/// Still-excluded cross-protocol variants (OscToMidi, HidToArtNet)
/// MUST NOT dispatch — pinning the contract that
/// `RouteEngine::compile()` still drops them with
/// `ExclusionReason::CrossProtocolTransformUnsupported` until their
/// runtime paths land.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_still_excluded_cross_protocol_variants_do_not_dispatch() {
    use conductor_core::config::types::{RouteConfig, SignalTransform};
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut config = create_route_only_test_config();
    // MidiToArtNet is now admitted, so the "still excluded" list
    // shrinks to HidToArtNet + OscToMidi. This test pins HidToArtNet
    // (needs InputEvent-shaped stage-9 plumbing — still pending).
    config.routes = vec![RouteConfig {
        from: "pads".to_string(),
        to: "absynth".to_string(),
        transform: Some(SignalTransform::HidToArtNet {
            trigger_to_channel: std::collections::HashMap::new(),
        }),
        filter: None,
        enabled: true,
        description: Some("still-excluded variant".to_string()),
        modes: Vec::new(),
    }];

    let mut manager = create_simulate_manager(config).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    let mut rx = manager.event_broadcast_tx.subscribe();

    let device_event = DeviceEvent::new(
        DeviceId::raw("pads"),
        InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(device_event)
        .await
        .expect("process_device_event must return Ok");

    assert!(
        drain_for_event_type(&mut rx, "route_forwarded").is_none(),
        "route_forwarded MUST NOT fire for still-excluded HidToArtNet — \
             compile() drops it per ExclusionReason::CrossProtocolTransformUnsupported"
    );

    let registry = manager.connector_registry.read().await;
    let connector = registry
        .get("absynth")
        .expect("absynth connector must still exist in registry");
    assert_eq!(
        connector.metrics.total_messages, 0,
        "record_activity MUST NOT fire when the route is compile-excluded"
    );
}
