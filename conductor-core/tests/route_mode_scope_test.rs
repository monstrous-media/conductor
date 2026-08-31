// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-036 Slice 1 — `RouteConfig` gains `modes` and `phase`.
//!
//! These fields are the schema-layer half of the dual-mechanism collapse
//! (`Trigger::Raw` + bare routes → routes-with-`modes`-and-`phase`). The
//! semantics — auto-lowering, validation, dispatch — land in later slices.
//! This slice only proves that the schema deserializes, defaults correctly,
//! and round-trips through serde.
//!
//! Spec reference: `docs/routing-unification/ADR-036-037-implementation-spec.md`
//! §§ 4.1, 5 (Slice 1). Closes #1659.

use conductor_core::Config;
use conductor_core::config::types::RouteConfig;

#[test]
fn parses_route_with_explicit_modes() {
    // Mode-scoped route: fires only when `Drums` is the active mode.
    let toml = r#"
[[modes]]
name = "Drums"

[[modes]]
name = "Keys"

[[routes]]
from = "pads"
to = "absynth"
modes = ["Drums"]
"#;
    let cfg: Config = toml::from_str(toml).expect("route with modes parses");
    let r = &cfg.routes[0];
    assert_eq!(r.modes, vec!["Drums".to_string()]);
}

#[test]
fn parses_route_with_multiple_modes() {
    let toml = r#"
[[modes]]
name = "Drums"

[[modes]]
name = "Keys"

[[routes]]
from = "pads"
to = "absynth"
modes = ["Drums", "Keys"]
"#;
    let cfg: Config = toml::from_str(toml).expect("route with multi-modes parses");
    let r = &cfg.routes[0];
    assert_eq!(r.modes, vec!["Drums".to_string(), "Keys".to_string()]);
}

#[test]
fn route_modes_defaults_to_empty_vec_when_omitted() {
    // Empty vec means "all modes" — the existing mode-independent behaviour
    // that bare routes have today. Backward compatible.
    let toml = r#"
[[modes]]
name = "Default"

[[routes]]
from = "pads"
to = "absynth"
"#;
    let cfg: Config = toml::from_str(toml).expect("route without modes parses");
    let r = &cfg.routes[0];
    assert!(r.modes.is_empty());
}

#[test]
fn route_with_modes_round_trips_through_serde() {
    let original = RouteConfig {
        from: "pads".into(),
        to: "absynth".into(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: vec!["Drums".into(), "Keys".into()],
    };
    let toml_str = toml::to_string(&original).expect("serialize");
    let parsed: RouteConfig = toml::from_str(&toml_str).expect("deserialize");
    assert_eq!(parsed.from, original.from);
    assert_eq!(parsed.to, original.to);
    assert_eq!(parsed.modes, original.modes);
}

#[test]
fn route_config_constructor_accepts_mode_scope() {
    // Pure compile-time check that downstream construction sites can
    // populate the mode-scope field. (Phase 3 removed `phase`.)
    let _r = RouteConfig {
        from: "pads".into(),
        to: "absynth".into(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: vec!["Drums".into()],
    };
}
