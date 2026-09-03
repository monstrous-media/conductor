// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Uniform inbound extension point for the unified event pump (ADR-039 §4.1).
//!
//! Every protocol that can *receive* events implements [`InputSource`]. The
//! trait lives here in `conductor-daemon` — **not** `conductor-core` (ADR-039
//! R4): its eventual pump-facing channel handoff (`start(tx)`)
//! is typed on a `tokio::sync::mpsc::Sender`, and `core` deliberately keeps
//! `tokio` optional to remain a runtime-free pure engine. The pure *data* types
//! it deals in ([`conductor_core::events::ProtocolEvent`] and friends) stay in
//! `core`.
//!
//! ## Scope (substrate + pump)
//!
//! The trait ships four **pure, behaviour-preserving** methods that are
//! testable without a live device or a running pump: [`InputSource::protocol`],
//! [`InputSource::source_alias`], [`InputSource::shutdown`], and
//! [`InputSource::metrics`].
//!
//! `start(tx)` adds the push-compatible sink handoff [`InputSource::start`] alongside
//! the single-lane shed-load substrate ([`enqueue`] + [`spawn_input_pump`])
//! that both `start(tx)` and the live MIDI/HID converters now share. The
//! existing managers are push-shaped (a midir OS callback / a gilrs polling
//! thread each owns its `Sender` and `try_send`s asynchronously), so `start`
//! **wraps** that existing delivery — the push task drains the manager's
//! intermediate stream and lands events on the pump via the §4.3 policy
//! (`try_send`; drop-newest on a full channel; never block). No manager
//! rewrite; behaviour-preserving.
//!
//! The two-lane (`input_tx`/`net_tx`) weighted-fair / deficit-round-robin
//! dispatch in spec §4.3 is **deferred to ADR-039-A** (R4): only
//! `ProtocolEvent::Input` (MIDI/HID) producers exist today, so a second lane
//! would be untestable dead substrate. `start(tx)` keeps the single unified
//! `DeviceEvent<ProtocolEvent>` channel and adds the backpressure + shutdown
//! quiesce that 039-A's network lane will build on.

use conductor_core::config::types::ConnectorProtocol;
use conductor_core::events::{InputEvent, ProtocolEvent};
use conductor_core::identity::{DeviceEvent, DeviceId};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::debug;

/// Point-in-time snapshot of a source's baseline observability counters
/// (ADR-039 R2 P1). Returned by [`InputSource::metrics`].
///
/// Debugging a unified cross-protocol pipeline is impossible without these:
/// how many events a source ingested, how many it shed under backpressure, how
/// many errored, and when it was last active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputSourceMetrics {
    /// Events successfully observed/forwarded by this source.
    pub events_in: u64,
    /// Events shed under backpressure (full channel — §4.3 drop-newest).
    pub dropped: u64,
    /// Send/parse/transport errors observed by this source.
    pub errors: u64,
    /// Timestamp of the most recent activity, or `None` if the source has not
    /// observed any event yet.
    pub last_activity: Option<Instant>,
}

#[derive(Debug)]
struct MetricsInner {
    /// Base instant the handle was created; `last_activity` is stored as micros
    /// elapsed from here so it can live in a lock-free `AtomicU64`.
    start: Instant,
    events_in: AtomicU64,
    dropped: AtomicU64,
    errors: AtomicU64,
    /// Micros since `start` of last activity. `0` sentinel = no activity yet.
    last_activity_micros: AtomicU64,
}

/// Cheaply-cloneable, lock-free handle to a source's metrics counters.
///
/// The source exposes a clone of this so the push path (`start(tx)`) can
/// call [`record_event`](Self::record_event) /
/// [`record_dropped`](Self::record_dropped) /
/// [`record_error`](Self::record_error) from inside its callback/poll loop
/// without taking a lock on the hot path. The counters exist and are
/// readable via [`snapshot`](Self::snapshot); they begin incrementing once the
/// source owns the send path.
#[derive(Debug, Clone)]
pub struct InputSourceMetricsHandle(Arc<MetricsInner>);

impl InputSourceMetricsHandle {
    /// Create a fresh, zeroed metrics handle.
    pub fn new() -> Self {
        InputSourceMetricsHandle(Arc::new(MetricsInner {
            start: Instant::now(),
            events_in: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            last_activity_micros: AtomicU64::new(0),
        }))
    }

    fn touch(&self) {
        // Saturating conversion: `as_micros()` is u128; micros-since-start can't
        // realistically exceed u64 (~584k years), but clamp to u64::MAX rather
        // than truncate/wrap. Never store the `0` sentinel for a real activity at
        // t=0 (hence `.max(1)`).
        let micros = u64::try_from(self.0.start.elapsed().as_micros())
            .unwrap_or(u64::MAX)
            .max(1);
        // `fetch_max`, not `store`: the handle is `Clone` and may be touched from
        // more than one thread; a slower thread carrying an older timestamp must
        // not clobber a newer one. `fetch_max` keeps `last_activity` monotonic.
        // `Relaxed` is sufficient — each counter is independent (no cross-field
        // ordering invariant) and a metrics snapshot tolerates eventual
        // visibility; we only need per-field atomicity, which RMW provides.
        self.0
            .last_activity_micros
            .fetch_max(micros, Ordering::Relaxed);
    }

    /// Record one successfully observed event.
    pub fn record_event(&self) {
        self.0.events_in.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    /// Record one event shed under backpressure.
    pub fn record_dropped(&self) {
        self.0.dropped.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    /// Record one transport/parse error.
    pub fn record_error(&self) {
        self.0.errors.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    /// Read a consistent-enough snapshot of the counters.
    pub fn snapshot(&self) -> InputSourceMetrics {
        let micros = self.0.last_activity_micros.load(Ordering::Relaxed);
        // `checked_add`, not `start + Duration`: if `micros` ever saturated to
        // u64::MAX (touch()'s overflow fallback), `Instant + Duration` would
        // panic. checked_add degrades to `None` instead of crashing — the same
        // shape as "no activity yet", which is the only sane reading of an
        // impossibly-distant timestamp.
        let last_activity = (micros != 0)
            .then(|| {
                self.0
                    .start
                    .checked_add(std::time::Duration::from_micros(micros))
            })
            .flatten();
        InputSourceMetrics {
            events_in: self.0.events_in.load(Ordering::Relaxed),
            dropped: self.0.dropped.load(Ordering::Relaxed),
            errors: self.0.errors.load(Ordering::Relaxed),
            last_activity,
        }
    }
}

impl Default for InputSourceMetricsHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// Uniform inbound extension point. Every protocol that can receive events
/// implements this.
///
/// See the [module docs](self) for why the pump-facing `start(tx)` method is
/// added later and why this trait lives in the daemon, not core.
pub trait InputSource: Send {
    /// The protocol this source speaks.
    fn protocol(&self) -> ConnectorProtocol;

    /// Stable alias of the endpoint this source serves (the route `from` key).
    fn source_alias(&self) -> &str;

    /// Tear down socket/handle cleanly (CFRunLoopStop for MIDI; close UDP for
    /// network sources). Must be idempotent.
    fn shutdown(&mut self);

    /// Baseline observability snapshot — required of every source (R2 P1).
    fn metrics(&self) -> InputSourceMetrics;

    /// Begin pushing this source's events into the unified pump channel using
    /// the §4.3 shed-load policy (`try_send`; drop-newest on a full channel;
    /// never block). The source owns `tx` for its lifetime — the push model
    /// that matches midir callbacks / gilrs polling.
    ///
    /// Returns `Err` if the source is not in a startable state — e.g. its
    /// hardware intermediate channel has not been established yet (call the
    /// source's `connect_*` first).
    fn start(&mut self, tx: mpsc::Sender<DeviceEvent<ProtocolEvent>>) -> Result<(), String>;
}

/// Outcome of a single non-blocking [`enqueue`] into the unified pump channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnqueueOutcome {
    /// Event accepted by the channel; `events_in` was incremented.
    Sent,
    /// Channel was full; the (newest) event was shed and `dropped` incremented.
    Dropped,
    /// Channel is closed (the pump is gone); the caller should stop pushing.
    Closed,
}

/// Push one event into the pump channel without ever blocking — the ADR-039
/// §4.3 shed-load policy, in one place so every source and converter applies it
/// identically.
///
/// - `try_send` succeeds → [`record_event`](InputSourceMetricsHandle::record_event),
///   [`Sent`](EnqueueOutcome::Sent).
/// - channel full → **drop the newest** event +
///   [`record_dropped`](InputSourceMetricsHandle::record_dropped),
///   [`Dropped`](EnqueueOutcome::Dropped). An Art-Net packet storm (039-A) must
///   never stall local MIDI, so we shed rather than block.
/// - channel closed → [`Closed`](EnqueueOutcome::Closed); no metric recorded
///   (the pump went away, not the source).
pub(crate) fn enqueue(
    tx: &mpsc::Sender<DeviceEvent<ProtocolEvent>>,
    metrics: &InputSourceMetricsHandle,
    event: DeviceEvent<ProtocolEvent>,
) -> EnqueueOutcome {
    match tx.try_send(event) {
        Ok(()) => {
            metrics.record_event();
            EnqueueOutcome::Sent
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            metrics.record_dropped();
            EnqueueOutcome::Dropped
        }
        Err(mpsc::error::TrySendError::Closed(_)) => EnqueueOutcome::Closed,
    }
}

/// Spawn the push task that drains a source's intermediate event stream and
/// lands each event on the unified pump channel via the §4.3 [`enqueue`]
/// policy. This is the concrete realization of [`InputSource::start`] for the
/// push-shaped midir/gilrs sources: the wrapped manager keeps its
/// existing OS callback / poll loop feeding `source_rx`; this task is the
/// non-blocking push side that the old blocking `send().await` converters
/// became.
///
/// `E: Into<InputEvent>` covers both the MIDI (`MidiEvent`) and HID
/// (`InputEvent`) intermediate streams with one body. The task exits when the
/// source stream ends (manager disconnected → `source_rx` closes) or the pump
/// channel closes ([`EnqueueOutcome::Closed`] — graceful).
///
/// **Lifetime contract (detached task).** The returned task **owns** everything
/// it touches — `source_rx`, a `pump_tx` clone, and an `Arc`-backed `metrics`
/// clone — so it holds **no borrow of the `InputManager`/source that spawned
/// it** and is safe to outlive them (the caller's `metrics` clone and the
/// task's clone are independent handles onto the same `Arc`; dropping the
/// manager touches neither). It cannot leak: it self-terminates the moment
/// `source_rx` closes, which the manager's teardown (`disconnect`/`shutdown`,
/// dropping the OS callback / poll thread that holds the sender) triggers. This
/// is the same detached-task shape the earlier blocking converters used,
/// minus the blocking `send().await`.
pub(crate) fn spawn_input_pump<E>(
    device_id: DeviceId,
    mut source_rx: mpsc::Receiver<E>,
    pump_tx: mpsc::Sender<DeviceEvent<ProtocolEvent>>,
    metrics: InputSourceMetricsHandle,
) -> tokio::task::JoinHandle<()>
where
    E: Into<InputEvent> + Send + 'static,
{
    tokio::spawn(async move {
        while let Some(raw) = source_rx.recv().await {
            let input: InputEvent = raw.into();
            let event = DeviceEvent::new(device_id.clone(), ProtocolEvent::Input(input));
            if enqueue(&pump_tx, &metrics, event) == EnqueueOutcome::Closed {
                debug!(device_id = %device_id, "Pump channel closed; input push task exiting");
                break;
            }
        }
        debug!(device_id = %device_id, "Input push task exited (source stream ended)");
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_input() -> InputEvent {
        InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(0),
            time: Instant::now(),
        }
    }

    fn sample_event() -> DeviceEvent<ProtocolEvent> {
        DeviceEvent::new(DeviceId::raw("t"), ProtocolEvent::Input(sample_input()))
    }

    // ----------------------------------------------------------------
    // ADR-039 §4.3 — shed-load enqueue policy
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn enqueue_sent_increments_events_in() {
        let (tx, mut rx) = mpsc::channel(4);
        let m = InputSourceMetricsHandle::new();
        assert_eq!(enqueue(&tx, &m, sample_event()), EnqueueOutcome::Sent);
        let snap = m.snapshot();
        assert_eq!(snap.events_in, 1);
        assert_eq!(snap.dropped, 0);
        assert!(rx.recv().await.is_some(), "event reaches the channel");
    }

    #[tokio::test]
    async fn enqueue_full_drops_newest_and_increments_dropped() {
        // Capacity 1, never drained: the second event has nowhere to go and is
        // shed (drop-newest) rather than blocking.
        let (tx, _rx) = mpsc::channel(1);
        let m = InputSourceMetricsHandle::new();
        assert_eq!(enqueue(&tx, &m, sample_event()), EnqueueOutcome::Sent);
        assert_eq!(enqueue(&tx, &m, sample_event()), EnqueueOutcome::Dropped);
        let snap = m.snapshot();
        assert_eq!(snap.events_in, 1, "only the first event fit");
        assert_eq!(snap.dropped, 1, "the newest was shed");
    }

    #[tokio::test]
    async fn enqueue_closed_is_graceful_and_records_nothing() {
        let (tx, rx) = mpsc::channel(4);
        drop(rx);
        let m = InputSourceMetricsHandle::new();
        assert_eq!(enqueue(&tx, &m, sample_event()), EnqueueOutcome::Closed);
        assert_eq!(
            m.snapshot(),
            InputSourceMetrics::default(),
            "a closed pump is not the source's fault — no metric moves"
        );
    }

    #[tokio::test]
    async fn spawn_input_pump_round_trips_events_and_counts_them() {
        let (src_tx, src_rx) = mpsc::channel::<InputEvent>(4);
        let (pump_tx, mut pump_rx) = mpsc::channel(4);
        let m = InputSourceMetricsHandle::new();
        let dev = DeviceId::raw("pads");
        let handle = spawn_input_pump(dev.clone(), src_rx, pump_tx, m.clone());

        src_tx.send(sample_input()).await.unwrap();
        let got = pump_rx.recv().await.expect("event reaches the pump");
        assert_eq!(got.device_id(), &dev);
        assert!(
            matches!(got.event(), ProtocolEvent::Input(_)),
            "source tags events as ProtocolEvent::Input"
        );

        drop(src_tx); // end the source stream → task exits cleanly
        handle.await.unwrap();
        assert_eq!(m.snapshot().events_in, 1);
    }

    #[tokio::test]
    async fn spawn_input_pump_sheds_when_pump_is_full_without_blocking() {
        // Pump capacity 1, never drained: one event fills the slot, the rest are
        // shed (drop-newest). The push task must keep draining the source and
        // terminate — proving it never blocks on the full pump.
        let (src_tx, src_rx) = mpsc::channel::<InputEvent>(8);
        let (pump_tx, _pump_rx) = mpsc::channel(1);
        let m = InputSourceMetricsHandle::new();
        let handle = spawn_input_pump(DeviceId::raw("pads"), src_rx, pump_tx, m.clone());

        for _ in 0..5 {
            src_tx.send(sample_input()).await.unwrap();
        }
        drop(src_tx);
        handle.await.unwrap();

        let snap = m.snapshot();
        assert_eq!(snap.events_in, 1, "one event fit the pump slot");
        assert_eq!(snap.dropped, 4, "the rest were shed (drop-newest)");
    }

    #[test]
    fn fresh_metrics_are_zeroed() {
        let h = InputSourceMetricsHandle::new();
        let m = h.snapshot();
        assert_eq!(m.events_in, 0);
        assert_eq!(m.dropped, 0);
        assert_eq!(m.errors, 0);
        assert!(m.last_activity.is_none());
        assert_eq!(m, InputSourceMetrics::default());
    }

    #[test]
    fn counters_increment_and_set_last_activity() {
        let h = InputSourceMetricsHandle::new();
        h.record_event();
        h.record_event();
        h.record_dropped();
        h.record_error();
        let m = h.snapshot();
        assert_eq!(m.events_in, 2);
        assert_eq!(m.dropped, 1);
        assert_eq!(m.errors, 1);
        assert!(
            m.last_activity.is_some(),
            "activity should set last_activity"
        );
    }

    #[test]
    fn handle_clone_shares_counters() {
        let h = InputSourceMetricsHandle::new();
        let clone = h.clone();
        clone.record_event();
        // The original observes the clone's increment — same underlying Arc.
        assert_eq!(h.snapshot().events_in, 1);
    }

    #[test]
    fn record_error_only_sets_last_activity_without_touching_other_counters() {
        // Edge path: an error with no events/drops still stamps last_activity.
        let h = InputSourceMetricsHandle::new();
        h.record_error();
        let m = h.snapshot();
        assert_eq!(m.events_in, 0);
        assert_eq!(m.dropped, 0);
        assert_eq!(m.errors, 1);
        assert!(m.last_activity.is_some());
    }

    #[test]
    fn record_dropped_only_sets_last_activity_without_touching_other_counters() {
        // Edge path: backpressure shedding with no successful events.
        let h = InputSourceMetricsHandle::new();
        h.record_dropped();
        let m = h.snapshot();
        assert_eq!(m.events_in, 0);
        assert_eq!(m.dropped, 1);
        assert_eq!(m.errors, 0);
        assert!(m.last_activity.is_some());
    }

    #[test]
    fn default_handle_is_zeroed() {
        // The Default impl must match a freshly-constructed, never-touched handle.
        assert_eq!(
            InputSourceMetricsHandle::default().snapshot(),
            InputSourceMetrics::default()
        );
    }

    #[test]
    fn concurrent_records_lose_no_updates_and_keep_last_activity_monotonic() {
        // The handle is Clone and may be touched from multiple
        // threads. `fetch_add` must not lose counter updates, and `fetch_max`
        // must keep `last_activity` monotonic (no older timestamp clobbering a
        // newer one) — both deterministic, so this test is not timing-flaky.
        use std::thread;
        const THREADS: usize = 8;
        const PER: usize = 1000;
        let h = InputSourceMetricsHandle::new();
        let mut joins = Vec::new();
        for _ in 0..THREADS {
            let h = h.clone();
            joins.push(thread::spawn(move || {
                let mut last = None;
                for _ in 0..PER {
                    h.record_event();
                    let cur = h.snapshot().last_activity;
                    if let (Some(p), Some(c)) = (last, cur) {
                        assert!(c >= p, "last_activity regressed under concurrency");
                    }
                    last = cur;
                }
            }));
        }
        for j in joins {
            j.join().unwrap();
        }
        assert_eq!(h.snapshot().events_in, (THREADS * PER) as u64);
    }
}
