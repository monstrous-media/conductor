// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-027 D16 — global concurrent-connection cap for the IPC
//! server.
//!
//! Closes part of audit Finding F-16 (resource-exhaustion DoS): a
//! same-user attacker who can connect to the daemon's Unix socket
//! (resolved by [`crate::daemon::state::get_socket_path`] —
//! platform-dependent: `$XDG_RUNTIME_DIR/conductor/`,
//! `~/.conductor/run/`, or platform Application Support) could
//! otherwise open thousands of connections, hold them idle, and
//! either OOM the daemon or starve other clients of
//! file-descriptor budget.
//!
//! This limiter caps the **total** number of concurrent connections
//! the server will hold open at any moment. Per-peer caps (the
//! companion mitigation in ADR-027 §D16) require peer credentials
//! from D1; this is the half that ships independently and matters
//! even without per-peer accounting because it bounds the daemon's
//! file-descriptor + memory footprint regardless of how many
//! distinct peers are connecting.
//!
//! The implementation is a thin wrapper over [`tokio::sync::Semaphore`]:
//!
//! - `try_acquire()` is non-blocking — when at-cap, returns `None`
//!   and the accept loop closes the new socket immediately.
//!   We deliberately don't queue / wait for a permit; queued
//!   connections look indistinguishable from active ones from the
//!   client side and would just defer the same OOM risk.
//! - The returned [`OwnedConnectionPermit`] is dropped when the
//!   handler task exits, returning the slot to the pool.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// ADR-027 §D16 default — `max_concurrent_connections = 32`.
/// Matches the documented policy. Tunable per-instance via
/// [`ConnectionLimiter::with_max`] for tests / future config.
pub const DEFAULT_MAX_CONCURRENT_CONNECTIONS: usize = 32;

/// Tracks the global concurrent-connection count for the IPC
/// server. Cheap to clone (it's an `Arc<Semaphore>` underneath),
/// so the accept loop and individual handler tasks can hold their
/// own handle without coordinating.
#[derive(Clone)]
pub struct ConnectionLimiter {
    permits: Arc<Semaphore>,
    max: usize,
}

/// A held permit. Drop releases the slot back to the limiter so
/// the next accept can proceed. The accept loop typically moves
/// this into the spawned handler task so the slot is released
/// exactly when the connection's task ends.
pub struct OwnedConnectionPermit {
    _inner: OwnedSemaphorePermit,
}

impl ConnectionLimiter {
    /// New limiter with the ADR-027 §D16 default cap.
    pub fn new() -> Self {
        Self::with_max(DEFAULT_MAX_CONCURRENT_CONNECTIONS)
    }

    /// New limiter with a custom cap. `max == 0` is permitted (the
    /// limiter then refuses every connection — useful for tests
    /// asserting the at-capacity behaviour).
    pub fn with_max(max: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max)),
            max,
        }
    }

    /// Try to acquire a slot. Returns `Some(permit)` when there's
    /// capacity, `None` when at-cap. The caller is expected to
    /// keep the permit alive for the lifetime of the connection
    /// — typically by moving it into the spawned handler task.
    ///
    /// Non-blocking by design: at-cap connections are CLOSED
    /// immediately, not queued, because queueing would defer the
    /// same memory pressure to a future moment without bounding
    /// the queue length.
    pub fn try_acquire(&self) -> Option<OwnedConnectionPermit> {
        match self.permits.clone().try_acquire_owned() {
            Ok(p) => Some(OwnedConnectionPermit { _inner: p }),
            Err(_) => None,
        }
    }

    /// Configured maximum (for logging / observability).
    pub fn max(&self) -> usize {
        self.max
    }

    /// Currently available slots. Mostly for tests + diagnostics —
    /// the live value can change between this read and any
    /// subsequent action.
    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }
}

impl Default for ConnectionLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Rate-limits the warn-level log emissions for IPC connection
/// refusals so a same-user attacker hammering the socket while
/// the server is at-cap can't drive arbitrary log volume.
///
/// Surfaced by Copilot review on PR #1028 (2026-05-02): the D16
/// per-refusal `warn!` line was a fresh DoS vector — disk/CPU
/// pressure from spammed warnings, even after the connection
/// itself was correctly refused.
///
/// Pattern: at most one warning per [`Self::DEFAULT_INTERVAL`]
/// (5s). Refusals during the suppression window are counted and
/// surfaced as a batched "suppressed=N" the next time the
/// window expires — operators see the real attempt rate without
/// every refusal hitting disk.
pub struct RefusalLogger {
    state: Mutex<RefusalState>,
    interval: Duration,
}

struct RefusalState {
    /// Monotonic-clock timestamp of the most recent emitted
    /// warning (via [`Instant::now`]). `None` means "never
    /// emitted yet"; the next refusal will warn. We use the
    /// monotonic clock rather than wall-clock so the rate-limit
    /// window can't be reset by NTP jumps / DST.
    last_warn: Option<Instant>,
    /// Refusals that have been recorded but suppressed since the
    /// most recent warning. Reset to 0 whenever a warning fires.
    suppressed: u64,
}

impl RefusalLogger {
    /// Window during which only one warning fires regardless of
    /// how many refusals come in. Aligned with the `tracing`
    /// ecosystem's typical "every-N-seconds" hint and short
    /// enough that operators see real-time attack rate during a
    /// flood, but long enough to absorb thousands of refusals.
    pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

    pub fn new() -> Self {
        Self::with_interval(Self::DEFAULT_INTERVAL)
    }

    pub fn with_interval(interval: Duration) -> Self {
        Self {
            state: Mutex::new(RefusalState {
                last_warn: None,
                suppressed: 0,
            }),
            interval,
        }
    }

    /// Records a refusal at the current monotonic-clock instant
    /// (`Instant::now`). Production callers use this; tests use
    /// [`Self::record_at`] for deterministic time control.
    pub fn record(&self) -> Option<u64> {
        self.record_at(Instant::now())
    }

    /// Records a refusal at the given monotonic-clock instant.
    /// Returns
    /// `Some(suppressed_count)` when the caller should emit a
    /// warn line now (with the count of *other* refusals
    /// suppressed since the last emission included), or `None`
    /// if this refusal is being silently absorbed into the
    /// running suppression counter.
    ///
    /// `suppressed_count == 0` means "this is the first refusal
    /// in a fresh window"; non-zero means N additional refusals
    /// were silently dropped during the rate-limit window before
    /// the window expired.
    pub fn record_at(&self, now: Instant) -> Option<u64> {
        // Lock contention here is fine: refusals only happen
        // when the daemon is at-cap, which is itself the rare
        // case. A short critical section reading two scalars and
        // writing one timestamp is fast enough.
        let mut state = match self.state.lock() {
            Ok(g) => g,
            // Treat poisoning as "lost the rate limit briefly"
            // and emit anyway — better than silently swallowing.
            Err(poisoned) => poisoned.into_inner(),
        };
        let due = match state.last_warn {
            None => true,
            // `saturating_duration_since` returns `Duration::ZERO`
            // when `now < last`, instead of panicking like
            // `duration_since` does (PR #1028 review, 2026-05-02).
            // A `now < last` reading is theoretically impossible
            // for `Instant` (it's monotonic), but a tokio runtime
            // restart, a containerised time-skew, or a future
            // mock-clock test could feed an out-of-order value;
            // saturate-to-zero means we just suppress the refusal
            // (treat as "less than the interval has passed") and
            // the daemon stays up. Same pattern as
            // `device_rate_limiter`.
            Some(last) => now.saturating_duration_since(last) >= self.interval,
        };
        if due {
            let batch = state.suppressed;
            state.last_warn = Some(now);
            state.suppressed = 0;
            Some(batch)
        } else {
            state.suppressed = state.suppressed.saturating_add(1);
            None
        }
    }
}

impl Default for RefusalLogger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn default_uses_adr_specified_cap() {
        let lim = ConnectionLimiter::new();
        assert_eq!(lim.max(), DEFAULT_MAX_CONCURRENT_CONNECTIONS);
        assert_eq!(lim.available(), DEFAULT_MAX_CONCURRENT_CONNECTIONS);
        assert_eq!(DEFAULT_MAX_CONCURRENT_CONNECTIONS, 32);
    }

    #[test]
    fn try_acquire_succeeds_under_cap() {
        let lim = ConnectionLimiter::with_max(3);
        let p1 = lim.try_acquire();
        let p2 = lim.try_acquire();
        let p3 = lim.try_acquire();
        assert!(p1.is_some());
        assert!(p2.is_some());
        assert!(p3.is_some());
        assert_eq!(lim.available(), 0);
    }

    #[test]
    fn try_acquire_returns_none_at_cap() {
        // The whole point of the limiter — at-capacity refusal is
        // what bounds the daemon's resource footprint. If this
        // assertion ever passes, an attacker can OOM the daemon
        // again.
        let lim = ConnectionLimiter::with_max(2);
        let _p1 = lim.try_acquire().expect("first");
        let _p2 = lim.try_acquire().expect("second");
        let p3 = lim.try_acquire();
        assert!(
            p3.is_none(),
            "third try_acquire at cap=2 must return None; got Some — \
             the limiter has stopped enforcing its cap and the daemon \
             can be OOMed by a flood of connections.",
        );
    }

    #[test]
    fn permit_release_lets_next_acquire_succeed() {
        // Slots are released as soon as the handler task drops
        // its OwnedConnectionPermit. Verifies the dropping
        // mechanism actually frees the slot (and isn't keeping a
        // hidden reference around).
        let lim = ConnectionLimiter::with_max(1);
        {
            let _p1 = lim.try_acquire().expect("first acquire");
            assert!(lim.try_acquire().is_none(), "at cap");
        } // _p1 dropped here
        let p2 = lim.try_acquire();
        assert!(
            p2.is_some(),
            "after dropping the only permit, a fresh try_acquire \
             must succeed — got None, suggesting the slot wasn't \
             actually released.",
        );
    }

    #[test]
    fn zero_cap_refuses_every_connection() {
        // Edge case: cap=0 is legal and immediately refuses
        // everything. Used in tests asserting "at capacity"
        // behaviour without needing to first exhaust the slots.
        let lim = ConnectionLimiter::with_max(0);
        assert!(lim.try_acquire().is_none());
        assert!(lim.try_acquire().is_none());
    }

    #[test]
    fn limiter_is_clonable_and_shares_state() {
        // Accept loop hands out clones to spawned handler tasks;
        // they all need to see the same permit pool. If clone
        // gave each caller its own pool, the cap would be per-
        // task instead of per-server.
        let lim = ConnectionLimiter::with_max(2);
        let lim_b = lim.clone();
        let _p1 = lim.try_acquire().expect("a1");
        let _p2 = lim_b.try_acquire().expect("b1");
        assert!(
            lim.try_acquire().is_none(),
            "clones must share the same permit pool; got Some — the \
             cap is being applied per-clone, defeating the purpose.",
        );
        assert!(lim_b.try_acquire().is_none());
    }

    // ─── ADR-027 D16 follow-up: log-flood mitigation ──────────
    // PR #1028 review (Copilot, 2026-05-02): every refused
    // connection logged a warn! line. A same-user attacker
    // hammering the socket while at-cap turns the mitigation into
    // a log-flood vector — disk pressure, journald CPU. These
    // tests pin the rate-limit semantics: at most one warn per
    // window, with the suppressed count surfaced when the next
    // warning eventually fires.

    #[test]
    fn refusal_logger_first_record_emits_with_zero_suppressed() {
        let logger = RefusalLogger::with_interval(Duration::from_secs(5));
        let t0 = Instant::now();
        assert_eq!(
            logger.record_at(t0),
            Some(0),
            "first refusal must produce a warn-now signal with \
             zero suppressed-count (no prior refusals to batch)",
        );
    }

    #[test]
    fn refusal_logger_suppresses_within_window() {
        let logger = RefusalLogger::with_interval(Duration::from_secs(5));
        let t0 = Instant::now();
        let _ = logger.record_at(t0);
        // 1ms later, well inside the 5s window:
        assert_eq!(
            logger.record_at(t0 + Duration::from_millis(1)),
            None,
            "second refusal inside the rate-limit window must be \
             suppressed (return None) — otherwise an attacker can \
             still drive arbitrary log volume.",
        );
        // 4.999s later, still inside:
        assert_eq!(logger.record_at(t0 + Duration::from_millis(4_999)), None,);
    }

    #[test]
    fn refusal_logger_emits_after_window_with_batched_count() {
        let logger = RefusalLogger::with_interval(Duration::from_secs(5));
        let t0 = Instant::now();
        logger.record_at(t0); // first warn
        logger.record_at(t0 + Duration::from_secs(1)); // suppressed
        logger.record_at(t0 + Duration::from_secs(2)); // suppressed
        logger.record_at(t0 + Duration::from_secs(3)); // suppressed
        assert_eq!(
            logger.record_at(t0 + Duration::from_secs(6)),
            Some(3),
            "the warn that fires after the window expires must \
             surface the batched suppressed-count so operators \
             can see the real attempt rate even though the logs \
             are sparse.",
        );
    }

    #[test]
    fn refusal_logger_resets_count_after_emission() {
        let logger = RefusalLogger::with_interval(Duration::from_secs(5));
        let t0 = Instant::now();
        logger.record_at(t0);
        logger.record_at(t0 + Duration::from_secs(1)); // suppressed
        // window expires, this one emits with count=1:
        let _ = logger.record_at(t0 + Duration::from_secs(6));
        // next-window kickoff — count must have reset to zero:
        assert_eq!(
            logger.record_at(t0 + Duration::from_secs(12)),
            Some(0),
            "after a warn fires, the suppressed-count must reset \
             — otherwise it grows unboundedly and confuses \
             operators reading subsequent warnings.",
        );
    }

    #[test]
    fn refusal_logger_record_uses_current_clock() {
        // Smoke test for the production API: `record()` (no
        // explicit time) and `record_at(Instant::now())` MUST
        // route through the same code path.
        //
        // PR #1028 review (2026-05-02 12:47): the previous
        // version used a 10ms interval + real `thread::sleep`,
        // which was timing-fragile under loaded CI (a >10ms
        // scheduling delay between two "immediate" calls would
        // make the second call emit, intermittently failing the
        // suppression assertion). Re-cast as a deterministic
        // assertion: with a long interval, two back-to-back
        // record() calls must produce one Some and one None,
        // regardless of how slow the runtime is.
        let logger = RefusalLogger::with_interval(Duration::from_secs(60));
        let first = logger.record();
        let second = logger.record();
        assert!(
            first.is_some(),
            "record() must emit on first call from a fresh logger",
        );
        assert!(
            second.is_none(),
            "record() must suppress an immediately-following \
             call (interval is 60s; back-to-back can't escape \
             the window even on the slowest CI)",
        );
    }

    #[test]
    fn refusal_logger_is_send_sync_for_use_in_ipc_server() {
        // Static assert: the IpcServer field needs Send+Sync to
        // be referenced from inside the accept loop and any
        // diagnostics endpoints. Compile-time check.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RefusalLogger>();
    }

    #[tokio::test]
    async fn permit_can_move_into_spawned_task() {
        // Production pattern: the accept loop calls try_acquire
        // and moves the permit into a tokio::spawn body so the
        // slot stays held for the lifetime of the connection.
        // Permit must be Send + 'static to support that move.
        let lim = ConnectionLimiter::with_max(1);
        let permit = lim.try_acquire().expect("acquire");

        let handle = tokio::spawn(async move {
            let _held = permit;
            tokio::task::yield_now().await;
        });
        handle.await.expect("task joins");

        // Permit dropped with task; cap is restored.
        assert_eq!(lim.available(), 1);
    }
}
