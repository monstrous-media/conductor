// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Route Engine — runtime evaluation of the signal routing graph
//! (ADR-031 § 4.4 / Phase 2B).
//!
//! Plugs in as a **9th stage, after the post-#1118 8-stage rule-engine
//! matcher** (per spec § 4.5 + ADR § D2). When the 8-stage rule
//! matcher doesn't match an input event, the daemon's event pump
//! asks this engine which routes (if any) want to forward the event
//! to which destination connectors. Mode-independent; fan-out by
//! default.
//!
//! Phase 2B scope (this module): route **construction + lookup**,
//! `filter_matches()` over the MIDI filter dimensions, and
//! same-protocol `MidiTransform` application in `route_destinations()`.
//! Disabled, cross-protocol-transform, and OSC-filter routes are
//! dropped at compile time and reported via
//! [`RouteEngine::excluded_routes`].
//!
//! Deferred beyond Phase 2B:
//!   - § 4.5 stage-9 integration into the `EngineManager` event pump
//!     (the call site that actually sends routed bytes to ports)
//!   - § 4.7 MCP route-management tools
//!   - Phase 5: cross-protocol transform runtime + OSC event matching
//!     (a route whose filter sets `osc_address_prefix` is excluded at
//!     compile time until then — see `compile`).

use crate::midi_bytes::extract_raw_midi;
use conductor_core::config::types::{RouteConfig, SignalFilter, SignalTransform};
use conductor_core::events::{InputEvent, OscInbound, ProtocolEvent};
use std::collections::HashMap;

/// Compiled filter for runtime evaluation — the owned, hot-path form
/// of a `SignalFilter` (no Arc/Borrow dance per event).
///
/// It deliberately carries only the **MIDI** filter dimensions, not
/// `SignalFilter::osc_address_prefix`: a route whose filter sets that
/// field is excluded at compile time (`ExclusionReason::OscFilterUnsupported`)
/// since no MIDI event can satisfy an OSC-domain constraint, so a
/// `CompiledFilter` that actually reaches the runtime never has one.
/// Keeping the field here would be permanently-`None` dead state
/// (Council review on PR #1175 finding #2).
#[derive(Debug, Clone)]
pub struct CompiledFilter {
    pub message_types: Vec<conductor_core::config::MidiMessageType>,
    pub channels: Vec<u8>,
    /// `(min, max)` CC numbers — **inclusive on both ends**. Only
    /// constrains CC events; other message types pass through.
    pub cc_range: Option<(u8, u8)>,
    /// `(min, max)` note numbers — **inclusive on both ends**. Only
    /// constrains Note on/off events; other message types pass through.
    pub note_range: Option<(u8, u8)>,
}

impl CompiledFilter {
    /// True when every dimension is empty/None — i.e. the filter
    /// constrains nothing and matches every event. An unconstrained
    /// `Some(filter)` must behave identically to `filter: None`
    /// (Council review on PR #1175 finding #1 — the prior code
    /// dropped system messages for an empty filter but forwarded
    /// them for no filter, a behavioral asymmetry).
    fn is_unconstrained(&self) -> bool {
        self.message_types.is_empty()
            && self.channels.is_empty()
            && self.cc_range.is_none()
            && self.note_range.is_none()
    }
}

/// Compiled route for runtime evaluation. Indexed by source alias in
/// `RouteEngine::routes` for O(1) hot-path lookup. Disabled routes
/// are filtered out at compile time so the runtime never has to
/// re-check `enabled`.
#[derive(Debug, Clone)]
pub struct CompiledRoute {
    pub to_alias: String,
    pub filter: Option<CompiledFilter>,
    /// Compiled transform. Typed as `Option<SignalTransform>` to admit
    /// cross-protocol variants (`MidiToOsc` and — when slices 5-7 of
    /// P5 land — `MidiToArtNet`, `HidToArtNet`). `compile()` still
    /// excludes the variants that have NOT yet shipped a runtime
    /// (currently `OscToMidi`, `MidiToArtNet`, `HidToArtNet`); only
    /// `Midi` and `MidiToOsc` reach the runtime today. The original
    /// `Option<MidiTransform>` shape (PR #1175) was correct then —
    /// widened in P5 slice 3 once `MidiToOsc` became executable. The
    /// per-event branch in `route_destinations` dispatches based on
    /// variant.
    pub transform: Option<SignalTransform>,
    pub description: Option<String>,
    /// Mode scope (ADR-036 D1). Empty = fires in all modes; non-empty =
    /// fires only when the active mode is one of these names. Checked per
    /// dispatch against the active mode passed to `route_destinations`.
    pub modes: Vec<String>,
}

/// True if a route with the given mode scope is eligible under `active_mode`.
/// Empty scope = all modes (legacy bare-route behaviour); otherwise the
/// active mode must be listed. (ADR-036 D1.)
fn mode_eligible(modes: &[String], active_mode: &str) -> bool {
    modes.is_empty() || modes.iter().any(|m| m == active_mode)
}

/// One output produced by a route for a given input event (ADR-031
/// § 4.4 / P5 slice 3). `kind` discriminates the destination protocol
/// so stage-9 dispatch can branch: MIDI bytes go through the action
/// executor's `Action::MidiForward` path; OSC packet bytes go
/// directly through `ConnectorRegistry::send_osc`.
///
/// `bytes` is the post-transform output — already the right wire
/// format for `kind`. Stage-9 callers do NOT need to re-encode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteOutput {
    pub to_alias: String,
    pub bytes: Vec<u8>,
    pub kind: RouteOutputKind,
}

/// Wire-protocol discriminator for `RouteOutput`. Mirrors the
/// `SignalTransform` variants that have a runtime today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteOutputKind {
    /// MIDI bytes — go through the existing action executor MIDI
    /// dispatch (`Action::MidiForward`).
    Midi,
    /// OSC packet bytes (already encoded by
    /// `transforms::midi_to_osc::apply`) — go through
    /// `ConnectorRegistry::send_osc` directly.
    Osc,
    /// Art-Net DMX update (channel + value) serialized as 3 bytes —
    /// `[channel_high, channel_low, value]` (channel big-endian u16).
    /// Stage-9 dispatch deserializes and calls
    /// `ConnectorRegistry::send_artnet`, which holds the persistent
    /// 512-channel DMX frame and emits OpDmx UDP packets.
    ///
    /// Why 3 bytes inside the existing `RouteOutput.bytes: Vec<u8>` shape
    /// instead of a new structured payload field: keeps the slice 8
    /// change tightly scoped (no `RouteOutput` shape refactor, no
    /// test-fixture churn). The bytes-as-wire-format invariant was
    /// already strained by OSC (encoded UDP packet, not raw wire); for
    /// Art-Net the "wire format" requires per-connector frame state
    /// (`ArtNetState` in `connector_registry`) that the route engine
    /// doesn't own. The 3-byte carrier is internal — `RouteOutputKind`
    /// is the contract, not the bytes' meaning.
    ArtNet,
}

/// The MIDI filter dimension that rejected an event, for the
/// `conductor_explain_route_match` MCP tool (ADR-036 D5 / Slice 9).
/// Returned by [`filter_match_detail`]; `None` there means the filter
/// matched. Serialised `snake_case` for the tool payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterDimension {
    /// `raw_midi` was empty — no status byte to evaluate.
    Empty,
    /// Status byte `>= 0xF0` (System message): carries no channel and
    /// is not a channel-voice type, so a constrained filter drops it.
    SystemMessage,
    /// The event's channel is not in the filter's `channels` set.
    Channel,
    /// The event's message type is not in the filter's `message_types`.
    MessageType,
    /// A Note event's note number is outside the filter's `note_range`.
    NoteRange,
    /// A CC event's controller number is outside the filter's `cc_range`.
    CcRange,
}

/// Why a candidate route did NOT fire for a given event, in the
/// `conductor_explain_route_match` trace (ADR-036 D5 / Slice 9).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RouteSkipReason {
    /// The route is mode-scoped and the active mode is not in its list.
    ModeIneligible {
        active_mode: String,
        route_modes: Vec<String>,
    },
    /// The route's filter rejected the event on `dimension`.
    FilterMismatch { dimension: FilterDimension },
    /// Mode + filter passed but the transform yielded no output (e.g. a
    /// `MidiToOsc` transform fed a message type it has no mapping for).
    TransformProducedNoOutput,
}

/// One candidate route's evaluation against an event, for the
/// `conductor_explain_route_match` MCP tool. `fired == true` iff the
/// route appears in [`RouteEngine::route_destinations`] for the same
/// inputs — both paths share [`evaluate_route`], so the explanation can
/// never drift from the dispatch decision.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RouteMatchExplanation {
    pub to_alias: String,
    pub modes: Vec<String>,
    pub fired: bool,
    /// `None` when `fired`; otherwise the reason it was skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<RouteSkipReason>,
}

/// Internal result of evaluating one route — either it produced an
/// output (dispatch keeps it) or it was skipped with a reason (explain
/// records it). Shared by [`RouteEngine::dispatch`] and the explain path.
enum RouteEval {
    Fired(RouteOutput),
    Skipped(RouteSkipReason),
}

/// A route excluded at compile time, with the reason. Surfaced via
/// `RouteEngine::excluded_routes()` so the IPC / MCP layer (Phase 2C)
/// can tell the user *which* declared routes are inactive and *why*
/// — a `tracing::warn!` alone is not API-level feedback (Council
/// review on PR #1175 finding #2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedRoute {
    pub from_alias: String,
    pub to_alias: String,
    pub reason: ExclusionReason,
}

/// Why a route didn't make it into the compiled engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExclusionReason {
    /// `enabled = false` in config — user-intentional.
    Disabled,
    /// Declares a cross-protocol transform (`MidiToOsc` / `OscToMidi`
    /// / `MidiToArtNet` / `HidToArtNet`) whose runtime translation is
    /// not yet implemented. Valid declaration; not yet executable.
    ///
    /// The variant name deliberately avoids encoding a project phase
    /// number (Council review on PR #1175) — when cross-protocol
    /// routing ships, this variant simply stops being produced, with
    /// no breaking rename of the public API.
    CrossProtocolTransformUnsupported,
    /// The route's filter sets `osc_address_prefix` — an OSC-domain
    /// constraint. AND-combined with every other dimension, no MIDI
    /// event can satisfy it, so the route is inert in the (MIDI-only)
    /// input pipeline. Excluded at compile time rather than dropped
    /// silently per-event inside `filter_matches` (Council review on
    /// PR #1175 finding #3). Stops being produced once OSC event
    /// matching ships.
    OscFilterUnsupported,
}

/// Routes unmatched events through configured routes (ADR-031 D6).
///
/// Called by the daemon event pump after `CompiledRuleSet.match_event()`
/// returns None — a 9th stage running after the 8-stage rule-engine
/// matcher. Supports fan-out (one input event → multiple route
/// destinations).
///
/// **Pre-validated input**: `RouteEngine` is the *runtime* half of
/// the routing graph and assumes its `&[RouteConfig]` input has
/// already passed `conductor_core::config::validation::validate_routes`
/// (Phase 2A, PR #1161). That validator rejects nonexistent endpoint
/// aliases, self-referencing routes, direct A→B + B→A cycles, and
/// cross-protocol routes without a compatible transform, and warns
/// on overlap/shadowing. The engine therefore does NOT re-check
/// those invariants on the hot path — config that reaches `compile()`
/// is structurally sound by construction.
///
/// **Thread safety**: `RouteEngine` is `Send + Sync` (it owns only
/// `String` / `Vec` / `HashMap` / `Option` of plain data — no `Rc`,
/// no interior mutability). The daemon holds it in
/// `Arc<ArcSwap<RouteEngine>>` for wait-free hot-path reads, which
/// requires `Send + Sync`; the static assertion below is a
/// compile-time canary (memory note from PR #909 — diagnose the
/// invariant break here, not via a misleading downstream error).
#[derive(Debug, Clone, Default)]
pub struct RouteEngine {
    /// Routes indexed by source alias for O(1) lookup. All routes are
    /// post-mapping (ADR-036 Phase 3) — dispatched after the rule-engine
    /// matcher. Disabled / cross-protocol routes are not stored — see
    /// `compile()`.
    routes: HashMap<String, Vec<CompiledRoute>>,
    /// Routes that were dropped at compile time, with reasons.
    /// Read by `excluded_routes()`; never consulted on the hot path.
    excluded: Vec<ExcludedRoute>,
}

// Compile-time canary: RouteEngine MUST stay Send + Sync so the
// daemon can hold it in Arc<ArcSwap<RouteEngine>>. If a future field
// breaks this (e.g. an Rc or a Cell), the error fires HERE with a
// clear cause rather than as a confusing trait-bound error at the
// ArcSwap construction site.
//
// The `Send + Sync` bound is enforced when the compiler type-checks
// `check`'s body — `assert_send_sync::<RouteEngine>()` is a use of the
// turbofish-instantiated fn, and *all* items (including unused nested
// fns) are type-checked. `check` need not run: `let _ = check;` is
// only there to silence the `dead_code` lint, not to trigger the
// assertion. Verified empirically — injecting an `Rc<()>` field makes
// this block fail to compile with E0277. (A bare `assert_send_sync::
// <RouteEngine>();` statement can't be used here: calling a non-const
// fn in a `const` block is itself an error.)
const _: () = {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    fn check() {
        assert_send_sync::<RouteEngine>();
    }
    let _ = check;
};

impl RouteEngine {
    /// Compile routes from config. Routes are excluded at compile time
    /// (never entering the map) when:
    /// - `enabled = false` — the runtime never re-checks the flag.
    /// - the route declares a **cross-protocol transform**
    ///   (`MidiToOsc` / `OscToMidi` / `MidiToArtNet` / `HidToArtNet`).
    ///   Runtime translation lands in Phase 5; the route never enters
    ///   the compiled map, so the per-event hot path never sees a
    ///   non-MIDI transform.
    /// - the route's filter sets **`osc_address_prefix`** — an
    ///   OSC-domain constraint no MIDI event can satisfy. The route is
    ///   inert in the MIDI-only pipeline; excluding it here surfaces it
    ///   via `excluded_routes()` instead of dropping its events
    ///   silently per-event. OSC event matching lands in Phase 5.
    ///
    /// `compile()` is a **pure function** — it produces a `RouteEngine`
    /// with the `excluded` list populated but performs no I/O or
    /// logging (Council review on PR #1175 — no side effects in the
    /// compiler). Callers that want a startup log of excluded routes
    /// call [`RouteEngine::log_exclusions`] explicitly.
    ///
    /// Surviving routes are grouped by `from` for O(1) source-alias
    /// lookup with fan-out (multiple routes per source).
    pub fn compile(routes: &[RouteConfig]) -> Self {
        let mut map: HashMap<String, Vec<CompiledRoute>> = HashMap::new();
        let mut excluded: Vec<ExcludedRoute> = Vec::new();

        for route in routes {
            if !route.enabled {
                excluded.push(ExcludedRoute {
                    from_alias: route.from.clone(),
                    to_alias: route.to.clone(),
                    reason: ExclusionReason::Disabled,
                });
                continue;
            }

            // Admit transforms that have a runtime today; exclude the
            // rest. As each P5 slice ships, the exclusion list shrinks:
            // - P5 slice 1 (#1341): `MidiToOsc` transform pure function
            // - P5 slice 2 (#1344): OSC sender in registry
            // - P5 slice 3+4 (#1347): `MidiToOsc` is now executable —
            //   no longer excluded
            // - P5 slice 5 (#1350): `MidiToArtNet` pure transform
            // - P5 slice 6 (#1353): Art-Net sender in registry
            // - P5 slice 7 (#1354): `HidToArtNet` pure transform
            // - P5 slice 8 (#1357): `MidiToArtNet` is now executable
            //   via stage-9 — no longer excluded.
            // - ADR-039-B (#1762): `HidToArtNet` is now executable — the
            //   route engine threads the structured `InputEvent` to the
            //   transform via `RouteEvalContext` (spec §6.2.1), so the pure
            //   `hid_to_artnet::apply(&InputEvent)` fn has its dispatch path.
            // - `OscToMidi` (ADR-039-A #1361): OSC input listener landed;
            //   admitted below. `OscToArtNet` (Slice 1b #2324) likewise.
            //
            // `compile()` stays a pure function: the exclusion is
            // recorded in `excluded` (structured feedback via
            // `excluded_routes()`) and the caller decides whether to log.
            let compiled_transform: Option<SignalTransform> = match &route.transform {
                None => None,
                Some(SignalTransform::Midi(_))
                | Some(SignalTransform::MidiToOsc { .. })
                | Some(SignalTransform::MidiToArtNet { .. })
                | Some(SignalTransform::HidToArtNet { .. })
                | Some(SignalTransform::HidToMidi { .. })
                | Some(SignalTransform::HidToOsc { .. })
                // ADR-039-A Slice 1 (#1361): OSC input listener has landed, so
                // OscToMidi now has a dispatch path (structured, reads
                // ctx.input.osc()).
                | Some(SignalTransform::OscToMidi { .. })
                // ADR-039-A Slice 1b (#2324): OscToArtNet rides the same
                // structured OSC path, dispatching RouteOutputKind::ArtNet.
                | Some(SignalTransform::OscToArtNet { .. }) => {
                    Some(route.transform.as_ref().expect("matched Some").clone())
                }
                // NB: every `SignalTransform` variant now has a dispatch path
                // (ADR-039-A admitted the last one, `OscToMidi`), so the match is
                // exhaustive. A future variant added without dispatch will fail
                // to compile here — the intended forcing function.
            };

            // A filter constrained on `osc_address_prefix` is
            // OSC-domain — AND-combined with every other dimension, no
            // MIDI event can satisfy it, so the route is inert in the
            // MIDI-only input pipeline. Exclude the whole route at
            // compile time (Council review on PR #1175 finding #3)
            // rather than dropping its events silently per-event in
            // `filter_matches` — `excluded_routes()` then surfaces it
            // as structured feedback. OSC event matching is Phase 5.
            if route
                .filter
                .as_ref()
                .is_some_and(|f| f.osc_address_prefix.is_some())
            {
                excluded.push(ExcludedRoute {
                    from_alias: route.from.clone(),
                    to_alias: route.to.clone(),
                    reason: ExclusionReason::OscFilterUnsupported,
                });
                continue;
            }

            let compiled = CompiledRoute {
                to_alias: route.to.clone(),
                // Normalize an unconstrained `Some(filter)` to `None`
                // at compile time (Council review on PR #1175) — so
                // the per-event hot path never runs an
                // `is_unconstrained()` check. `compile_filter`
                // returns `None` for an all-empty filter.
                filter: route.filter.as_ref().and_then(compile_filter),
                transform: compiled_transform,
                description: route.description.clone(),
                modes: route.modes.clone(),
            };

            // ADR-036 Phase 3: all routes are post-mapping (the `pre_mapping`
            // escape hatch was removed). Dispatched after the rule-engine
            // matcher.
            map.entry(route.from.clone()).or_default().push(compiled);
        }

        Self {
            routes: map,
            excluded,
        }
    }

    /// Routes dropped at compile time, with reasons. Empty in the
    /// common case. Used by the IPC `GetRoutingGraph` command and the
    /// `conductor_list_routes` MCP tool (Phase 2C) to tell the user
    /// which declared routes are inactive — disabled, cross-protocol,
    /// or OSC-filter (both pending Phase 5). Not on the hot path.
    pub fn excluded_routes(&self) -> &[ExcludedRoute] {
        &self.excluded
    }

    /// Emit a one-time `tracing::warn!` per route that was excluded at
    /// compile time because it is not yet *executable* — cross-protocol
    /// transforms and OSC-filter routes. Callers (e.g. `EngineManager`
    /// after `compile()` / `reload_config()`) opt into this — keeping
    /// `compile()` itself pure (Council review on PR #1175). Disabled
    /// routes are intentionally NOT logged: `enabled = false` is a
    /// user-deliberate choice, not a surprise.
    pub fn log_exclusions(&self) {
        for ex in &self.excluded {
            match ex.reason {
                ExclusionReason::Disabled => {}
                ExclusionReason::CrossProtocolTransformUnsupported => {
                    tracing::warn!(
                        target: "route_engine",
                        from_alias = %ex.from_alias,
                        to_alias = %ex.to_alias,
                        "Cross-protocol route excluded from the compiled engine — \
                         this SignalTransform variant is not yet implemented"
                    );
                }
                ExclusionReason::OscFilterUnsupported => {
                    tracing::warn!(
                        target: "route_engine",
                        from_alias = %ex.from_alias,
                        to_alias = %ex.to_alias,
                        "Route excluded from the compiled engine — its filter \
                         sets osc_address_prefix, which no MIDI event can \
                         satisfy; OSC event matching is not yet implemented"
                    );
                }
            }
        }
    }

    /// Hot-path lookup: return `(destination_alias, output_bytes)`
    /// pairs for every enabled route originating at `source_alias`
    /// whose filter (if any) matches the event, with the route's
    /// transform (if any) applied.
    ///
    /// **Allocation profile**: an *unknown* source alias is the cheap
    /// path — the `HashMap` miss returns an empty `Vec` with no
    /// allocation. A *matched* source allocates: one output `Vec<u8>`
    /// per surviving route (either `raw_midi.to_vec()` or the
    /// `MidiTransform::apply` result) plus the outer result `Vec`.
    /// This is unavoidable — each fan-out destination needs its own
    /// owned byte buffer to send downstream.
    ///
    /// Per-route processing:
    /// - **No filter, no transform**: forward `raw_midi` as-is.
    /// - **Filter present**: evaluated via `filter_matches()`; route
    ///   is skipped on no-match.
    /// - **`SignalTransform::Midi(MidiTransform)` present**: bytes
    ///   are transformed via the existing `MidiTransform::apply`
    ///   (ADR-009 Gap 2 / v4.25.0). Routes whose transform produces
    ///   empty bytes (invalid input message) are skipped.
    ///
    /// Cross-protocol transforms are NOT handled here — they are
    /// excluded from the compiled map by `compile()`, so any route
    /// reaching this method has either no transform or a MIDI one.
    ///
    /// ADR-039 #1759 — protocol-tagged route entry point.
    ///
    /// This is the public route API: it accepts a [`ProtocolEvent`] so the
    /// route stage is no longer tied to MIDI byte streams (the prerequisite
    /// that previously blocked `OscToMidi`/`HidToArtNet` — the route engine
    /// "only takes bytes"). It `#[inline]` tag-dispatches:
    ///
    /// - `Input` (MIDI/HID): reconstruct the wire bytes once and delegate to
    ///   the byte-core [`route_destinations_midi`](Self::route_destinations_midi).
    ///   Events with no MIDI wire form (e.g. `EncoderTurned`) route to nothing.
    /// - `Osc` / `Dmx`: no inbound OSC/Art-Net listener exists yet (deferred to
    ///   sub-ADRs 039-A / 039-C), so these route to nothing for now.
    ///
    /// **Hot-path note (perf gate, spec §4.5):** the daemon's per-event hot
    /// path calls [`route_destinations_midi`](Self::route_destinations_midi)
    /// directly with the `raw_midi` it already extracted for dispatch, so this
    /// shim's extraction does NOT run on the hot path — production instructions
    /// are unchanged from pre-refactor `main`. This shim is the API that #1760
    /// (pump rewrite) and the 039-A/C listeners call once they thread a
    /// `ProtocolEvent` to the route stage.
    #[inline]
    pub fn route_destinations(
        &self,
        source_alias: &str,
        event: &ProtocolEvent,
        active_mode: &str,
    ) -> Vec<RouteOutput> {
        match event {
            ProtocolEvent::Input(input_event) => {
                // Preserve the byte-core's "unknown source is allocation-free"
                // property (Copilot review): when no route is registered from
                // this source, skip the wire-byte reconstruction entirely —
                // matters once #1760 callers route through this shim.
                if !self.routes.contains_key(source_alias) {
                    return Vec::new();
                }
                match extract_raw_midi(input_event) {
                    // ADR-039-B (#1762): thread the structured `input_event`
                    // alongside the extracted bytes so structured HID transforms
                    // (HidToArtNet, …) can recover gamepad-native semantics the
                    // lossy byte form drops (spec §6.2.1).
                    Some(raw_midi) => {
                        let ctx = RouteEvalContext {
                            raw_midi: &raw_midi,
                            input: RouteInput::Event(input_event),
                            mode: active_mode,
                        };
                        self.route_destinations_ctx(source_alias, &ctx)
                    }
                    None => Vec::new(),
                }
            }
            // ADR-039-A Slice 1 (#1361): OSC inbound routes through the engine
            // ONLY (never the mapping engine). The decoded `OscInbound` is
            // threaded via `RouteInput::Osc`; there are no MIDI bytes
            // (`raw_midi` empty) and no `InputEvent`. Structured
            // `OscToMidi`/`OscToArtNet` arms read `ctx.input.osc()`.
            // Allocation-free for an unregistered source.
            ProtocolEvent::Osc(osc) => {
                if !self.routes.contains_key(source_alias) {
                    return Vec::new();
                }
                let ctx = RouteEvalContext {
                    raw_midi: &[],
                    input: RouteInput::Osc(osc),
                    mode: active_mode,
                };
                self.route_destinations_ctx(source_alias, &ctx)
            }
            // TODO(ADR-039-C): route Art-Net inbound once the listener lands.
            ProtocolEvent::Dmx(_) => Vec::new(),
        }
    }

    /// MIDI byte-stream route core (ADR-031 §4.4; was `route_destinations`
    /// pre-ADR-039-#1759). The hot path calls this directly with bytes it has
    /// already materialized, so it incurs no extra allocation. The protocol-
    /// tagged [`route_destinations`](Self::route_destinations) shim delegates
    /// here for the `Input` variant.
    ///
    /// Byte-only entry: no structured event is available, so structured
    /// transforms (HidToArtNet) cannot fire (they `Skip`). Callers that DO have
    /// the `InputEvent` (the hot path) should use
    /// [`route_destinations_ctx`](Self::route_destinations_ctx) to make them work.
    pub fn route_destinations_midi(
        &self,
        source_alias: &str,
        raw_midi: &[u8],
        active_mode: &str,
    ) -> Vec<RouteOutput> {
        let ctx = RouteEvalContext {
            raw_midi,
            input: RouteInput::None,
            mode: active_mode,
        };
        self.route_destinations_ctx(source_alias, &ctx)
    }

    /// Route an event carrying BOTH the extracted `raw_midi` and (optionally) the
    /// structured `InputEvent`, via a [`RouteEvalContext`] (ADR-039-B §6.2.1).
    /// The hot path builds the context from the bytes it already extracted plus
    /// a borrow of the live event — zero extra allocation, MIDI byte path
    /// unchanged. Structured HID transforms read `ctx.input.event()`; byte/MIDI arms
    /// ignore it.
    pub fn route_destinations_ctx(
        &self,
        source_alias: &str,
        ctx: &RouteEvalContext,
    ) -> Vec<RouteOutput> {
        Self::dispatch(&self.routes, source_alias, ctx)
    }

    /// Shared per-event route processing: look up the
    /// source alias, drop mode-ineligible routes, apply filter + transform,
    /// and collect outputs. (ADR-036 D1/D4.)
    fn dispatch(
        map: &HashMap<String, Vec<CompiledRoute>>,
        source_alias: &str,
        ctx: &RouteEvalContext,
    ) -> Vec<RouteOutput> {
        let Some(routes) = map.get(source_alias) else {
            return Vec::new();
        };
        routes
            .iter()
            .filter_map(|r| match evaluate_route(r, ctx) {
                RouteEval::Fired(output) => Some(output),
                RouteEval::Skipped(_) => None,
            })
            .collect()
    }

    /// Explain why each candidate route fired or was skipped for a given
    /// `(source_alias, raw_midi, active_mode)`. All routes are post-mapping
    /// (ADR-036 Phase 3). Backs the `conductor_explain_route_match` MCP tool.
    ///
    /// Routes are evaluated through the same [`evaluate_route`] path that
    /// [`route_destinations`](Self::route_destinations) uses, so an entry's
    /// `fired` flag is exactly the dispatch decision. An unknown source
    /// alias yields an empty `Vec` (the tool layer then reports "no routes
    /// from <alias>").
    pub fn explain_route_match(
        &self,
        source_alias: &str,
        raw_midi: &[u8],
        active_mode: &str,
    ) -> Vec<RouteMatchExplanation> {
        let mut out = Vec::new();
        let Some(routes) = self.routes.get(source_alias) else {
            return out;
        };
        // Byte-only explain entry (no structured event): structured HID
        // transforms (HidToArtNet) will report Skipped here, matching the
        // byte-path dispatch decision for a caller without the event.
        let ctx = RouteEvalContext {
            raw_midi,
            input: RouteInput::None,
            mode: active_mode,
        };
        for r in routes {
            let (fired, skip_reason) = match evaluate_route(r, &ctx) {
                RouteEval::Fired(_) => (true, None),
                RouteEval::Skipped(reason) => (false, Some(reason)),
            };
            out.push(RouteMatchExplanation {
                to_alias: r.to_alias.clone(),
                modes: r.modes.clone(),
                fired,
                skip_reason,
            });
        }
        out
    }
}

/// Per-event routing context (ADR-039-B §6.2.1). Carries the already-extracted
/// `raw_midi` (the byte path the MIDI hot path and filters use), the active
/// `mode`, and an OPTIONAL borrow of the structured `event`.
///
/// `event` is `None` for byte-only callers (`route_destinations_midi`,
/// `explain_route_match`, most tests) and `Some(&InputEvent)` when a caller has
/// the live event (the hot path, the `route_destinations(&ProtocolEvent)` shim).
/// It is read ONLY inside the structured-transform arms of [`evaluate_route`]
/// (HidToArtNet, …), which the lossy/ambiguous byte serialization can't serve —
/// so the MIDI byte path is byte-identical and perf-neutral.
///
/// Carried as a struct (not widened args) so future per-event context
/// (timestamps, clock) extends the seam without churning every signature. The
/// structured payload borrows `&InputEvent` (not `&ProtocolEvent`) because the
/// post-#1760 pump unwraps to `InputEvent` before the route stage (a
/// `ProtocolEvent` here would force a hot-path clone). ADR-039-A (#1361)
/// initially landed OSC as a separate `Option` field; Slice 1b (#2324)
/// collapsed the per-protocol fields into [`RouteInput`] per the Council R2
/// mandate — one borrowed enum, so ADR-039-C's Art-Net `Dmx` is a new variant
/// rather than a third field. MIDI/HID byte-path stays zero-cost.
pub struct RouteEvalContext<'a> {
    /// Wire MIDI bytes already extracted from the event (byte path + filters).
    pub raw_midi: &'a [u8],
    /// The structured source payload, when the caller has one (ADR-039-A
    /// Slice 1b, #2324 — Council R2 mandate). One enum, not per-protocol
    /// `Option` fields, so a third structured source (Art-Net `Dmx`,
    /// ADR-039-C) is a new variant — not another field every caller must
    /// initialise. [`RouteInput::None`] ⇒ structured transforms skip.
    pub input: RouteInput<'a>,
    /// Active mode name (ADR-036 D1 mode-scope check).
    pub mode: &'a str,
}

/// Structured source payload for route evaluation (#2324).
///
/// Borrowed, `Copy` — building a context stays allocation-free and the MIDI
/// byte hot path (always [`RouteInput::None`]) is unchanged.
#[derive(Debug, Clone, Copy, Default)]
pub enum RouteInput<'a> {
    /// Byte-only caller (MIDI byte hot path, explain tool) — no structured
    /// payload; structured transform arms skip.
    #[default]
    None,
    /// The unified structured [`InputEvent`] (ADR-039-B §6.2.1) — read by the
    /// structured HID transforms, ignored by byte/MIDI arms.
    Event(&'a InputEvent),
    /// The decoded OSC message (ADR-039-A) — read by the `OscToMidi` /
    /// `OscToArtNet` arms.
    Osc(&'a OscInbound),
}

impl<'a> RouteInput<'a> {
    /// The structured `InputEvent`, when this caller carries one.
    pub fn event(&self) -> Option<&'a InputEvent> {
        match self {
            RouteInput::Event(ev) => Some(ev),
            _ => None,
        }
    }

    /// The decoded OSC message, when this caller carries one.
    pub fn osc(&self) -> Option<&'a OscInbound> {
        match self {
            RouteInput::Osc(osc) => Some(osc),
            _ => None,
        }
    }
}

/// Evaluate a single compiled route against an event: mode scope (ADR-036
/// D1) → filter (against raw MIDI, before transform) → transform. Returns
/// the produced [`RouteOutput`] when the route fires, or a
/// [`RouteSkipReason`] when it doesn't. Single source of truth shared by
/// the hot-path `dispatch` (keeps `Fired`) and `explain_route_match`
/// (records both) so the two can never disagree.
fn evaluate_route(r: &CompiledRoute, ctx: &RouteEvalContext) -> RouteEval {
    let raw_midi = ctx.raw_midi;
    let active_mode = ctx.mode;
    // Mode scope check (ADR-036 D1): skip routes not eligible in the
    // active mode before any filter/transform work.
    if !mode_eligible(&r.modes, active_mode) {
        return RouteEval::Skipped(RouteSkipReason::ModeIneligible {
            active_mode: active_mode.to_string(),
            route_modes: r.modes.clone(),
        });
    }

    // Filter check (always against raw MIDI — the filter applies to the
    // INPUT event, before transform). `filter_match_detail` returns the
    // failing dimension for the explain trace; `None` = matched.
    if let Some(f) = &r.filter
        && let Some(dimension) = filter_match_detail(f, raw_midi)
    {
        return RouteEval::Skipped(RouteSkipReason::FilterMismatch { dimension });
    }

    // Transform application. Variant determines BOTH the output bytes AND
    // the `RouteOutputKind` discriminator so stage-9 dispatch can branch
    // on protocol.
    let (bytes, kind) = match &r.transform {
        None => {
            // Passthrough (no transform) forwards the wire MIDI bytes as-is.
            // ADR-039-A: an OSC-sourced route reaches here with `raw_midi`
            // empty (OSC carries no MIDI bytes); a bare passthrough of OSC is
            // not implemented in Slice 1, so skip rather than emit an empty
            // MIDI message. Harmless for MIDI (its `raw_midi` is never empty
            // when a route fires).
            if raw_midi.is_empty() {
                return RouteEval::Skipped(RouteSkipReason::TransformProducedNoOutput);
            }
            (raw_midi.to_vec(), RouteOutputKind::Midi)
        }
        Some(SignalTransform::Midi(mt)) => {
            let out = mt.apply(raw_midi);
            if out.is_empty() {
                // `apply` returns empty for invalid / truncated input —
                // drop this route's contribution rather than emit empty
                // bytes downstream.
                return RouteEval::Skipped(RouteSkipReason::TransformProducedNoOutput);
            }
            (out, RouteOutputKind::Midi)
        }
        Some(t @ SignalTransform::MidiToOsc { .. }) => {
            // P5 slice 3 — cross-protocol path. The transform returns
            // `None` for inputs that don't match its template fields (e.g.
            // CC bytes when only `note_to_address` is set) OR for invalid
            // MIDI; drop the contribution either way.
            match crate::transforms::midi_to_osc::apply(t, raw_midi) {
                Some(packet) => (packet, RouteOutputKind::Osc),
                None => return RouteEval::Skipped(RouteSkipReason::TransformProducedNoOutput),
            }
        }
        Some(t @ SignalTransform::MidiToArtNet { .. }) => {
            // P5 slice 8 — MIDI → Art-Net. The pure transform returns
            // `None` for inputs not in any mapping table (e.g. NoteOn for a
            // CC-only transform). We serialize the DmxUpdate as 3 bytes —
            // `[channel_high, channel_low, value]` (BE u16 channel + u8
            // value) — and discriminate via `RouteOutputKind::ArtNet` so
            // stage-9 dispatch can deserialize and call `send_artnet`.
            match crate::transforms::midi_to_artnet::apply(t, raw_midi) {
                Some(update) => {
                    let bytes = vec![
                        (update.channel >> 8) as u8,
                        update.channel as u8,
                        update.value,
                    ];
                    (bytes, RouteOutputKind::ArtNet)
                }
                None => return RouteEval::Skipped(RouteSkipReason::TransformProducedNoOutput),
            }
        }
        Some(t @ SignalTransform::HidToArtNet { .. }) => {
            // ADR-039-B (#1762) — HID → Art-Net. STRUCTURED transform: the
            // byte serialization is lossy/ambiguous for gamepad (button 128 →
            // note 0), so this reads the original `InputEvent` from the context
            // (§6.2.1). A byte-only caller (`ctx.input.event()` is `None`) cannot serve
            // it, so it skips. The pure fn returns `None` for non-HID or
            // unmapped-trigger events. Same DmxUpdate→3-byte serialization +
            // `RouteOutputKind::ArtNet` as the MidiToArtNet arm.
            match ctx
                .input
                .event()
                .and_then(|ev| crate::transforms::hid_to_artnet::apply(t, ev))
            {
                Some(update) => {
                    let bytes = vec![
                        (update.channel >> 8) as u8,
                        update.channel as u8,
                        update.value,
                    ];
                    (bytes, RouteOutputKind::ArtNet)
                }
                None => return RouteEval::Skipped(RouteSkipReason::TransformProducedNoOutput),
            }
        }
        Some(t @ SignalTransform::HidToMidi { .. }) => {
            // ADR-039-B (#1762 step 2) — HID → MIDI. STRUCTURED transform like
            // HidToArtNet: reads the original `InputEvent` from the context
            // (§6.2.1), since the lossy byte form can't recover the gamepad
            // trigger. Emits a 3-byte CC; `RouteOutputKind::Midi` routes it
            // through the existing MIDI output dispatch. Skips for a byte-only
            // caller (`ctx.input.event()` is `None`) or an unmapped/ non-gamepad event.
            match ctx
                .input
                .event()
                .and_then(|ev| crate::transforms::hid_to_midi::apply(t, ev))
            {
                Some(bytes) => (bytes, RouteOutputKind::Midi),
                None => return RouteEval::Skipped(RouteSkipReason::TransformProducedNoOutput),
            }
        }
        Some(t @ SignalTransform::HidToOsc { .. }) => {
            // ADR-039-B (#1762 step 3) — HID → OSC. STRUCTURED transform: reads
            // the original `InputEvent` from the context (§6.2.1). Emits an
            // encoded OSC packet; `RouteOutputKind::Osc` routes it through the
            // OSC sender (same as MidiToOsc). Skips for a byte-only caller
            // (`ctx.input.event()` is `None`) or an unmapped / non-gamepad event.
            match ctx
                .input
                .event()
                .and_then(|ev| crate::transforms::hid_to_osc::apply(t, ev))
            {
                Some(packet) => (packet, RouteOutputKind::Osc),
                None => return RouteEval::Skipped(RouteSkipReason::TransformProducedNoOutput),
            }
        }
        Some(t @ SignalTransform::OscToMidi { .. }) => {
            // ADR-039-A Slice 1 (#1361) — OSC → MIDI. STRUCTURED transform: reads
            // the decoded `OscInbound` from the context (no byte re-parse, since
            // OSC has no canonical MIDI wire form). Emits a 3-byte CC/NoteOn;
            // `RouteOutputKind::Midi` routes it through the existing MIDI output
            // dispatch. Skips for a non-OSC caller (`ctx.input.osc()` is `None`) or an
            // unmatched address / out-of-range value / uncoercible arg.
            match ctx
                .input
                .osc()
                .and_then(|o| crate::transforms::osc_to_midi::apply(t, o))
            {
                Some(bytes) => (bytes, RouteOutputKind::Midi),
                None => return RouteEval::Skipped(RouteSkipReason::TransformProducedNoOutput),
            }
        }
        Some(t @ SignalTransform::OscToArtNet { .. }) => {
            // ADR-039-A Slice 1b (#2324) — OSC → Art-Net. STRUCTURED transform
            // reading the decoded `OscInbound` (same as the OscToMidi arm);
            // the DmxUpdate is serialized to the same 3-byte
            // `[channel_high, channel_low, value]` form as the MidiToArtNet /
            // HidToArtNet arms and discriminated via `RouteOutputKind::ArtNet`.
            // Skips for a non-OSC caller (`ctx.input.osc()` is `None`), an unmatched
            // address, an out-of-universe `{dmx}` capture, or an uncoercible
            // arg.
            match ctx
                .input
                .osc()
                .and_then(|o| crate::transforms::osc_to_artnet::apply(t, o))
            {
                Some(update) => {
                    let bytes = vec![
                        (update.channel >> 8) as u8,
                        update.channel as u8,
                        update.value,
                    ];
                    (bytes, RouteOutputKind::ArtNet)
                }
                None => return RouteEval::Skipped(RouteSkipReason::TransformProducedNoOutput),
            }
        } // Every `SignalTransform` variant is handled above (ADR-039-A Slice 1b
          // wired the last one, `OscToArtNet`), so this match is exhaustive. A
          // future variant added without a dispatch arm will fail to compile
          // here — intentional.
    };

    RouteEval::Fired(RouteOutput {
        to_alias: r.to_alias.clone(),
        bytes,
        kind,
    })
}

/// Why a route output was dropped by the re-entrancy guard (ADR-036 D4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReentrancyError {
    /// The source alias was already visited in this dispatch chain — a
    /// cycle (e.g. A→B→A). Carries the offending alias.
    CycleDetected(String),
    /// The chain reached `max_depth` hops without cycling — defends against
    /// long fan-out chains the static A→B+B→A validator can't catch.
    DepthExceeded(usize),
}

/// Tracks which source aliases an event has already visited while a single
/// input event fans out through chained routes (a route's `to` can be
/// another route's `from`). Bounds the chain to catch cycles and runaway
/// depth without a full graph-cycle analysis (ADR-036 D4.3 / spec § 4.5).
///
/// Constructed per dispatch chain with `max_depth` from
/// `advanced_settings.max_route_depth`. The event pump wiring lands in
/// Slice 7; this is the reusable guard the pump will drive.
///
/// **Cloning (Slice 7 contract)**: a guard instance models a single
/// linear chain, not the whole dispatch tree. For fan-out — one source
/// with multiple destination routes — the caller MUST clone the guard
/// before descending into each branch. Sharing one guard across siblings
/// would make a legitimate diamond (A→{B,C}, B→D, C→D) report a false
/// `CycleDetected("D")` on the second branch, since D was already visited
/// on the first. Clone-per-branch keeps each path's visited-set
/// independent. (Copilot review on PR #1685.)
#[derive(Debug, Clone)]
pub struct DispatchGuard {
    visited: Vec<String>,
    max_depth: usize,
}

impl DispatchGuard {
    pub fn new(max_depth: usize) -> Self {
        Self {
            visited: Vec::new(),
            max_depth,
        }
    }

    /// Record entry into `alias`. Returns `Err` if the alias was already
    /// visited (cycle) or the chain is at `max_depth` (too deep). On `Ok`
    /// the alias is recorded; the caller proceeds with that hop's routes.
    ///
    /// The cycle check runs first: when a repeat alias arrives at max
    /// depth, `CycleDetected` is the more actionable diagnosis than
    /// `DepthExceeded` (Copilot review on PR #1685).
    pub fn enter(&mut self, alias: &str) -> Result<(), ReentrancyError> {
        if self.visited.iter().any(|a| a == alias) {
            return Err(ReentrancyError::CycleDetected(alias.to_string()));
        }
        if self.visited.len() >= self.max_depth {
            return Err(ReentrancyError::DepthExceeded(self.max_depth));
        }
        self.visited.push(alias.to_string());
        Ok(())
    }
}

/// Per-event filter evaluation. All populated filter dimensions
/// combine with AND.
///
/// **Invariant**: `compile()` normalizes unconstrained filters to
/// `None`, so a `CompiledRoute` carrying `Some(filter)` always has a
/// genuinely-constrained filter. This function is therefore only
/// ever called with constrained filters on the hot path — the
/// `debug_assert!` below documents and (in debug builds) enforces
/// that. There is no runtime unconstrained early-return: the
/// `debug_assert!` fires in debug builds if the invariant is
/// violated (e.g. a future caller bypasses `compile_filter`), but
/// costs nothing in release builds.
///
/// Status byte layout (MIDI):
///   `0x80` NoteOff, `0x90` NoteOn, `0xA0` PolyAftertouch,
///   `0xB0` CC, `0xC0` ProgramChange, `0xD0` Aftertouch
///   (channel pressure), `0xE0` PitchBend. Low nibble = channel (0-15).
///
/// `CompiledFilter` carries only MIDI dimensions — OSC-filter routes
/// are excluded at compile time (`ExclusionReason::OscFilterUnsupported`),
/// so this function never has an OSC constraint to evaluate. Actual
/// OSC event matching is Phase 5.
/// Returns `None` when the filter matches `raw_midi`, or `Some(dimension)`
/// naming the FIRST dimension that rejected the event. The hot path
/// (`evaluate_route`) treats `None` as "matched"; the same call yields the
/// failing dimension for the `conductor_explain_route_match` trace
/// (Slice 9) — one source of truth so explanation and dispatch can never
/// diverge.
fn filter_match_detail(filter: &CompiledFilter, raw_midi: &[u8]) -> Option<FilterDimension> {
    // `compile_filter` normalizes unconstrained filters to `None` —
    // a `CompiledRoute.filter` of `Some(_)` always carries a
    // genuinely-constrained filter, so we don't pay an
    // `is_unconstrained()` check on every event in release builds
    // (Copilot review on PR #1175). The `debug_assert!` documents and
    // catches any invariant slip in debug builds; if a future direct
    // caller bypasses `compile_filter`, the assertion fires loudly.
    debug_assert!(
        !filter.is_unconstrained(),
        "filter_match_detail called with an unconstrained filter — compile() should have normalized it to None"
    );
    if raw_midi.is_empty() {
        return Some(FilterDimension::Empty);
    }
    let status = raw_midi[0];
    // System messages (>= 0xF0) carry no channel nibble and aren't
    // one of the 7 channel-voice types, so they cannot satisfy any
    // populated channel / message-type / range constraint. A
    // constrained filter therefore correctly drops them. (An
    // *unconstrained* filter never reaches here — normalized to None.)
    if status >= 0xF0 {
        return Some(FilterDimension::SystemMessage);
    }
    let high_nibble = status & 0xF0;
    let channel = status & 0x0F;

    // Channel filter
    if !filter.channels.is_empty() && !filter.channels.contains(&channel) {
        return Some(FilterDimension::Channel);
    }

    // Message-type filter.
    //
    // MIDI convention (Council review on PR #1175 finding #4): a
    // `NoteOn` with velocity 0 is semantically a `NoteOff` — many
    // devices use it for running-status efficiency. For message-type
    // classification we honour that: `0x90` with data byte 2 == 0
    // classifies as `NoteOff`, so a `message_types = [NoteOff]`
    // filter correctly matches a zero-velocity NoteOn. (The raw bytes
    // are still forwarded unchanged — only the *classification* for
    // filtering treats it as NoteOff.)
    use conductor_core::config::MidiMessageType;
    let event_msg_type = match high_nibble {
        0x80 => Some(MidiMessageType::NoteOff),
        0x90 => {
            // velocity is data byte 2; absent on a truncated message
            let is_zero_velocity = raw_midi.get(2) == Some(&0);
            if is_zero_velocity {
                Some(MidiMessageType::NoteOff)
            } else {
                Some(MidiMessageType::NoteOn)
            }
        }
        0xA0 => Some(MidiMessageType::PolyAftertouch),
        0xB0 => Some(MidiMessageType::CC),
        0xC0 => Some(MidiMessageType::ProgramChange),
        // 0xD0 is MIDI Channel Pressure — classified as `Aftertouch`
        // (channel-wide aftertouch), the vocabulary the routes
        // validator accepts and the rest of the pipeline uses
        // (event_processor, action_executor, the Aftertouch trigger).
        // `MidiMessageType::ChannelPressure` is validator-rejected as
        // reserved, so it must NOT be produced here (Copilot review on
        // PR #1175).
        0xD0 => Some(MidiMessageType::Aftertouch),
        0xE0 => Some(MidiMessageType::PitchBend),
        _ => None,
    };
    if !filter.message_types.is_empty() {
        match &event_msg_type {
            Some(t) if filter.message_types.contains(t) => {}
            _ => return Some(FilterDimension::MessageType),
        }
    }

    // Note range — only applies to Note events. Non-note events
    // pass through note_range (no false-drop on non-applicable
    // dimension per spec § 4.4).
    if let Some((min, max)) = filter.note_range
        && matches!(high_nibble, 0x80 | 0x90)
    {
        if raw_midi.len() < 2 {
            return Some(FilterDimension::NoteRange);
        }
        let note = raw_midi[1];
        if note < min || note > max {
            return Some(FilterDimension::NoteRange);
        }
    }

    // CC range — only applies to CC events. Non-CC events pass
    // through cc_range.
    if let Some((min, max)) = filter.cc_range
        && high_nibble == 0xB0
    {
        if raw_midi.len() < 2 {
            return Some(FilterDimension::CcRange);
        }
        let cc_num = raw_midi[1];
        if cc_num < min || cc_num > max {
            return Some(FilterDimension::CcRange);
        }
    }

    None
}

/// Compile a config `SignalFilter` into a `CompiledFilter`, returning
/// `None` when the filter constrains nothing.
///
/// Normalizing an unconstrained filter to `None` at compile time
/// (Council review on PR #1175) means the per-event hot path never
/// runs an `is_unconstrained()` check — a `CompiledRoute` either has
/// no filter or a genuinely-constrained one. It also makes an empty
/// `Some(filter)` behave identically to `filter: None` by
/// construction, not by a runtime special-case.
///
/// `SignalFilter::osc_address_prefix` is intentionally not copied:
/// `compile()` excludes any route whose filter sets it *before*
/// calling this function, so the field is always `None` here.
fn compile_filter(f: &SignalFilter) -> Option<CompiledFilter> {
    debug_assert!(
        f.osc_address_prefix.is_none(),
        "compile_filter called with an osc_address_prefix filter — compile() should have excluded the route"
    );
    let compiled = CompiledFilter {
        message_types: f.message_types.clone(),
        channels: f.channels.clone(),
        cc_range: f.cc_range,
        note_range: f.note_range,
    };
    if compiled.is_unconstrained() {
        None
    } else {
        Some(compiled)
    }
}

#[cfg(test)]
mod osc_route_tests {
    //! ADR-039-A Slice 1 (#1361): OSC inbound → route engine → MIDI output.
    use super::*;
    use conductor_core::actions::OscArg;
    use std::time::Instant;

    fn osc_to_midi_route() -> RouteConfig {
        RouteConfig {
            from: "console".to_string(),
            to: "synth".to_string(),
            transform: Some(SignalTransform::OscToMidi {
                address_to_cc: Some("/eos/fader/{cc}".to_string()),
                address_to_note: None,
                channel: Some(0),
            }),
            filter: None,
            enabled: true,
            description: None,
            modes: vec![],
        }
    }

    fn osc_event(address: &str, value: f32) -> ProtocolEvent {
        ProtocolEvent::Osc(OscInbound {
            address: address.to_string(),
            args: vec![OscArg::Float(value)],
            time: Instant::now(),
        })
    }

    #[test]
    fn osc_catch_all_route_produces_midi_via_osctomidi() {
        let engine = RouteEngine::compile(&[osc_to_midi_route()]);
        // OscToMidi must NOT be excluded (it has a dispatch path now).
        assert!(
            engine.excluded_routes().is_empty(),
            "OscToMidi should be admitted, not excluded: {:?}",
            engine.excluded_routes()
        );
        let outs = engine.route_destinations("console", &osc_event("/eos/fader/7", 1.0), "Default");
        assert_eq!(outs.len(), 1, "one MIDI output expected");
        assert_eq!(outs[0].to_alias, "synth");
        assert_eq!(outs[0].bytes, vec![0xB0, 7, 127], "CC#7 value 127 on ch 0");
        assert!(matches!(outs[0].kind, RouteOutputKind::Midi));
    }

    #[test]
    fn osc_unmatched_address_skips_route() {
        let engine = RouteEngine::compile(&[osc_to_midi_route()]);
        let outs = engine.route_destinations("console", &osc_event("/other/1", 0.5), "Default");
        assert!(
            outs.is_empty(),
            "address not matching the template → route skipped"
        );
    }

    #[test]
    fn osc_from_unregistered_source_is_allocation_free_empty() {
        let engine = RouteEngine::compile(&[osc_to_midi_route()]);
        let outs = engine.route_destinations("nope", &osc_event("/eos/fader/7", 1.0), "Default");
        assert!(outs.is_empty(), "no route from this source");
    }

    #[test]
    fn midi_byte_path_unaffected_osc_field_none() {
        // Regression: the MIDI byte-core path passes osc=None; an OSC route is
        // not registered for a MIDI byte source, so byte routing is unchanged.
        let engine = RouteEngine::compile(&[osc_to_midi_route()]);
        let outs = engine.route_destinations_midi("console", &[0x90, 60, 100], "Default");
        assert!(
            outs.is_empty(),
            "OscToMidi route does not fire on raw MIDI input"
        );
    }

    // ── ADR-039-A Slice 1b (#2324): OSC inbound → route engine → Art-Net ──

    fn osc_to_artnet_route() -> RouteConfig {
        RouteConfig {
            from: "console".to_string(),
            to: "dmx".to_string(),
            transform: Some(SignalTransform::OscToArtNet {
                address_to_dmx: "/dmx/{dmx}".to_string(),
            }),
            filter: None,
            enabled: true,
            description: None,
            modes: vec![],
        }
    }

    #[test]
    fn osc_catch_all_route_produces_dmx_via_osctoartnet() {
        let engine = RouteEngine::compile(&[osc_to_artnet_route()]);
        // OscToArtNet must be admitted (Slice 1b dispatch path).
        assert!(
            engine.excluded_routes().is_empty(),
            "OscToArtNet should be admitted, not excluded: {:?}",
            engine.excluded_routes()
        );
        let outs = engine.route_destinations("console", &osc_event("/dmx/300", 1.0), "Default");
        assert_eq!(outs.len(), 1, "one Art-Net output expected");
        assert_eq!(outs[0].to_alias, "dmx");
        // DmxUpdate { channel: 300, value: 255 } serialized BE-u16 + u8,
        // same wire form as the MidiToArtNet / HidToArtNet arms.
        assert_eq!(outs[0].bytes, vec![0x01, 0x2C, 255]);
        assert!(matches!(outs[0].kind, RouteOutputKind::ArtNet));
    }

    #[test]
    fn osc_to_artnet_out_of_universe_channel_skips() {
        let engine = RouteEngine::compile(&[osc_to_artnet_route()]);
        let outs = engine.route_destinations("console", &osc_event("/dmx/513", 1.0), "Default");
        assert!(outs.is_empty(), "channel above the DMX universe → skip");
        let outs = engine.route_destinations("console", &osc_event("/dmx/0", 1.0), "Default");
        assert!(outs.is_empty(), "channel 0 below the DMX universe → skip");
    }

    #[test]
    fn osc_to_artnet_route_does_not_fire_on_raw_midi() {
        let engine = RouteEngine::compile(&[osc_to_artnet_route()]);
        let outs = engine.route_destinations_midi("console", &[0xB0, 7, 100], "Default");
        assert!(
            outs.is_empty(),
            "OscToArtNet route does not fire on raw MIDI input"
        );
    }
}
