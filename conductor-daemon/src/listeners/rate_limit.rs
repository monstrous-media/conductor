// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase A — rate-limit edge at the listener (Slice A.4).
//!
//! [`RateLimitEdge`] is the second stage of the listener edge: it runs after
//! the ACL filter (Slice A.3) and before the audit edge (A.5). It enforces two
//! buckets per the reasoning-tier Council decision (spec §4.3, R6):
//!
//! 1. **Per-sender** bucket, checked *first*. `governor`'s `check()` consumes a
//!    token on success, so checking the shared `total` bucket first would let
//!    one abusive (but on-ACL) sender drain the global budget with packets the
//!    per-sender check would have rejected anyway — starving honest senders.
//!    Per-sender-first means the `total` bucket is only ever charged for packets
//!    a sender is itself entitled to send.
//! 2. **Total** per-listener bucket, checked only if the per-sender check
//!    passed. This bucket lives *outside* the LRU and is never evicted — it caps
//!    aggregate ingress regardless of how many (spoofable) source IPs appear and
//!    is the real DoS guarantee.
//!
//! Per-sender buckets live in a **bounded LRU** ([`MAX_PER_SENDER_BUCKETS`]).
//! UDP source IPs are spoofable; an unbounded per-sender map is a trivial OOM
//! kill. A spoofed flood thrashes the LRU (evicting honest and attacker buckets
//! alike), so per-sender limiting is best-effort — the `total` bucket remains
//! the guarantee. Eviction is fail-safe: an evicted sender re-enters as a fresh
//! bucket and the `total` bucket plus the ACL still apply.

use std::net::IpAddr;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Mutex;

use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};

/// Hard cap on distinct per-sender rate-limit buckets per listener (spec §4.3,
/// Council reasoning-tier P0). Bounds the spoofed-source OOM surface.
const MAX_PER_SENDER_BUCKETS: usize = 8_192;

/// Two-bucket rate limiter for a single network listener.
pub struct RateLimitEdge {
    /// Per-listener aggregate bucket — outside the LRU, never evicted.
    total: DefaultDirectRateLimiter,
    /// Quota cloned into each per-sender bucket on first sight of a source IP.
    per_sender_quota: Quota,
    /// Bounded LRU of per-sender limiters; evicts the least-recently-used entry
    /// on insert once at [`MAX_PER_SENDER_BUCKETS`].
    per_sender: Mutex<lru::LruCache<IpAddr, DefaultDirectRateLimiter>>,
}

/// Which bucket rejected a packet.
#[derive(Debug, thiserror::Error)]
pub enum RateLimitError {
    /// The per-listener aggregate bucket is exhausted.
    #[error("total rate limit exceeded")]
    Total,
    /// The per-sender bucket for this source IP is exhausted.
    #[error("per-sender rate limit exceeded for {0}")]
    PerSender(IpAddr),
}

impl RateLimitEdge {
    /// OSC listener limits (spec §5 A.4 defaults: total 1000 / per-sender 200).
    pub fn for_osc(total_rps: u32, per_sender_rps: u32) -> Self {
        Self::new(total_rps, per_sender_rps)
    }

    /// Art-Net listener limits (spec §5 A.4 defaults: total 100 / per-sender 50).
    pub fn for_artnet(total_rps: u32, per_sender_rps: u32) -> Self {
        Self::new(total_rps, per_sender_rps)
    }

    fn new(total_rps: u32, per_sender_rps: u32) -> Self {
        let total_q = Quota::per_second(NonZeroU32::new(total_rps.max(1)).unwrap());
        let per_sender_quota = Quota::per_second(NonZeroU32::new(per_sender_rps.max(1)).unwrap());
        Self {
            total: RateLimiter::direct(total_q),
            per_sender_quota,
            per_sender: Mutex::new(lru::LruCache::new(
                NonZeroUsize::new(MAX_PER_SENDER_BUCKETS).unwrap(),
            )),
        }
    }

    /// `Ok(())` if the packet may proceed, else the bucket that rejected it.
    ///
    /// Per-sender is checked first (R6 ordering P0): a per-sender rejection
    /// returns before the `total` bucket is ever consulted, so it never charges
    /// a token to the shared budget.
    pub fn check(&self, sender: IpAddr) -> Result<(), RateLimitError> {
        // Normalize IPv4-mapped IPv6 first so a dual-stack peer can't get two
        // per-sender buckets (one per address form) and evade the limit
        // (Copilot review on #1953). The error still reports the original.
        let key = crate::listeners::normalize_mapped_ip(sender);
        {
            let mut buckets = self.per_sender.lock().unwrap();
            // Touch-or-create the per-sender bucket. `get` updates LRU recency;
            // `put` bounded-inserts (evicting the LRU entry when at capacity).
            if buckets.get(&key).is_none() {
                buckets.put(key, RateLimiter::direct(self.per_sender_quota));
            }
            let limiter = buckets.get(&key).expect("just inserted/touched");
            if limiter.check().is_err() {
                return Err(RateLimitError::PerSender(sender));
            }
        }
        if self.total.check().is_err() {
            return Err(RateLimitError::Total);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn per_sender_bucket_is_independent_per_source() {
        let edge = RateLimitEdge::for_osc(10_000, 1);
        let a = ip("127.0.0.1");
        let b = ip("127.0.0.2");
        assert!(edge.check(a).is_ok());
        assert!(matches!(edge.check(a), Err(RateLimitError::PerSender(_))));
        // b is unaffected by a's exhausted bucket.
        assert!(edge.check(b).is_ok());
    }

    #[test]
    fn total_bucket_caps_aggregate() {
        let edge = RateLimitEdge::for_artnet(2, 1000);
        assert!(edge.check(ip("127.0.0.1")).is_ok());
        assert!(edge.check(ip("127.0.0.2")).is_ok());
        assert!(matches!(
            edge.check(ip("127.0.0.3")),
            Err(RateLimitError::Total)
        ));
    }

    #[test]
    fn ipv4_mapped_shares_per_sender_bucket_with_ipv4() {
        // Copilot #1953: a dual-stack peer must not get two buckets by alternating
        // 127.0.0.1 and ::ffff:127.0.0.1. per_sender = 1 → the second form is
        // rejected because it hits the same (normalized) bucket.
        let edge = RateLimitEdge::for_osc(10_000, 1);
        assert!(edge.check(ip("127.0.0.1")).is_ok());
        assert!(matches!(
            edge.check(ip("::ffff:127.0.0.1")),
            Err(RateLimitError::PerSender(_))
        ));
    }
}
