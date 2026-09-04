// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-042 Phase A — Slice A.6a: ListenerManager + ordered listener edge.
//
// ListenerManager builds one ListenerEdge per enabled Input/Bidirectional
// OSC/Art-Net endpoint. Each edge runs the per-packet checks in the mandated
// order (spec §4.2.1): ACL filter FIRST, then the rate-limit edge. The ordering
// is a security property: an off-ACL packet must be rejected
// before the rate limiter is consulted, so a flood of off-ACL traffic can never
// consume a listener's rate-limit budget and starve a legitimate sender.
//
// A loopback listener with no explicit network_acl gets the loopback-default
// ACL (spec D1) so it accepts loopback sources. Sockets are bound in Slice A.6b;
// this slice is the socket-free edge + manager.
//
// Acceptance (spec §5 A.6):
//   cargo test --package conductor-daemon --test listener_manager_test

use std::net::IpAddr;

use conductor_core::Config;
use conductor_daemon::listeners::{EdgeDecision, ListenerManager};

fn parse(s: &str) -> Config {
    toml::from_str(s).expect("config parses")
}

fn ip(s: &str) -> IpAddr {
    s.parse().expect("valid IP")
}

/// An OSC loopback listener with a tight total budget for deterministic tests.
fn osc_listener_cfg() -> Config {
    parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_in"
direction = "Input"
type = "OscEndpoint"
host = "127.0.0.1"
port = 9000
network_acl = ["127.0.0.0/8"]
rate_limit_total = 3
rate_limit_per_sender = 10
allow_sensitive_actions = true
"#,
    )
}

#[test]
fn builds_an_edge_for_a_loopback_osc_input() {
    let mgr = ListenerManager::from_config(&osc_listener_cfg()).expect("builds");
    assert_eq!(mgr.len(), 1);
    let edge = mgr.edge("osc_in").expect("edge present");
    assert_eq!(edge.port(), 9000);
    assert!(edge.allow_sensitive_actions());
}

#[test]
fn edge_admits_on_acl_and_rejects_off_acl() {
    let mgr = ListenerManager::from_config(&osc_listener_cfg()).unwrap();
    let edge = mgr.edge("osc_in").unwrap();
    assert_eq!(edge.admit(ip("127.0.0.1")), EdgeDecision::Accept);
    assert_eq!(edge.admit(ip("10.0.0.1")), EdgeDecision::RejectAcl);
}

#[test]
fn off_acl_flood_does_not_consume_rate_budget() {
    // THE security proof (spec §4.2.1, G2): ACL runs before rate-limit, so an
    // off-ACL flood never charges the rate buckets — an on-ACL sender still
    // gets the full total budget afterward.
    let mgr = ListenerManager::from_config(&osc_listener_cfg()).unwrap();
    let edge = mgr.edge("osc_in").unwrap();

    for _ in 0..1000 {
        assert_eq!(edge.admit(ip("10.0.0.1")), EdgeDecision::RejectAcl);
    }

    // total budget is 3; three distinct on-ACL senders each get one packet.
    assert_eq!(edge.admit(ip("127.0.0.1")), EdgeDecision::Accept);
    assert_eq!(edge.admit(ip("127.0.0.2")), EdgeDecision::Accept);
    assert_eq!(edge.admit(ip("127.0.0.3")), EdgeDecision::Accept);
    // 4th on-ACL sender trips the total bucket — proving the off-ACL flood
    // consumed none of it.
    assert_eq!(edge.admit(ip("127.0.0.4")), EdgeDecision::RejectRateLimit);
}

#[test]
fn loopback_default_acl_when_network_acl_omitted() {
    // No network_acl → loopback-default (spec D1): accepts loopback, rejects
    // a non-loopback source.
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
    let mgr = ListenerManager::from_config(&cfg).unwrap();
    let edge = mgr.edge("osc_in").unwrap();
    assert_eq!(edge.admit(ip("127.0.0.1")), EdgeDecision::Accept);
    assert_eq!(edge.admit(ip("::1")), EdgeDecision::Accept);
    assert_eq!(edge.admit(ip("192.168.1.5")), EdgeDecision::RejectAcl);
}

#[test]
fn output_and_non_network_endpoints_are_skipped() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[endpoints]]
alias = "osc_out"
direction = "Output"
type = "OscEndpoint"
host = "127.0.0.1"
port = 9000

[[endpoints]]
alias = "midi_in"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]
"#,
    );
    let mgr = ListenerManager::from_config(&cfg).unwrap();
    // Output OSC is not a listener; MIDI Matcher is not a network listener.
    assert!(mgr.is_empty());
}

#[test]
fn disabled_listener_is_skipped() {
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
enabled = false
"#,
    );
    let mgr = ListenerManager::from_config(&cfg).unwrap();
    assert!(mgr.edge("osc_in").is_none());
}
