// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ── Suppress action dispatch during MIDI Learn ───────────────
//
// Pre-fix: pressing a mapped pad while Learn was active both
// captured the event AND dispatched the mapped action (keystroke,
// app launch, volume change). The user expected pad presses during
// Learn to be capture-only — letting the existing mapping fire is
// a strict bug: surprises the user with side effects mid-learning.
//
// Fix sites — two dispatch paths in engine_manager.rs both check
// `midi_learn_active` before dispatching (the legacy
// `process_input_event` path was consolidated into `process_device_event`):
//   1. process_device_event (the unified hot path)
//   2. process_timer_tick (hold dispatch — `check_holds()`
//      produces ProcessedEvent::HoldDetected events that get
//      dispatched to the executor thread)
//
// The tests below pin the suppression on `process_device_event` via
// `mapping_matched` monitor events (only emitted on successful
// dispatch — absence proves the guard fired) plus a regression-guard
// converse with learn inactive. The timer-tick path has a structurally
// identical guard; an integration test for it would need a long-press
// event spawned via check_holds — flagged as a coverage gap rather
// than overstating what's actually tested.

// These two tests originally exercised `process_input_event` (the
// legacy single-device hot path). After consolidation, the same suppression
// guard lives on `process_device_event` and is fed via a synthesised
// `DeviceEvent` — analogous to what the legacy converter task now emits
// for configs without `[[bindings]]`.

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_process_device_event_suppresses_dispatch_when_learn_active_legacy() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_learn_suppression_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    manager.capture_actions = true;
    // Mix mode (index 0) is the default initial mode; note 36 → ModeChange (benign).
    manager.midi_learn_active.store(true, Ordering::SeqCst);

    let mut rx = manager.event_broadcast_tx.subscribe();

    let device_event = DeviceEvent::new(
        DeviceId::raw("default"),
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
        .expect("process_device_event must return Ok — pipeline error would mask suppression");

    assert!(
        !drain_for_mapping_matched(&mut rx),
        "mapping_matched must NOT fire when midi_learn_active is true (#836)"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_process_device_event_dispatches_normally_when_learn_inactive_legacy() {
    // Regression guard: with learn OFF, the same mapping must
    // continue to dispatch as before.
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_learn_suppression_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    manager.capture_actions = true;
    manager.midi_learn_active.store(false, Ordering::SeqCst);

    let mut rx = manager.event_broadcast_tx.subscribe();

    let device_event = DeviceEvent::new(
        DeviceId::raw("default"),
        InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager.process_device_event(device_event).await.expect(
        "process_device_event must return Ok — pipeline error would mask the regression check",
    );

    assert!(
        drain_for_mapping_matched(&mut rx),
        "mapping_matched MUST fire when midi_learn_active is false (regression guard)"
    );
}

/// The mute gate in `process_device_event` (D8) must actually
/// DROP events from a muted device, not merely flip the `InputManager`
/// flag. The pre-existing `test_device_mute_drops_events` only asserted
/// `is_device_enabled` toggled — a regression that ignored the flag in
/// the processing path would still pass it. This exercises the real
/// pipeline: a muted device's note-36 event yields no `mapping_matched`
/// (dispatch suppressed), while an *enabled* device's identical event
/// still flows through and dispatches. Mirrors the learn-suppression
/// tests above; `mapping_matched` only fires on successful dispatch, so
/// its absence proves the mute gate fired.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_process_device_event_drops_muted_device() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_learn_suppression_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    manager.capture_actions = true;
    manager.midi_learn_active.store(false, Ordering::SeqCst);

    // The InputManager is None by default (mute gate skipped). Populate
    // it and mute "pads"; "keys" keeps its default-enabled state.
    let muted = DeviceId::from_alias("pads");
    let enabled = DeviceId::from_alias("keys");
    {
        let mut guard = manager.input_manager.lock().await;
        let mut im = InputManager::new(None, false, InputMode::MidiOnly);
        im.set_device_enabled(&muted, false);
        assert!(!im.is_device_enabled(&muted));
        assert!(im.is_device_enabled(&enabled));
        *guard = Some(im);
    }

    let mut rx = manager.event_broadcast_tx.subscribe();

    // Muted device → the note-36 event must be dropped before dispatch.
    // `mapping_matched` (pushed synchronously on successful dispatch)
    // must NOT fire. We assert on that signal rather than the resulting
    // ModeChange because action execution is async (dispatched to the
    // action_dispatcher task) and is not yet applied when this returns.
    let muted_event = DeviceEvent::new(
        muted.clone(),
        InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(muted_event)
        .await
        .expect("process_device_event must return Ok — a pipeline error would mask the drop");
    assert!(
        !drain_for_mapping_matched(&mut rx),
        "a muted device's event must be dropped — no mapping_matched (#1499)"
    );

    // Enabled device → the identical event still flows through and
    // dispatches (the gate is per-device, not global).
    let enabled_event = DeviceEvent::new(
        enabled.clone(),
        InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(0),
            time: StdInstant::now(),
        },
    );
    manager
        .process_device_event(enabled_event)
        .await
        .expect("process_device_event must return Ok");
    assert!(
        drain_for_mapping_matched(&mut rx),
        "an enabled device's event must still flow through and dispatch (#1499)"
    );
}

// ── Stage-9 route dispatch wiring ──────────────────
//
// `RouteEngine::route_destinations()` is now invoked from
// `process_device_event` after the 8-stage rule matcher misses.
// Previously the engine compiled fine and was hot-swapped
// on config reload but was never called in production — declared
// `[[routes]]` did nothing at runtime. Tests below pin the wiring.

/// An event on a source connector with no
/// matching trigger but a configured route MUST emit a
/// `route_forwarded` monitor event. The previous behavior was
/// silent drop (route_destinations never called in production).
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_route_dispatches_when_no_trigger_matches() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_route_only_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    manager.midi_learn_active.store(false, Ordering::SeqCst);

    let mut rx = manager.event_broadcast_tx.subscribe();

    // PadPressed on "pads" — there's no trigger for note 36, so
    // the 8-stage matcher returns None; stage 9 must dispatch via
    // the "pads → absynth" route declared in the config.
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

    let route_event = drain_for_event_type(&mut rx, "route_forwarded").expect(
        "stage 9 must fire route_forwarded when no trigger matches but a route is configured",
    );

    // Payload shape check — later work enriches with more fields; the
    // base contract is {from, to, bytes_in}.
    let payload = route_event
        .payload
        .as_ref()
        .expect("route_forwarded must carry a payload");
    assert_eq!(
        payload["from"], "pads",
        "payload.from must be the source connector alias"
    );
    assert_eq!(
        payload["to"], "absynth",
        "payload.to must be the destination connector alias"
    );
    assert!(
        payload["bytes_in"].as_u64().unwrap_or(0) >= 3,
        "payload.bytes_in must be the raw MIDI byte count (NoteOn = 3); got: {:?}",
        payload["bytes_in"]
    );
}

/// AC #1 negative case: an event on a source with NO matching
/// route MUST NOT emit `route_forwarded`. Guards against an
/// accidental "fire on every miss" wiring.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_silent_when_no_route_matches() {
    use conductor_core::identity::{DeviceEvent, DeviceId};
    use std::time::Instant as StdInstant;

    let mut manager = create_simulate_manager(create_route_only_test_config()).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);

    let mut rx = manager.event_broadcast_tx.subscribe();

    // Event on a device alias that has NO route configured —
    // stage 9's route_destinations() must return empty, so
    // route_forwarded must not fire.
    let device_event = DeviceEvent::new(
        DeviceId::raw("unrouted_device"),
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
        "route_forwarded must NOT fire when no route matches the source alias"
    );
}

// ── ADR-038 §2: RouteDisposition gate decision (unit) ────────────
//
// These live in-file (not an external integration test) so
// `RouteDisposition` stays `pub(crate)` / pump-internal per its doc
// comment. The behavioural pump tests
// (`test_stage9_*`) exercise the gate end-to-end; these pin the decision
// logic directly.

/// No rule matched → `NoMatch` → routes fire (pre-ADR-038 behaviour).
#[test]
fn route_disposition_no_match_allows_route() {
    let d = RouteDisposition::from_envelope(None);
    assert!(matches!(d, RouteDisposition::NoMatch));
    assert!(d.allows_route());
}

/// (a) `let_through = true` → `LetThrough` carrying the mapping_id → route fires.
#[test]
fn route_disposition_let_through_allows_route_and_preserves_cause() {
    let d = RouteDisposition::from_envelope(Some(&lt_test_envelope(true, Some(7))));
    assert!(
        matches!(d, RouteDisposition::LetThrough(Some(7))),
        "let_through=true must yield LetThrough carrying the mapping_id (cause preserved, not a bool)"
    );
    assert!(d.allows_route());
}

/// (b) `let_through = false` → `Consumed` carrying the mapping_id → route skipped.
#[test]
fn route_disposition_consumed_skips_route_and_preserves_cause() {
    let d = RouteDisposition::from_envelope(Some(&lt_test_envelope(false, Some(7))));
    assert!(matches!(d, RouteDisposition::Consumed(Some(7))));
    assert!(!d.allows_route());
}

/// The gate is a pure function of the envelope's `let_through` flag — never
/// of the action's dispatch outcome. This is what guarantees (c) a slow
/// action doesn't gate the route and (d) a backpressure-dropped action still
/// lets the route proceed (the pump never consults `try_dispatch`'s result).
#[test]
fn route_disposition_is_dispatch_independent() {
    let lt = lt_test_envelope(true, Some(1));
    assert!(RouteDisposition::from_envelope(Some(&lt)).allows_route());
    let consumed = lt_test_envelope(false, Some(1));
    assert!(!RouteDisposition::from_envelope(Some(&consumed)).allows_route());
}

/// ADR-038: the dispatch trace's (matched_mapping, let_through) is
/// derived from the disposition, never a bool — so NoMatch and LetThrough
/// route entries stay distinguishable (and Consumed carries its id too,
/// even though it never reaches the route trace).
#[test]
fn route_disposition_trace_fields() {
    assert_eq!(RouteDisposition::NoMatch.trace_fields(), (None, false));
    assert_eq!(
        RouteDisposition::LetThrough(Some(3)).trace_fields(),
        (Some(3), true)
    );
    assert_eq!(
        RouteDisposition::Consumed(Some(3)).trace_fields(),
        (Some(3), false)
    );
}

/// AC #1 trigger-precedence guard: when a trigger DOES match and consumes
/// (the default `let_through = false`), stage 9 must NOT fire — per ADR-031
/// precedence, stages 1-8 win over stage 9. This asserts the route gate's
/// `RouteDisposition::Consumed` branch (ADR-038 §2) skips routes.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_skipped_when_trigger_matches() {
    use conductor_core::config::types::{
        ActionConfig, ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind, Mapping,
        RouteConfig, Trigger,
    };
    use conductor_core::identity::{DeviceEvent, DeviceId, DeviceMatcher};
    use std::time::Instant as StdInstant;

    // Same source as the happy-path test but ADD a trigger that
    // matches note 36 on "pads". The matched trigger fires its
    // action; stage 9 must NOT also forward via the route.
    let config = Config {
        mcp: Default::default(),
        per_app_modes: None,
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![
            EndpointConfig {
                alias: "pads".to_string(),
                direction: ConnectorDirection::Input,
                protocol: None,
                description: None,
                enabled: true,
                channels: vec![],
                kind: EndpointKind::Matcher {
                    matchers: vec![],
                    input_matchers: vec![],
                    output_matchers: vec![],
                    no_probe: true,
                },
            },
            EndpointConfig {
                alias: "absynth".to_string(),
                direction: ConnectorDirection::Output,
                protocol: Some(ConnectorProtocol::Midi),
                description: None,
                enabled: true,
                channels: vec![],
                kind: EndpointKind::Matcher {
                    input_matchers: Vec::new(),
                    output_matchers: Vec::new(),
                    matchers: vec![DeviceMatcher::ExactName {
                        value: "Absynth".to_string(),
                    }],
                    no_probe: false,
                },
            },
        ],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 36,
                    velocity_min: None,
                    channel: None,
                    device: Some("pads".to_string()),
                },
                action: ActionConfig::ModeChange {
                    mode: "Default".to_string(),
                },
                description: Some("trigger that pre-empts the route".to_string()),
                let_through: false,
            }],
        }],
        global_mappings: vec![],
        advanced_settings: Default::default(),
        logging: None,
        last_selected_mode: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![RouteConfig {
            from: "pads".to_string(),
            to: "absynth".to_string(),
            transform: None,
            filter: None,
            enabled: true,
            description: None,
            modes: Vec::new(),
        }],
    };

    let mut manager = create_simulate_manager(config).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    manager.capture_actions = true;

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

    // Two assertions on the same drained stream — collect once,
    // then check both. Order matters: mapping_matched fires for
    // the trigger; route_forwarded must NOT be present.
    let mut saw_mapping_matched = false;
    let mut saw_route_forwarded = false;
    while let Ok(ev) = rx.try_recv() {
        match ev.event_type.as_str() {
            "mapping_matched" => saw_mapping_matched = true,
            "route_forwarded" => saw_route_forwarded = true,
            _ => {}
        }
    }
    assert!(
        saw_mapping_matched,
        "mapping_matched MUST fire when a trigger matches (regression guard)"
    );
    assert!(
        !saw_route_forwarded,
        "route_forwarded MUST NOT fire when a trigger matched first — \
             per ADR-031 precedence, stages 1-8 win over stage 9"
    );
}

/// ADR-038 §2 acceptance (a): when a trigger matches with `let_through =
/// true`, the action fires (`mapping_matched`) AND the post-mapping route
/// ALSO fires (`route_forwarded`) — the `RouteDisposition::LetThrough`
/// branch lets the event continue to the route stage instead of swallowing
/// it. Mirror of `test_stage9_skipped_when_trigger_matches` with the flag
/// flipped.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)]
async fn test_stage9_fires_when_trigger_lets_through() {
    use conductor_core::config::types::{
        ActionConfig, ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind, Mapping,
        RouteConfig, Trigger,
    };
    use conductor_core::identity::{DeviceEvent, DeviceId, DeviceMatcher};
    use std::time::Instant as StdInstant;

    let config = Config {
        mcp: Default::default(),
        per_app_modes: None,
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![
            EndpointConfig {
                alias: "pads".to_string(),
                direction: ConnectorDirection::Input,
                protocol: None,
                description: None,
                enabled: true,
                channels: vec![],
                kind: EndpointKind::Matcher {
                    matchers: vec![],
                    input_matchers: vec![],
                    output_matchers: vec![],
                    no_probe: true,
                },
            },
            EndpointConfig {
                alias: "absynth".to_string(),
                direction: ConnectorDirection::Output,
                protocol: Some(ConnectorProtocol::Midi),
                description: None,
                enabled: true,
                channels: vec![],
                kind: EndpointKind::Matcher {
                    input_matchers: Vec::new(),
                    output_matchers: Vec::new(),
                    matchers: vec![DeviceMatcher::ExactName {
                        value: "Absynth".to_string(),
                    }],
                    no_probe: false,
                },
            },
        ],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 36,
                    velocity_min: None,
                    channel: None,
                    device: Some("pads".to_string()),
                },
                action: ActionConfig::ModeChange {
                    mode: "Default".to_string(),
                },
                description: Some("observe-and-let-through".to_string()),
                // The one difference from the precedence test: let the event
                // continue to the route stage after firing the action.
                let_through: true,
            }],
        }],
        global_mappings: vec![],
        advanced_settings: Default::default(),
        logging: None,
        last_selected_mode: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![RouteConfig {
            from: "pads".to_string(),
            to: "absynth".to_string(),
            transform: None,
            filter: None,
            enabled: true,
            description: None,
            modes: Vec::new(),
        }],
    };

    let mut manager = create_simulate_manager(config).await;
    manager.event_monitor_active.store(true, Ordering::Relaxed);
    manager.capture_actions = true;

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

    let mut saw_mapping_matched = false;
    let mut saw_route_forwarded = false;
    while let Ok(ev) = rx.try_recv() {
        match ev.event_type.as_str() {
            "mapping_matched" => saw_mapping_matched = true,
            "route_forwarded" => saw_route_forwarded = true,
            _ => {}
        }
    }
    assert!(
        saw_mapping_matched,
        "mapping_matched MUST fire — the let-through mapping still runs its action"
    );
    assert!(
        saw_route_forwarded,
        "route_forwarded MUST fire when the matched mapping set let_through=true \
             (RouteDisposition::LetThrough lets the event continue to the route stage)"
    );
}
