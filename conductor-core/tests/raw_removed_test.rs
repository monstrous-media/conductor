// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-036 Phase 2 (#1696) — `Trigger::Raw` is removed entirely.
//!
//! Phase 1 (Slice 3) auto-lowered `Raw` + `MidiForward` to a `pre_mapping`
//! route with a deprecation warning. Phase 2 removes `Raw` as an input
//! concept: the parser **rejects** it, and `Config::load` surfaces a
//! migration hint pointing at `conductorctl migrate-config --routing`.
//!
//! Spec reference:
//! `docs/routing-unification/ADR-036-037-implementation-spec.md` § 6 Phase 2.

use conductor_core::Config;

// NOTE: the I/O is declared with `[[endpoints]]` (ADR-035), not the legacy
// `[[bindings]]`/`[[connectors]]` blocks — those are now rejected at load
// (#2124), and using them here would mask the `Raw`-rejection this test
// exercises by erroring earlier.
const RAW_CONFIG: &str = r#"
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

[[modes.mappings]]
trigger = { type = "Raw", device = "pads" }
action = { type = "MidiForward", target = "absynth" }
"#;

#[test]
fn raw_trigger_is_rejected_by_parser() {
    let result: Result<Config, _> = toml::from_str(RAW_CONFIG);
    assert!(
        result.is_err(),
        "Trigger::Raw must be rejected at parse after ADR-036 Phase 2"
    );
}

#[test]
fn config_load_rejects_raw_with_migration_hint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, RAW_CONFIG).expect("write config");

    let err =
        Config::load(path.to_str().unwrap()).expect_err("Config::load must reject a Raw trigger");
    let msg = err.to_string();
    assert!(
        msg.contains("migrate-config"),
        "rejection should name the migration tool; got: {msg}"
    );
}
