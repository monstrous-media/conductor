// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase A — network-listener edge.
//!
//! The listener edge is the chain a UDP packet traverses before it can reach
//! the mapping engine: ACL filter (Slice A.3) → rate limiter (A.4) → audit
//! (A.5), wired into the listener/engine in A.6. Phase A binds loopback only.

pub mod acl_filter;
pub mod audit_edge;
pub mod manager;
pub mod rate_limit;
pub mod runtime;
pub mod startup_cleanup;

pub use acl_filter::{AclFilter, AclFilterError};
pub use audit_edge::{AuditEventKind, AuditRateLimiter};
pub use manager::{EdgeDecision, ListenerEdge, ListenerError, ListenerManager};
pub use rate_limit::{RateLimitEdge, RateLimitError};
pub use runtime::{AcceptedPacket, EdgeAuditSink, NetworkListener, spawn_listener};

use std::net::IpAddr;

/// Normalize an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) to its IPv4 form;
/// other addresses pass through unchanged.
///
/// Every edge stage that keys on a source IP — the ACL filter (A.3), the
/// per-sender rate-limit buckets (A.4), and the audit suppression map (A.5) —
/// MUST normalize first, or a dual-stack socket that delivers the same IPv4
/// peer as both `a.b.c.d` and `::ffff:a.b.c.d` would get two distinct keys and
/// bypass per-sender limiting / dedup (Copilot review on #1953).
pub(crate) fn normalize_mapped_ip(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(addr),
        v4 => v4,
    }
}
