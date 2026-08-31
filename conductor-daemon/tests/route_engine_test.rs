// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-031 Phase 2B § 4.4 — `RouteEngine` runtime.
//!
//! Covers the full Phase 2B runtime surface for ADR-031 P2 (#1142):
//! the engine is built from `&[RouteConfig]`, indexes routes by source
//! alias for O(1) lookup, supports fan-out (multiple routes from one
//! source), excludes disabled / cross-protocol-transform / OSC-filter
//! routes at compile time, evaluates `filter_matches()` across the
//! MIDI filter dimensions, and applies same-protocol `MidiTransform`
//! in `route_destinations()`.
//!
//! Spec reference: `docs/signal-routing/ADR-031-implementation-spec.md` § 4.4.
//!
//! Deferred beyond Phase 2B:
//!   - § 4.5 stage-9 integration into `EngineManager` event pump
//!   - § 4.7 4 MCP tools (`conductor_create_route` /
//!     `conductor_list_routes` / `conductor_update_route` /
//!     `conductor_delete_route`)
//!   - Phase 5: cross-protocol transform runtime + OSC event matching
//!     (routes with `osc_address_prefix` are excluded at compile time
//!     and reported via `excluded_routes()`)

use conductor_core::config::MidiMessageType;
use conductor_core::config::types::{RouteConfig, SignalFilter, SignalTransform};
use conductor_core::transform::MidiTransform;
use conductor_daemon::route_engine::{ExclusionReason, RouteEngine, RouteOutput, RouteOutputKind};

/// Test helper — extract just the destination aliases from the
/// `RouteOutput`s returned by `route_destinations`. Used by slice 1+2
/// tests that don't care about the transformed bytes or output kind.
fn aliases(outputs: &[RouteOutput]) -> Vec<String> {
    outputs.iter().map(|o| o.to_alias.clone()).collect()
}

// ── Helpers for raw MIDI bytes (slice 2+) ──
//
// Status byte layout: high nibble = message type, low nibble = channel.
// 0x80 = NoteOff, 0x90 = NoteOn, 0xA0 = PolyAftertouch,
// 0xB0 = CC, 0xC0 = ProgramChange, 0xD0 = Aftertouch (channel pressure),
// 0xE0 = PitchBend.

fn note_on(channel: u8, note: u8, velocity: u8) -> Vec<u8> {
    vec![0x90 | (channel & 0x0F), note, velocity]
}

fn note_off(channel: u8, note: u8) -> Vec<u8> {
    vec![0x80 | (channel & 0x0F), note, 0]
}

fn cc(channel: u8, controller: u8, value: u8) -> Vec<u8> {
    vec![0xB0 | (channel & 0x0F), controller, value]
}

/// MIDI timing-clock — a System Real-Time message (status `0xF8`,
/// `>= 0xF0`). System messages carry no channel nibble.
fn system_clock() -> Vec<u8> {
    vec![0xF8]
}

fn route(from: &str, to: &str) -> RouteConfig {
    RouteConfig {
        from: from.into(),
        to: to.into(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    }
}

fn route_disabled(from: &str, to: &str) -> RouteConfig {
    RouteConfig {
        enabled: false,
        ..route(from, to)
    }
}

#[test]
fn compile_empty_routes_yields_empty_engine() {
    let engine = RouteEngine::compile(&[]);
    assert_eq!(
        aliases(&engine.route_destinations_midi("anything", &note_on(0, 60, 64), "Default")),
        Vec::<String>::new()
    );
}

#[test]
fn compile_single_route_indexes_by_source() {
    let engine = RouteEngine::compile(&[route("pads", "absynth")]);
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default")),
        vec!["absynth".to_string()]
    );
}

#[test]
fn compile_disabled_route_is_excluded() {
    // Per spec § 4.4: `if !route.enabled { continue; }` — disabled
    // routes don't enter the compiled map at all, so the runtime can
    // skip the enabled check on every event.
    let engine = RouteEngine::compile(&[route_disabled("pads", "absynth")]);
    assert!(
        engine
            .route_destinations_midi("pads", &note_on(0, 60, 64), "Default")
            .is_empty()
    );
}

#[test]
fn compile_two_routes_from_same_source_fans_out() {
    // Per spec § 4.4: fan-out is the default for routes — multiple
    // routes from the same source produce N output destinations per
    // event. This is what makes "send pads to both Absynth AND a
    // recording bus" expressible in one config block.
    let engine = RouteEngine::compile(&[route("pads", "absynth"), route("pads", "recorder")]);
    let mut dests =
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default"));
    dests.sort();
    assert_eq!(dests, vec!["absynth".to_string(), "recorder".to_string()]);
}

#[test]
fn compile_routes_from_different_sources_indexed_separately() {
    let engine = RouteEngine::compile(&[route("pads", "absynth"), route("faders", "console")]);
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default")),
        vec!["absynth".to_string()]
    );
    assert_eq!(
        aliases(&engine.route_destinations_midi("faders", &note_on(0, 60, 64), "Default")),
        vec!["console".to_string()]
    );
}

#[test]
fn route_destinations_unknown_source_returns_empty() {
    // The hot path: `aliases(&engine.route_destinations_midi(alias, "Default"))` is called for
    // EVERY event the rule engine doesn't match (stage 9 of the
    // 8-stage matcher per ADR-031 §D2). Unknown-source must be a
    // cheap empty return — no allocation, no logging.
    let engine = RouteEngine::compile(&[route("pads", "absynth")]);
    assert!(
        engine
            .route_destinations_midi("ghost", &note_on(0, 60, 64), "Default")
            .is_empty()
    );
    assert!(
        engine
            .route_destinations_midi("", &note_on(0, 60, 64), "Default")
            .is_empty()
    );
}

#[test]
fn compile_mixes_enabled_and_disabled_correctly() {
    // Mixed: source has 3 routes, 2 enabled and 1 disabled. Only the
    // 2 enabled destinations should appear.
    let engine = RouteEngine::compile(&[
        route("pads", "absynth"),
        route_disabled("pads", "muted-synth"),
        route("pads", "recorder"),
    ]);
    let mut dests =
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default"));
    dests.sort();
    assert_eq!(dests, vec!["absynth".to_string(), "recorder".to_string()]);
}

#[test]
fn compile_route_with_filter_or_transform_still_indexed() {
    // Slice 1 doesn't apply filter or transform — those land in later
    // slices. But routes with filter/transform fields populated MUST
    // still index correctly (the runtime will evaluate them per-event).
    let cfg = RouteConfig {
        from: "pads".into(),
        to: "absynth".into(),
        transform: None,
        filter: Some(SignalFilter {
            message_types: vec![],
            channels: vec![0, 9],
            cc_range: None,
            note_range: None,
            osc_address_prefix: None,
        }),
        enabled: true,
        description: None,
        modes: Vec::new(),
    };
    let engine = RouteEngine::compile(&[cfg]);
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default")),
        vec!["absynth".to_string()]
    );
}

// ── Slice 2: filter_matches per-event evaluation ──
//
// Per spec § 4.4 + § 4.1: a route's filter is evaluated against each
// event before forwarding. Filter dimensions:
//   - `message_types: Vec<MidiMessageType>` (empty = all types)
//   - `channels: Vec<u8>` (empty = all channels, 0-15)
//   - `cc_range: Option<(u8, u8)>` (only CC events; inclusive range)
//   - `note_range: Option<(u8, u8)>` (only Note events; inclusive)
//   - `osc_address_prefix: Option<String>` (OSC events only — Phase 5)
//
// All populated dimensions combine with AND. Slice 2 uses raw MIDI
// bytes directly — status byte's high nibble is the message type, low
// nibble is the channel (0-indexed). Slice 3+ adds transform application.

fn route_with_filter(from: &str, to: &str, filter: SignalFilter) -> RouteConfig {
    RouteConfig {
        from: from.into(),
        to: to.into(),
        transform: None,
        filter: Some(filter),
        enabled: true,
        description: None,
        modes: Vec::new(),
    }
}

fn empty_filter() -> SignalFilter {
    SignalFilter {
        message_types: vec![],
        channels: vec![],
        cc_range: None,
        note_range: None,
        osc_address_prefix: None,
    }
}

#[test]
fn filter_with_no_constraints_matches_any_event() {
    let engine = RouteEngine::compile(&[route_with_filter("pads", "absynth", empty_filter())]);
    // All-empty filter passes anything through.
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default")),
        vec!["absynth".to_string()]
    );
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &cc(9, 7, 100), "Default")),
        vec!["absynth".to_string()]
    );
}

#[test]
fn channel_filter_matches_only_listed_channels() {
    let mut filter = empty_filter();
    filter.channels = vec![0, 9]; // ch.1 + ch.10 (0-indexed)
    let engine = RouteEngine::compile(&[route_with_filter("pads", "absynth", filter)]);

    // Channel 0 → match
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default")),
        vec!["absynth".to_string()]
    );
    // Channel 9 → match
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(9, 36, 100), "Default")),
        vec!["absynth".to_string()]
    );
    // Channel 5 → drop
    assert!(
        engine
            .route_destinations_midi("pads", &note_on(5, 60, 64), "Default")
            .is_empty()
    );
}

#[test]
fn note_range_filter_matches_only_in_range() {
    let mut filter = empty_filter();
    filter.note_range = Some((36, 96)); // typical pad range
    let engine = RouteEngine::compile(&[route_with_filter("pads", "absynth", filter)]);

    // In range → match
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default")),
        vec!["absynth".to_string()]
    );
    // Boundary inclusive → match
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 36, 64), "Default")),
        vec!["absynth".to_string()]
    );
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 96, 64), "Default")),
        vec!["absynth".to_string()]
    );
    // Below range → drop
    assert!(
        engine
            .route_destinations_midi("pads", &note_on(0, 35, 64), "Default")
            .is_empty()
    );
    // Above range → drop
    assert!(
        engine
            .route_destinations_midi("pads", &note_on(0, 97, 64), "Default")
            .is_empty()
    );
}

#[test]
fn cc_range_filter_matches_only_in_range() {
    let mut filter = empty_filter();
    filter.cc_range = Some((1, 64));
    let engine = RouteEngine::compile(&[route_with_filter("faders", "console", filter)]);

    // In range → match
    assert_eq!(
        aliases(&engine.route_destinations_midi("faders", &cc(0, 7, 100), "Default")),
        vec!["console".to_string()]
    );
    // Boundary inclusive → match
    assert_eq!(
        aliases(&engine.route_destinations_midi("faders", &cc(0, 1, 100), "Default")),
        vec!["console".to_string()]
    );
    assert_eq!(
        aliases(&engine.route_destinations_midi("faders", &cc(0, 64, 100), "Default")),
        vec!["console".to_string()]
    );
    // Outside range → drop
    assert!(
        engine
            .route_destinations_midi("faders", &cc(0, 0, 100), "Default")
            .is_empty()
    );
    assert!(
        engine
            .route_destinations_midi("faders", &cc(0, 65, 100), "Default")
            .is_empty()
    );
}

#[test]
fn message_types_filter_matches_only_listed_types() {
    let mut filter = empty_filter();
    filter.message_types = vec![MidiMessageType::NoteOn];
    let engine = RouteEngine::compile(&[route_with_filter("pads", "absynth", filter)]);

    // NoteOn → match
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default")),
        vec!["absynth".to_string()]
    );
    // NoteOff → drop (not in filter list)
    assert!(
        engine
            .route_destinations_midi("pads", &note_off(0, 60), "Default")
            .is_empty()
    );
    // CC → drop
    assert!(aliases(&engine.route_destinations_midi("pads", &cc(0, 7, 100), "Default")).is_empty());
}

#[test]
fn channel_aftertouch_classifies_as_aftertouch_not_channel_pressure() {
    // Copilot review on PR #1175: status 0xD0 (MIDI Channel Pressure /
    // channel aftertouch) must classify as `MidiMessageType::Aftertouch`
    // — the vocabulary the routes validator accepts and the rest of the
    // pipeline uses (event_processor, action_executor, the Aftertouch
    // trigger). `MidiMessageType::ChannelPressure` is rejected by the
    // validator as reserved, so classifying 0xD0 as ChannelPressure
    // means a valid `message_types = [Aftertouch]` filter could never
    // match a real channel-aftertouch event.
    let mut filter = empty_filter();
    filter.message_types = vec![MidiMessageType::Aftertouch];
    let engine = RouteEngine::compile(&[route_with_filter("pads", "synth", filter)]);

    // 0xD0 channel-aftertouch event (status + pressure) → match
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &[0xD0, 90], "Default")),
        vec!["synth".to_string()],
        "0xD0 must classify as Aftertouch so an Aftertouch filter matches it"
    );
    // A NoteOn must still NOT match an Aftertouch-only filter.
    assert!(
        engine
            .route_destinations_midi("pads", &note_on(0, 60, 64), "Default")
            .is_empty()
    );
}

#[test]
fn multiple_filter_dimensions_combine_with_and() {
    // Channel = 0 AND note_range = [36..96] — both must match.
    let mut filter = empty_filter();
    filter.channels = vec![0];
    filter.note_range = Some((36, 96));
    let engine = RouteEngine::compile(&[route_with_filter("pads", "absynth", filter)]);

    // Both match
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default")),
        vec!["absynth".to_string()]
    );
    // Channel matches, note out-of-range
    assert!(
        engine
            .route_destinations_midi("pads", &note_on(0, 100, 64), "Default")
            .is_empty()
    );
    // Note matches, channel mismatch
    assert!(
        engine
            .route_destinations_midi("pads", &note_on(5, 60, 64), "Default")
            .is_empty()
    );
}

#[test]
fn cc_range_does_not_apply_to_non_cc_events() {
    // Per spec: cc_range only constrains CC events. Note events should
    // pass through regardless of cc_range value (no false-drop on
    // non-applicable dimension).
    let mut filter = empty_filter();
    filter.cc_range = Some((1, 64));
    let engine = RouteEngine::compile(&[route_with_filter("pads", "absynth", filter)]);

    // Note event → match (cc_range is N/A)
    assert_eq!(
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default")),
        vec!["absynth".to_string()]
    );
}

#[test]
fn note_range_does_not_apply_to_non_note_events() {
    // Symmetrically: note_range doesn't constrain CC events.
    let mut filter = empty_filter();
    filter.note_range = Some((36, 96));
    let engine = RouteEngine::compile(&[route_with_filter("faders", "console", filter)]);

    // CC event → match (note_range is N/A)
    assert_eq!(
        aliases(&engine.route_destinations_midi("faders", &cc(0, 7, 100), "Default")),
        vec!["console".to_string()]
    );
}

#[test]
fn osc_address_prefix_filter_never_matches_midi_events() {
    // Copilot review on PR #1175 finding #4 / Council finding #3:
    // `osc_address_prefix` is an OSC-domain constraint a raw MIDI event
    // can never satisfy. A route filtered on it is inert in the
    // MIDI-only pipeline — `route_destinations()` must yield nothing
    // for any MIDI event. (The route is also reported via
    // `excluded_routes()` — see `excluded_routes_reports_osc_filter_routes`.
    // Actual OSC routing lands in Phase 5.)
    let mut filter = empty_filter();
    filter.osc_address_prefix = Some("/synth".to_string());
    let engine = RouteEngine::compile(&[route_with_filter("pads", "absynth", filter)]);

    // MIDI note → drop (cannot satisfy an OSC address constraint)
    assert!(
        engine
            .route_destinations_midi("pads", &note_on(0, 60, 64), "Default")
            .is_empty()
    );
    // MIDI CC → drop too
    assert!(
        engine
            .route_destinations_midi("pads", &cc(0, 7, 100), "Default")
            .is_empty()
    );
}

// ── Slice 3: SignalTransform application ──
//
// Per spec § 4.4: the route's transform (if any) is applied to the
// raw MIDI bytes to produce the output bytes sent downstream.
// `SignalTransform::Midi(MidiTransform)` reuses the existing
// `MidiTransform::apply` from ADR-009 Gap 2 / v4.25.0.
//
// Cross-protocol variants (`MidiToOsc` / `OscToMidi` /
// `MidiToArtNet` / `HidToArtNet`) are validated as legal in Phase 2A
// but their runtime translation is not yet implemented; slice 3 verifies
// they are excluded at compile time (see `cross_protocol_transform_is_excluded_at_compile_time`).

fn route_with_transform(from: &str, to: &str, transform: SignalTransform) -> RouteConfig {
    RouteConfig {
        from: from.into(),
        to: to.into(),
        transform: Some(transform),
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    }
}

fn midi_transform_channel(channel: u8) -> SignalTransform {
    SignalTransform::Midi(MidiTransform {
        channel: Some(channel),
        cc: None,
        note: None,
        velocity_scale: None,
        velocity_offset: None,
        invert_value: false,
        curve: None,
    })
}

#[test]
fn route_without_transform_passes_raw_bytes_through() {
    // Sanity baseline: no transform → output bytes equal input bytes.
    let engine = RouteEngine::compile(&[route("pads", "absynth")]);
    let outputs = engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default");
    assert_eq!(outputs.len(), 1);
    let RouteOutput {
        to_alias: alias,
        bytes,
        ..
    } = &outputs[0];
    assert_eq!(alias, "absynth");
    assert_eq!(bytes, &note_on(0, 60, 64));
}

#[test]
fn route_with_midi_transform_applies_channel_remap() {
    // MidiTransform.channel = Some(5) on a NoteOn(ch.0, n.60, v.64)
    // → NoteOn(ch.5, n.60, v.64).
    let engine = RouteEngine::compile(&[route_with_transform(
        "pads",
        "absynth",
        midi_transform_channel(5),
    )]);
    let outputs = engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default");
    assert_eq!(outputs.len(), 1);
    let RouteOutput {
        to_alias: alias,
        bytes,
        ..
    } = &outputs[0];
    assert_eq!(alias, "absynth");
    // Status byte's low nibble (channel) should be 5 now.
    assert_eq!(bytes[0] & 0x0F, 5, "channel remapped to 5");
    assert_eq!(bytes[0] & 0xF0, 0x90, "still NoteOn");
    assert_eq!(bytes[1], 60, "note unchanged");
    assert_eq!(bytes[2], 64, "velocity unchanged");
}

#[test]
fn all_cross_protocol_transforms_are_admitted() {
    // The "rotate the fixture to the next still-excluded transform" lifecycle
    // (MidiToOsc → MidiToArtNet → HidToArtNet → OscToMidi) ENDED with
    // ADR-039-A (#1361): `OscToMidi` was the last cross-protocol transform
    // without a runtime, and it is now admitted (the OSC input listener feeds
    // it the structured `OscInbound` via `RouteEvalContext.osc`). So NO
    // `SignalTransform` variant is excluded as
    // `CrossProtocolTransformUnsupported` any more — `compile()`'s match is
    // exhaustive, and a future unwired variant fails to compile rather than
    // being excluded at runtime.
    let xform = SignalTransform::OscToMidi {
        address_to_cc: Some("/eos/fader/{cc}".into()),
        address_to_note: None,
        channel: None,
    };
    let engine = RouteEngine::compile(&[route_with_transform("console", "synth", xform)]);
    // OscToMidi is admitted — nothing excluded for an unsupported transform.
    assert!(
        engine.excluded_routes().is_empty(),
        "OscToMidi is admitted now; excluded_routes() should be empty; got: {:?}",
        engine.excluded_routes()
    );
    // It is STRUCTURED: a MIDI byte caller (no `OscInbound`, `ctx.osc == None`)
    // produces no output — the route skips rather than firing on the wrong
    // protocol path.
    let outputs = engine.route_destinations_midi("console", &note_on(0, 60, 64), "Default");
    assert!(
        outputs.is_empty(),
        "OscToMidi must skip for a MIDI byte caller (osc is None); got: {:?}",
        outputs
    );
}

#[test]
fn fan_out_with_mixed_transforms_per_route() {
    // One source, two routes: one with channel-remap, one passthrough.
    // Each destination should receive its own transformed bytes.
    let engine = RouteEngine::compile(&[
        route("pads", "passthrough"),
        route_with_transform("pads", "remapped", midi_transform_channel(9)),
    ]);
    let mut outputs = engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default");
    outputs.sort_by(|a, b| a.to_alias.cmp(&b.to_alias));
    assert_eq!(outputs.len(), 2);

    // passthrough route: bytes unchanged
    assert_eq!(outputs[0].to_alias, "passthrough");
    assert_eq!(outputs[0].bytes, note_on(0, 60, 64));

    // remapped route: channel changed to 9
    assert_eq!(outputs[1].to_alias, "remapped");
    assert_eq!(outputs[1].bytes[0] & 0x0F, 9);
}

#[test]
fn route_with_transform_producing_empty_output_is_skipped() {
    // `MidiTransform::apply` returns Vec::new() for invalid /
    // truncated input. The route engine must NOT emit empty bytes
    // downstream — drop the route's contribution instead.
    let engine = RouteEngine::compile(&[route_with_transform(
        "pads",
        "absynth",
        midi_transform_channel(5),
    )]);
    // Truncated NoteOn (only 1 byte) — apply returns empty.
    let outputs = engine.route_destinations_midi("pads", &[0x90], "Default");
    // status byte 0x90 alone — no note/velocity bytes for the
    // transform to work with → MidiTransform::apply returns empty
    // → route is skipped (no entry in outputs).
    assert!(
        outputs.is_empty(),
        "truncated input must skip the route; got: {:?}",
        outputs
    );
}

// ── Slice 6: Council review on slice 5 ──
//
// Council FAIL (2f058e3e) flagged two real bugs:
//   #1 System-message behavioral asymmetry — `filter: None` routes a
//      system message through, but `filter: Some(empty)` dropped it
//      (filter_matches early-returned false for status >= 0xF0). An
//      empty filter must behave identically to no filter.
//   #2 Hot-path `tracing::warn!` — cross-protocol transforms logged a
//      warning on EVERY event; should be a one-time compile-time
//      warning + the route excluded from the map entirely.

#[test]
fn system_message_with_no_filter_route_is_forwarded() {
    // Baseline: a route with `filter: None` forwards a system message
    // (timing clock, 0xF8) untouched.
    let engine = RouteEngine::compile(&[route("clock-src", "clock-sink")]);
    let outputs = engine.route_destinations_midi("clock-src", &system_clock(), "Default");
    assert_eq!(outputs.len(), 1, "no-filter route forwards system message");
    assert_eq!(outputs[0].to_alias, "clock-sink");
    assert_eq!(outputs[0].bytes, system_clock());
}

#[test]
fn system_message_with_empty_filter_route_is_also_forwarded() {
    // Council finding #1: an EMPTY filter (all dimensions
    // unconstrained) must behave identically to `filter: None` —
    // both mean "match everything". Pre-fix, the empty-filter route
    // dropped the system message while the no-filter route forwarded
    // it: a behavioral asymmetry.
    let engine =
        RouteEngine::compile(&[route_with_filter("clock-src", "clock-sink", empty_filter())]);
    let outputs = engine.route_destinations_midi("clock-src", &system_clock(), "Default");
    assert_eq!(
        outputs.len(),
        1,
        "empty filter must behave like no filter — system message forwarded"
    );
    assert_eq!(outputs[0].to_alias, "clock-sink");
}

#[test]
fn system_message_with_constrained_filter_is_dropped() {
    // Correctness boundary: a filter that DOES have constraints
    // (e.g. `message_types = [NoteOn]`) genuinely cannot match a
    // system message — system messages have no channel and aren't
    // one of the 7 standard channel-voice types. Dropping here is
    // correct, NOT the asymmetry bug.
    let mut filter = empty_filter();
    filter.message_types = vec![MidiMessageType::NoteOn];
    let engine = RouteEngine::compile(&[route_with_filter("clock-src", "clock-sink", filter)]);
    let outputs = engine.route_destinations_midi("clock-src", &system_clock(), "Default");
    assert!(
        outputs.is_empty(),
        "a constrained filter correctly drops a system message; got: {:?}",
        outputs
    );
}

#[test]
fn cross_protocol_route_is_excluded_at_compile_time() {
    // Council finding #2: cross-protocol transforms must be excluded
    // from the compiled map entirely (with a one-time warning at
    // compile()), NOT skipped per-event via a hot-path `tracing::warn!`.
    //
    // Observable contract: a config with ONLY a cross-protocol route
    // for a source alias compiles to an engine where that source has
    // no routes at all — `route_destinations` returns empty without
    // ever reaching a per-event branch.
    //
    // #1523: this used `MidiToOsc`, but P5 slice 3+4 (#1347) ADMITTED
    // `MidiToOsc`, so asserting it is excluded contradicted the runtime
    // contract (and risked pressuring a regression). Use `OscToMidi`,
    // which — like `HidToArtNet` in
    // `cross_protocol_transform_is_excluded_at_compile_time` — remains
    // excluded (needs an OSC input listener). The two tests now cover
    // distinct still-excluded transforms.
    let xform = SignalTransform::OscToMidi {
        address_to_cc: Some("/lights/cc".into()),
        address_to_note: None,
        channel: None,
    };
    let engine = RouteEngine::compile(&[route_with_transform("pads", "midi-out", xform)]);
    // The source alias should not even be a key in the routes map —
    // route_destinations returns empty (same allocation-free path as
    // a genuinely unknown source).
    let outputs = engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default");
    assert!(
        outputs.is_empty(),
        "cross-protocol route must be excluded at compile time; got: {:?}",
        outputs
    );
}

#[test]
fn cross_protocol_route_does_not_suppress_sibling_midi_route() {
    // A source with TWO routes — one cross-protocol (excluded) and
    // one plain MIDI (kept) — should still fire the MIDI route. The
    // compile-time exclusion must be per-route, not per-source.
    //
    // #1523: use the still-excluded `OscToMidi` rather than the now-admitted
    // `MidiToOsc`, so "one cross-protocol route is excluded" is unambiguously
    // true under the current transform matrix.
    let xform = SignalTransform::OscToMidi {
        address_to_cc: Some("/lights/cc".into()),
        address_to_note: None,
        channel: None,
    };
    let engine = RouteEngine::compile(&[
        route_with_transform("pads", "midi-out", xform),
        route("pads", "absynth"),
    ]);
    let outputs = engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default");
    assert_eq!(
        outputs.len(),
        1,
        "the plain MIDI sibling route must survive; got: {:?}",
        outputs
    );
    assert_eq!(outputs[0].to_alias, "absynth");
}

// ── Slice 7: Council review on slice 6 ──
//
// Council FAIL (1b31848b) flagged four concerns:
//   #1 Send+Sync not explicit — fixed via a compile-time static
//      assertion in route_engine.rs (no runtime test needed).
//   #2 Cross-protocol exclusion had no API-level feedback — fixed
//      via `RouteEngine::excluded_routes()` (tested here).
//   #3 "Allocation-free" doc overstated — doc-only reword.
//   #4 NoteOn velocity=0 should classify as NoteOff for message-type
//      filtering (MIDI running-status convention) — tested here.

#[test]
fn excluded_routes_reports_disabled_routes() {
    let engine = RouteEngine::compile(&[
        route("pads", "absynth"),
        route_disabled("faders", "muted-sink"),
    ]);
    let excluded = engine.excluded_routes();
    assert_eq!(excluded.len(), 1, "one disabled route should be reported");
    assert_eq!(excluded[0].from_alias, "faders");
    assert_eq!(excluded[0].to_alias, "muted-sink");
    assert_eq!(excluded[0].reason, ExclusionReason::Disabled);
}

// NOTE: `excluded_routes_reports_cross_protocol_routes` was removed in
// ADR-039-A (#1361). It asserted that a cross-protocol route is reported via
// `excluded_routes()` with `CrossProtocolTransformUnsupported`, using whichever
// transform was still unimplemented as the fixture. `OscToMidi` was the LAST
// such transform and is now admitted, so that exclusion reason is unreachable
// (the `compile()` match is exhaustive). The `excluded_routes()` API is still
// covered for the `Disabled` reason by `excluded_routes_reports_disabled_routes`,
// and the "everything is admitted" milestone by `all_cross_protocol_transforms_are_admitted`.

#[test]
fn midi_to_artnet_route_emits_3_byte_artnet_output() {
    // P5 slice 8: a `MidiToArtNet` route is now admitted at compile
    // time and emits a `RouteOutput` with `kind: ArtNet` and the
    // DmxUpdate serialized as 3 bytes — `[chan_high, chan_low, value]`.
    // This contract is what the engine_manager stage-9 dispatch
    // deserializes back into a `DmxUpdate` for `send_artnet`.
    let mut cc_to_dmx = std::collections::HashMap::new();
    cc_to_dmx.insert(7u8, 50u16); // CC 7 (volume) → DMX channel 50
    let xform = SignalTransform::MidiToArtNet {
        cc_to_dmx,
        note_to_dmx: std::collections::HashMap::new(),
    };
    let engine = RouteEngine::compile(&[route_with_transform("pads", "lights-artnet", xform)]);

    // CC 7 value 127 → DmxUpdate { channel: 50, value: 255 }
    //   (7-bit 127 scales to 8-bit 255 — see midi_to_artnet docs)
    // Serialized: [0x00, 0x32, 0xFF].
    let outputs = engine.route_destinations_midi("pads", &[0xB0, 7, 127], "Default");
    assert_eq!(outputs.len(), 1, "MidiToArtNet route should fire");
    assert_eq!(outputs[0].to_alias, "lights-artnet");
    assert_eq!(
        outputs[0].kind,
        conductor_daemon::route_engine::RouteOutputKind::ArtNet
    );
    assert_eq!(outputs[0].bytes, vec![0x00, 0x32, 0xFF]);
}

#[test]
fn midi_to_artnet_route_drops_input_not_in_mapping_table() {
    // A `MidiToArtNet` with `cc_to_dmx` populated but `note_to_dmx`
    // empty must not fire on a NoteOn — the transform returns None,
    // and the route's contribution is dropped (no empty / sentinel
    // output downstream). Mirrors the OSC-side behaviour from
    // `route_with_transform_producing_empty_output_is_skipped`.
    let mut cc_to_dmx = std::collections::HashMap::new();
    cc_to_dmx.insert(7u8, 50u16);
    let xform = SignalTransform::MidiToArtNet {
        cc_to_dmx,
        note_to_dmx: std::collections::HashMap::new(),
    };
    let engine = RouteEngine::compile(&[route_with_transform("pads", "lights-artnet", xform)]);

    let outputs = engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default");
    assert!(
        outputs.is_empty(),
        "NoteOn against a CC-only MidiToArtNet must produce no output; got: {:?}",
        outputs
    );
}

#[test]
fn excluded_routes_reports_osc_filter_routes() {
    // Council review on PR #1175 finding #3: a route whose filter sets
    // `osc_address_prefix` can never match a MIDI event (OSC-domain
    // constraint, AND-combined with every other dimension). Dropping it
    // silently inside `filter_matches()` per-event gave the user no
    // feedback that the route is inert. It is now excluded at compile
    // time — same machinery as disabled / cross-protocol routes — so
    // `excluded_routes()` surfaces it. Real OSC matching is Phase 5.
    let mut filter = empty_filter();
    filter.osc_address_prefix = Some("/synth".to_string());
    let engine = RouteEngine::compile(&[
        route("pads", "absynth"),
        route_with_filter("faders", "osc-sink", filter),
    ]);
    let excluded = engine.excluded_routes();
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].from_alias, "faders");
    assert_eq!(excluded[0].to_alias, "osc-sink");
    assert_eq!(excluded[0].reason, ExclusionReason::OscFilterUnsupported);
}

#[test]
fn excluded_routes_is_empty_when_all_routes_compile() {
    let engine = RouteEngine::compile(&[route("pads", "absynth"), route("faders", "console")]);
    assert!(
        engine.excluded_routes().is_empty(),
        "no exclusions expected for all-valid config"
    );
}

#[test]
fn note_on_velocity_zero_classifies_as_note_off_for_filter() {
    // Council finding #4: `NoteOn` with velocity 0 is semantically a
    // `NoteOff` (MIDI running-status convention). A route filtering
    // for `message_types = [NoteOff]` must match a zero-velocity
    // NoteOn.
    let mut filter = empty_filter();
    filter.message_types = vec![MidiMessageType::NoteOff];
    let engine = RouteEngine::compile(&[route_with_filter("pads", "absynth", filter)]);

    // NoteOn status (0x90) with velocity 0 → classified as NoteOff → matches
    let outputs = engine.route_destinations_midi("pads", &note_on(0, 60, 0), "Default");
    assert_eq!(
        outputs.len(),
        1,
        "zero-velocity NoteOn must match a NoteOff filter; got: {:?}",
        outputs
    );

    // NoteOn with non-zero velocity → genuine NoteOn → does NOT match
    let outputs = engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default");
    assert!(
        outputs.is_empty(),
        "non-zero-velocity NoteOn must NOT match a NoteOff filter; got: {:?}",
        outputs
    );
}

#[test]
fn note_on_velocity_zero_does_not_match_note_on_filter() {
    // Mirror of the above: a `message_types = [NoteOn]` filter must
    // NOT match a zero-velocity NoteOn (it's a NoteOff semantically).
    let mut filter = empty_filter();
    filter.message_types = vec![MidiMessageType::NoteOn];
    let engine = RouteEngine::compile(&[route_with_filter("pads", "absynth", filter)]);

    let outputs = engine.route_destinations_midi("pads", &note_on(0, 60, 0), "Default");
    assert!(
        outputs.is_empty(),
        "zero-velocity NoteOn must NOT match a NoteOn filter; got: {:?}",
        outputs
    );
}

// ── Slice 9 (#1667): explain_route_match ──
//
// `conductor_explain_route_match` needs a per-route trace: for a given
// (source_alias, raw_midi, active_mode), report each candidate route as
// fired or skipped + the reason (mode-ineligible / filter-mismatch with
// the failing dimension / transform-produced-no-output). Shares the
// single per-route evaluation path with `route_destinations` so the
// explanation can never drift from what actually dispatches.

#[test]
fn explain_reports_fired_skipped_modes_and_filter() {
    use conductor_daemon::route_engine::{FilterDimension, RouteSkipReason};

    let routes = vec![
        // Fires: no mode scope, no filter.
        route("pads", "absynth"),
        // Mode-ineligible: scoped to "Drums" but active mode is "Keys".
        RouteConfig {
            modes: vec!["Drums".into()],
            ..route("pads", "drum_synth")
        },
        // Filter mismatch on channel: filter wants ch 5, event is ch 0.
        route_with_filter(
            "pads",
            "ch5_only",
            SignalFilter {
                channels: vec![5],
                ..empty_filter()
            },
        ),
    ];
    let engine = RouteEngine::compile(&routes);
    let ex = engine.explain_route_match("pads", &note_on(0, 60, 64), "Keys");
    assert_eq!(ex.len(), 3, "one explanation per candidate route");

    let fired: Vec<&str> = ex
        .iter()
        .filter(|e| e.fired)
        .map(|e| e.to_alias.as_str())
        .collect();
    assert_eq!(fired, vec!["absynth"], "only the unconstrained route fires");

    let drum = ex
        .iter()
        .find(|e| e.to_alias == "drum_synth")
        .expect("drum_synth explained");
    assert!(!drum.fired);
    assert!(
        matches!(&drum.skip_reason, Some(RouteSkipReason::ModeIneligible { active_mode, route_modes })
            if active_mode == "Keys" && route_modes == &vec!["Drums".to_string()]),
        "drum_synth skipped as mode-ineligible: {:?}",
        drum.skip_reason
    );

    let ch5 = ex
        .iter()
        .find(|e| e.to_alias == "ch5_only")
        .expect("ch5_only explained");
    assert!(!ch5.fired);
    assert!(
        matches!(
            &ch5.skip_reason,
            Some(RouteSkipReason::FilterMismatch {
                dimension: FilterDimension::Channel
            })
        ),
        "ch5_only skipped on channel filter: {:?}",
        ch5.skip_reason
    );
}

#[test]
fn explain_unknown_source_is_empty() {
    let engine = RouteEngine::compile(&[route("pads", "absynth")]);
    assert!(
        engine
            .explain_route_match("ghost", &note_on(0, 60, 64), "Default")
            .is_empty(),
        "no routes for an unknown source → empty explanation (tool reports 'no routes')"
    );
}

#[test]
fn explain_matches_dispatch_decision() {
    // Invariant: a route is reported `fired` in explain iff it appears in
    // route_destinations for the same inputs. Guards against explain/
    // dispatch drift.
    let routes = vec![
        route("pads", "absynth"),
        RouteConfig {
            modes: vec!["Drums".into()],
            ..route("pads", "drum_synth")
        },
        route_with_filter(
            "pads",
            "ch5_only",
            SignalFilter {
                channels: vec![5],
                ..empty_filter()
            },
        ),
    ];
    let engine = RouteEngine::compile(&routes);
    let event = note_on(0, 60, 64);
    let mode = "Keys";

    let dispatched: std::collections::BTreeSet<String> = engine
        .route_destinations_midi("pads", &event, mode)
        .into_iter()
        .map(|o| o.to_alias)
        .collect();
    let explained_fired: std::collections::BTreeSet<String> = engine
        .explain_route_match("pads", &event, mode)
        .into_iter()
        .filter(|e| e.fired)
        .map(|e| e.to_alias)
        .collect();
    assert_eq!(
        dispatched, explained_fired,
        "explain `fired` set must equal route_destinations output set"
    );
}

// ── ADR-039 #1759: protocol-tagged `route_destinations(&ProtocolEvent)` shim ──
//
// The byte core `route_destinations_midi` is exercised exhaustively above.
// These cover the new tag-dispatch shim: Input delegates to the core; Osc/Dmx
// route to nothing (no inbound listener until 039-A/C); an Input with no MIDI
// wire form routes to nothing.

use conductor_core::events::{
    DMX_UNIVERSE_SIZE, DmxFrame, DmxInbound, InputEvent, OscInbound, ProtocolEvent,
};
use std::time::Instant;

#[test]
fn shim_input_variant_matches_the_byte_core() {
    let engine = RouteEngine::compile(&[route("pads", "absynth")]);
    // PadPressed(ch0, note60, vel64) reconstructs to note_on(0, 60, 64).
    let pe = ProtocolEvent::Input(InputEvent::PadPressed {
        pad: 60,
        velocity: 64,
        channel: Some(0),
        time: Instant::now(),
    });
    assert_eq!(
        aliases(&engine.route_destinations("pads", &pe, "Default")),
        aliases(&engine.route_destinations_midi("pads", &note_on(0, 60, 64), "Default")),
        "shim Input path must match the byte core for the same event"
    );
    assert_eq!(
        aliases(&engine.route_destinations("pads", &pe, "Default")),
        vec!["absynth".to_string()]
    );
}

#[test]
fn shim_osc_and_dmx_variants_route_to_nothing() {
    let engine = RouteEngine::compile(&[route("pads", "absynth"), route("eos", "console")]);
    let osc = ProtocolEvent::Osc(OscInbound {
        address: "/1/fader1".to_string(),
        args: vec![],
        time: Instant::now(),
    });
    // ADR-039-A (#1361) landed OSC inbound routing, but these routes carry NO
    // transform. A no-transform route can't forward an OSC event (OSC has no
    // MIDI wire bytes to pass through), so it skips — producing nothing — rather
    // than emitting an empty MIDI message. (A real OSC route needs an
    // `OscToMidi`/`OscToArtNet` transform; see `osc_route_tests`.)
    assert!(engine.route_destinations("eos", &osc, "Default").is_empty());
    assert!(
        engine
            .route_destinations("pads", &osc, "Default")
            .is_empty()
    );

    // Art-Net (Dmx) inbound still has no consumer (ADR-039-C) — empty.
    let dmx = ProtocolEvent::Dmx(DmxFrame::new(DmxInbound {
        universe: 0,
        channels: [0u8; DMX_UNIVERSE_SIZE],
        time: Instant::now(),
    }));
    assert!(
        engine
            .route_destinations("pads", &dmx, "Default")
            .is_empty()
    );
}

#[test]
fn shim_input_without_wire_bytes_routes_to_nothing() {
    let engine = RouteEngine::compile(&[route("pads", "absynth")]);
    // EncoderTurned has no MIDI wire form (extract_raw_midi → None).
    let pe = ProtocolEvent::Input(InputEvent::EncoderTurned {
        encoder: 1,
        value: 64,
        channel: Some(0),
        analog: None,
        time: Instant::now(),
    });
    assert!(engine.route_destinations("pads", &pe, "Default").is_empty());
}

#[test]
fn shim_input_unknown_source_routes_to_nothing() {
    // Copilot review: the shim short-circuits on an unknown source before
    // reconstructing wire bytes (preserving the byte-core's allocation-free
    // unknown-source property). Behaviourally: unknown source → empty.
    let engine = RouteEngine::compile(&[route("pads", "absynth")]);
    let pe = ProtocolEvent::Input(InputEvent::PadPressed {
        pad: 60,
        velocity: 64,
        channel: Some(0),
        time: Instant::now(),
    });
    assert!(
        engine
            .route_destinations("ghost", &pe, "Default")
            .is_empty()
    );
    // Sanity: the known source still routes.
    assert_eq!(
        aliases(&engine.route_destinations("pads", &pe, "Default")),
        vec!["absynth".to_string()]
    );
}

// ── ADR-039-B (#1762): structured HID transform routing (spec §6.2.1) ──
//
// HidToArtNet is the first STRUCTURED transform: it reads the original
// `InputEvent` threaded via `RouteEvalContext`, because the lossy MIDI byte
// form is ambiguous for gamepad (button 128 → note 0). These tests prove the
// dispatch fires through the generalized route path on a real gamepad event,
// and skips when only bytes are available.

fn hid_to_artnet_route(from: &str, to: &str, trigger: &str, channel: u16) -> RouteConfig {
    let mut trigger_to_channel = std::collections::HashMap::new();
    trigger_to_channel.insert(trigger.to_string(), channel);
    RouteConfig {
        from: from.into(),
        to: to.into(),
        transform: Some(SignalTransform::HidToArtNet { trigger_to_channel }),
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    }
}

/// A gamepad button event: `channel: None` (vs MIDI's `Some`) and `pad` in the
/// HID range 128-255 (SOUTH = 128).
fn gamepad_button(pad: u8, velocity: u8) -> conductor_core::events::InputEvent {
    conductor_core::events::InputEvent::PadPressed {
        pad,
        velocity,
        channel: None,
        time: std::time::Instant::now(),
    }
}

#[test]
fn hid_to_artnet_is_no_longer_excluded_at_compile() {
    // #1762 un-excludes HidToArtNet now that the structured-event seam exists.
    let engine = RouteEngine::compile(&[hid_to_artnet_route("gamepad", "dmx_out", "south", 5)]);
    assert!(
        engine.excluded_routes().is_empty(),
        "HidToArtNet must compile (not be excluded): {:?}",
        engine.excluded_routes()
    );
}

#[test]
fn hid_to_artnet_route_fires_on_structured_gamepad_event() {
    // The acceptance shape: a gamepad button routes to an Art-Net output through
    // the generalized route path, via the threaded structured event.
    let engine = RouteEngine::compile(&[hid_to_artnet_route("gamepad", "dmx_out", "south", 5)]);
    let south = conductor_core::events::ProtocolEvent::Input(gamepad_button(128, 100));

    let outs = engine.route_destinations("gamepad", &south, "Default");
    assert_eq!(
        outs.len(),
        1,
        "south press should fire the HidToArtNet route"
    );
    assert_eq!(outs[0].to_alias, "dmx_out");
    assert_eq!(outs[0].kind, RouteOutputKind::ArtNet);
    // DmxUpdate{channel:5,value} serialized big-endian: [chan_hi, chan_lo, value].
    assert_eq!(outs[0].bytes.len(), 3);
    assert_eq!(outs[0].bytes[0], 0, "channel high byte");
    assert_eq!(outs[0].bytes[1], 5, "channel low byte");
    assert!(
        outs[0].bytes[2] > 0,
        "velocity 100 scales to a non-zero DMX level"
    );
}

#[test]
fn hid_to_artnet_skips_without_a_structured_event() {
    // The byte-only entry (`route_destinations_midi`) has no structured event,
    // so HidToArtNet cannot fire — it must NOT be driven by the lossy/ambiguous
    // byte serialization (button 128 collides with MIDI note 0).
    let engine = RouteEngine::compile(&[hid_to_artnet_route("gamepad", "dmx_out", "south", 5)]);
    let outs = engine.route_destinations_midi("gamepad", &note_on(0, 0, 100), "Default");
    assert!(
        outs.is_empty(),
        "HidToArtNet must skip without the structured event, got {outs:?}"
    );
}

#[test]
fn hid_to_artnet_skips_unmapped_trigger() {
    // A gamepad button absent from `trigger_to_channel` yields no output.
    let engine = RouteEngine::compile(&[hid_to_artnet_route("gamepad", "dmx_out", "south", 5)]);
    let north = conductor_core::events::ProtocolEvent::Input(gamepad_button(131, 100));
    let outs = engine.route_destinations("gamepad", &north, "Default");
    assert!(outs.is_empty(), "unmapped trigger (north) should not fire");
}

// ── ADR-039-B (#1762 step 2): HidToMidi structured transform ──

fn hid_to_midi_route(from: &str, to: &str, trigger: &str, cc: u8, channel: u8) -> RouteConfig {
    let mut trigger_to_cc = std::collections::HashMap::new();
    trigger_to_cc.insert(trigger.to_string(), cc);
    RouteConfig {
        from: from.into(),
        to: to.into(),
        transform: Some(SignalTransform::HidToMidi {
            trigger_to_cc,
            channel,
        }),
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    }
}

#[test]
fn hid_to_midi_is_not_excluded_at_compile() {
    let engine = RouteEngine::compile(&[hid_to_midi_route("gamepad", "synth", "south", 20, 0)]);
    assert!(
        engine.excluded_routes().is_empty(),
        "HidToMidi must compile (not be excluded): {:?}",
        engine.excluded_routes()
    );
}

#[test]
fn hid_to_midi_route_fires_on_structured_gamepad_event() {
    // A gamepad button routes to a MIDI output through the generalized route
    // path, via the threaded structured event — emitting a CC message.
    let engine = RouteEngine::compile(&[hid_to_midi_route("gamepad", "synth", "south", 20, 2)]);
    let south = conductor_core::events::ProtocolEvent::Input(gamepad_button(128, 100));

    let outs = engine.route_destinations("gamepad", &south, "Default");
    assert_eq!(outs.len(), 1, "south press should fire the HidToMidi route");
    assert_eq!(outs[0].to_alias, "synth");
    assert_eq!(outs[0].kind, RouteOutputKind::Midi);
    // CC: [0xB0 | channel, cc, value]; the 7-bit gamepad value passes through.
    assert_eq!(outs[0].bytes, vec![0xB0 | 2, 20, 100]);
}

#[test]
fn hid_to_midi_skips_without_a_structured_event() {
    // Byte-only entry has no structured event ⇒ HidToMidi cannot fire (the
    // lossy byte form must not drive it: button 128 vs MIDI note 0).
    let engine = RouteEngine::compile(&[hid_to_midi_route("gamepad", "synth", "south", 20, 0)]);
    let outs = engine.route_destinations_midi("gamepad", &note_on(0, 0, 100), "Default");
    assert!(
        outs.is_empty(),
        "HidToMidi must skip without the structured event, got {outs:?}"
    );
}

// ── ADR-039-B (#1762 step 3): HidToOsc structured transform ──

fn hid_to_osc_route(from: &str, to: &str, trigger: &str, address: &str) -> RouteConfig {
    let mut trigger_to_address = std::collections::HashMap::new();
    trigger_to_address.insert(trigger.to_string(), address.to_string());
    RouteConfig {
        from: from.into(),
        to: to.into(),
        transform: Some(SignalTransform::HidToOsc {
            trigger_to_address,
            value_to_float: true,
        }),
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    }
}

#[test]
fn hid_to_osc_is_not_excluded_at_compile() {
    let engine = RouteEngine::compile(&[hid_to_osc_route("gamepad", "eos", "south", "/pad/a")]);
    assert!(
        engine.excluded_routes().is_empty(),
        "HidToOsc must compile (not be excluded): {:?}",
        engine.excluded_routes()
    );
}

#[test]
fn hid_to_osc_route_fires_on_structured_gamepad_event() {
    // A gamepad button routes to an OSC output through the generalized route
    // path, via the threaded structured event — emitting an encoded OSC packet.
    let engine = RouteEngine::compile(&[hid_to_osc_route("gamepad", "eos", "south", "/pad/a")]);
    let south = conductor_core::events::ProtocolEvent::Input(gamepad_button(128, 127));

    let outs = engine.route_destinations("gamepad", &south, "Default");
    assert_eq!(outs.len(), 1, "south press should fire the HidToOsc route");
    assert_eq!(outs[0].to_alias, "eos");
    assert_eq!(outs[0].kind, RouteOutputKind::Osc);
    // Decode the OSC packet to confirm address + normalized float arg.
    let pkt = rosc::decoder::decode_udp(&outs[0].bytes)
        .expect("valid OSC")
        .1;
    match pkt {
        rosc::OscPacket::Message(m) => {
            assert_eq!(m.addr, "/pad/a");
            assert_eq!(
                m.args,
                vec![rosc::OscType::Float(1.0)],
                "velocity 127 → 1.0"
            );
        }
        other => panic!("expected OscMessage, got {other:?}"),
    }
}

#[test]
fn hid_to_osc_skips_without_a_structured_event() {
    let engine = RouteEngine::compile(&[hid_to_osc_route("gamepad", "eos", "south", "/pad/a")]);
    let outs = engine.route_destinations_midi("gamepad", &note_on(0, 0, 100), "Default");
    assert!(
        outs.is_empty(),
        "HidToOsc must skip without the structured event, got {outs:?}"
    );
}
