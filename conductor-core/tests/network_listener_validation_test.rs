// ADR-042 Phase A — Slice A.2: EndpointConfig network-security schema +
// loopback-only validator.
//
// Phase A is loopback-only: an OSC/Art-Net *listener* (Input/Bidirectional)
// may bind a loopback host only; any non-loopback host is a config-load error
// directing the operator at Phase B-early (keychain-HMAC approval). The
// `allow_network`/`network_acl` schema is still parsed and shape-validated so
// configs are forward-compatible. Output endpoints (which *send* to a remote
// host) are unaffected — a lighting rig at 10.0.0.5 is normal.
//
// Acceptance (issue #1898, spec §5 A.2): `cargo test --package conductor-core`.

use conductor_core::Config;
use conductor_core::config::types::{EndpointConfig, EndpointKind};
use conductor_core::config::validation::validate_config;

fn parse(s: &str) -> Config {
    toml::from_str(s).expect("config parses")
}

fn errors_mentioning(cfg: &Config, needles: &[&str]) -> bool {
    let report = validate_config(cfg);
    report.errors.iter().any(|e| {
        needles
            .iter()
            .all(|n| e.message.contains(n) || e.path.contains(n))
    })
}

// ── Loopback listener: clean ───────────────────────────────────────

#[test]
fn loopback_osc_input_validates_clean() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 9000
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "a loopback OSC listener is valid in Phase A: {:?}",
        validate_config(&cfg).errors
    );
}

#[test]
fn ipv6_loopback_osc_input_validates_clean() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "::1"
port = 9000
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "an IPv6 loopback OSC listener is valid: {:?}",
        validate_config(&cfg).errors
    );
}

// ── Non-loopback listener: config-load error ───────────────────────

#[test]
fn wildcard_host_osc_input_is_phase_b_error() {
    // host = "0.0.0.0" (bind all interfaces) is the canonical non-loopback
    // listener mistake — must error and point at Phase B-early.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "0.0.0.0"
port = 9000
"#,
    );
    assert!(
        !validate_config(&cfg).is_valid(),
        "0.0.0.0 listener must error"
    );
    assert!(
        errors_mentioning(&cfg, &["Phase B-early", "osc_in"]),
        "error names the endpoint and points at Phase B-early: {:?}",
        validate_config(&cfg).errors
    );
}

#[test]
fn wildcard_host_with_allow_network_is_permitted_in_b_early() {
    // ADR-042 Phase B-early A.2 lift: `allow_network = true` + a valid
    // `network_acl` now LIFTS the config-load loopback gate. A non-loopback
    // host (including the bind-all wildcard `0.0.0.0`) is allowed to reach a
    // bind attempt — where it is gated on an HMAC-verified approval. The
    // config-load error is replaced by the runtime bind-gate.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "0.0.0.0"
port = 9000
allow_network = true
network_acl = ["192.168.1.0/24"]
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "allow_network + valid network_acl lifts the config-load gate in B-early: {:?}",
        validate_config(&cfg).errors
    );
}

#[test]
fn non_loopback_lan_host_with_allow_network_is_permitted_in_b_early() {
    // A concrete LAN listener with explicit opt-in + sender allow-list passes
    // config-load in B-early; the approval gate enforces binding at runtime.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "lan_in"
direction = "Input"
type = "OscEndpoint"
host = "192.168.1.10"
port = 9000
allow_network = true
network_acl = ["192.168.1.0/24"]
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "a LAN listener with allow_network + acl is permitted at config-load in B-early: {:?}",
        validate_config(&cfg).errors
    );
}

#[test]
fn non_loopback_literal_host_is_phase_b_error() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "lan_in"
direction = "Input"
type = "OscEndpoint"
host = "192.168.1.10"
port = 9000
"#,
    );
    assert!(
        errors_mentioning(&cfg, &["Phase B-early", "lan_in"]),
        "a LAN listener host errors in Phase A: {:?}",
        validate_config(&cfg).errors
    );
}

// ── Output endpoints are unaffected (they send, not listen) ─────────

#[test]
fn non_loopback_output_osc_is_valid() {
    // Sending OSC to a remote host is the normal output case — the loopback
    // gate is a *listener* control and must not touch Output endpoints.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "lights"
direction = "Output"
type = "OscEndpoint"
host = "10.0.0.5"
port = 9000
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "non-loopback Output OSC is valid (it sends): {:?}",
        validate_config(&cfg).errors
    );
}

#[test]
fn broadcast_output_artnet_is_valid() {
    // Default Art-Net host is 255.255.255.255 (broadcast) — an Output rig is
    // the normal case and must stay valid.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "dmx"
direction = "Output"
type = "ArtNetEndpoint"
universe = 1
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "default-broadcast Output Art-Net is valid: {:?}",
        validate_config(&cfg).errors
    );
}

// ── allow_network shape checks ─────────────────────────────────────

#[test]
fn allow_network_requires_non_empty_acl() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 9000
allow_network = true
"#,
    );
    assert!(
        errors_mentioning(&cfg, &["network_acl"]),
        "allow_network = true with empty network_acl is a shape error: {:?}",
        validate_config(&cfg).errors
    );
}

#[test]
fn acl_rejects_any_v4() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 9000
allow_network = true
network_acl = ["0.0.0.0/0"]
"#,
    );
    assert!(
        !validate_config(&cfg).is_valid(),
        "0.0.0.0/0 in the ACL must be rejected (D11)"
    );
}

#[test]
fn acl_rejects_any_v6() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "::1"
port = 9000
allow_network = true
network_acl = ["::/0"]
"#,
    );
    assert!(
        !validate_config(&cfg).is_valid(),
        "::/0 in the ACL must be rejected (D11)"
    );
}

// ── Aggregate amplification (Art-Net broadcast) ────────────────────

#[test]
fn aggregate_amplification_shard_bypass_closed() {
    // Two /25s aggregate to a /24-equivalent (512 > 256) under broadcast
    // without the acknowledgement → error. Proves the shard bypass is closed.
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "dmx_in"
direction = "Input"
type = "ArtNetEndpoint"
universe = 1
host = "127.0.0.1"
allow_broadcast = true
allow_network = true
network_acl = ["10.0.0.0/24", "10.0.1.0/24"]
"#,
    );
    assert!(
        errors_mentioning(&cfg, &["amplification"]),
        "sharded broad ACL + broadcast without the flag must error: {:?}",
        validate_config(&cfg).errors
    );
}

#[test]
fn aggregate_amplification_with_ack_is_ok() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "dmx_in"
direction = "Input"
type = "ArtNetEndpoint"
universe = 1
host = "127.0.0.1"
allow_broadcast = true
allow_network = true
i_understand_amplification_risk = true
network_acl = ["10.0.0.0/24", "10.0.1.0/24"]
"#,
    );
    assert!(
        validate_config(&cfg).is_valid(),
        "amplification ack lifts the aggregate check: {:?}",
        validate_config(&cfg).errors
    );
}

// ── Round-trip serialisation preserves all fields ──────────────────

#[test]
fn round_trip_preserves_security_fields() {
    let src = r#"
        alias = "osc_in"
        direction = "Input"
        type = "OscEndpoint"
        host = "127.0.0.1"
        port = 9000
        allow_network = true
        network_acl = ["192.168.1.0/24"]
        sender_acl = ["192.168.1.50"]
        rate_limit_total = 1000
        rate_limit_per_sender = 200
        i_understand_amplification_risk = true
        allow_sensitive_actions = true
    "#;
    let ep: EndpointConfig = toml::from_str(src).expect("parses");
    let serialized = toml::to_string(&ep).expect("serializes");
    let reparsed: EndpointConfig = toml::from_str(&serialized).expect("re-parses");
    match reparsed.kind {
        EndpointKind::OscEndpoint { host, security, .. } => {
            assert_eq!(host, "127.0.0.1");
            assert!(security.allow_network);
            assert_eq!(security.network_acl, vec!["192.168.1.0/24".to_string()]);
            assert_eq!(security.sender_acl, vec!["192.168.1.50".to_string()]);
            assert_eq!(security.rate_limit_total, Some(1000));
            assert_eq!(security.rate_limit_per_sender, Some(200));
            assert!(security.i_understand_amplification_risk);
            assert!(
                security.allow_sensitive_actions,
                "allow_sensitive_actions (D17) survives round-trip"
            );
        }
        other => panic!("expected OscEndpoint after round-trip, got {other:?}"),
    }
}

// ── Executor path: EndpointKind deserialises from a JSON value ──────

#[test]
fn endpoint_kind_deserialises_from_json_with_security_fields() {
    // The LLM executor builds `EndpointKind` via `serde_json::from_value`
    // (conductor-daemon llm/executor.rs). The flattened security struct must
    // not break that derive path.
    let v = serde_json::json!({
        "type": "OscEndpoint",
        "host": "127.0.0.1",
        "port": 9000,
        "allow_network": true,
        "network_acl": ["192.168.1.0/24"],
        "allow_sensitive_actions": true
    });
    let kind: EndpointKind = serde_json::from_value(v).expect("EndpointKind parses from JSON");
    match kind {
        EndpointKind::OscEndpoint { host, security, .. } => {
            assert_eq!(host, "127.0.0.1");
            assert!(security.allow_network);
            assert!(security.allow_sensitive_actions);
            assert_eq!(security.network_acl, vec!["192.168.1.0/24".to_string()]);
        }
        other => panic!("expected OscEndpoint, got {other:?}"),
    }

    let av = serde_json::json!({
        "type": "ArtNetEndpoint",
        "universe": 1,
        "host": "127.0.0.1",
        "allow_broadcast": true,
        "allow_network": true,
        "network_acl": ["10.0.0.0/25"]
    });
    let akind: EndpointKind = serde_json::from_value(av).expect("ArtNet parses from JSON");
    match akind {
        EndpointKind::ArtNetEndpoint {
            allow_broadcast,
            security,
            ..
        } => {
            assert!(allow_broadcast);
            assert!(security.allow_network);
        }
        other => panic!("expected ArtNetEndpoint, got {other:?}"),
    }
}
