// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-042 Phase A — audit edge at the listener (Slice A.5).
//!
//! [`AuditRateLimiter`] is the third listener-edge stage (after the ACL filter
//! and rate limiter). It caps audit-log emissions to **one entry per 60 seconds
//! per `(listener, source, kind)`** so that a packet flood — which the rate
//! limiter already drops at the edge — can't *also* flood the audit log with a
//! line per dropped packet.
//!
//! The suppression state is a **bounded LRU** ([`MAX_SUPPRESSION_ENTRIES`]), not
//! an unbounded map (spec §4.4, Council reasoning-tier P0): every spoofed packet
//! is a non-suppressed event and therefore an insert, so a TTL-only map would
//! still grow without bound under a spoofed-source flood. The LRU caps it — at
//! worst a stale source emits one extra (already rate-limited) audit line when
//! its entry is evicted. Fail-safe.

use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Suppression window: at most one audit line per key per this duration.
const AUDIT_WINDOW: Duration = Duration::from_secs(60);

/// Hard cap on suppression-state entries (spec §4.4, Council reasoning-tier P0).
/// Bounds the spoofed-source OOM surface of the suppression map.
const MAX_SUPPRESSION_ENTRIES: usize = 16_384;

/// Kind of network-listener audit event, used as a suppression-key dimension so
/// distinct event classes for the same source are not collapsed. Maps to the
/// persisted `AuditEventType` variants at emission time (Slice A.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuditEventKind {
    /// A packet was accepted by the edge and handed to the parser (A.6b-3c).
    /// Dedups `NetworkListenerActivity` emission to at most one line per 60s
    /// per `(listener, source)`.
    Activity,
    /// Source IP rejected by the ACL filter (Slice A.3).
    AclRejected,
    /// Packet dropped by the rate-limit edge (Slice A.4).
    RateLimited,
    /// Listener failed to bind its socket.
    BindFailed,
    /// A network-origin trigger was refused by the action-class gate (A.6.6/D17).
    ActionClassBlocked,
}

/// Suppresses duplicate audit emissions for the same `(listener, source, kind)`
/// within [`AUDIT_WINDOW`]. Bounded LRU — never an unbounded map.
pub struct AuditRateLimiter {
    last_emit: Mutex<lru::LruCache<(String, IpAddr, AuditEventKind), Instant>>,
}

impl Default for AuditRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl AuditRateLimiter {
    pub fn new() -> Self {
        Self {
            last_emit: Mutex::new(lru::LruCache::new(
                NonZeroUsize::new(MAX_SUPPRESSION_ENTRIES).unwrap(),
            )),
        }
    }

    /// `true` if an audit line for this `(listener, source, kind)` should be
    /// emitted now — i.e. none was emitted within the last [`AUDIT_WINDOW`].
    /// Records the emission time (bounded LRU insert) when it returns `true`.
    pub fn should_emit(&self, listener: &str, source: IpAddr, kind: AuditEventKind) -> bool {
        let mut map = self.last_emit.lock().unwrap();
        // Normalize IPv4-mapped IPv6 so a dual-stack sender can't dodge dedup by
        // alternating address forms (Copilot review on #1953).
        let key = (
            listener.to_string(),
            crate::listeners::normalize_mapped_ip(source),
            kind,
        );
        let now = Instant::now();
        // `get` is the LRU touch.
        match map.get(&key) {
            Some(prev) if now.duration_since(*prev) < AUDIT_WINDOW => false,
            _ => {
                map.put(key, now); // bounded insert; LRU-evicts when at cap
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppresses_within_window_emits_distinct_keys() {
        let a = AuditRateLimiter::new();
        let s: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(a.should_emit("l", s, AuditEventKind::AclRejected));
        assert!(!a.should_emit("l", s, AuditEventKind::AclRejected));
        // different dimension → not suppressed
        assert!(a.should_emit("l", s, AuditEventKind::RateLimited));
    }

    #[test]
    fn ipv4_mapped_source_dedups_with_ipv4() {
        // Copilot #1953: the IPv4-mapped form of a sender must share the
        // suppression key with its IPv4 form, not bypass dedup.
        let a = AuditRateLimiter::new();
        let v4: IpAddr = "127.0.0.1".parse().unwrap();
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(a.should_emit("l", v4, AuditEventKind::AclRejected));
        assert!(!a.should_emit("l", mapped, AuditEventKind::AclRejected));
    }
}
