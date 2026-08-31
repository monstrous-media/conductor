// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-031 Phase 2A § 4.1–4.2 — `[[routes]]` config block parsing.
//!
//! Smallest atomic TDD slice for ADR-031 P2 (#1142): the new
//! `RouteConfig` / `SignalFilter` / `SignalTransform` types deserialize
//! from TOML into a `Config` struct field `routes: Vec<RouteConfig>`.
//!
//! Spec reference: `docs/signal-routing/ADR-031-implementation-spec.md` §§ 4.1, 4.2.
//!
//! Subsequent slices in this PR will add: validation (4 reject rules +
//! 3 overlap-warning rules per § 4.3), the `RouteEngine` runtime in
//! conductor-daemon (§ 4.4), stage-9 integration into the event pump
//! (§ 4.5), and 4 MCP tools (§ 4.7) with 5-surface LLM-discoverability
//! coverage.

use conductor_core::Config;
use conductor_core::config::MidiMessageType;
use conductor_core::config::types::{RouteConfig, SignalFilter, SignalTransform};

#[test]
fn parses_minimal_route_block() {
    // Per spec § 4.1 defaults: `enabled = true`, no transform, no filter,
    // no description.
    let toml = r#"
[[modes]]
name = "Default"

[[routes]]
from = "pads"
to = "absynth"
"#;
    let cfg: Config = toml::from_str(toml).expect("minimal route parses");
    assert_eq!(cfg.routes.len(), 1);
    let r = &cfg.routes[0];
    assert_eq!(r.from, "pads");
    assert_eq!(r.to, "absynth");
    assert!(r.enabled);
    assert!(r.transform.is_none());
    assert!(r.filter.is_none());
    assert!(r.description.is_none());
}

#[test]
fn parses_route_with_filter_message_types() {
    // SignalFilter.message_types reuses MidiMessageType from ADR-030 P1.
    // This is the spec § 4.1 post-#1139 update — same vocabulary as
    // Trigger::Raw.message_types so users learn one set of names.
    let toml = r#"
[[modes]]
name = "Default"

[[routes]]
from = "pads"
to = "absynth"

[routes.filter]
message_types = ["NoteOn", "NoteOff", "CC"]
channels = [0, 9]
cc_range = [1, 64]
note_range = [36, 96]
"#;
    let cfg: Config = toml::from_str(toml).expect("filter parses");
    let f = cfg.routes[0].filter.as_ref().expect("filter must be Some");
    assert_eq!(f.message_types.len(), 3);
    assert!(f.message_types.contains(&MidiMessageType::NoteOn));
    assert!(f.message_types.contains(&MidiMessageType::NoteOff));
    assert!(f.message_types.contains(&MidiMessageType::CC));
    assert_eq!(f.channels, vec![0, 9]);
    assert_eq!(f.cc_range, Some((1, 64)));
    assert_eq!(f.note_range, Some((36, 96)));
}

#[test]
fn parses_route_with_midi_transform_variant() {
    // SignalTransform::Midi(MidiTransform { ... }) — the existing
    // ADR-009 transform reused as the in-route transform variant.
    let toml = r#"
[[modes]]
name = "Default"

[[routes]]
from = "pads"
to = "absynth"

[routes.transform]
type = "Midi"
channel = 5
"#;
    let cfg: Config = toml::from_str(toml).expect("MidiTransform parses");
    // Slice 9 hardening (Council finding): pin the inner value too
    // — bare `Some(SignalTransform::Midi(_)) => {}` silently passed
    // even when `channel = 5` was dropped on the floor by serde.
    match &cfg.routes[0].transform {
        Some(SignalTransform::Midi(t)) => {
            assert_eq!(
                t.channel,
                Some(5),
                "MidiTransform.channel must round-trip; got: {:?}",
                t.channel
            );
        }
        other => panic!("expected SignalTransform::Midi, got {:?}", other),
    }
}

#[test]
fn parses_route_with_description_and_disabled_flag() {
    let toml = r#"
[[modes]]
name = "Default"

[[routes]]
from = "pads"
to = "absynth"
enabled = false
description = "Pad → Absynth feed (paused while debugging)"
"#;
    let cfg: Config = toml::from_str(toml).expect("description + enabled parses");
    let r = &cfg.routes[0];
    assert!(!r.enabled);
    assert_eq!(
        r.description.as_deref(),
        Some("Pad → Absynth feed (paused while debugging)")
    );
}

#[test]
fn config_with_no_routes_block_has_empty_vec() {
    // Backward compat — every existing config has no [[routes]];
    // the field must default to an empty vec, not be required.
    let toml = r#"
[[modes]]
name = "Default"
"#;
    let cfg: Config = toml::from_str(toml).expect("config without [[routes]] parses");
    assert!(cfg.routes.is_empty());
}

#[test]
fn empty_signal_filter_round_trips() {
    // A SignalFilter with all defaults must round-trip cleanly.
    // This sanity-checks serde defaults match field-by-field absence.
    let f = SignalFilter {
        message_types: vec![],
        channels: vec![],
        cc_range: None,
        note_range: None,
        osc_address_prefix: None,
    };
    let toml = toml::to_string(&f).expect("empty filter serializes");
    // All fields default to empty/None so round-trip should yield
    // an effectively-empty struct.
    let back: SignalFilter = toml::from_str(&toml).expect("round-trips");
    assert!(back.message_types.is_empty());
    assert!(back.channels.is_empty());
    assert!(back.cc_range.is_none());
    assert!(back.note_range.is_none());
    assert!(back.osc_address_prefix.is_none());
}

#[test]
fn parses_route_with_osc_address_prefix_filter() {
    // Council finding on PR #1161: SignalFilter.osc_address_prefix
    // had no test coverage. Pin the round-trip shape so future serde
    // changes don't silently drop the field.
    let toml = r#"
[[modes]]
name = "Default"

[[routes]]
from = "lights-in"
to = "lights-out"

[routes.filter]
osc_address_prefix = "/lights/zone-a/"
"#;
    let cfg: Config = toml::from_str(toml).expect("osc_address_prefix parses");
    let f = cfg.routes[0].filter.as_ref().expect("filter must be Some");
    assert_eq!(f.osc_address_prefix.as_deref(), Some("/lights/zone-a/"));
    // Unrelated fields default to empty/None.
    assert!(f.message_types.is_empty());
    assert!(f.channels.is_empty());
    assert!(f.cc_range.is_none());
    assert!(f.note_range.is_none());
}

#[test]
fn route_config_constructor_smoke() {
    // Pure compile-time check that the type is constructible by hand
    // (used by validator + MCP-tool tests in subsequent slices).
    let _r = RouteConfig {
        from: "pads".into(),
        to: "absynth".into(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    };
}
