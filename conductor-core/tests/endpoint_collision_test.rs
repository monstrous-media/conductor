// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-035 — normalize_to_endpoints duplicate-alias hard-fail +
// canonical_endpoint_digest (spec §4.3).
//
// Post-legacy-removal: the only authored I/O block is [[endpoints]].
// normalize_to_endpoints dedups authored endpoint aliases (hard error on a
// repeat) and the digest is stable/ order-independent.

use conductor_core::Config;
use conductor_core::config::loader::{canonical_endpoint_digest, normalize_to_endpoints};

fn parse(toml_str: &str) -> Config {
    toml::from_str::<Config>(toml_str).expect("config parses")
}

const CLEAN: &str = r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "ep1"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "A" }]

[[endpoints]]
alias = "ep2"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "B" }]

[[endpoints]]
alias = "ep3"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "C" }]
"#;

#[test]
fn clean_config_normalizes_without_error_or_findings() {
    let config = parse(CLEAN);
    let (endpoints, findings) =
        normalize_to_endpoints(&config).expect("clean config normalizes without error");
    let aliases: Vec<&str> = endpoints.iter().map(|e| e.alias.as_str()).collect();
    assert_eq!(aliases, vec!["ep1", "ep2", "ep3"]);
    // No legacy blocks remain, so there are never deprecation findings.
    assert!(findings.is_empty(), "no legacy blocks → no findings");
}

#[test]
fn duplicate_endpoint_alias_collides() {
    let config = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "dup"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "A" }]

[[endpoints]]
alias = "dup"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "B" }]
"#,
    );
    let err = normalize_to_endpoints(&config).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("dup"), "names the colliding alias: {msg}");
    assert!(msg.contains("endpoints"), "names the source block: {msg}");
}

#[test]
fn empty_config_normalizes_to_empty() {
    let config = parse("[[modes]]\nname = \"Default\"\n");
    let (endpoints, findings) = normalize_to_endpoints(&config).expect("normalizes");
    assert!(endpoints.is_empty());
    assert!(findings.is_empty(), "no endpoints → no findings");
}

// ── canonical_endpoint_digest (§4.3) ───────────────────────────────

#[test]
fn digest_is_order_independent() {
    // Same endpoints in a different authored order → identical digest
    // (the digest sorts by alias before hashing).
    let a = normalize_to_endpoints(&parse(CLEAN)).unwrap().0;
    let mut reordered = a.clone();
    reordered.reverse();
    assert_eq!(
        canonical_endpoint_digest(&a).to_string(),
        canonical_endpoint_digest(&reordered).to_string(),
        "digest is stable under input reordering"
    );
}

#[test]
fn digest_changes_when_an_endpoint_changes() {
    let base = normalize_to_endpoints(&parse(CLEAN)).unwrap().0;
    let mut changed = base.clone();
    changed[0].alias = "ep1-renamed".to_string();
    assert_ne!(
        canonical_endpoint_digest(&base).to_string(),
        canonical_endpoint_digest(&changed).to_string(),
        "a material change to an endpoint changes the digest"
    );
}

// ── End-to-end: Config::load rejects duplicate endpoint aliases ─────

#[test]
fn config_load_rejects_duplicate_endpoint_aliases_end_to_end() {
    use std::io::Write;
    let dir = std::env::temp_dir();
    // PID + nanotime so parallel/repeated runs never share a path.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!(
        "conductor-collision-{}-{}.toml",
        std::process::id(),
        nanos
    ));
    let mut f = std::fs::File::create(&path).expect("create temp config");
    write!(
        f,
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "dup"
direction = "Input"
type = "Matcher"
matchers = [{{ type = "NameContains", value = "A" }}]

[[endpoints]]
alias = "dup"
direction = "Output"
type = "Matcher"
matchers = [{{ type = "NameContains", value = "B" }}]
"#
    )
    .unwrap();
    let result = Config::load(path.to_str().unwrap());
    let _ = std::fs::remove_file(&path);
    assert!(
        result.is_err(),
        "Config::load must reject a duplicate endpoint alias"
    );
}
