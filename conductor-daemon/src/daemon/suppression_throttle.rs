// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Coalesce per-kind `midi_*_suppressed` MonitorEvents.
//!
//! From the stuck-notes investigation (symptom (i)): the recursion
//! guard emits one `midi_echo_suppressed` / `midi_cascade_suppressed`
//! MonitorEvent for **every** suppressed input event. A tight feedback loop or
//! chord storm therefore floods the monitor broadcast stream 1:1, saturating
//! the GUI events panel and adding needless IPC/serialisation work.
//!
//! This throttle collapses that flood: at most ONE summary MonitorEvent per
//! kind per [`SuppressionThrottle::DEFAULT_INTERVAL`] window, carrying the count
//! of events coalesced since the last emission. It mirrors
//! [`crate::daemon::connection_limiter::RefusalLogger`]'s batched "suppressed=N"
//! pattern (monotonic clock so NTP/DST jumps can't reset the window).
//!
//! IMPORTANT: this changes ONLY the telemetry emission cadence. The actual
//! suppression decision (drop the event) is unchanged and happens regardless of
//! whether a summary is emitted — the caller still `return`s after recording.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Per-kind coalescer for suppressed-MIDI MonitorEvents. Keyed by the kind's
/// `&'static str` (`"midi_echo_suppressed"` / `"midi_cascade_suppressed"`), so
/// the map is bounded to the small fixed set of suppression kinds — no
/// unbounded growth on the hot path.
pub struct SuppressionThrottle {
    state: Mutex<HashMap<&'static str, KindState>>,
    interval: Duration,
}

#[derive(Default)]
struct KindState {
    /// Monotonic timestamp of the most recent emitted summary for this kind.
    /// `None` = never emitted; the next suppression emits immediately.
    last_emit: Option<Instant>,
    /// Suppressions of this kind absorbed (not emitted) since `last_emit`.
    /// Reset to 0 each time a summary fires.
    absorbed: u64,
}

impl SuppressionThrottle {
    /// One summary per kind per window. 1s keeps the events panel informative
    /// in near-real-time while collapsing 1:1 floods of thousands of events.
    pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);

    pub fn new() -> Self {
        Self::with_interval(Self::DEFAULT_INTERVAL)
    }

    pub fn with_interval(interval: Duration) -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
            interval,
        }
    }

    /// Production entry: record a suppression of `kind` at the current
    /// monotonic instant. See [`Self::record_at`].
    pub fn record(&self, kind: &'static str) -> Option<u64> {
        self.record_at(kind, Instant::now())
    }

    /// Record a suppression of `kind` at monotonic instant `now`.
    ///
    /// Returns `Some(absorbed)` when the caller SHOULD emit a summary
    /// MonitorEvent now: `absorbed` is the number of OTHER suppressions of this
    /// kind silently coalesced since the last emission, so the summary
    /// represents `absorbed + 1` events including this one. `Some(0)` means
    /// "first suppression in a fresh window".
    ///
    /// Returns `None` when this suppression is absorbed into the running
    /// counter (the window is still open) — the caller emits nothing.
    pub fn record_at(&self, kind: &'static str, now: Instant) -> Option<u64> {
        // Short critical section (HashMap lookup + a couple of scalar writes).
        // On poisoning, recover the inner state and proceed with the normal
        // due/absorb logic below (so it may still emit or absorb depending on
        // the window) — a poisoned lock must never panic the MIDI input hot
        // path. Same recover-and-continue stance as RefusalLogger.
        let mut state = match self.state.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry = state.entry(kind).or_default();
        let due = match entry.last_emit {
            None => true,
            // saturating: a non-monotonic `now < last` (mock clock, runtime
            // restart) is treated as "window still open" rather than panicking.
            Some(last) => now.saturating_duration_since(last) >= self.interval,
        };
        if due {
            let absorbed = entry.absorbed;
            entry.last_emit = Some(now);
            entry.absorbed = 0;
            Some(absorbed)
        } else {
            // saturating_add: overflow is unreachable in practice (would need
            // 2^64 suppressions inside one window) but matches RefusalLogger and
            // can't wrap in release builds.
            entry.absorbed = entry.absorbed.saturating_add(1);
            None
        }
    }
}

impl Default for SuppressionThrottle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECHO: &str = "midi_echo_suppressed";
    const CASCADE: &str = "midi_cascade_suppressed";

    #[test]
    fn first_suppression_emits_with_zero_absorbed() {
        let t = SuppressionThrottle::with_interval(Duration::from_secs(1));
        let t0 = Instant::now();
        assert_eq!(
            t.record_at(ECHO, t0),
            Some(0),
            "the first suppression in a fresh window must emit a summary (0 prior absorbed)"
        );
    }

    #[test]
    fn flood_within_window_is_coalesced_then_summarised() {
        let t = SuppressionThrottle::with_interval(Duration::from_secs(1));
        let t0 = Instant::now();
        // Storm: first emits, the rest are absorbed silently within the window.
        assert_eq!(t.record_at(ECHO, t0), Some(0));
        for i in 1..=999u64 {
            assert_eq!(
                t.record_at(ECHO, t0 + Duration::from_millis(i)),
                None,
                "suppressions inside the window must be absorbed (no 1:1 flood)"
            );
        }
        // After the window elapses, the next suppression emits a summary
        // accounting for the 999 absorbed since the last emit.
        assert_eq!(
            t.record_at(ECHO, t0 + Duration::from_millis(1500)),
            Some(999),
            "the first suppression after the window must summarise the absorbed flood"
        );
    }

    #[test]
    fn kinds_are_tracked_independently() {
        let t = SuppressionThrottle::with_interval(Duration::from_secs(1));
        let t0 = Instant::now();
        // Each kind gets its own fresh-window emit; one kind's window does not
        // consume the other's.
        assert_eq!(t.record_at(ECHO, t0), Some(0));
        assert_eq!(t.record_at(CASCADE, t0), Some(0));
        // Both now coalesce independently.
        assert_eq!(t.record_at(ECHO, t0 + Duration::from_millis(10)), None);
        assert_eq!(t.record_at(CASCADE, t0 + Duration::from_millis(10)), None);
    }

    #[test]
    fn non_monotonic_now_does_not_panic_and_keeps_window_open() {
        let t = SuppressionThrottle::with_interval(Duration::from_secs(1));
        let t0 = Instant::now() + Duration::from_secs(10);
        assert_eq!(t.record_at(ECHO, t0), Some(0));
        // An earlier `now` (clock skew) saturates to ZERO elapsed → absorbed.
        assert_eq!(t.record_at(ECHO, t0 - Duration::from_secs(5)), None);
    }
}
