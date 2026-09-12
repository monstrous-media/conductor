// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Route validation and route-overlap analysis (ADR-031/036).

use super::*;

pub(super) fn validate_routes(config: &Config, ctx: &mut ValidationCtx) {
    // Routes resolve against the unified `[[endpoints]]` set (ADR-035).
    let endpoint_aliases: HashSet<&str> =
        config.endpoints.iter().map(|e| e.alias.as_str()).collect();

    // Build an `alias → Protocol` map for cross-protocol detection.
    // Connector protocols map 1:1 onto binding-side Protocol (same 4
    // variants — Midi/Hid/Osc/ArtNet).
    use crate::config::protocol::Protocol;
    use crate::config::types::ConnectorProtocol;
    let protocol_for: std::collections::HashMap<&str, Protocol> = config
        .endpoints
        .iter()
        .map(|e| {
            (
                e.alias.as_str(),
                connector_proto_to_proto(e.effective_protocol()),
            )
        })
        .collect();

    // ADR-036 D1: set of declared mode names, for validating
    // each route's `modes` scope references something real.
    let mode_names: HashSet<&str> = config.modes.iter().map(|m| m.name.as_str()).collect();

    // Track forward edges so we can detect A→B + B→A direct cycles.
    let mut forward_edges: HashSet<(&str, &str)> = HashSet::new();

    for (idx, route) in config.routes.iter().enumerate() {
        let path = format!("routes[{}]", idx);

        // Rule 6 (ADR-036 D1): every name in `route.modes` must reference
        // a declared [[modes]] block. A typo or stale name yields a route
        // that can never become active — fail loudly at load.
        for (m_idx, mode_name) in route.modes.iter().enumerate() {
            if !mode_names.contains(mode_name.as_str()) {
                ctx.error(
                    format!("{}.modes[{}]", path, m_idx),
                    format!(
                        "Route mode scope references unknown mode '{}' — must match a declared \
                         [[modes]] block (ADR-036 D1). The route would never fire.",
                        mode_name
                    ),
                );
            }
        }

        // (ADR-036 Phase 3) The route `phase` field was removed — all routes
        // are post-mapping. A lingering `phase = "..."` is rejected at config
        // load (`Config::check_removed_route_phase`), so there is nothing to
        // validate here.

        // Rule 1a: `from` references a known endpoint
        let from_known = endpoint_aliases.contains(route.from.as_str());
        if !from_known {
            ctx.error(
                format!("{}.from", path),
                format!(
                    "Route 'from' references unknown alias '{}' — must match an [[endpoints]] \
                     entry (ADR-035).",
                    route.from
                ),
            );
        }

        // Rule 1b: `to` references a known endpoint
        let to_known = endpoint_aliases.contains(route.to.as_str());
        if !to_known {
            ctx.error(
                format!("{}.to", path),
                format!(
                    "Route 'to' references unknown alias '{}' — must match an [[endpoints]] \
                     entry (ADR-035).",
                    route.to
                ),
            );
        }

        // Rule 2: self-reference
        if route.from == route.to {
            ctx.error(
                path.clone(),
                format!(
                    "Route from '{0}' to '{0}' is a self-loop — pick distinct endpoints \
                     (ADR-031 § 4.3).",
                    route.from
                ),
            );
        }

        // Rule 3: A→B + B→A direct (depth-1) cycle.
        //
        // SCOPE: per ADR-031 spec § 4.3, only direct 2-cycles are
        // detected at config load. Multi-hop cycles (A→B→C→A) are
        // intentionally OUT OF SCOPE for Phase 2A — they'd need a
        // graph walk + cycle search, which has its own cost/complexity
        // trade-offs and would need a separate spec section. The
        // runtime route engine (Phase 2B § 4.5) is the second line of
        // defence: it can break cycles via per-event recursion guards
        // (mirrors `MidiRecursionGuard` from ADR-015 D8). Multi-hop
        // static detection is tracked as a future hardening item.
        //
        // Gate on both endpoints being known + non-self-ref so we don't
        // cascade an extra "cycle" error onto an already-broken route.
        if from_known && to_known && route.from != route.to {
            if forward_edges.contains(&(route.to.as_str(), route.from.as_str())) {
                ctx.error(
                    path.clone(),
                    format!(
                        "Route '{}' → '{}' forms a direct cycle with an earlier '{}' → '{}' route — \
                         would feedback-loop on every event (ADR-031 § 4.3).",
                        route.from, route.to, route.to, route.from
                    ),
                );
            }
            forward_edges.insert((route.from.as_str(), route.to.as_str()));
        }

        // Rule 4: cross-protocol transform compatibility.
        //
        // Three cases per `ExpectedTransform`:
        //   - SameProtocol: no requirement; any transform or None is fine.
        //   - Required(variant): cross-protocol pair with a defined
        //     SignalTransform variant. Route MUST declare that exact
        //     variant; missing or mismatched is an error.
        //   - Unsupported: cross-protocol pair with NO defined variant
        //     in ADR-031 (e.g. HID→OSC, ArtNet→MIDI). Route is rejected
        //     regardless of transform value.
        //
        // (Historically, a prior `Some("MidiToOsc")` fallback let HID→OSC
        // routes silently validate; that gap is now closed.)
        if from_known && to_known {
            let from_proto = protocol_for.get(route.from.as_str()).copied();
            let to_proto = protocol_for.get(route.to.as_str()).copied();
            if let (Some(fp), Some(tp)) = (from_proto, to_proto) {
                match expected_transform_variant(fp, tp) {
                    ExpectedTransform::SameProtocol => {
                        // No transform required; nothing to check here.
                    }
                    ExpectedTransform::Unsupported => {
                        ctx.error(
                            path.clone(),
                            format!(
                                "Route '{}' ({:?}) → '{}' ({:?}): unsupported protocol pair — \
                                 no matching SignalTransform variant exists in ADR-031 for \
                                 this direction. Either re-route through an intermediate \
                                 protocol (e.g. HID→MIDI→OSC via two routes) or wait for \
                                 a future ADR to add the variant.",
                                route.from, fp, route.to, tp
                            ),
                        );
                    }
                    ExpectedTransform::Required(expected) => {
                        match &route.transform {
                            None => {
                                ctx.error(
                                    path.clone(),
                                    format!(
                                        "Cross-protocol route '{}' ({:?}) → '{}' ({:?}) must \
                                         declare `transform.type = \"{}\"` — without one the \
                                         payload bytes are forwarded raw to a wire that doesn't \
                                         speak that protocol (ADR-031 § 4.3).",
                                        route.from, fp, route.to, tp, expected
                                    ),
                                );
                            }
                            Some(t) if transform_variant_name(t) != expected => {
                                ctx.error(
                                    path.clone(),
                                    format!(
                                        "Route '{}' ({:?}) → '{}' ({:?}) declares \
                                         `transform.type = \"{}\"` but the (from_protocol, \
                                         to_protocol) pair requires '{}'. A wrong transform \
                                         variant is a runtime no-op for the protocol gap — \
                                         payloads still hit the wrong wire (ADR-031 § 4.3).",
                                        route.from,
                                        fp,
                                        route.to,
                                        tp,
                                        transform_variant_name(t),
                                        expected
                                    ),
                                );
                            }
                            Some(t) => {
                                // Variant matches expected — validate its
                                // value ranges so out-of-range config is
                                // REJECTED at load, not silently masked at
                                // runtime. Mirrors the
                                // existing CC/channel range checks elsewhere.
                                if let crate::config::types::SignalTransform::HidToMidi {
                                    trigger_to_cc,
                                    channel,
                                } = t
                                {
                                    if *channel > 15 {
                                        ctx.error(
                                            path.clone(),
                                            format!(
                                                "HidToMidi channel {} out of range (must be \
                                                 0-15) on route '{}' → '{}'",
                                                channel, route.from, route.to
                                            ),
                                        );
                                    }
                                    for (trigger, cc) in trigger_to_cc {
                                        if *cc > 127 {
                                            ctx.error(
                                                path.clone(),
                                                format!(
                                                    "HidToMidi CC {} for trigger '{}' out of \
                                                     range (must be 0-127) on route '{}' → '{}'",
                                                    cc, trigger, route.from, route.to
                                                ),
                                            );
                                        }
                                    }
                                }
                                // HidToOsc: OSC addresses must
                                // start with '/' — reject malformed ones at
                                // config-load rather than emit invalid packets.
                                if let crate::config::types::SignalTransform::HidToOsc {
                                    trigger_to_address,
                                    ..
                                } = t
                                {
                                    for (trigger, address) in trigger_to_address {
                                        if !address.starts_with('/') {
                                            ctx.error(
                                                path.clone(),
                                                format!(
                                                    "HidToOsc address '{}' for trigger '{}' is \
                                                     invalid (OSC addresses must start with '/') \
                                                     on route '{}' → '{}'",
                                                    address, trigger, route.from, route.to
                                                ),
                                            );
                                        }
                                    }
                                }
                                // OscToArtNet (ADR-039-A): the
                                // address template must be a valid OSC address
                                // (starts with '/') carrying exactly one
                                // `{dmx}` placeholder — without it the
                                // transform can never extract a channel and
                                // the route would silently never fire.
                                if let crate::config::types::SignalTransform::OscToArtNet {
                                    address_to_dmx,
                                } = t
                                    && (!address_to_dmx.starts_with('/')
                                        || address_to_dmx.matches("{dmx}").count() != 1)
                                {
                                    ctx.error(
                                        path.clone(),
                                        format!(
                                            "OscToArtNet address_to_dmx '{}' is invalid \
                                             (must start with '/' and contain exactly one \
                                             '{{dmx}}' placeholder) on route '{}' → '{}'",
                                            address_to_dmx, route.from, route.to
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }

                // Rule 4b (ADR-039-B §6.2.1): byte-filters on
                // HID-source routes are rejected. A HID event serializes to
                // MIDI bytes lossily (gamepad button 128 → `pad & 0x7F` = MIDI
                // note 0), so a `channels`/`cc_range`/`note_range`/
                // `message_types` filter evaluated against that serialization
                // fires on non-deterministic ghost triggers. V1 allows only
                // catch-all (no-filter) HID routes; structured HID filters are
                // deferred (to be designed with OSC-input routing).
                if fp == crate::config::protocol::Protocol::Hid
                    && route.filter.as_ref().is_some_and(signal_filter_is_active)
                {
                    ctx.error(
                        format!("{}.filter", path),
                        format!(
                            "Route '{}' ({:?}) → '{}' has a byte-filter, but HID-source routes \
                             must be catch-all (no filter): a gamepad event serializes to MIDI \
                             bytes lossily (button 128 → note 0), so the filter would match \
                             non-deterministic ghost triggers (ADR-039-B §6.2.1). Remove the \
                             `[routes.filter]` block; structured HID filters are deferred.",
                            route.from, fp, route.to
                        ),
                    );
                }

                // ADR-039-A: OSC-source routes must currently be catch-all.
                // MIDI byte-filters are meaningless for OSC (there are
                // no MIDI bytes); OSC-address filtering arrives with typed
                // triggers separately. Reject any active filter on an OSC source.
                if fp == crate::config::protocol::Protocol::Osc
                    && route.filter.as_ref().is_some_and(signal_filter_is_active)
                {
                    ctx.error(
                        format!("{}.filter", path),
                        format!(
                            "Route '{}' (Osc) → '{}' has a filter, but OSC-source routes must be \
                             catch-all (no filter) in Slice 1: OSC carries no MIDI bytes, so a \
                             MIDI/HID byte-filter cannot apply. OSC-address filtering arrives with \
                             typed triggers (ADR-039-A Slice 2). Remove the `[routes.filter]` block.",
                            route.from, route.to
                        ),
                    );
                }

                // ADR-039-A D8: cross-protocol feedback-loop guard. An
                // OSC-source route whose MIDI output is ALSO a Conductor MIDI
                // input forms a system-level loop (OSC → OscToMidi → MIDI out →
                // OS/virtual loopback → MIDI in → mapping engine → Action) that
                // the route-only D17 argument cannot otherwise see — the
                // re-entrant bytes carry no OSC provenance, so taint-tracking
                // would never catch them. Fail closed at config-load. Detectable
                // in-app cases: a Bidirectional MIDI target, or a distinct MIDI
                // input endpoint with identical matchers. (Partial-overlap and
                // cross-application loopback are an accepted, documented Phase-A
                // residual — ADR-042.)
                if fp == crate::config::protocol::Protocol::Osc
                    && tp == crate::config::protocol::Protocol::Midi
                    && let Some(to_ep) = config.endpoints.iter().find(|e| e.alias == route.to)
                    && midi_output_is_self_ingested(config, to_ep)
                {
                    ctx.error(
                        format!("{}.to", path),
                        format!(
                            "Route '{}' (Osc) → '{}' targets a MIDI output that Conductor also \
                             ingests as a MIDI input, forming an OSC→MIDI→input feedback loop that \
                             could reach actions (ADR-039-A D8 / ADR-042 D17). Point the route at a \
                             MIDI output Conductor does not also listen on, or split the device into \
                             distinct in/out endpoints.",
                            route.from, route.to
                        ),
                    );
                }
            }
        }

        // Rule 5: cc_range / note_range min > max
        if let Some(filter) = &route.filter {
            if let Some((min, max)) = filter.cc_range
                && min > max
            {
                ctx.error(
                    format!("{}.filter.cc_range", path),
                    format!(
                        "Route filter `cc_range = [{}, {}]` has min > max — would silently \
                         match nothing (per spec § 4.1, same diagnose-class).",
                        min, max
                    ),
                );
            }
            if let Some((min, max)) = filter.note_range
                && min > max
            {
                ctx.error(
                    format!("{}.filter.note_range", path),
                    format!(
                        "Route filter `note_range = [{}, {}]` has min > max — would silently \
                         match nothing (per spec § 4.1, same diagnose-class).",
                        min, max
                    ),
                );
            }

            // Rule 5b: channel values must be 0-15 (parity with
            // `devices[*].channels` per ADR-022).
            for &ch in &filter.channels {
                if ch > 15 {
                    ctx.error(
                        format!("{}.filter.channels", path),
                        format!("Route filter channel {} is out of range (must be 0-15)", ch),
                    );
                }
            }

            // Rule 5c: reject SysEx / ChannelPressure in `message_types`
            // — the input pipeline doesn't emit them yet, so a route with
            // such a filter would silently never match. Mirrors the Raw
            // trigger validator's identical check.
            for mt in &filter.message_types {
                if matches!(
                    mt,
                    crate::config::MidiMessageType::SysEx
                        | crate::config::MidiMessageType::ChannelPressure
                ) {
                    ctx.error(
                        format!("{}.filter.message_types", path),
                        format!(
                            "Route filter message_type '{:?}' is not supported by the current \
                             event pipeline. SysEx and ChannelPressure are reserved for a future \
                             ADR-030 phase; remove them or use NoteOn/NoteOff/CC/ProgramChange/\
                             Aftertouch/PitchBend.",
                            mt
                        ),
                    );
                }
            }
        }
    }

    // ── ADR-031 § 4.3 Phase 2A — overlap warnings (non-fatal) ──
    //
    // Two classes (ADR-031 § 4.3 Phase 2A):
    //   (b) Route shadowed by a specific trigger (Note/CC/...) on the
    //       route's source device — the trigger fires first; the route
    //       only sees what the trigger doesn't intercept.
    //   (c) Exact-duplicate route — same `from`+`to` + same filter shape.
    //       Wasted CPU + same event sent twice.
    warn_route_overlaps(config, ctx);
}

pub(super) fn warn_route_overlaps(config: &Config, ctx: &mut ValidationCtx) {
    use crate::config::types::Trigger;

    if config.routes.is_empty() {
        return;
    }

    // Triggers fire on input events from physical input endpoints. A route
    // whose source is an output-only endpoint (e.g. an OSC output, an
    // MCP-created output, a virtual MIDI port) doesn't emit trigger events,
    // so trigger-shadowing warnings against those routes are false positives.
    // Build the input-capable endpoint-alias set once and gate the scan on it.
    // (ADR-035: input = direction Input or Bidirectional.)
    use crate::config::types::ConnectorDirection;
    let binding_aliases: std::collections::HashSet<&str> = config
        .endpoints
        .iter()
        .filter(|e| {
            matches!(
                e.direction,
                ConnectorDirection::Input | ConnectorDirection::Bidirectional
            )
        })
        .map(|e| e.alias.as_str())
        .collect();

    // Collect every mapping (mode + global) so we can scan once per route.
    let mut all_triggers: Vec<&Trigger> =
        config.global_mappings.iter().map(|m| &m.trigger).collect();
    for mode in &config.modes {
        all_triggers.extend(mode.mappings.iter().map(|m| &m.trigger));
    }

    for (idx, route) in config.routes.iter().enumerate() {
        let path = format!("routes[{}]", idx);
        let from = route.from.as_str();
        let is_binding_source = binding_aliases.contains(from);

        // (a) + (b): scan triggers for source-device overlap, but only
        //     when the route source is a binding alias (triggers don't
        //     fire on connector sources). The route source `X` is
        //     shadowed by any trigger with `device = X` OR
        //     `device = None` (any-device).
        if is_binding_source {
            for trig in &all_triggers {
                let trig_device = trig.device();
                // Borrow-compare to avoid the per-trigger `to_string()`
                // allocation.
                let device_matches =
                    trig_device.is_none() || trig_device.map(|s| s.as_str()) == Some(from);
                if !device_matches {
                    continue;
                }
                ctx.warning(
                    path.clone(),
                    format!(
                        "Route source '{}' overlaps a specific trigger — the specific \
                         rule fires first; the route only sees events that don't match \
                         the trigger (ADR-031 §D11).",
                        from
                    ),
                );
                break;
            }
        }

        // (c): exact-duplicate route — compare against earlier routes.
        //
        // ADR-036 D1 refinement: two routes are duplicates only when their
        // mode scopes overlap. Disjoint mode scopes (e.g. ["Drums"] vs
        // ["Keys"]) never both fire for the same event, so they're
        // legitimate — not a duplicate. An empty scope means "all modes" and
        // overlaps anything. (Phase 3 removed the `phase` axis — all routes
        // are post-mapping.)
        for (prev_idx, prev) in config.routes.iter().enumerate().take(idx) {
            if route_shapes_equal(prev, route) && mode_scopes_overlap(&prev.modes, &route.modes) {
                ctx.warning(
                    path.clone(),
                    format!(
                        "Route is a duplicate of routes[{}] (same from/to/filter/transform, \
                         overlapping mode scope) — events would be forwarded twice. \
                         Either remove one, differentiate their filters, or narrow their modes.",
                        prev_idx
                    ),
                );
                break; // one warning per duplicate
            }
        }
    }
}

/// Routes are "equal in shape" when their `from`, `to`, filter, AND
/// transform all match. `description` is ignored (it's a label, not a
/// behavior). `enabled` is also ignored (toggle is a state change, not
/// a shape change).
///
/// Including transform in the comparison preserves the "different
/// transforms on the same source/dest is legitimate fan-out" intent:
/// two routes with same from/to/filter but DIFFERENT transforms are
/// correctly distinguished and don't false-warn as duplicates.
/// (Historically the implementation excluded transform, which inverted
/// the intended logic.)
/// Two route mode scopes "overlap" when at least one mode could be
/// active for both. An empty scope means "all modes" (legacy bare-route
/// behaviour), so it overlaps any other scope. Two non-empty scopes
/// overlap iff they share at least one mode name. (ADR-036 D1.)
fn mode_scopes_overlap(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    a.iter().any(|m| b.contains(m))
}

fn route_shapes_equal(
    a: &crate::config::types::RouteConfig,
    b: &crate::config::types::RouteConfig,
) -> bool {
    if a.from != b.from || a.to != b.to {
        return false;
    }
    // Compare filter + transform shape via JSON serialization
    // (cheap, exact — handles the nested `MidiTransform` and the
    // tagged `SignalTransform` enum uniformly).
    let af = serde_json::to_value(&a.filter).unwrap_or(serde_json::Value::Null);
    let bf = serde_json::to_value(&b.filter).unwrap_or(serde_json::Value::Null);
    if af != bf {
        return false;
    }
    let at = serde_json::to_value(&a.transform).unwrap_or(serde_json::Value::Null);
    let bt = serde_json::to_value(&b.transform).unwrap_or(serde_json::Value::Null);
    at == bt
}

/// Result of looking up the required `SignalTransform` variant for a
/// (from_protocol, to_protocol) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedTransform {
    /// Same protocol — no transform required (any transform or `None`
    /// is acceptable; the same-protocol pass-through is the canonical case).
    SameProtocol,
    /// Cross-protocol with a defined variant — the route must declare
    /// `transform.type = <variant>` exactly.
    Required(&'static str),
    /// Cross-protocol with NO defined variant in ADR-031 — the route
    /// should be rejected as unsupported regardless of transform.
    /// Returning a "best-guess" variant (the prior behavior) silently
    /// validated nonsense like HID→OSC + transform.type = "MidiToOsc".
    Unsupported,
}

/// Map a `ConnectorProtocol` (the endpoint's wire protocol) to the routing
/// `Protocol` vocabulary. The 4 variants map 1:1. Shared by endpoint-protocol
/// seeding, route validation, and the `HidForward` action validator so they
/// can't drift.
pub(super) fn connector_proto_to_proto(
    p: crate::config::types::ConnectorProtocol,
) -> crate::config::protocol::Protocol {
    use crate::config::protocol::Protocol;
    use crate::config::types::ConnectorProtocol;
    match p {
        ConnectorProtocol::Midi => Protocol::Midi,
        ConnectorProtocol::Hid => Protocol::Hid,
        ConnectorProtocol::Osc => Protocol::Osc,
        ConnectorProtocol::ArtNet => Protocol::ArtNet,
    }
}

/// Whether a `SignalFilter` actually constrains anything. An all-empty
/// filter (`[routes.filter]` with no fields set) is equivalent to no filter
/// — a catch-all — so it does not trip the HID byte-filter ban (Rule 4b).
fn signal_filter_is_active(f: &crate::config::types::SignalFilter) -> bool {
    !f.message_types.is_empty()
        || !f.channels.is_empty()
        || f.cc_range.is_some()
        || f.note_range.is_some()
        || f.osc_address_prefix.is_some()
}

/// ADR-039-A D8: does this MIDI-output endpoint also get ingested as a
/// MIDI input by Conductor? Detects the two config-load-visible self-loop
/// shapes: a Bidirectional MIDI endpoint (both out and in), or a distinct
/// enabled MIDI input/bidirectional endpoint with an identical matcher set
/// (the same physical device wired in and out). Partial-overlap and
/// cross-application loopback are an accepted Phase-A residual.
fn midi_output_is_self_ingested(
    config: &Config,
    to_ep: &crate::config::types::EndpointConfig,
) -> bool {
    use crate::config::types::{ConnectorDirection, ConnectorProtocol, EndpointKind};
    if to_ep.effective_protocol() != ConnectorProtocol::Midi {
        return false;
    }
    // A bidirectional MIDI endpoint used as the route target is itself both the
    // output and an input — a guaranteed loop.
    if to_ep.direction == ConnectorDirection::Bidirectional {
        return true;
    }

    // Virtual MIDI port output: a `MidiVirtualPort`
    // has NO DeviceMatchers — it is matched by `port_name` — so the
    // matcher-signature path below would always miss it (empty signature). Yet a
    // Conductor-created virtual port is the *classic* loopback vector: a route
    // to it loops if any enabled MIDI input/bidir endpoint also targets that
    // port name (another virtual port with the same name, or a matcher that
    // names it). Check that explicitly, by port name.
    if let EndpointKind::MidiVirtualPort { port_name } = &to_ep.kind {
        return config.endpoints.iter().any(|e| {
            e.enabled
                && e.alias != to_ep.alias
                && e.effective_protocol() == ConnectorProtocol::Midi
                && matches!(
                    e.direction,
                    ConnectorDirection::Input | ConnectorDirection::Bidirectional
                )
                && endpoint_targets_port_name(e, port_name)
        });
    }

    let out_sig = endpoint_matcher_signature(to_ep, ConnectorDirection::Output);
    if out_sig.is_empty() {
        return false;
    }
    config.endpoints.iter().any(|e| {
        e.enabled
            && e.alias != to_ep.alias
            && e.effective_protocol() == ConnectorProtocol::Midi
            && matches!(
                e.direction,
                ConnectorDirection::Input | ConnectorDirection::Bidirectional
            )
            && endpoint_matcher_signature(e, ConnectorDirection::Input) == out_sig
    })
}

/// Whether endpoint `e` (an input/bidir MIDI endpoint) would bind a port called
/// `name` — i.e. it is a `MidiVirtualPort` of that exact name, or a `Matcher`
/// whose input-side matchers name it (`ExactName`, or a `NameContains`
/// substring). Used by the D8 virtual-port self-loop check.
fn endpoint_targets_port_name(e: &crate::config::types::EndpointConfig, name: &str) -> bool {
    use crate::config::types::{ConnectorDirection, EndpointKind};
    use crate::identity::DeviceMatcher;
    match &e.kind {
        EndpointKind::MidiVirtualPort { port_name } => port_name == name,
        EndpointKind::Matcher { .. } => e
            .kind
            .effective_matchers(ConnectorDirection::Input)
            .iter()
            .any(|m| match m {
                DeviceMatcher::ExactName { value } => value == name,
                DeviceMatcher::NameContains { value } => name.contains(value.as_str()),
                _ => false,
            }),
        _ => false,
    }
}

/// Sorted debug signatures of an endpoint's effective matchers for `dir`, so
/// two endpoints targeting the same device compare equal regardless of order.
fn endpoint_matcher_signature(
    ep: &crate::config::types::EndpointConfig,
    dir: crate::config::types::ConnectorDirection,
) -> Vec<String> {
    let mut sig: Vec<String> = ep
        .kind
        .effective_matchers(dir)
        .iter()
        .map(|m| format!("{m:?}"))
        .collect();
    sig.sort();
    sig
}

/// Look up the required `SignalTransform` variant name for a
/// (from_protocol, to_protocol) pair. See `ExpectedTransform`.
fn expected_transform_variant(
    from: crate::config::protocol::Protocol,
    to: crate::config::protocol::Protocol,
) -> ExpectedTransform {
    use crate::config::protocol::Protocol;
    if from == to {
        return ExpectedTransform::SameProtocol;
    }
    match (from, to) {
        (Protocol::Midi, Protocol::Osc) => ExpectedTransform::Required("MidiToOsc"),
        (Protocol::Osc, Protocol::Midi) => ExpectedTransform::Required("OscToMidi"),
        (Protocol::Midi, Protocol::ArtNet) => ExpectedTransform::Required("MidiToArtNet"),
        (Protocol::Hid, Protocol::ArtNet) => ExpectedTransform::Required("HidToArtNet"),
        (Protocol::Hid, Protocol::Midi) => ExpectedTransform::Required("HidToMidi"),
        (Protocol::Hid, Protocol::Osc) => ExpectedTransform::Required("HidToOsc"),
        (Protocol::Osc, Protocol::ArtNet) => ExpectedTransform::Required("OscToArtNet"),
        // Pairs without a defined SignalTransform variant (ArtNet→*, etc.)
        // are explicitly Unsupported. A future ADR/slice may
        // add variants; until then the validator must reject rather than guess.
        _ => ExpectedTransform::Unsupported,
    }
}

/// Tag name of a `SignalTransform` variant — used in error messages.
pub(super) fn transform_variant_name(t: &crate::config::types::SignalTransform) -> &'static str {
    use crate::config::types::SignalTransform;
    match t {
        SignalTransform::Midi(_) => "Midi",
        SignalTransform::MidiToOsc { .. } => "MidiToOsc",
        SignalTransform::OscToMidi { .. } => "OscToMidi",
        SignalTransform::MidiToArtNet { .. } => "MidiToArtNet",
        SignalTransform::HidToArtNet { .. } => "HidToArtNet",
        SignalTransform::HidToMidi { .. } => "HidToMidi",
        SignalTransform::HidToOsc { .. } => "HidToOsc",
        SignalTransform::OscToArtNet { .. } => "OscToArtNet",
    }
}

// ────────────────────────────────────────────────────────────────
// Cross-field validation (NEW)
// ────────────────────────────────────────────────────────────────
