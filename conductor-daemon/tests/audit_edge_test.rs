// ADR-042 Phase A — Slice A.5: audit edge at the network listener.
//
// `AuditRateLimiter` is the third listener-edge stage (after ACL and rate
// limit). It suppresses duplicate audit emissions for the same
// (listener, source, kind) within a 60-second window so a flood can't also
// flood the audit log — held in a BOUNDED LRU (spec §4.4) so the suppression
// state itself can't be turned into a spoofed-source OOM vector.
//
// Acceptance (issue #1898, spec §5 A.5):
//   cargo test --package conductor-daemon --test audit_edge_test

use std::net::IpAddr;

use conductor_daemon::listeners::{AuditEventKind, AuditRateLimiter};

fn ip(s: &str) -> IpAddr {
    s.parse().expect("valid IP")
}

#[test]
fn first_event_for_a_key_emits() {
    let a = AuditRateLimiter::new();
    assert!(a.should_emit("osc-in", ip("127.0.0.1"), AuditEventKind::AclRejected));
}

#[test]
fn duplicate_within_window_is_suppressed() {
    let a = AuditRateLimiter::new();
    let s = ip("127.0.0.1");
    assert!(a.should_emit("osc-in", s, AuditEventKind::RateLimited));
    // same (listener, source, kind) within 60s → suppressed.
    assert!(!a.should_emit("osc-in", s, AuditEventKind::RateLimited));
}

#[test]
fn distinct_kind_is_not_suppressed() {
    let a = AuditRateLimiter::new();
    let s = ip("127.0.0.1");
    assert!(a.should_emit("osc-in", s, AuditEventKind::AclRejected));
    assert!(a.should_emit("osc-in", s, AuditEventKind::RateLimited));
}

#[test]
fn distinct_source_is_not_suppressed() {
    let a = AuditRateLimiter::new();
    assert!(a.should_emit("osc-in", ip("127.0.0.1"), AuditEventKind::AclRejected));
    assert!(a.should_emit("osc-in", ip("127.0.0.2"), AuditEventKind::AclRejected));
}

#[test]
fn distinct_listener_is_not_suppressed() {
    let a = AuditRateLimiter::new();
    let s = ip("127.0.0.1");
    assert!(a.should_emit("osc-in", s, AuditEventKind::BindFailed));
    assert!(a.should_emit("artnet-in", s, AuditEventKind::BindFailed));
}

#[test]
fn action_class_blocked_is_a_distinct_kind() {
    // A.6.6 emits NetworkActionClassBlocked through this edge; confirm the kind
    // exists and is independently suppressed.
    let a = AuditRateLimiter::new();
    let s = ip("127.0.0.1");
    assert!(a.should_emit("osc-in", s, AuditEventKind::ActionClassBlocked));
    assert!(!a.should_emit("osc-in", s, AuditEventKind::ActionClassBlocked));
}
