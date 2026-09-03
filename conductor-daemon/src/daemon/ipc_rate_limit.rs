// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 §D16 (full) + §D12 — per-peer IPC message rate limit.
//!
//! The D16 concurrent-connection cap closes the
//! "open N sockets and pin memory" attack arm. This module closes
//! the orthogonal arm: a same-user attacker holding ONE connection
//! and flooding it with messages, exhausting daemon CPU on the
//! dispatch path. ADR-027 §D12 specifies the rate-limit identity
//! as `(peer_pid, peer_exe_path)` — same-PID-different-exe peers
//! (e.g. a forked attacker re-execing into a different binary) are
//! tracked as distinct buckets so a malicious re-exec doesn't
//! inherit a benign predecessor's remaining budget. Each peer
//! identity gets a 100-messages-per-second budget; exceedances
//! return a structured `RateLimitExceeded` denial that the IPC
//! server logs and emits over the audit stream (D13a).
//!
//! Implementation mirrors `keystroke_policy::RateState` — a
//! `VecDeque<Instant>` sliding window per peer, pruned on every
//! check. Per-peer state is held in a bounded `HashMap` keyed by
//! `(pid, exe)`; entries are garbage-collected lazily when their
//! window is empty and, if still full, the stalest peer is
//! evicted so a churn of short-lived peers can't leak memory.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// ADR-027 §D16 default — `max_messages_per_second_per_peer = 100`.
pub const STANDARD_IPC_MSGS_PER_SEC: u32 = 100;

/// Sliding-window length. 1s mirrors the keystroke-policy choice;
/// pairs cleanly with the per-second rate constant.
pub const IPC_RATE_WINDOW: Duration = Duration::from_secs(1);

/// Cap on the size of the per-peer HashMap. A same-user attacker who
/// can churn PIDs (fork+connect, exit) would otherwise grow this
/// unboundedly. 1024 is generous for any realistic concurrent-peer
/// count (the D16 connection cap is 32) while keeping memory
/// bounded.
const MAX_TRACKED_PEERS: usize = 1024;

/// Compound identity used as the rate-limit key (ADR-027 §D12):
/// `(peer_pid, peer_exe_path)`. Two clients with the same PID but
/// different exe paths — e.g. a malicious re-exec — get distinct
/// buckets so one identity can't inherit another's budget.
///
/// **Path canonicalisation invariant**: the only writer to this
/// key in production is `handle_client` (see `ipc.rs`), which uses
/// `PinnedPeer::initial_exe` — populated from kernel APIs that
/// return canonical, symlink-resolved paths (`readlink(/proc/<pid>/exe)`
/// on Linux, `proc_pidpath` on macOS). An attacker connecting via
/// IPC cannot supply a non-canonical path like `/path/./to/exe` to
/// fork a fresh bucket: the kernel canonicalises before the daemon
/// ever sees the bytes. Direct `PeerKey` construction with
/// non-canonical paths is only reachable from tests, where the
/// caller controls both sides.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerKey {
    pub pid: u32,
    pub exe: PathBuf,
}

impl PeerKey {
    pub fn new(pid: u32, exe: impl Into<PathBuf>) -> Self {
        Self {
            pid,
            exe: exe.into(),
        }
    }
}

/// Outcome of a rate-limit check. Carries the counters the audit
/// log surfaces so an operator can see whether the cap was just
/// barely exceeded or floored.
#[derive(Debug, PartialEq, Eq)]
pub enum RateLimitDecision {
    /// Slot consumed; the request may proceed.
    Allowed,
    /// Peer is over the per-second cap. Connection should be told
    /// to back off; the request is dropped.
    Denied {
        /// Number of messages from this peer already in the window.
        recent_count: u32,
        /// The hard cap (currently [`STANDARD_IPC_MSGS_PER_SEC`]).
        max_per_window: u32,
    },
    /// Internal state corruption (lock poisoning). Fail-closed: the
    /// security limiter must NOT proceed on possibly-corrupt state.
    ///
    /// **DoS tradeoff**: a poisoned `Mutex` will reject EVERY peer
    /// for the daemon's lifetime, which is itself a DoS surface.
    /// Accepted because:
    /// (a) mirrors `keystroke_policy::InternalStateCorrupt` — same
    /// rate-limiter fail-closed posture, consistency matters more
    /// than per-module reinvention; (b) operations under the lock
    /// (HashMap insert/remove, VecDeque push/pop) don't panic
    /// under any normal input, so poisoning in production is
    /// extraordinarily rare; (c) the alternative — recovering via
    /// `into_inner()` after a panic — risks letting through more
    /// messages than the cap, which is the bypass class we're
    /// trying to prevent. Operator audit visibility is preserved
    /// via the dedicated variant.
    InternalStateCorrupt,
}

/// Thread-safe per-peer IPC message rate limiter. One instance is
/// shared across all client-handler tasks via `Arc`; each task
/// passes its peer identity to [`check`].
#[derive(Default)]
pub struct IpcMessageRateLimiter {
    state: Mutex<LimiterState>,
}

#[derive(Default)]
struct LimiterState {
    /// `(pid, exe) → sliding window of recent timestamps`. Capped
    /// at [`MAX_TRACKED_PEERS`] entries; oldest-idle entries are
    /// evicted first and then (if still full) the stalest peer is
    /// evicted.
    peers: HashMap<PeerKey, VecDeque<Instant>>,
}

impl IpcMessageRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check and register a request from `(pid, exe)`. Returns
    /// [`RateLimitDecision::Allowed`] if the request fits in the
    /// peer's per-second budget, [`RateLimitDecision::Denied`] if
    /// it would exceed it.
    ///
    /// Production callers use this; tests use [`check_at`] for
    /// deterministic time control.
    pub fn check(&self, pid: u32, exe: &Path) -> RateLimitDecision {
        self.check_at(pid, exe, Instant::now())
    }

    /// Check + register at the given wall-clock time. Same
    /// semantics as [`check`] but the clock is injected for
    /// deterministic tests.
    pub fn check_at(&self, pid: u32, exe: &Path, now: Instant) -> RateLimitDecision {
        let key = PeerKey::new(pid, exe.to_path_buf());
        let mut state = match self.state.lock() {
            Ok(g) => g,
            // Mirrors keystroke_policy: a poisoned lock means a
            // prior panic may have left state inconsistent. A
            // security limiter MUST fail closed rather than risk
            // letting through more messages than the cap.
            Err(_) => return RateLimitDecision::InternalStateCorrupt,
        };

        // Lazy GC: if the peer map is at the cap and we're about to
        // touch a new identity, prune empty entries first so a churn
        // of short-lived peers can't grow the map unboundedly.
        if state.peers.len() >= MAX_TRACKED_PEERS && !state.peers.contains_key(&key) {
            prune_empty_entries(&mut state.peers, now);
            // If still full, evict one stale peer to preserve a hard
            // cap on map size under non-expiring PID churn.
            if state.peers.len() >= MAX_TRACKED_PEERS {
                evict_stalest_peer(&mut state.peers);
            }
        }

        let window = state.peers.entry(key).or_default();

        // Prune expired timestamps. Use `<=` so a timestamp landing
        // EXACTLY on the window boundary is treated as expired —
        // semantically "older than 1s ago" includes the inclusive
        // boundary.
        if let Some(cutoff) = now.checked_sub(IPC_RATE_WINDOW) {
            while let Some(&front) = window.front() {
                if front <= cutoff {
                    window.pop_front();
                } else {
                    break;
                }
            }
        }

        let recent_count = window.len() as u32;
        if recent_count >= STANDARD_IPC_MSGS_PER_SEC {
            return RateLimitDecision::Denied {
                recent_count,
                max_per_window: STANDARD_IPC_MSGS_PER_SEC,
            };
        }

        window.push_back(now);
        // Memory invariant: after the early-return above, `recent_count`
        // (the pre-push length) is at most `STANDARD_IPC_MSGS_PER_SEC - 1`,
        // so post-push length is at most `STANDARD_IPC_MSGS_PER_SEC`.
        // Worst-case memory is therefore
        // O(MAX_TRACKED_PEERS × STANDARD_IPC_MSGS_PER_SEC) — bounded
        // and predictable, no post-push trim needed.
        // (Earlier trim was dead code.)
        RateLimitDecision::Allowed
    }

    /// Drop a peer's tracking state entirely — used on connection
    /// close so a peer that connects, disconnects, then reconnects
    /// doesn't accumulate stale state. Cheap and idempotent.
    ///
    /// On a poisoned `Mutex`, logs a warning and returns without
    /// touching state. This mirrors `check_at`'s fail-closed
    /// posture: cleanup is a best-effort hint, not a security
    /// invariant; the next `check` from this peer will hit the
    /// poisoned-lock branch and deny via `InternalStateCorrupt`,
    /// so the prior peer's stale window can't be exploited.
    pub fn forget(&self, pid: u32, exe: &Path) {
        match self.state.lock() {
            Ok(mut state) => {
                state.peers.remove(&PeerKey::new(pid, exe.to_path_buf()));
            }
            Err(_) => {
                tracing::warn!(
                    "ipc_rate_limit: forget({}, {:?}) skipped — Mutex poisoned",
                    pid,
                    exe
                );
            }
        }
    }

    #[cfg(test)]
    fn tracked_peer_count_for_test(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.peers.len())
            .unwrap_or(0)
    }
}

/// Evict peer entries whose sliding window is empty after pruning
/// expired timestamps. Bounded work: visits each entry at most
/// once, no allocation. Called only when the map is at
/// [`MAX_TRACKED_PEERS`].
fn prune_empty_entries(peers: &mut HashMap<PeerKey, VecDeque<Instant>>, now: Instant) {
    let cutoff = now.checked_sub(IPC_RATE_WINDOW);
    peers.retain(|_, window| {
        if let Some(cutoff) = cutoff {
            while let Some(&front) = window.front() {
                // Match `check_at`'s `<=` semantics so the two
                // pruning sites stay consistent.
                if front <= cutoff {
                    window.pop_front();
                } else {
                    break;
                }
            }
        }
        !window.is_empty()
    });
}

/// Evict exactly one tracked peer, preferring the least-recently
/// seen (oldest newest-timestamp). If any empty windows exist, those
/// naturally sort first and are evicted first.
fn evict_stalest_peer(peers: &mut HashMap<PeerKey, VecDeque<Instant>>) {
    let to_evict = peers
        .iter()
        .min_by_key(|(_, window)| window.back().copied())
        .map(|(key, _)| key.clone());
    if let Some(key) = to_evict {
        peers.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER_PID: u32 = 12345;
    fn exe_a() -> &'static Path {
        Path::new("/Applications/Claude.app/Contents/MacOS/Claude")
    }
    fn exe_b() -> &'static Path {
        Path::new("/Applications/Cursor.app/Contents/MacOS/Cursor")
    }

    #[test]
    fn allows_burst_up_to_cap() {
        let limiter = IpcMessageRateLimiter::new();
        let t = Instant::now();
        for _ in 0..STANDARD_IPC_MSGS_PER_SEC {
            assert_eq!(
                limiter.check_at(PEER_PID, exe_a(), t),
                RateLimitDecision::Allowed
            );
        }
    }

    #[test]
    fn denies_when_cap_exceeded() {
        let limiter = IpcMessageRateLimiter::new();
        let t = Instant::now();
        for _ in 0..STANDARD_IPC_MSGS_PER_SEC {
            assert_eq!(
                limiter.check_at(PEER_PID, exe_a(), t),
                RateLimitDecision::Allowed
            );
        }
        match limiter.check_at(PEER_PID, exe_a(), t) {
            RateLimitDecision::Denied {
                recent_count,
                max_per_window,
            } => {
                assert_eq!(max_per_window, STANDARD_IPC_MSGS_PER_SEC);
                assert_eq!(recent_count, STANDARD_IPC_MSGS_PER_SEC);
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn per_peer_isolation_by_pid() {
        let limiter = IpcMessageRateLimiter::new();
        let t = Instant::now();
        // Peer A (pid=1) maxes out its budget.
        for _ in 0..STANDARD_IPC_MSGS_PER_SEC {
            assert_eq!(limiter.check_at(1, exe_a(), t), RateLimitDecision::Allowed);
        }
        assert!(matches!(
            limiter.check_at(1, exe_a(), t),
            RateLimitDecision::Denied { .. }
        ));
        // Peer B (pid=2) is untouched.
        for _ in 0..STANDARD_IPC_MSGS_PER_SEC {
            assert_eq!(limiter.check_at(2, exe_a(), t), RateLimitDecision::Allowed);
        }
    }

    /// ADR-027 §D12: same PID, different exe paths — e.g.
    /// a forked attacker `execve`ing into a different binary —
    /// must NOT inherit the predecessor's budget.
    #[test]
    fn same_pid_different_exe_paths_are_isolated() {
        let limiter = IpcMessageRateLimiter::new();
        let t = Instant::now();
        // Same PID 12345, exe path A maxes out.
        for _ in 0..STANDARD_IPC_MSGS_PER_SEC {
            assert_eq!(
                limiter.check_at(PEER_PID, exe_a(), t),
                RateLimitDecision::Allowed
            );
        }
        assert!(matches!(
            limiter.check_at(PEER_PID, exe_a(), t),
            RateLimitDecision::Denied { .. }
        ));
        // Same PID 12345, DIFFERENT exe path — fresh bucket. A
        // re-exec is a distinct identity and gets its own budget.
        for _ in 0..STANDARD_IPC_MSGS_PER_SEC {
            assert_eq!(
                limiter.check_at(PEER_PID, exe_b(), t),
                RateLimitDecision::Allowed
            );
        }
    }

    #[test]
    fn sliding_window_recovers_after_pause() {
        let limiter = IpcMessageRateLimiter::new();
        let t0 = Instant::now();
        for _ in 0..STANDARD_IPC_MSGS_PER_SEC {
            limiter.check_at(PEER_PID, exe_a(), t0);
        }
        let t1 = t0 + IPC_RATE_WINDOW + Duration::from_millis(1);
        assert_eq!(
            limiter.check_at(PEER_PID, exe_a(), t1),
            RateLimitDecision::Allowed
        );
    }

    #[test]
    fn forget_clears_per_peer_state() {
        let limiter = IpcMessageRateLimiter::new();
        let t = Instant::now();
        for _ in 0..STANDARD_IPC_MSGS_PER_SEC {
            limiter.check_at(PEER_PID, exe_a(), t);
        }
        assert!(matches!(
            limiter.check_at(PEER_PID, exe_a(), t),
            RateLimitDecision::Denied { .. }
        ));
        limiter.forget(PEER_PID, exe_a());
        assert_eq!(
            limiter.check_at(PEER_PID, exe_a(), t),
            RateLimitDecision::Allowed
        );
    }

    /// `forget` is scoped to the exact `(pid, exe)` key — forgetting
    /// PID 12345 / exe A must NOT erase the bucket for PID 12345 /
    /// exe B.
    #[test]
    fn forget_does_not_cross_exe_boundary() {
        let limiter = IpcMessageRateLimiter::new();
        let t = Instant::now();
        // Fill both buckets to cap.
        for _ in 0..STANDARD_IPC_MSGS_PER_SEC {
            limiter.check_at(PEER_PID, exe_a(), t);
            limiter.check_at(PEER_PID, exe_b(), t);
        }
        // Forget only exe_a's bucket.
        limiter.forget(PEER_PID, exe_a());
        // exe_a now has a fresh budget.
        assert_eq!(
            limiter.check_at(PEER_PID, exe_a(), t),
            RateLimitDecision::Allowed
        );
        // exe_b is still capped.
        assert!(matches!(
            limiter.check_at(PEER_PID, exe_b(), t),
            RateLimitDecision::Denied { .. }
        ));
    }

    #[test]
    fn empty_entries_pruned_when_map_at_cap() {
        let limiter = IpcMessageRateLimiter::new();
        let t0 = Instant::now();
        for pid in 0..MAX_TRACKED_PEERS as u32 {
            limiter.check_at(pid, exe_a(), t0);
        }
        let t1 = t0 + IPC_RATE_WINDOW + Duration::from_millis(1);
        let new_pid = MAX_TRACKED_PEERS as u32 + 1;
        assert_eq!(
            limiter.check_at(new_pid, exe_a(), t1),
            RateLimitDecision::Allowed
        );
    }

    #[test]
    fn map_stays_bounded_when_churning_non_expired_peers() {
        let limiter = IpcMessageRateLimiter::new();
        let t = Instant::now();

        for pid in 0..MAX_TRACKED_PEERS as u32 {
            assert_eq!(
                limiter.check_at(pid, exe_a(), t),
                RateLimitDecision::Allowed
            );
        }
        assert_eq!(limiter.tracked_peer_count_for_test(), MAX_TRACKED_PEERS);

        for pid in MAX_TRACKED_PEERS as u32..(MAX_TRACKED_PEERS as u32 + 32) {
            assert_eq!(
                limiter.check_at(pid, exe_a(), t),
                RateLimitDecision::Allowed
            );
            assert_eq!(limiter.tracked_peer_count_for_test(), MAX_TRACKED_PEERS);
        }
    }
}
