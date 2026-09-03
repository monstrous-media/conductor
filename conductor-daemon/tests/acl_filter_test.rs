// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-042 Phase A — Slice A.3: ACL filter at the listener edge.
//
// `AclFilter` wraps a compiled `NetworkAcl` (CIDR allow-list) plus an optional
// `sender_acl` (individual sender IPs). `check(source)` is the per-packet gate
// at the listener edge — it runs BEFORE the rate limiter (spec §4.2.1). Both
// filters must pass: the source must be inside the CIDR allow-list, and — when
// a `sender_acl` is populated — also one of its explicit IPs.
//
// Acceptance (issue #1898, spec §5 A.3):
//   cargo test --package conductor-daemon --test acl_filter_test

use std::net::IpAddr;

use conductor_core::security::NetworkAcl;
use conductor_daemon::listeners::AclFilter;

fn ip(s: &str) -> IpAddr {
    s.parse().expect("valid IP")
}

fn acl(entries: &[&str]) -> NetworkAcl {
    NetworkAcl::parse(
        &entries.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        false,
        false,
    )
    .expect("parse network_acl")
    .0
}

#[test]
fn sender_inside_network_acl_is_allowed() {
    let f = AclFilter::new(acl(&["192.168.1.0/24"]), vec![]);
    assert!(f.check(ip("192.168.1.50")));
}

#[test]
fn sender_outside_network_acl_is_rejected() {
    let f = AclFilter::new(acl(&["192.168.1.0/24"]), vec![]);
    assert!(!f.check(ip("10.0.0.1")));
    assert!(!f.check(ip("192.168.2.1")));
}

#[test]
fn sender_in_network_acl_but_not_sender_acl_is_rejected() {
    // sender_acl is populated → it narrows network_acl. A source that passes
    // the CIDR but is not an explicit sender IP is rejected.
    let f = AclFilter::new(acl(&["192.168.1.0/24"]), vec![ip("192.168.1.10")]);
    assert!(!f.check(ip("192.168.1.50")));
}

#[test]
fn sender_in_both_is_allowed() {
    let f = AclFilter::new(acl(&["192.168.1.0/24"]), vec![ip("192.168.1.10")]);
    assert!(f.check(ip("192.168.1.10")));
}

#[test]
fn sender_in_sender_acl_but_outside_network_acl_is_rejected() {
    // network_acl is the outer gate — a sender_acl entry can't widen it.
    let f = AclFilter::new(acl(&["192.168.1.0/24"]), vec![ip("10.0.0.1")]);
    assert!(!f.check(ip("10.0.0.1")));
}

#[test]
fn ipv4_mapped_ipv6_source_is_normalized() {
    // A dual-stack socket may deliver an IPv4 peer as ::ffff:a.b.c.d; it must
    // match both an IPv4 CIDR entry and an IPv4 sender_acl entry.
    let f = AclFilter::new(acl(&["192.168.1.0/24"]), vec![ip("192.168.1.10")]);
    assert!(f.check(ip("::ffff:192.168.1.10")));
    assert!(
        !f.check(ip("::ffff:192.168.1.50")),
        "passes CIDR but not sender_acl"
    );
}

#[test]
fn from_config_parses_strings() {
    let f = AclFilter::from_config(
        &["192.168.1.0/24".to_string()],
        &["192.168.1.10".to_string()],
        false,
        false,
    )
    .expect("parses");
    assert!(f.check(ip("192.168.1.10")));
    assert!(!f.check(ip("192.168.1.50")));
}

#[test]
fn from_config_rejects_bad_sender_ip() {
    let err = AclFilter::from_config(
        &["192.168.1.0/24".to_string()],
        &["not-an-ip".to_string()],
        false,
        false,
    );
    assert!(err.is_err(), "an invalid sender_acl IP is a hard error");
}

#[test]
fn from_config_propagates_network_acl_errors() {
    // D11: 0.0.0.0/0 in the network_acl must surface as an error.
    let err = AclFilter::from_config(&["0.0.0.0/0".to_string()], &[], false, false);
    assert!(
        err.is_err(),
        "whole-internet CIDR rejected via NetworkAcl::parse"
    );
}
