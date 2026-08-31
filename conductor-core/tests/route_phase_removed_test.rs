// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-036 Phase 3 (#1697) — the route `phase` field (and `pre_mapping`) is
//! removed entirely. All routes are post-mapping.
//!
//! Phase 1 (Slice 3) auto-lowered `Raw` to a `pre_mapping` route; Phase 2
//! rejected `Raw` at parse. Phase 3 eliminates the `pre_mapping` escape hatch:
//! the `phase` field no longer exists on `RouteConfig`. Because serde silently
//! ignores unknown fields, `Config::load` performs an explicit check and
//! surfaces a clear, actionable error when a config still sets `phase`.
//!
//! Spec reference:
//! `docs/routing-unification/ADR-036-037-implementation-spec.md` § 6 Phase 3.

use conductor_core::Config;

fn config_with_phase(phase: &str) -> String {
    format!(
        r#"
[[bindings]]
alias = "pads"
[bindings.input]
matchers = [{{ type = "NameContains", value = "Mikro" }}]

[[connectors]]
alias = "absynth"
endpoint = {{ type = "Matcher", matchers = [] }}

[[modes]]
name = "Default"

[[routes]]
from = "pads"
to = "absynth"
phase = "{phase}"
"#
    )
}

#[test]
fn config_load_rejects_pre_mapping_phase_with_clear_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, config_with_phase("pre_mapping")).expect("write config");

    let err = Config::load(path.to_str().unwrap())
        .expect_err("Config::load must reject a route `phase` field");
    let msg = err.to_string();
    assert!(
        msg.contains("phase"),
        "rejection should name the removed `phase` field; got: {msg}"
    );
    assert!(
        msg.contains("post-mapping") || msg.contains("ADR-036"),
        "rejection should explain all routes are post-mapping; got: {msg}"
    );
}

#[test]
fn config_load_rejects_post_mapping_phase_too() {
    // Even the (former) default value is rejected — the field is gone, not
    // just the `pre_mapping` variant.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, config_with_phase("post_mapping")).expect("write config");

    assert!(
        Config::load(path.to_str().unwrap()).is_err(),
        "any `phase` value must be rejected after ADR-036 Phase 3"
    );
}

#[test]
fn route_without_phase_still_loads() {
    let toml = r#"
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "absynth"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Absynth" }]

[[modes]]
name = "Default"

[[routes]]
from = "pads"
to = "absynth"
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, toml).expect("write config");

    let cfg = Config::load(path.to_str().unwrap()).expect("phase-free route loads");
    assert_eq!(cfg.routes.len(), 1);
}
