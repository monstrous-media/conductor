// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-045 D4 (#2495) — the runtime `[mcp] enabled` toggle.
//!
//! Even read-only MCP is a local socket surface; `[mcp] enabled = false`
//! must leave the socket unbound entirely (ADR-027 minimal-surface).
//! Default is ON (absent `[mcp]` block ⇒ enabled), and a config without
//! the block must keep its canonical form BYTE-IDENTICAL (the
//! `[security]`/`[config]` skip-serializing precedent) so ConfigRevisions
//! never shift.
//!
//! The parse-level tests run in EVERY composition (the `McpConfig` type is
//! ungated core config); only the service bind-decision seam is `mcp`-gated
//! — so a feature mix-up in CI can never silently skip the core property
//! (Council review on PR #2607).

use conductor_core::config::Config;
use std::io::Write;

fn load(toml: &str) -> Config {
    let mut f = tempfile::NamedTempFile::with_suffix(".toml").expect("tempfile");
    f.write_all(toml.as_bytes()).expect("write");
    Config::load(f.path().to_str().unwrap()).expect("config loads")
}

const BASE: &str = r#"
[[modes]]
name = "Default"
"#;

#[test]
fn mcp_enabled_defaults_to_true_when_block_absent() {
    assert!(
        load(BASE).mcp.enabled,
        "absent [mcp] block must default to enabled"
    );
}

#[test]
fn mcp_enabled_false_parses() {
    assert!(
        !load(&format!("{BASE}\n[mcp]\nenabled = false\n"))
            .mcp
            .enabled
    );
}

#[test]
fn mcp_enabled_true_is_explicitly_accepted() {
    assert!(
        load(&format!("{BASE}\n[mcp]\nenabled = true\n"))
            .mcp
            .enabled
    );
}

/// The [security]/[config] precedent, asserted at BYTE level: a config with
/// an explicitly-authored default `[mcp]` block and one with no block at all
/// must serialize to IDENTICAL bytes (the default block is dropped), and
/// serialization must be round-trip stable — so authoring (or not authoring)
/// the default block can never shift a ConfigRevision hash.
#[test]
fn absent_and_default_mcp_blocks_serialize_byte_identically() {
    let without_block = load(BASE);
    let with_default_block = load(&format!("{BASE}\n[mcp]\nenabled = true\n"));

    let ser_without = toml::to_string(&without_block).expect("serializes");
    let ser_with = toml::to_string(&with_default_block).expect("serializes");
    assert_eq!(
        ser_without, ser_with,
        "explicit-default [mcp] block must serialize byte-identically to an absent block"
    );
    assert!(
        !ser_without.contains("[mcp]"),
        "default [mcp] must be skipped in canonical serialization:\n{ser_without}"
    );

    // Round-trip stability: parse the serialized form back and re-serialize;
    // bytes must not drift.
    let reparsed: Config = toml::from_str(&ser_without).expect("reparses");
    let ser_again = toml::to_string(&reparsed).expect("serializes again");
    assert_eq!(
        ser_without, ser_again,
        "serialization must be round-trip stable"
    );

    // A NON-default block must survive the round trip (it is real content).
    let disabled = load(&format!("{BASE}\n[mcp]\nenabled = false\n"));
    let ser_disabled = toml::to_string(&disabled).expect("serializes");
    assert!(
        ser_disabled.contains("[mcp]") && ser_disabled.contains("enabled = false"),
        "non-default [mcp] must be preserved:\n{ser_disabled}"
    );
}

/// The service bind-decision seam (only this needs the `mcp` feature).
#[cfg(feature = "mcp")]
#[test]
fn mcp_socket_enabled_follows_the_config_toggle() {
    use conductor_daemon::daemon::service::mcp_socket_enabled;
    assert!(mcp_socket_enabled(&load(BASE)), "socket binds by default");
    assert!(
        !mcp_socket_enabled(&load(&format!("{BASE}\n[mcp]\nenabled = false\n"))),
        "[mcp] enabled = false must leave the socket unbound"
    );
}
