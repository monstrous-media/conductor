// ADR-042 Phase A — Slice A.4: rate-limit edge at the network listener.
//
// `RateLimitEdge` is the second stage of the listener edge (after the ACL
// filter, before audit — spec §4.2.1). It enforces TWO buckets per the
// reasoning-tier Council decision (R6):
//
//   1. a per-sender bucket, checked FIRST so a rejected packet never charges
//      the shared budget (spec §4.3 ordering P0), held in a BOUNDED LRU so a
//      spoofed-source flood can't OOM the daemon; and
//   2. a per-listener `total` bucket — outside the LRU, never evicted — which
//      is the real aggregate-ingress DoS guarantee.
//
// Acceptance (issue #1898, spec §5 A.4):
//   cargo test --package conductor-daemon --test rate_limit_edge_test

use std::net::IpAddr;

use conductor_daemon::listeners::{RateLimitEdge, RateLimitError};

fn ip(s: &str) -> IpAddr {
    s.parse().expect("valid IP")
}

#[test]
fn per_sender_limit_rejects_excess() {
    // total budget huge so only the per-sender bucket can bite.
    let edge = RateLimitEdge::for_osc(10_000, 3);
    let src = ip("127.0.0.1");
    for i in 0..3 {
        assert!(
            edge.check(src).is_ok(),
            "packet {i} within per-sender budget"
        );
    }
    match edge.check(src) {
        Err(RateLimitError::PerSender(s)) => assert_eq!(s, src),
        other => panic!("expected PerSender rejection, got {other:?}"),
    }
}

#[test]
fn distinct_senders_have_independent_buckets() {
    let edge = RateLimitEdge::for_osc(10_000, 2);
    let a = ip("127.0.0.1");
    let b = ip("127.0.0.2");
    assert!(edge.check(a).is_ok());
    assert!(edge.check(a).is_ok());
    assert!(matches!(edge.check(a), Err(RateLimitError::PerSender(_))));
    // b's bucket is independent of a's exhaustion.
    assert!(edge.check(b).is_ok());
    assert!(edge.check(b).is_ok());
    assert!(matches!(edge.check(b), Err(RateLimitError::PerSender(_))));
}

#[test]
fn total_limit_rejects_aggregate_across_senders() {
    // total small, per-sender generous → the total bucket bites across senders.
    let edge = RateLimitEdge::for_osc(3, 1000);
    for i in 0..3 {
        let s = ip(&format!("127.0.0.{}", 10 + i));
        assert!(edge.check(s).is_ok(), "sender {i} within total budget");
    }
    // a 4th distinct sender (fresh per-sender bucket) trips the aggregate cap.
    match edge.check(ip("127.0.0.99")) {
        Err(RateLimitError::Total) => {}
        other => panic!("expected Total rejection, got {other:?}"),
    }
}

#[test]
fn per_sender_reject_does_not_charge_total() {
    // R6 ORDERING P0: a packet rejected by the per-sender bucket must NOT
    // consume a token from the shared `total` bucket. total=2, per_sender=1.
    let edge = RateLimitEdge::for_osc(2, 1);
    let a = ip("127.0.0.1");
    assert!(edge.check(a).is_ok()); // total charged: 1
    // a's 2nd packet is rejected per-sender; if ordering were total-first this
    // would (wrongly) charge the total bucket.
    assert!(matches!(edge.check(a), Err(RateLimitError::PerSender(_))));
    let b = ip("127.0.0.2");
    // b succeeds only if a's rejected packet did NOT charge total (→ total=2).
    assert!(
        edge.check(b).is_ok(),
        "per-sender reject leaked a token into the total bucket"
    );
    // total now exhausted (2/2): a fresh sender with a clean per-sender bucket
    // hits the total cap.
    match edge.check(ip("127.0.0.3")) {
        Err(RateLimitError::Total) => {}
        other => panic!("ordering broken — total mischarged: {other:?}"),
    }
}

#[test]
fn artnet_default_burst_differs_from_osc() {
    // Art-Net Phase A defaults (spec §5 A.4): total 100 / per-sender 50.
    let edge = RateLimitEdge::for_artnet(100, 50);
    let s = ip("127.0.0.1");
    for i in 0..50 {
        assert!(
            edge.check(s).is_ok(),
            "artnet packet {i} within per-sender 50"
        );
    }
    assert!(matches!(edge.check(s), Err(RateLimitError::PerSender(_))));
}
