// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! The legacy `[device]` / `[[devices]]` /
//! `[[bindings]]` / `[[connectors]]` I/O blocks were removed in ADR-035. They
//! are no longer `Config` fields and `Config` has no `deny_unknown_fields`, so
//! `Config::load` would otherwise silently drop them — a config authored in the
//! old format would load with NO I/O and no error, leaving the daemon running
//! with no devices.
//!
//! `Config::load` now rejects each legacy I/O block at the file-load boundary
//! with a migration hint to `[[endpoints]]`. (Raw `toml::from_str::<Config>`
//! still ignores unknown blocks for internal/programmatic use — see
//! `types::tests::test_config_deserialize_ignores_legacy_device_block`.)

use conductor_core::Config;

fn load_str(toml: &str) -> Result<Config, String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, toml).expect("write config");
    Config::load(path.to_str().unwrap()).map_err(|e| e.to_string())
}

const MODE_ONLY: &str = r#"
[[modes]]
name = "Default"
"#;

#[test]
fn singular_device_block_is_rejected() {
    let toml = format!("[device]\nname = \"Mikro\"\nauto_connect = true\n{MODE_ONLY}");
    let err = load_str(&toml).expect_err("a [device]-only config must be rejected, not dropped");
    assert!(
        err.contains("[device]") && err.contains("[[endpoints]]"),
        "rejection must name the legacy block and the migration target; got: {err}"
    );
}

#[test]
fn plural_devices_block_is_rejected() {
    let toml = format!("[[devices]]\nalias = \"pads\"\n{MODE_ONLY}");
    let err = load_str(&toml).expect_err("[[devices]] must be rejected");
    assert!(err.contains("[[devices]]"), "got: {err}");
}

#[test]
fn legacy_bindings_block_is_rejected() {
    let toml = format!("[[bindings]]\nalias = \"pads\"\n{MODE_ONLY}");
    let err = load_str(&toml).expect_err("[[bindings]] must be rejected");
    assert!(
        err.contains("[[bindings]]") && err.contains("[[endpoints]]"),
        "got: {err}"
    );
}

#[test]
fn legacy_connectors_block_is_rejected() {
    let toml = format!("[[connectors]]\nalias = \"absynth\"\n{MODE_ONLY}");
    let err = load_str(&toml).expect_err("[[connectors]] must be rejected");
    assert!(err.contains("[[connectors]]"), "got: {err}");
}

#[test]
fn endpoints_config_still_loads() {
    // The current authored I/O form must NOT be a false positive.
    let toml = r#"
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[modes]]
name = "Default"
"#;
    let cfg = load_str(toml).expect("an [[endpoints]] config must still load");
    assert_eq!(cfg.endpoints.len(), 1);
}

#[test]
fn mode_only_config_still_loads() {
    // No I/O blocks at all — fine (e.g. a keyboard-only mapping config).
    let cfg = load_str(MODE_ONLY).expect("a config with no I/O blocks must load");
    assert_eq!(cfg.modes.len(), 1);
}

#[test]
fn scalar_named_like_legacy_key_is_not_a_block() {
    // The rejection keys off the legacy BLOCK shape (`[device]` table /
    // `[[…]]` array), not merely the key name. A scalar that happens to share a
    // name (here `device = "x"`) is NOT a legacy I/O block, so it is not
    // rejected by this check — serde ignores the unknown scalar and the config
    // loads.
    let toml = format!("device = \"x\"\n{MODE_ONLY}");
    let cfg = load_str(&toml).expect("a scalar sharing a legacy key name must not be rejected");
    assert_eq!(cfg.modes.len(), 1);
}
