// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! SysEx Universal Device Identity probe coordinator (ADR-026 Phase 1.A).
//!
//! Runs on top of [`SysExIdentity`]: orchestrates the request/reply
//! protocol, serialises probes globally, rate-limits per-port, and caches
//! identified devices for the daemon session. Tokio-free — uses
//! `std::sync` + `crossbeam_channel` so it drops into both sync and
//! async callers. Async callers wrap with `tokio::task::spawn_blocking`.
//!
//! Flow:
//!
//! 1. Caller invokes [`ProbeCoordinator::probe`] with a port name and a
//!    closure that writes bytes to the paired MIDI output.
//! 2. The coordinator takes a global lock (one in-flight probe at a time
//!    across the whole daemon — ADR-026 D1), records a pending slot
//!    keyed on port name, and invokes the send closure with the 6-byte
//!    [`IDENTITY_REQUEST`] sequence.
//! 3. The daemon's MIDI ingress filter calls [`ProbeCoordinator::observe_reply`]
//!    on every SysEx byte sequence it receives. When the bytes parse as
//!    an Identity Reply AND a pending probe exists for that port, the
//!    coordinator delivers the parsed [`SysExIdentity`] to the waiter.
//! 4. The waiter blocks on a crossbeam channel up to `default_timeout`
//!    (1s by default). Reply → `Ok(ProbeResult::Identified)`;
//!    silence → `Ok(ProbeResult::NoReply)`. Sync-start failures
//!    (rate-limit hit, send_fn error, etc.) surface as
//!    `Err(ProbeStartError)`. See [`ProbeResult`] / [`ProbeStartError`].
//!
//! Rate limiting (60s per-port by default) prevents accidental hammering
//! from probe-on-connect / hot-plug churn. The global lock ensures two
//! devices on the same interface don't see interleaved broadcast probes.

use crate::device_intelligence::sysex_identity::{
    IDENTITY_REQUEST, SysExIdentity, is_identity_reply_shape,
};
use crossbeam_channel::bounded;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Abstract side-observing hook for SysEx Identity Reply delivery.
///
/// `ProbeCoordinator` is the production implementation; daemon callbacks
/// take `Option<&dyn SysExObserver>` so tests can inject a counting /
/// recording observer and verify that gating logic (e.g. "non-SysEx
/// must not reach the hook") actually gates, rather than relying on
/// the coordinator's own defensive early-return to mask a broken gate.
///
/// Implementors MUST NOT panic and MUST NOT signal "consume this event"
/// via any side-channel (ADR-026 D9 side-observing contract).
pub trait SysExObserver: Send + Sync {
    /// Observe a SysEx byte sequence received on `port_name`. See
    /// [`ProbeCoordinator::observe_reply`] for the production semantics.
    fn observe_reply(&self, port_name: &str, bytes: &[u8]);
}

impl SysExObserver for ProbeCoordinator {
    fn observe_reply(&self, port_name: &str, bytes: &[u8]) {
        ProbeCoordinator::observe_reply(self, port_name, bytes);
    }
}

/// Default in-flight probe timeout (ADR-026 D2).
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1000);
/// Default per-port rate-limit floor (ADR-026 D1).
const DEFAULT_RATE_LIMIT: Duration = Duration::from_secs(60);

/// Clock abstraction. Production uses [`SystemClock`]; tests inject
/// [`FakeClock`] so rate-limit windows can be exercised without sleeping.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn unix_now_ms(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn unix_now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Confidence label for a cached SysEx identity. Phase 3.A introduces
/// the type with a single produced variant (`DirectPairedPort`) so
/// downstream code (PortInfo, MCP cache reads, GUI badge) can carry
/// the signal without further type churn when Phase 3.B adds
/// cross-port reply correlation.
///
/// - `DirectPairedPort`: the probe was sent to a single paired output
///   and the reply landed on the expected input — the typical
///   single-cable case. Safe to auto-promote a binding from this.
/// - `SharedRoute`: the same Identity Reply was observed on multiple
///   input ports within a short window, suggesting a thru-box or
///   merger. NOT auto-promoted — caller surfaces a confirmation UI.
///   *Detection logic ships in Phase 3.B; the variant exists in 3.A
///   so the type and downstream wiring don't churn again.*
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IdentityConfidence {
    DirectPairedPort,
    SharedRoute,
}

/// Successful outcome of a probe attempt — i.e. the wait completed
/// normally (with or without a reply). Phase 3.B.1 splits the legacy
/// flat `ProbeOutcome` enum into [`ProbeResult`] (this type, returned
/// in the `Ok` branch) and [`ProbeStartError`] (sync-start failures,
/// returned in `Err`). The split lets callers distinguish "we tried
/// and the device didn't reply" (data) from "we never got to send"
/// (operational error) without pattern-matching on a single enum.
///
/// `MultipleIdentified` is reserved for Phase 3.B.2 — the coordinator
/// is still first-reply-wins today, so this variant is never produced
/// by `probe()`. It exists in the enum so downstream consumers
/// (Phase 3.C auto-promote, Phase 3.D GUI badge) can match against
/// it ahead of the correlation logic landing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status")]
pub enum ProbeResult {
    Identified {
        identity: SysExIdentity,
        /// Confidence label for the observation. `DirectPairedPort`
        /// means the reply landed on the expected input — safe for
        /// auto-promotion. `SharedRoute` means the only reply
        /// observed during the correlation window came from a
        /// non-target port (typically a thru-box echo); callers
        /// should ask the user before binding (ADR-026 Phase
        /// 3.C.2). The cache entry on the actual reply port carries
        /// the same confidence value.
        confidence: IdentityConfidence,
        rtt_ms: u64,
        probed_at_unix_ms: u64,
    },
    /// Cross-port correlation observed multiple distinct replies
    /// during the window (thru-box echoing the same device's reply
    /// onto multiple inputs, or a merger feeding multiple devices
    /// into one route). Confidence is implicit (SharedRoute on every
    /// observed port) because the variant itself denotes the
    /// ambiguous case.
    MultipleIdentified {
        identities: Vec<SysExIdentity>,
        rtt_ms: u64,
        probed_at_unix_ms: u64,
    },
    NoReply {
        timeout_ms: u64,
    },
}

/// Synchronous-start failures from `ProbeCoordinator::probe()` —
/// returned in the `Err` branch. The coordinator never queued the
/// request on the wire (or never had a chance to wait), so callers
/// that distinguish "retry-able now" from "absent reply, retry later"
/// can branch on the error type alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status")]
pub enum ProbeStartError {
    /// Pre-flight rate-limit check rejected this attempt.
    RateLimited { retry_after_ms: u64 },
    /// SysEx is globally disabled (Phase 4 settings flag — defined
    /// here so the type-surface is stable; not yet produced).
    SysExDisabled,
    /// The send-closure reported no paired output for this port.
    NoPairedOutput { port_name: String },
    /// The probe could not be started for this port after resolution
    /// of the output path. Covers OS / driver write rejections from
    /// the send-closure, and any operational failure the daemon
    /// surfaces before the wait begins (e.g. input port currently
    /// disconnected, `spawn_blocking` task join error during shutdown).
    /// `reason` carries a human-readable description of the specific
    /// cause so callers can render it without needing variant-level
    /// branching for each sub-case.
    SendFailed { port_name: String, reason: String },
}

/// Wire-format wrapper for serialising
/// `Result<ProbeResult, ProbeStartError>` as a flat JSON object with
/// a single `status` discriminator (matches the shape Phase 2 MCP
/// callers parse). `#[serde(untagged)]` over two inner enums whose
/// variants share the same `status` namespace produces a single flat
/// JSON object with `{"status": "...", ...}` — same wire shape as
/// the legacy `ProbeOutcome`, no LLM-tool-schema breakage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ProbeOutcomeWire {
    Result(ProbeResult),
    Error(ProbeStartError),
}

impl From<Result<ProbeResult, ProbeStartError>> for ProbeOutcomeWire {
    fn from(value: Result<ProbeResult, ProbeStartError>) -> Self {
        match value {
            Ok(r) => ProbeOutcomeWire::Result(r),
            Err(e) => ProbeOutcomeWire::Error(e),
        }
    }
}

/// ADR-026 Phase 4.3b — single entry in the per-port probe-attempt
/// ring buffer. The coordinator records every completed `probe()`
/// call (Ok or Err) so the GUI's identity-history panel and the MCP
/// audit-replay path can show recent activity without going through
/// the audit log. Pure in-memory state; lost on daemon restart by
/// design — diagnostic visibility, not durable telemetry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeHistoryEntry {
    /// Wall-clock timestamp the probe completed (unix epoch ms).
    /// `Identified` outcomes also carry their own `probed_at_unix_ms`
    /// inside the outcome — kept here too so all entry types share a
    /// uniform top-level timestamp the UI can sort on.
    pub timestamp_ms: u64,
    /// Input port the probe was issued against.
    pub port_name: String,
    /// Flat outcome wire shape — same enum the MCP `probe_device_identity`
    /// tool returns, so the GUI can reuse the existing
    /// `probeOutcomeToBadgeState` helper to render history rows.
    pub outcome: ProbeOutcomeWire,
}

/// How many entries to retain per port. The buffer is hand-sized for
/// "last few minutes of activity at <1Hz" — small enough to render
/// without virtualisation, big enough to span a typical
/// hot-plug-storm + RateLimited cooldown cycle.
const PROBE_HISTORY_PER_PORT_LIMIT: usize = 32;

/// Send-closure error. Maps to [`ProbeStartError::NoPairedOutput`]
/// or [`ProbeStartError::SendFailed`] depending on which variant the
/// send closure returns.
#[derive(Debug)]
pub enum SendError {
    NoPairedOutput,
    WriteFailed(String),
}

/// Internal: per-port rate-limit bookkeeping.
struct RateLimitEntry {
    last_attempt: Instant,
}

/// Internal: state for the single in-flight probe. Phase 3.B.2
/// replaced the per-port pending map with this single slot — the
/// global lock already serialises probes (one in-flight at a time
/// across the whole daemon), so a hashmap was overkill, and the
/// coordinator now needs to gather replies that arrive on
/// **any** input port (cross-port correlation for thru-box / merger
/// detection), not just the target. A single slot makes that lookup
/// O(1) and removes the question of "which slot do I deliver this
/// reply into".
struct InFlightProbe {
    /// The port the probe was sent against — recorded for the
    /// `has_pending(port_name)` test helper and so `invalidate()` can
    /// match the in-flight probe by port. The collector accepts
    /// replies from any port, not just this one.
    target_port: String,
    /// Reply collector state. `Open(Vec)` while the probe is active;
    /// `Closed` after `drain_and_close` runs. Late `observe()` calls
    /// (those holding an `Arc<InFlightProbe>` clone they grabbed
    /// before drain ran) hit the `Closed` branch and drop their
    /// reply rather than pushing into a vec nobody reads. This is
    /// what makes the drain-vs-observe race unrepresentable:
    /// observe and drain hold the same mutex, so the transition
    /// from Open to Closed is atomic relative to push attempts.
    state: Mutex<CollectorState>,
    /// Single-shot wake signal. The first reply triggers a `try_send`;
    /// subsequent replies may also try (no-op once the channel is
    /// full). The waiter uses this to break out of the initial-wait
    /// timeout window — the correlation window is then a fixed sleep,
    /// not another channel wait, so it doesn't matter that subsequent
    /// sends drop.
    wake_tx: crossbeam_channel::Sender<()>,
}

/// Internal: collector lifecycle. See `InFlightProbe::state`.
enum CollectorState {
    Open(Vec<(String, SysExIdentity)>),
    Closed,
}

impl InFlightProbe {
    /// Construct a fresh in-flight probe in the `Open` state.
    fn new(target_port: String, wake_tx: crossbeam_channel::Sender<()>) -> Self {
        Self {
            target_port,
            state: Mutex::new(CollectorState::Open(Vec::new())),
            wake_tx,
        }
    }

    /// Record an observed reply. No-op if the collector has already
    /// been closed via `drain_and_close` — the reply is dropped
    /// rather than written into a vec nobody reads, which is the
    /// race-safe behaviour for callbacks that cloned the Arc before
    /// drain ran. Wakes the probe waiter best-effort.
    fn observe(&self, port_name: String, identity: SysExIdentity) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let CollectorState::Open(vec) = &mut *state {
            vec.push((port_name, identity));
            // Best-effort wake — the wake_tx is bounded(1) so
            // subsequent wakes after the first drop silently. That's
            // intentional: the correlation window is a sleep, not
            // another channel wait.
            let _ = self.wake_tx.try_send(());
        }
    }

    /// Atomically drain the collector and transition to `Closed`.
    /// Returns the collected replies. After this call, further
    /// `observe()` invocations on the same `InFlightProbe` (held via
    /// outstanding `Arc` clones) will drop their replies. Idempotent:
    /// calling twice returns an empty Vec the second time.
    fn drain_and_close(&self) -> Vec<(String, SysExIdentity)> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match std::mem::replace(&mut *state, CollectorState::Closed) {
            CollectorState::Open(vec) => vec,
            CollectorState::Closed => Vec::new(),
        }
    }
}

/// Default cross-port reply correlation window (Phase 3.B.2). After
/// the first reply lands, the coordinator holds open this much extra
/// time to allow sibling-port replies to arrive (thru-box / merger
/// case). 80 ms is long enough for hardware MIDI thru-boxes to echo
/// the reply back via every output without blowing the total probe
/// budget out of proportion. Tests override via
/// `with_correlation_window` to keep iteration fast.
const DEFAULT_CORRELATION_WINDOW: Duration = Duration::from_millis(80);

/// Coordinator state. Shared across threads via `Arc<ProbeCoordinator>`.
pub struct ProbeCoordinator {
    clock: Box<dyn Clock>,
    global_lock: Mutex<()>,
    /// Single in-flight probe (or None when idle). Phase 3.B.2 — see
    /// [`InFlightProbe`]. Held under a short-lock pattern: callers
    /// take the lock, clone the `Arc<InFlightProbe>` if present, drop
    /// the lock, then operate on the inner state without blocking
    /// other observers.
    current_probe: Mutex<Option<Arc<InFlightProbe>>>,
    // Grows unbounded across a daemon session, but bounded by
    // distinct port names the daemon has ever probed. In practice
    // that's a handful — no sweeper needed.
    rate_limits: Mutex<HashMap<String, RateLimitEntry>>,
    cache: Mutex<HashMap<String, (SysExIdentity, IdentityConfidence)>>,
    default_timeout: Duration,
    rate_limit_window: Duration,
    correlation_window: Duration,
    /// Phase 4.1 master toggle. When false, every `probe()` call
    /// short-circuits to `Err(ProbeStartError::SysExDisabled)` —
    /// before rate-limit / global-lock / send_fn — so a user-driven
    /// opt-out can never silently send a SysEx request. Daemon wires
    /// this to `AdvancedSettings::sysex_identity_probing` on config
    /// load and reload. `AtomicBool` is sufficient: writes are rare
    /// (config events), reads are hot but tolerate Relaxed ordering
    /// because the worst case is one stale-by-a-cycle probe.
    enabled: AtomicBool,
    /// Phase 4.3b per-port probe-attempt ring buffer. Bounded to
    /// `PROBE_HISTORY_PER_PORT_LIMIT` entries per port; oldest evicted
    /// on overflow. Newest-first when read via `history_for_port`. Each
    /// completed `probe()` call (Ok or Err) appends one entry.
    history: Mutex<HashMap<String, VecDeque<ProbeHistoryEntry>>>,
}

impl Default for ProbeCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ProbeCoordinator {
    pub fn new() -> Self {
        Self::with_clock(Box::new(SystemClock))
    }

    pub fn with_clock(clock: Box<dyn Clock>) -> Self {
        Self {
            clock,
            global_lock: Mutex::new(()),
            current_probe: Mutex::new(None),
            rate_limits: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
            default_timeout: DEFAULT_TIMEOUT,
            rate_limit_window: DEFAULT_RATE_LIMIT,
            correlation_window: DEFAULT_CORRELATION_WINDOW,
            // Phase 4.1: probing is on by default to match the
            // `default_sysex_identity_probing()` config default. The
            // daemon overrides this from config on startup + reload.
            enabled: AtomicBool::new(true),
            history: Mutex::new(HashMap::new()),
        }
    }

    /// Phase 4.3b — return the per-port probe history, newest-first.
    /// Cheap-to-call (clones a small bounded VecDeque slice). Returns
    /// an empty Vec for ports that have never been probed.
    pub fn history_for_port(&self, port_name: &str) -> Vec<ProbeHistoryEntry> {
        let history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        history
            .get(port_name)
            .map(|q| q.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// Phase 4.3b — append a completed probe attempt to the per-port
    /// ring buffer. Bounded to `PROBE_HISTORY_PER_PORT_LIMIT` entries;
    /// the oldest evicts when the cap is reached. Internal — called
    /// at every `probe()` exit point (Ok and Err alike).
    fn record_history(&self, port_name: &str, outcome: ProbeOutcomeWire) {
        let timestamp_ms = self.clock.unix_now_ms();
        let entry = ProbeHistoryEntry {
            timestamp_ms,
            port_name: port_name.to_string(),
            outcome,
        };
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        let queue = history.entry(port_name.to_string()).or_default();
        queue.push_back(entry);
        // Evict oldest until we're at or under the cap. Tolerates
        // accidental over-fill (e.g. if the cap shrinks in a future
        // change) without panicking.
        while queue.len() > PROBE_HISTORY_PER_PORT_LIMIT {
            queue.pop_front();
        }
    }

    /// Phase 4.1 — set the master probe enable flag from
    /// `AdvancedSettings::sysex_identity_probing`. Safe to spam from a
    /// config-watch loop.
    ///
    /// Disabling (`set_enabled(false)`) acquires the global probe lock
    /// before flipping the flag, so it WAITS for any in-flight probe
    /// to finish its send + reply window before returning. This is
    /// what makes "no SysEx leaves the daemon" a hard contract rather
    /// than a best-effort one (#975 review): once this call returns,
    /// every subsequent `probe()` short-circuits to `SysExDisabled`
    /// AND no probe is currently mid-send. Worst case wait is the
    /// outer probe timeout (~1s for SysEx) — acceptable on a config-
    /// reload path that's already milliseconds-class. The probe
    /// throughput in this codebase is <1Hz so the lock contention is
    /// negligible.
    ///
    /// Enabling (`set_enabled(true)`) does NOT take the lock — there's
    /// nothing to wait for, and config reloads that re-enable probing
    /// shouldn't pay any cost.
    pub fn set_enabled(&self, enabled: bool) {
        if !enabled {
            // Hold the global lock long enough for any in-flight probe
            // to drain. Mutex poisoning is recovered (the daemon
            // should never panic mid-probe, but if it did the
            // toggle-to-disable path is exactly when we'd want to fail
            // safe — toward "off" — anyway).
            let _drain = self.global_lock.lock().unwrap_or_else(|e| e.into_inner());
            self.enabled.store(false, AtomicOrdering::Relaxed);
            // _drain is released at end of scope.
        } else {
            self.enabled.store(true, AtomicOrdering::Relaxed);
        }
    }

    /// Phase 4.1 — current master toggle state. Useful for diagnostic
    /// surfaces (status command, GUI badges) that want to surface
    /// "probing disabled" without invoking `probe()` itself.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(AtomicOrdering::Relaxed)
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    pub fn with_rate_limit(mut self, window: Duration) -> Self {
        self.rate_limit_window = window;
        self
    }

    /// Override the cross-port reply correlation window (Phase 3.B.2).
    /// After the first reply lands, the coordinator holds open this
    /// extra duration to allow sibling-port replies to arrive
    /// (thru-box / merger case). Tests use this to keep iterations
    /// fast; production uses the [`DEFAULT_CORRELATION_WINDOW`].
    pub fn with_correlation_window(mut self, window: Duration) -> Self {
        self.correlation_window = window;
        self
    }

    /// Fire a probe and block up to `self.default_timeout` waiting for a
    /// matching reply. Safe to call from sync or async contexts — async
    /// callers should wrap with `tokio::task::spawn_blocking` to avoid
    /// holding the tokio worker hostage for the timeout window.
    ///
    /// **Total duration under contention:** `queue_wait + reply_timeout`.
    /// The `reply_timeout` above is just the reply-wait window; the full
    /// call can additionally block for up to another `reply_timeout`
    /// behind an in-flight probe on another port (see "Global
    /// serialisation" below). If you need bounded latency, put this on
    /// a timeout at the caller side.
    ///
    /// Global serialisation: the global lock is held across the ENTIRE
    /// request→reply window, not just the send (ADR-026 D1). This is
    /// defensive — in principle only outbound collisions matter, and
    /// dropping the lock after send would let other probes start
    /// immediately. We keep the full-window hold for simpler reasoning
    /// and because probe throughput is <1Hz in practice (probe-on-
    /// connect + manual). Revisit if probe fan-out becomes a bottleneck.
    pub fn probe<F>(&self, port_name: &str, send_fn: F) -> Result<ProbeResult, ProbeStartError>
    where
        F: FnOnce(&[u8]) -> Result<(), SendError>,
    {
        // Phase 4.3b: outer wrapper records the completed outcome to
        // the per-port ring buffer. Inner `probe_inner` keeps the
        // existing branchy logic; this layer is a single
        // record-on-exit point so we don't have to thread history
        // calls through every early return below.
        let outcome = self.probe_inner(port_name, send_fn);
        let wire: ProbeOutcomeWire = outcome.clone().into();
        self.record_history(port_name, wire);
        outcome
    }

    fn probe_inner<F>(&self, port_name: &str, send_fn: F) -> Result<ProbeResult, ProbeStartError>
    where
        F: FnOnce(&[u8]) -> Result<(), SendError>,
    {
        // Phase 4.1: master toggle fast-path gate. Reads the flag with
        // Relaxed ordering and short-circuits if disabled. This is the
        // common case (toggle never flipped at runtime) and avoids
        // taking the global_lock for nothing. The HARD guarantee that
        // no SysEx leaves the daemon when disabled comes from
        // `set_enabled(false)` itself acquiring the global_lock to
        // drain in-flight probes (see its docstring) — combined with
        // the second `enabled` check we do AFTER acquiring the
        // global_lock below (#975 review). Together those close the
        // race window completely: any probe that's in-flight when
        // disable lands gets to finish; any probe that's queued
        // behind the lock will see the new state and bail.
        if !self.enabled.load(AtomicOrdering::Relaxed) {
            return Err(ProbeStartError::SysExDisabled);
        }

        // Step 1: rate-limit pre-check (fast path). Rate-limited calls
        // short-circuit without taking the global lock so they don't
        // stall real probes queued behind them.
        if let Some(retry_ms) = self.check_rate_limit(port_name) {
            return Err(ProbeStartError::RateLimited {
                retry_after_ms: retry_ms,
            });
        }

        // Step 2: global serialisation lock. Held for the entire probe.
        let _global = self.global_lock.lock().unwrap_or_else(|e| e.into_inner());

        // Step 2a: master-toggle re-check under the global lock
        // (#975 review). The fast-path gate above is Relaxed; a probe
        // that passed it could otherwise race a `set_enabled(false)`
        // call that landed between the two operations. Because
        // `set_enabled(false)` ITSELF takes the global lock to drain
        // in-flight probes, this re-check sees a consistent snapshot:
        // we are inside the lock that disable would have to wait for,
        // so no disable can interleave between this check and the
        // send_fn below.
        if !self.enabled.load(AtomicOrdering::Relaxed) {
            return Err(ProbeStartError::SysExDisabled);
        }

        // Step 2b: rate-limit re-check under the global lock. Two threads
        // racing on the same port can both pass the pre-lock check before
        // either stamps — the winner completes normally; by the time the
        // loser acquires the global lock, the winner has already stamped
        // the rate-limit window, so the loser must yield. Without this
        // re-check, same-port concurrent probes would bypass the 60s floor.
        if let Some(retry_ms) = self.check_rate_limit(port_name) {
            return Err(ProbeStartError::RateLimited {
                retry_after_ms: retry_ms,
            });
        }

        // Step 3: install in-flight slot + send request.
        //
        // Panic-safety: we install an RAII guard that clears the
        // in-flight slot on drop. Without this, a panic in `send_fn`
        // (or anywhere before the explicit `take_in_flight` at step 5)
        // would unwind past the cleanup, leaving a stale
        // `InFlightProbe` in `current_probe`. During that stale
        // window, subsequent `observe_reply` calls would push into a
        // collector nobody reads, so a real Identity Reply could be
        // "delivered" but effectively lost. A later probe would
        // unconditionally overwrite the stale slot (the install below
        // doesn't check current_probe), but reply delivery during the
        // stale window — plus invalidate semantics keyed on
        // `target_port` and tests that assume cleanup happened — may
        // then behave unexpectedly.
        //
        // The happy path calls `.disarm()` after the explicit step-5
        // `take_in_flight`, so the guard's `Drop` is a no-op at normal
        // function exit — avoids a redundant mutex lock/unlock per probe.
        struct InFlightCleanupGuard<'a> {
            coordinator: &'a ProbeCoordinator,
            armed: bool,
        }
        impl InFlightCleanupGuard<'_> {
            fn disarm(&mut self) {
                self.armed = false;
            }
        }
        impl Drop for InFlightCleanupGuard<'_> {
            fn drop(&mut self) {
                if self.armed {
                    let _ = self.coordinator.take_in_flight();
                }
            }
        }

        // wake_tx is bounded(1): the first reply wakes the waiter;
        // subsequent observe_reply calls during the correlation
        // window try_send and silently drop (channel full). The
        // correlation window itself is a sleep, not another channel
        // wait, so dropped wakes don't matter — the collector is the
        // source of truth.
        //
        // CRITICAL: probe() does NOT hold a long-lived local Arc
        // clone of the `InFlightProbe` on the *pre-wake* path.
        // `observe_reply()` may clone the Arc transiently while
        // processing a reply, but `current_probe` is the only
        // long-lived owner during the recv_timeout wait. That is
        // what makes `invalidate(target_port)` work: clearing
        // `current_probe` is sufficient to disconnect `wake_rx` once
        // any in-progress observers finish and drop their temporary
        // clones, breaking the wait early. If `probe()` held its own
        // local clone here, invalidate would no longer be able to
        // wake the probe before its full timeout. The Arc is
        // recovered at drain time via `take_in_flight()`.
        //
        // After the first wake fires (see `post_wake_arc` further
        // down) probe() *does* take a local clone — by that point
        // we no longer rely on Sender-drop for waking, and we do
        // need to keep the collector alive across the
        // correlation-window sleep so an invalidate during that
        // window can't lose already-recorded replies.
        let (wake_tx, wake_rx) = bounded::<()>(1);
        *self.current_probe.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Arc::new(InFlightProbe::new(port_name.to_string(), wake_tx)));
        let mut in_flight_cleanup = InFlightCleanupGuard {
            coordinator: self,
            armed: true,
        };

        // RTT is a wall-clock duration — always use `Instant::now()`, NOT
        // `self.clock`. Rate-limit tests inject a `FakeClock` that doesn't
        // advance during `recv_timeout`; measuring RTT from that clock would
        // report `rtt_ms: 0` across the board.
        let started = Instant::now();

        // Send the identity request. If it fails, bail *before*
        // recording the rate-limit stamp: a failed send means no
        // bytes hit the wire, so there's nothing to rate-limit. A
        // port with a missing paired output would otherwise be
        // silently cooldown'd for 60s after the first attempt.
        // (In-flight slot cleanup is handled by the RAII guard above.)
        if let Err(send_err) = send_fn(&IDENTITY_REQUEST) {
            return Err(match send_err {
                SendError::NoPairedOutput => ProbeStartError::NoPairedOutput {
                    port_name: port_name.to_string(),
                },
                SendError::WriteFailed(reason) => ProbeStartError::SendFailed {
                    port_name: port_name.to_string(),
                    reason,
                },
            });
        }

        // Send succeeded — now stamp the rate limit for this port.
        self.record_attempt(port_name);

        // Step 4: wait for the first reply (or timeout).
        //
        // `recv_timeout` blocks on real wall-clock time. The abstract
        // `Clock` is used only for rate-limit window checks (step 1
        // / 2b) and for stamping the Unix timestamp on a successful
        // reply. The probe timeout itself is a real duration and the
        // measured RTT is a real `Instant::elapsed()` — neither is
        // fakeable.
        let first_reply_result = wake_rx.recv_timeout(self.default_timeout);

        // Capture the request → first-reply elapsed *before* the
        // correlation-window sleep. Otherwise `rtt_ms` would inflate
        // by the full window (e.g. +80 ms in production), which
        // contradicts the field's documented "round-trip" meaning
        // and would make latency dashboards lie.
        let first_reply_elapsed = first_reply_result.is_ok().then(|| started.elapsed());

        // Once the first wake has fired, hold a *local* Arc clone of
        // the in-flight slot for the duration of the correlation
        // window + drain. This guards against `invalidate()` racing
        // us during the sleep: invalidate clears `current_probe`
        // (drops one Arc reference), but the local clone keeps the
        // `InFlightProbe` — and its collected replies — alive until
        // we drain. Without this clone, an invalidate during the
        // correlation window would drop the only Arc, take_in_flight
        // would later return None, and we'd report NoReply despite
        // having a recorded reply. The pre-wake half of the timeout
        // intentionally does NOT hold a local clone: we still need
        // invalidate's sender-drop to break `recv_timeout` early
        // when the device is unplugged before any reply arrives
        // (covered by `invalidate_wakes_in_flight_probe_immediately`).
        let post_wake_arc: Option<Arc<InFlightProbe>> = if first_reply_result.is_ok() {
            self.current_probe
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(Arc::clone)
        } else {
            None
        };

        // Step 4b: cross-port correlation window (Phase 3.B.2). If we
        // got a wake (first reply landed), hold open the correlation
        // window so sibling-port replies (thru-box / merger echoes)
        // can still be collected. We deliberately use a thread::sleep
        // rather than additional channel waits: extra wakes during
        // the window may silently drop (the wake_tx is bounded(1)),
        // but that's fine — the collector is the source of truth and
        // observe_reply pushes there unconditionally.
        if first_reply_result.is_ok() {
            std::thread::sleep(self.correlation_window);
        }

        // Step 5: take ownership of the in-flight slot, then atomic
        // drain-and-close. Two layers of race protection: (1)
        // clearing `current_probe` means new `observe_reply` callers
        // won't even see the in-flight slot (they short-circuit
        // before parsing); (2) `drain_and_close` transitions the
        // collector to `Closed` under its own mutex, so any caller
        // that already cloned the `Arc<InFlightProbe>` before
        // Step 5 / before `take_in_flight()` and races us to push
        // will hit the `Closed` branch and drop — no replies leak
        // into a forgotten vec.
        //
        // `take_in_flight()` returns None if `invalidate()` already
        // cleared the slot during the correlation-window sleep. In
        // that case fall back to `post_wake_arc` so a reply that
        // arrived before the invalidate isn't silently lost.
        let in_flight = self.take_in_flight().or(post_wake_arc);
        in_flight_cleanup.disarm(); // happy-path cleanup done

        let mut replies: Vec<(String, SysExIdentity)> = in_flight
            .as_ref()
            .map(|slot| slot.drain_and_close())
            .unwrap_or_default();

        // Dedup by (port, identity) — guards against a single
        // physical reply being observed twice (e.g. if the daemon's
        // ingress filter ever fires `observe_reply` more than once
        // for the same bytes, or a thru-box truly echoes the same
        // reply repeatedly on the same port). Keeps the
        // `MultipleIdentified` discriminator honest: it should mean
        // "two distinct routes", not "we saw two physical packets".
        //
        // The sort key is the FULL identity (manufacturer + family +
        // model + version), not just manufacturer. Two identities
        // sharing a manufacturer ID but differing in any other field
        // are distinct devices and must NOT collapse during dedup —
        // and the partial sort key would leave them non-adjacent in
        // the worst case, causing dedup to miss legitimate
        // duplicates. Sort on the same tuple dedup compares.
        replies.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.manufacturer_id.cmp(&b.1.manufacturer_id))
                .then(a.1.family.cmp(&b.1.family))
                .then(a.1.model.cmp(&b.1.model))
                .then(a.1.version.cmp(&b.1.version))
        });
        replies.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);

        // `rtt_ms` is the request → first-reply elapsed (captured
        // pre-sleep above). Falls back to total-elapsed only on the
        // no-reply path where there's no first-wake to measure from.
        let rtt_ms = first_reply_elapsed
            .unwrap_or_else(|| started.elapsed())
            .as_millis() as u64;
        let probed_at_unix_ms = self.clock.unix_now_ms();

        Ok(match replies.len() {
            0 => ProbeResult::NoReply {
                timeout_ms: self.default_timeout.as_millis() as u64,
            },
            1 => {
                // Single reply. `DirectPairedPort` semantics
                // (per the IdentityConfidence doc) require the reply
                // to land on the *expected* input. If it lands on a
                // different port (only physically possible via a
                // thru-box that forwarded the device's reply onto a
                // sibling input), label as `SharedRoute` so Phase
                // 3.C's auto-promote gate flags it for user
                // confirmation rather than silently binding the
                // wrong port.
                let (reply_port, identity) = replies.into_iter().next().unwrap();
                let confidence = if reply_port == port_name {
                    IdentityConfidence::DirectPairedPort
                } else {
                    IdentityConfidence::SharedRoute
                };
                self.cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(reply_port, (identity.clone(), confidence));
                ProbeResult::Identified {
                    identity,
                    confidence,
                    rtt_ms,
                    probed_at_unix_ms,
                }
            }
            _ => {
                // Multiple replies → SharedRoute, but the cache write
                // policy gets nuanced when a single port produced
                // more than one *distinct* identity (the merger
                // case: thru-box feeding two devices into one
                // input). Caching either identity would lie about
                // which device the port actually carries, so we
                // skip the cache entry for ambiguous ports
                // entirely — Phase 3.C will surface the
                // confirmation event so the user can disambiguate
                // explicitly. Unambiguous ports (single distinct
                // identity within the window) still cache normally.
                let mut replies_per_port: HashMap<&str, usize> = HashMap::new();
                for (reply_port, _) in &replies {
                    *replies_per_port.entry(reply_port.as_str()).or_insert(0) += 1;
                }
                let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                for (reply_port, identity) in &replies {
                    if replies_per_port.get(reply_port.as_str()).copied() == Some(1) {
                        cache.insert(
                            reply_port.clone(),
                            (identity.clone(), IdentityConfidence::SharedRoute),
                        );
                    }
                }
                drop(cache);
                let identities: Vec<SysExIdentity> =
                    replies.into_iter().map(|(_, id)| id).collect();
                ProbeResult::MultipleIdentified {
                    identities,
                    rtt_ms,
                    probed_at_unix_ms,
                }
            }
        })
    }

    /// Ingress filter hook: called by the daemon for every SysEx message
    /// that lands on an input port. **Side-observing** — returns `()`
    /// unconditionally. The caller MUST continue normal dispatch
    /// regardless of whether the bytes matched a pending probe.
    ///
    /// Prefer calling this via the [`SysExObserver`] trait where the
    /// call-site needs to be testable (the daemon's MIDI callback
    /// uses `&dyn SysExObserver` so tests can inject a counting
    /// observer to verify gate behaviour).
    ///
    /// If the bytes parse as a valid Identity Reply AND a pending probe
    /// exists for this port, the parsed `SysExIdentity` is delivered to
    /// the waiting probe. Otherwise the call is a cheap no-op (single
    /// `HashMap::get` behind the shape pre-check).
    ///
    /// Per ADR-026 D9: the daemon's existing `MidiMessage` parser
    /// discards SysEx regardless of this hook, so non-mapping SysEx
    /// bytes fall out of the pipeline naturally — a future raw-SysEx
    /// debugger that plumbs past the parser will see matching replies
    /// too. The `()` return type prevents future callers from acquiring
    /// a "consume this event" interpretation.
    pub fn observe_reply(&self, port_name: &str, bytes: &[u8]) {
        // Quick structural check — cheap rejection of non-Identity SysEx.
        // Full parse happens only after we know there's an in-flight
        // probe to deliver to.
        if !is_identity_reply_shape(bytes) {
            return;
        }

        // Phase 3.B.2: any in-flight probe collects replies from every
        // input port (cross-port correlation), not just the target.
        // Short-lock the slot lookup, then drop the lock before
        // touching the inner state — observe_reply may be called from
        // the daemon's MIDI callback thread while probe() holds the
        // global lock; we don't want to add another point of
        // contention.
        let in_flight = {
            let guard = self.current_probe.lock().unwrap_or_else(|e| e.into_inner());
            guard.as_ref().map(Arc::clone)
        };
        let Some(in_flight) = in_flight else {
            return;
        };
        let Some(identity) = SysExIdentity::parse(bytes) else {
            return;
        };
        // `InFlightProbe::observe` checks the collector state under
        // its internal mutex. If `drain_and_close` already ran (e.g.
        // a slow callback racing with probe()'s drain), the push is
        // dropped rather than leaking into a vec nobody reads. Wake
        // is also handled by `observe`.
        in_flight.observe(port_name.to_string(), identity);
    }

    /// Test-only introspection: whether an in-flight probe exists for
    /// `port_name`. Used to assert post-panic cleanup and to
    /// synchronise reply-delivery threads in tests. Phase 3.B.2: now
    /// global-single-slot, so this matches if the in-flight probe's
    /// `target_port` equals the queried port.
    #[cfg(test)]
    pub(crate) fn has_pending(&self, port_name: &str) -> bool {
        self.current_probe
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|slot| slot.target_port == port_name)
    }

    /// Returns the cached identity + confidence label for a port if a
    /// probe succeeded during this session. Phase 3.A always returns
    /// `IdentityConfidence::DirectPairedPort` for the confidence
    /// component; Phase 3.B's correlation logic may surface
    /// `SharedRoute`.
    pub fn cached(&self, port_name: &str) -> Option<(SysExIdentity, IdentityConfidence)> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(port_name)
            .cloned()
    }

    /// All cached identities + confidence labels, sorted by port name
    /// for deterministic output (MCP/CLI consumers need stable
    /// ordering).
    pub fn snapshot(&self) -> Vec<(String, SysExIdentity, IdentityConfidence)> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries: Vec<(String, SysExIdentity, IdentityConfidence)> = cache
            .iter()
            .map(|(k, (id, c))| (k.clone(), id.clone(), *c))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    /// Invalidate the session cache for a port. Called on device
    /// disconnect (hot-plug) so a replug forces a fresh probe.
    ///
    /// If the currently in-flight probe was targeting this port, also
    /// drops the in-flight slot: dropping the `InFlightProbe` drops
    /// its `wake_tx` `Sender`, which causes the waiting `probe()`
    /// caller to wake immediately with
    /// `RecvTimeoutError::Disconnected` (treated as a timeout — no
    /// replies in the collector). Without this, a disconnect during
    /// an active probe would leave the global lock held for up to
    /// the full timeout window, stalling subsequent probes on other
    /// ports.
    pub fn invalidate(&self, port_name: &str) {
        // Order: in-flight first so a concurrent probe wakes before
        // cache / rate-limit cleanup runs. Each lock is acquired and
        // released independently — no cross-map invariants to hold.
        {
            let mut current = self.current_probe.lock().unwrap_or_else(|e| e.into_inner());
            if current
                .as_ref()
                .is_some_and(|slot| slot.target_port == port_name)
            {
                *current = None;
            }
        }
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(port_name);
        self.rate_limits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(port_name);
    }

    // --- internal helpers ---

    fn check_rate_limit(&self, port_name: &str) -> Option<u64> {
        let rate_limits = self.rate_limits.lock().unwrap_or_else(|e| e.into_inner());
        let entry = rate_limits.get(port_name)?;
        let now = self.clock.now();
        let elapsed = now.saturating_duration_since(entry.last_attempt);
        if elapsed < self.rate_limit_window {
            let remaining = self.rate_limit_window - elapsed;
            // Round up to 1ms minimum — sub-millisecond remainders would
            // otherwise truncate to 0, leading to a confusing
            // `RateLimited { retry_after_ms: 0 }` that suggests "retry
            // immediately" when the caller is in fact still within the
            // rate-limit window.
            let ms = remaining.as_millis() as u64;
            Some(if ms == 0 { 1 } else { ms })
        } else {
            None
        }
    }

    fn record_attempt(&self, port_name: &str) {
        let mut rate_limits = self.rate_limits.lock().unwrap_or_else(|e| e.into_inner());
        rate_limits.insert(
            port_name.to_string(),
            RateLimitEntry {
                last_attempt: self.clock.now(),
            },
        );
    }

    /// Take ownership of the current in-flight probe, leaving `None`
    /// in its place. Returns the previous value. Used by `probe()`
    /// (happy path + RAII guard) to atomically end the in-flight
    /// window before draining the collector.
    fn take_in_flight(&self) -> Option<Arc<InFlightProbe>> {
        self.current_probe
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

// -----------------------------------------------------------------------------
// Test helpers — test-only, excluded from release builds
// -----------------------------------------------------------------------------

/// Injectable fake clock for rate-limit testing. Tests advance the clock
/// deterministically instead of calling `std::thread::sleep`.
///
/// `#[cfg(test)]` so this code is excluded entirely from release builds.
/// If Phase 1.B (daemon wire-up) needs cross-crate access, promote to a
/// `test-helpers` feature-gated export instead of loosening the cfg gate.
#[cfg(test)]
struct FakeClock {
    base: Instant,
    offset: Mutex<Duration>,
}

#[cfg(test)]
impl FakeClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            base: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        })
    }

    fn advance(&self, by: Duration) {
        *self.offset.lock().unwrap_or_else(|e| e.into_inner()) += by;
    }
}

/// Delegates to a shared `Arc<FakeClock>` so tests can advance the clock
/// from the test thread without re-wrapping.
#[cfg(test)]
struct SharedFakeClock(Arc<FakeClock>);

#[cfg(test)]
impl Clock for SharedFakeClock {
    fn now(&self) -> Instant {
        let offset = *self.0.offset.lock().unwrap_or_else(|e| e.into_inner());
        self.0.base + offset
    }
    fn unix_now_ms(&self) -> u64 {
        let offset = *self.0.offset.lock().unwrap_or_else(|e| e.into_inner());
        offset.as_millis() as u64
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    /// A canned KORG identity reply for use in multiple tests.
    fn korg_reply() -> Vec<u8> {
        vec![
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0xF7,
        ]
    }

    fn ni_reply() -> Vec<u8> {
        vec![
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x00, 0x21, 0x09, 0x00, 0x16, 0x00, 0x00, 0x01, 0x00,
            0x05, 0x00, 0xF7,
        ]
    }

    #[test]
    fn probe_completes_on_matching_reply() {
        // Reply arrives on a background thread; the probe thread receives
        // the parsed identity and returns Identified.
        //
        // Synchronisation: the reply thread waits on `registered_rx`, and
        // the probe's `send_fn` (which runs only AFTER the pending slot
        // is registered — see `probe()` step 3) signals via
        // `registered_tx`. This is deterministic under CI load; a prior
        // `thread::sleep(50ms)` version was flaky when the reply thread
        // raced past the slot registration.
        let coord = Arc::new(ProbeCoordinator::new().with_timeout(Duration::from_millis(500)));
        let coord_bg = coord.clone();
        let (registered_tx, registered_rx) = bounded::<()>(1);

        let reply_handle = thread::spawn(move || {
            registered_rx
                .recv_timeout(Duration::from_millis(500))
                .expect("probe send_fn should signal slot registration");
            coord_bg.observe_reply("korg-port", &korg_reply());
            // observe_reply is side-observing — return value is () and
            // the delivery signal is the probe() call's outcome below.
        });

        let outcome = coord.probe("korg-port", |bytes| {
            assert_eq!(bytes, &IDENTITY_REQUEST);
            registered_tx
                .send(())
                .expect("reply thread should still be waiting");
            Ok(())
        });
        reply_handle.join().expect("reply thread panicked");

        match outcome {
            Ok(ProbeResult::Identified { identity, .. }) => {
                assert_eq!(identity.manufacturer_id, vec![0x42]);
                assert_eq!(identity.family, 0x0034);
            }
            other => panic!("expected Ok(Identified), got {:?}", other),
        }

        // Cache populated.
        assert!(coord.cached("korg-port").is_some());
    }

    #[test]
    fn probe_times_out_when_no_reply() {
        let coord = ProbeCoordinator::new().with_timeout(Duration::from_millis(30));
        let outcome = coord.probe("silent-port", |_| Ok(()));
        match outcome {
            Ok(ProbeResult::NoReply { timeout_ms }) => assert_eq!(timeout_ms, 30),
            other => panic!("expected Ok(NoReply), got {:?}", other),
        }
    }

    #[test]
    fn probe_returns_no_paired_output_on_send_err() {
        let coord = ProbeCoordinator::new().with_timeout(Duration::from_millis(10));
        let outcome = coord.probe("unpaired-port", |_| Err(SendError::NoPairedOutput));
        match outcome {
            Err(ProbeStartError::NoPairedOutput { port_name }) => {
                assert_eq!(port_name, "unpaired-port")
            }
            other => panic!("expected Err(NoPairedOutput), got {:?}", other),
        }
    }

    #[test]
    fn probe_returns_send_failed_with_reason_on_write_err() {
        // WriteFailed carries a descriptive reason string — must
        // survive the mapping to ProbeStartError (regression: prior
        // impl mapped all SendError variants to NoPairedOutput and
        // lost the detail).
        let coord = ProbeCoordinator::new().with_timeout(Duration::from_millis(10));
        let outcome = coord.probe("flaky-port", |_| {
            Err(SendError::WriteFailed("ALSA: ENODEV".to_string()))
        });
        match outcome {
            Err(ProbeStartError::SendFailed { port_name, reason }) => {
                assert_eq!(port_name, "flaky-port");
                assert_eq!(reason, "ALSA: ENODEV");
            }
            other => panic!("expected Err(SendFailed), got {:?}", other),
        }
    }

    #[test]
    fn failed_send_does_not_rate_limit_port() {
        // A send that fails with NoPairedOutput must NOT record a rate-
        // limit stamp — an unpaired port would otherwise be silently
        // cooldown'd for 60s despite never getting a MIDI request out.
        let clock = FakeClock::new();
        let coord = ProbeCoordinator::with_clock(Box::new(SharedFakeClock(clock.clone())))
            .with_timeout(Duration::from_millis(10))
            .with_rate_limit(Duration::from_secs(60));

        // First attempt fails — no stamp recorded.
        let first = coord.probe("unpaired", |_| Err(SendError::NoPairedOutput));
        assert!(matches!(first, Err(ProbeStartError::NoPairedOutput { .. })));

        // Second attempt immediately after: should NOT be rate-limited.
        // (Will also fail with NoPairedOutput — that's fine, we're
        // asserting the rate-limiter didn't engage.)
        let second = coord.probe("unpaired", |_| Err(SendError::NoPairedOutput));
        assert!(
            matches!(second, Err(ProbeStartError::NoPairedOutput { .. })),
            "second attempt must re-try, not RateLimited — got {:?}",
            second
        );
    }

    // ── Phase 4.3b — probe history ring buffer ─────────────────────────
    // The coordinator records every completed probe attempt (success +
    // error) into a per-port ring buffer bounded to 32 entries so the
    // GUI can show a recent-probes panel without going through the
    // MCP audit log. Pure in-memory state; lost on daemon restart.

    #[test]
    fn history_records_successful_probe_attempts_per_port() {
        // Each Ok-resolving probe lands one ProbeHistoryEntry on the
        // matching port's ring buffer.
        let coord = ProbeCoordinator::new().with_timeout(Duration::from_millis(10));
        let _ = coord.probe("p1", |_| Ok(()));

        let history = coord.history_for_port("p1");
        assert_eq!(history.len(), 1, "first probe should record one entry");
        // Outcome wire shape is flat: status + payload — uses the same
        // ProbeOutcomeWire as the MCP tool already returns, so the GUI
        // can reuse `probeOutcomeToBadgeState` to render it.
        assert!(matches!(history[0].outcome, ProbeOutcomeWire::Result(_)));
    }

    #[test]
    fn history_records_error_outcomes_too() {
        // ProbeStartError::SysExDisabled is recorded just like a
        // successful NoReply — diagnostic visibility into "I tried
        // and couldn't even start" matters as much as "I tried and
        // got no reply".
        let coord = ProbeCoordinator::new().with_timeout(Duration::from_millis(10));
        coord.set_enabled(false);
        let _ = coord.probe("p1", |_| Ok(()));

        let history = coord.history_for_port("p1");
        assert_eq!(history.len(), 1);
        assert!(matches!(history[0].outcome, ProbeOutcomeWire::Error(_)));
    }

    #[test]
    fn history_per_port_is_isolated() {
        // Probes on port A do not appear in port B's history.
        let coord = ProbeCoordinator::new().with_timeout(Duration::from_millis(10));
        let _ = coord.probe("p1", |_| Ok(()));
        let _ = coord.probe("p2", |_| Ok(()));

        assert_eq!(coord.history_for_port("p1").len(), 1);
        assert_eq!(coord.history_for_port("p2").len(), 1);
        assert_eq!(coord.history_for_port("never-probed").len(), 0);
    }

    #[test]
    fn history_is_bounded_per_port_oldest_evicted_first() {
        // Per spec §4.3 the buffer holds the last 32 attempts per
        // port. Beyond that, the oldest entries roll off so the
        // buffer can never grow unbounded over a long-running daemon
        // session.
        let clock = FakeClock::new();
        let coord = ProbeCoordinator::with_clock(Box::new(SharedFakeClock(clock.clone())))
            .with_timeout(Duration::from_millis(10))
            .with_rate_limit(Duration::from_millis(0)); // skip rate-limit gating

        for i in 0..40 {
            let _ = coord.probe("p1", |_| Ok(()));
            // Ensure each entry has a distinct timestamp so we can
            // verify which was evicted.
            clock.advance(Duration::from_millis(1));
            let _ = i;
        }

        let history = coord.history_for_port("p1");
        assert_eq!(history.len(), 32, "history must be bounded to 32 entries");
        // Newest-first order means the most recent attempts are at the
        // front of the returned Vec — easier for the GUI to render
        // "last 5 attempts" without slicing from the back.
        // Asserting the relative ordering only since the FakeClock
        // semantics here are platform-shared.
        for win in history.windows(2) {
            assert!(
                win[0].timestamp_ms >= win[1].timestamp_ms,
                "history must be returned newest-first; got {} before {}",
                win[0].timestamp_ms,
                win[1].timestamp_ms,
            );
        }
    }

    #[test]
    fn probe_returns_sysex_disabled_when_master_toggle_off() {
        // Phase 4.1 — `sysex_identity_probing = false` short-circuits
        // every probe call, regardless of the rate-limit / global-lock
        // / send_fn. The check sits at the very top of `probe()` so
        // SysExDisabled wins over RateLimited (an explicit user opt-
        // out beats a transient cooldown), and so disabling SysEx
        // never silently sends a request the user has prohibited.
        let coord = ProbeCoordinator::new().with_timeout(Duration::from_millis(10));

        // Default state: probing is enabled. Smoke-test that the
        // bypass path doesn't fire on a fresh coordinator.
        let allowed = coord.probe("p1", |_| Ok(()));
        assert!(matches!(allowed, Ok(ProbeResult::NoReply { .. })));

        // Disable globally. Subsequent probes must return SysExDisabled
        // *without* invoking send_fn — including for ports that have
        // never been probed (no rate-limit entry to gate on).
        coord.set_enabled(false);
        let send_called = Arc::new(AtomicUsize::new(0));
        let send_called_cl = send_called.clone();
        let blocked = coord.probe("never-probed-port", move |_| {
            send_called_cl.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(matches!(blocked, Err(ProbeStartError::SysExDisabled)));
        assert_eq!(
            send_called.load(Ordering::SeqCst),
            0,
            "send_fn must not run when SysEx probing is disabled",
        );

        // Re-enabling restores normal operation — same coordinator
        // instance, no need to rebuild the state machine.
        coord.set_enabled(true);
        let restored = coord.probe("p2", |_| Ok(()));
        assert!(matches!(restored, Ok(ProbeResult::NoReply { .. })));
    }

    #[test]
    fn set_enabled_false_waits_for_in_flight_probe_to_complete() {
        // PR #975 review (Copilot): the original implementation used
        // `Relaxed` atomics with a single load at the top of probe(),
        // which left a race window — a probe past the gate could still
        // call send_fn after a config flip. To honour the doc promise
        // "no SysEx leaves the daemon when disabled", `set_enabled(false)`
        // must wait for any in-flight probe to finish its send before
        // returning. After it returns, the caller gets a hard
        // guarantee: no further bytes will hit the wire.
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use std::thread;
        use std::time::Duration as StdDuration;

        let coord = Arc::new(ProbeCoordinator::new().with_timeout(StdDuration::from_millis(50)));
        let send_started = Arc::new(AtomicUsize::new(0));
        let send_completed = Arc::new(AtomicUsize::new(0));

        // Spawn a probe whose send_fn sleeps for 100ms — long enough to
        // observe the disable race deterministically.
        let coord_t = Arc::clone(&coord);
        let started_t = Arc::clone(&send_started);
        let completed_t = Arc::clone(&send_completed);
        let probe_handle = thread::spawn(move || {
            coord_t.probe("p1", move |_| {
                started_t.fetch_add(1, Ordering::SeqCst);
                thread::sleep(StdDuration::from_millis(100));
                completed_t.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        });

        // Wait until the probe is mid-send before disabling.
        while send_started.load(Ordering::SeqCst) == 0 {
            thread::sleep(StdDuration::from_millis(2));
        }
        // The send_fn has been entered. Before send_completed bumps,
        // call set_enabled(false). This must block until the in-flight
        // send finishes — otherwise the contract leaks.
        let disable_start = Instant::now();
        coord.set_enabled(false);
        let disable_elapsed = disable_start.elapsed();

        // The disable call should have waited for the in-flight send to
        // finish (≥ ~50ms remaining of the 100ms sleep). Allow generous
        // slack for thread scheduling on slow CI.
        assert!(
            send_completed.load(Ordering::SeqCst) == 1,
            "set_enabled(false) returned before in-flight send completed",
        );
        assert!(
            disable_elapsed >= StdDuration::from_millis(20),
            "set_enabled(false) returned in {:?} — should have waited \
             for the ≈100ms sleeping send_fn to finish",
            disable_elapsed,
        );

        let _ = probe_handle.join();

        // Post-disable: subsequent probes are blocked.
        let send_called = Arc::new(AtomicUsize::new(0));
        let send_called_cl = Arc::clone(&send_called);
        let blocked = coord.probe("p2", move |_| {
            send_called_cl.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(matches!(blocked, Err(ProbeStartError::SysExDisabled)));
        assert_eq!(send_called.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn probe_sysex_disabled_takes_precedence_over_rate_limit() {
        // If a port is mid-cooldown AND probing has just been disabled
        // globally, the disabled error is the more useful signal — the
        // user changed config, the cooldown is stale. RateLimited
        // would suggest "wait 60s and try again", but the toggle has
        // moved the goalposts. Assert the order.
        let clock = FakeClock::new();
        let coord = ProbeCoordinator::with_clock(Box::new(SharedFakeClock(clock.clone())))
            .with_timeout(Duration::from_millis(10))
            .with_rate_limit(Duration::from_secs(60));

        // First probe records a rate-limit entry.
        let _ = coord.probe("p1", |_| Ok(()));
        clock.advance(Duration::from_secs(10)); // still in cooldown

        // Now disable globally. Despite cooldown still active, the
        // toggle takes precedence.
        coord.set_enabled(false);
        let result = coord.probe("p1", |_| panic!("send_fn must not run"));
        assert!(matches!(result, Err(ProbeStartError::SysExDisabled)));
    }

    #[test]
    fn probe_is_rate_limited_within_window() {
        // Fake clock so we can advance time without sleeping.
        let clock = FakeClock::new();
        let coord = ProbeCoordinator::with_clock(Box::new(SharedFakeClock(clock.clone())))
            .with_timeout(Duration::from_millis(10))
            .with_rate_limit(Duration::from_secs(60));

        // First probe: rate limit fresh, attempt proceeds and times out
        // (no reply). That's a real attempt — records the timestamp.
        let first = coord.probe("p1", |_| Ok(()));
        assert!(matches!(first, Ok(ProbeResult::NoReply { .. })));

        // Advance 30s — still within the 60s window.
        clock.advance(Duration::from_secs(30));

        let second = coord.probe("p1", |_| panic!("send_fn should not be called"));
        match second {
            Err(ProbeStartError::RateLimited { retry_after_ms }) => {
                // Remaining is ~30s. Allow a bit of slack in case the
                // fake-clock resolution loses a ms here or there.
                assert!(retry_after_ms > 29_000 && retry_after_ms <= 30_000);
            }
            other => panic!("expected Err(RateLimited), got {:?}", other),
        }
    }

    #[test]
    fn probe_is_allowed_after_rate_limit_window() {
        let clock = FakeClock::new();
        let coord = ProbeCoordinator::with_clock(Box::new(SharedFakeClock(clock.clone())))
            .with_timeout(Duration::from_millis(10))
            .with_rate_limit(Duration::from_secs(60));

        let _ = coord.probe("p1", |_| Ok(()));
        clock.advance(Duration::from_secs(61));

        // send_fn should be called this time.
        let send_called = Arc::new(AtomicUsize::new(0));
        let send_called_cl = send_called.clone();
        let outcome = coord.probe("p1", move |_| {
            send_called_cl.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(matches!(outcome, Ok(ProbeResult::NoReply { .. })));
        assert_eq!(send_called.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reply_on_unmatched_port_is_silent_noop() {
        // No pending probe for "unknown-port" — observe_reply must
        // return silently (no panic, no cache poisoning).
        let coord = ProbeCoordinator::new();
        coord.observe_reply("unknown-port", &korg_reply());
        assert!(coord.cached("unknown-port").is_none());
        assert!(!coord.has_pending("unknown-port"));
    }

    #[test]
    fn malformed_reply_bytes_ignored() {
        // Deterministic sync: the probe's send_fn signals that the
        // pending slot has been registered. Prior `sleep(2ms)` version
        // could race under CI load.
        //
        // observe_reply returns `()` — the evidence that malformed
        // bytes were not consumed is the final probe outcome: the
        // pending slot survives each observe_reply call (has_pending
        // assertion), and the probe ultimately times out with NoReply
        // rather than being misidentified.
        let coord = Arc::new(ProbeCoordinator::new().with_timeout(Duration::from_millis(100)));
        let coord_bg = coord.clone();
        let coord_main = coord.clone();
        let (registered_tx, registered_rx) = bounded::<()>(1);

        let handle = thread::spawn(move || {
            coord_bg.probe("port-x", |_| {
                registered_tx.send(()).unwrap();
                Ok(())
            })
        });

        registered_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("probe should register pending slot promptly");

        // Each call must leave the pending slot intact — a valid
        // Identity Reply would remove the slot (match + deliver).
        let malformed_frames: &[&[u8]] = &[
            // Too short.
            &[0xF0, 0x7E, 0xF7],
            // Wrong sub-id-1 (not 0x06).
            &[
                0xF0, 0x7E, 0x00, 0x04, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
                0xF7,
            ],
            // Wrong sub-id-2 (request, not reply).
            &[
                0xF0, 0x7E, 0x00, 0x06, 0x01, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
                0xF7,
            ],
            // Missing F7 terminator.
            &[
                0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
                0x00,
            ],
        ];
        for bytes in malformed_frames {
            coord_main.observe_reply("port-x", bytes);
            assert!(
                coord_main.has_pending("port-x"),
                "malformed bytes must not consume the pending slot; frame={:02X?}",
                bytes
            );
        }

        // Let the probe time out — if any malformed byte had been
        // accepted, the slot would have been removed and the probe
        // would have completed with Identified instead.
        let outcome = handle.join().unwrap();
        assert!(matches!(outcome, Ok(ProbeResult::NoReply { .. })));
    }

    #[test]
    fn concurrent_same_port_probes_respect_rate_limit() {
        // Regression: pre-lock rate-limit check is not atomic with the
        // global lock. Two threads probing the SAME port can both pass
        // the initial check before either stamps — without the
        // post-lock re-check they'd both proceed, bypassing the 60s
        // floor. With the fix, the first probe succeeds and the second
        // should hit RateLimited.
        let coord = Arc::new(
            ProbeCoordinator::new()
                .with_timeout(Duration::from_millis(80))
                .with_rate_limit(Duration::from_secs(60)),
        );

        // Spawn two probes on the same port. Give them matching reply
        // senders so the first completes Identified; the second must
        // be rejected as RateLimited because the first stamped.
        //
        // Sync: whichever probe reaches `send_fn` first signals
        // `registered_tx`. The replier waits on that signal before
        // calling observe_reply, so the "winning" probe always has a
        // pending slot at delivery time. (Only one send_fn runs — the
        // loser hits the post-lock rate-limit re-check first.)
        let (registered_tx, registered_rx) = bounded::<()>(1);
        let coord_reply = coord.clone();
        let replier = thread::spawn(move || {
            registered_rx
                .recv_timeout(Duration::from_millis(500))
                .expect("one probe should have reached send_fn");
            coord_reply.observe_reply("shared-port", &korg_reply());
        });

        let c1 = coord.clone();
        let c2 = coord.clone();
        let reg_tx1 = registered_tx.clone();
        let reg_tx2 = registered_tx.clone();
        let h1 = thread::spawn(move || {
            c1.probe("shared-port", move |_| {
                let _ = reg_tx1.try_send(());
                Ok(())
            })
        });
        let h2 = thread::spawn(move || {
            c2.probe("shared-port", move |_| {
                let _ = reg_tx2.try_send(());
                Ok(())
            })
        });

        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        let _ = replier.join();

        // Exactly one Identified, one RateLimited — regardless of order.
        let identified = [&r1, &r2]
            .iter()
            .filter(|r| matches!(r, Ok(ProbeResult::Identified { .. })))
            .count();
        let rate_limited = [&r1, &r2]
            .iter()
            .filter(|r| matches!(r, Err(ProbeStartError::RateLimited { .. })))
            .count();
        assert_eq!(
            identified, 1,
            "expected exactly one Identified, got r1={:?} r2={:?}",
            r1, r2
        );
        assert_eq!(
            rate_limited, 1,
            "expected exactly one RateLimited, got r1={:?} r2={:?}",
            r1, r2
        );
    }

    #[test]
    fn concurrent_probes_serialise() {
        // Two probes against different ports — second must wait for first.
        // Verified by counting the peak concurrency of in-flight send_fn
        // calls, which should be 1 even when probes are racing.
        let coord = Arc::new(ProbeCoordinator::new().with_timeout(Duration::from_millis(80)));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mk_send = |in_flight: Arc<AtomicUsize>, peak: Arc<AtomicUsize>| {
            move |_bytes: &[u8]| -> Result<(), SendError> {
                let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                // Hold the "in flight" state through the probe window.
                thread::sleep(Duration::from_millis(30));
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            }
        };

        let c1 = coord.clone();
        let if1 = in_flight.clone();
        let pk1 = peak.clone();
        let h1 = thread::spawn(move || c1.probe("port-a", mk_send(if1, pk1)));

        let c2 = coord.clone();
        let if2 = in_flight.clone();
        let pk2 = peak.clone();
        let h2 = thread::spawn(move || c2.probe("port-b", mk_send(if2, pk2)));

        let _ = h1.join().unwrap();
        let _ = h2.join().unwrap();

        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "concurrent probes must serialise — peak in-flight was {}",
            peak.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn invalidate_drops_cache_and_rate_limit() {
        let coord = Arc::new(ProbeCoordinator::new().with_timeout(Duration::from_millis(200)));
        let coord_bg = coord.clone();
        let (reg_tx, reg_rx) = bounded::<()>(1);
        let reply_handle = thread::spawn(move || {
            reg_rx.recv_timeout(Duration::from_millis(500)).unwrap();
            coord_bg.observe_reply("ni-port", &ni_reply());
        });
        let _ = coord.probe("ni-port", |_| {
            reg_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().expect("reply thread panicked");
        assert!(coord.cached("ni-port").is_some());

        coord.invalidate("ni-port");
        assert!(coord.cached("ni-port").is_none());

        // After invalidate, rate limit is cleared too — a fresh probe
        // proceeds without hitting RateLimited.
        let outcome = coord.probe("ni-port", |_| Ok(()));
        // No replier this time, so it times out — but it WAS allowed to run.
        assert!(matches!(outcome, Ok(ProbeResult::NoReply { .. })));
    }

    #[test]
    fn snapshot_returns_entries_sorted_by_port_name() {
        let coord = Arc::new(ProbeCoordinator::new().with_timeout(Duration::from_millis(200)));

        // Populate two identities by running back-to-back probes.
        let mut handles = Vec::new();
        for (port, reply) in [("zed-port", korg_reply()), ("alpha-port", ni_reply())] {
            let coord_bg = coord.clone();
            let reply_bytes = reply.clone();
            let port_s = port.to_string();
            let (reg_tx, reg_rx) = bounded::<()>(1);
            handles.push(thread::spawn(move || {
                reg_rx.recv_timeout(Duration::from_millis(500)).unwrap();
                coord_bg.observe_reply(&port_s, &reply_bytes);
            }));
            let _ = coord.probe(port, |_| {
                reg_tx.send(()).unwrap();
                Ok(())
            });
        }
        for h in handles {
            h.join().expect("reply thread panicked");
        }

        let snap = coord.snapshot();
        let port_names: Vec<&str> = snap.iter().map(|(p, _, _)| p.as_str()).collect();
        assert_eq!(port_names, vec!["alpha-port", "zed-port"]);
    }

    #[test]
    fn timeout_after_reply_already_sent_is_ignored() {
        // Regression: if the daemon's filter delivered a reply before the
        // waiter woke, the coordinator must return Identified, not NoReply.
        let coord = Arc::new(ProbeCoordinator::new().with_timeout(Duration::from_millis(200)));
        let coord_bg = coord.clone();
        let (reg_tx, reg_rx) = bounded::<()>(1);
        let reply_handle = thread::spawn(move || {
            reg_rx.recv_timeout(Duration::from_millis(500)).unwrap();
            coord_bg.observe_reply("fast-port", &korg_reply());
        });
        let outcome = coord.probe("fast-port", |_| {
            reg_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().expect("reply thread panicked");
        assert!(matches!(outcome, Ok(ProbeResult::Identified { .. })));
    }

    #[test]
    fn invalidate_wakes_in_flight_probe_immediately() {
        // Regression: invalidate() used to clear cache+rate-limits but
        // NOT the pending slot, so a disconnect during a probe would
        // leave the waiter blocking on the global lock for the full
        // timeout. Fix drops the pending Sender, triggering
        // RecvTimeoutError::Disconnected (mapped to NoReply) and
        // releasing the lock immediately.
        let coord = Arc::new(ProbeCoordinator::new().with_timeout(Duration::from_secs(5)));
        let coord_bg = coord.clone();
        let (reg_tx, reg_rx) = bounded::<()>(1);

        // Invalidate the port once probe() has registered its pending
        // slot — signalled via send_fn. Cuts the probe's wait short.
        let invalidator = thread::spawn(move || {
            reg_rx.recv_timeout(Duration::from_millis(500)).unwrap();
            coord_bg.invalidate("disconnected-port");
        });

        let started = Instant::now();
        let outcome = coord.probe("disconnected-port", |_| {
            reg_tx.send(()).unwrap();
            Ok(())
        });
        let elapsed = started.elapsed();
        invalidator.join().expect("invalidator thread panicked");

        // Must have completed well before the 5-second timeout.
        assert!(
            elapsed < Duration::from_millis(500),
            "probe should have been woken by invalidate, took {:?}",
            elapsed
        );
        assert!(matches!(outcome, Ok(ProbeResult::NoReply { .. })));
    }

    #[test]
    fn panic_in_send_fn_cleans_up_pending_slot() {
        // Regression: without the RAII guard, a panic in send_fn would
        // unwind past the explicit drop_pending call, leaving a dead
        // sender in the pending map. Subsequent observe_reply calls
        // on that port would consume Identity Replies from the daemon
        // ingress even though no probe was waiting — silently breaking
        // identity handling for the port.
        let coord = Arc::new(ProbeCoordinator::new().with_timeout(Duration::from_millis(50)));
        let coord_bg = coord.clone();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            coord_bg.probe("panicky-port", |_| -> Result<(), SendError> {
                panic!("simulated send failure");
            })
        }));
        assert!(result.is_err(), "send_fn panic should propagate");

        // Invariant: pending slot must be gone after the panic. Verify
        // directly via the test-only introspection helper. A leaked
        // slot would let observe_reply find a dead sender later and
        // silently break probe completion for this port.
        assert!(
            !coord.has_pending("panicky-port"),
            "pending slot must be cleaned up on panic"
        );

        // Also verify the observe_reply call itself is a safe no-op
        // post-cleanup — no panic, no spurious cache write.
        coord.observe_reply("panicky-port", &korg_reply());
        assert!(coord.cached("panicky-port").is_none());
    }

    #[test]
    fn reply_after_timeout_drops_silently() {
        // If a late reply arrives AFTER the waiter has timed out, the
        // filter's observe_reply must not panic — the pending slot is
        // gone (probe()'s step-5 drop_pending ran) and the bytes are
        // effectively stale.
        let coord = Arc::new(ProbeCoordinator::new().with_timeout(Duration::from_millis(10)));

        // Run a probe that will time out immediately (no replier).
        let outcome = coord.probe("late-port", |_| Ok(()));
        assert!(matches!(outcome, Ok(ProbeResult::NoReply { .. })));
        assert!(!coord.has_pending("late-port"));

        // Late reply is a no-op — no panic, no stale cache entry.
        coord.observe_reply("late-port", &korg_reply());
        assert!(coord.cached("late-port").is_none());
    }

    // ─────────────────────────────────────────────────────────────────
    // Phase 3.A — IdentityConfidence
    // ─────────────────────────────────────────────────────────────────

    /// `cached()` returns the tuple `(SysExIdentity, IdentityConfidence)`
    /// after a successful probe. Phase 3.A defaults every freshly-stored
    /// identity to `IdentityConfidence::DirectPairedPort` — the common
    /// case where a probe is sent to a single paired output and only one
    /// reply is expected. Phase 3.B will add cross-port correlation that
    /// produces `SharedRoute` for thru-box / merge scenarios.
    #[test]
    fn cached_returns_identity_with_direct_paired_port_confidence_by_default() {
        let coord = Arc::new(ProbeCoordinator::new());
        let coord_filter = Arc::clone(&coord);
        let (registered_tx, registered_rx) = std::sync::mpsc::sync_channel(0);
        let reply_handle = std::thread::spawn(move || {
            let _ = registered_rx.recv();
            for _ in 0..100 {
                std::thread::sleep(Duration::from_millis(2));
                if coord_filter.has_pending("phase3a-port") {
                    coord_filter.observe_reply("phase3a-port", &korg_reply());
                    return;
                }
            }
        });

        let outcome = coord.probe("phase3a-port", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().unwrap();
        assert!(matches!(outcome, Ok(ProbeResult::Identified { .. })));

        let cached = coord.cached("phase3a-port");
        match cached {
            Some((identity, confidence)) => {
                assert_eq!(identity.manufacturer_id, vec![0x42]);
                assert_eq!(confidence, IdentityConfidence::DirectPairedPort);
            }
            None => panic!("expected cached identity after successful probe"),
        }
    }

    /// `snapshot()` carries `IdentityConfidence` alongside each entry so
    /// MCP / CLI consumers can render the badge state without a second
    /// round-trip.
    #[test]
    fn snapshot_includes_confidence_alongside_identity() {
        let coord = Arc::new(ProbeCoordinator::new());
        let coord_filter = Arc::clone(&coord);
        let (registered_tx, registered_rx) = std::sync::mpsc::sync_channel(0);
        let reply_handle = std::thread::spawn(move || {
            let _ = registered_rx.recv();
            for _ in 0..100 {
                std::thread::sleep(Duration::from_millis(2));
                if coord_filter.has_pending("snap-port") {
                    coord_filter.observe_reply("snap-port", &korg_reply());
                    return;
                }
            }
        });

        let outcome = coord.probe("snap-port", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().unwrap();
        assert!(matches!(outcome, Ok(ProbeResult::Identified { .. })));

        let snap = coord.snapshot();
        assert_eq!(snap.len(), 1);
        let (port, identity, confidence) = &snap[0];
        assert_eq!(port, "snap-port");
        assert_eq!(identity.manufacturer_id, vec![0x42]);
        assert_eq!(*confidence, IdentityConfidence::DirectPairedPort);
    }

    // ─────────────────────────────────────────────────────────────────
    // Phase 3.B.1 — Result<ProbeResult, ProbeStartError> split
    // ─────────────────────────────────────────────────────────────────

    /// `probe()` returns `Result<ProbeResult, ProbeStartError>`.
    /// Successful timeout maps to `Ok(ProbeResult::NoReply { .. })` —
    /// async outcome, even though the reply window expired (the wait
    /// itself completed normally; absence-of-reply is data, not an
    /// error). This pins the discriminator: errors are *start*
    /// failures, not absent replies.
    #[test]
    fn probe_returns_ok_no_reply_on_timeout() {
        let coord = ProbeCoordinator::new().with_timeout(Duration::from_millis(20));
        let result = coord.probe("silent-port", |_| Ok(()));
        match result {
            Ok(ProbeResult::NoReply { timeout_ms }) => assert_eq!(timeout_ms, 20),
            other => panic!("expected Ok(NoReply), got {:?}", other),
        }
    }

    /// Rate-limit hits during the *start* phase before any wait
    /// happens, so it surfaces as `Err(ProbeStartError::RateLimited)`.
    /// Distinguishes "we tried but the device went silent" (Ok) from
    /// "we never got to send" (Err) — useful for callers that want
    /// to retry differently.
    #[test]
    fn probe_returns_err_rate_limited_when_called_inside_window() {
        let coord = ProbeCoordinator::new()
            .with_timeout(Duration::from_millis(20))
            .with_rate_limit(Duration::from_secs(60));
        let _ = coord.probe("rate-port", |_| Ok(()));
        let second = coord.probe("rate-port", |_| Ok(()));
        match second {
            Err(ProbeStartError::RateLimited { retry_after_ms }) => {
                assert!(retry_after_ms <= 60_000, "got {}", retry_after_ms);
            }
            other => panic!("expected Err(RateLimited), got {:?}", other),
        }
    }

    /// `SendError::NoPairedOutput` from the send-closure is a *start*
    /// failure — we never queued the request on the wire.
    #[test]
    fn probe_returns_err_no_paired_output_when_send_fn_signals_unpaired() {
        let coord = ProbeCoordinator::new().with_timeout(Duration::from_millis(20));
        let result = coord.probe("unpaired-port", |_| Err(SendError::NoPairedOutput));
        match result {
            Err(ProbeStartError::NoPairedOutput { port_name }) => {
                assert_eq!(port_name, "unpaired-port");
            }
            other => panic!("expected Err(NoPairedOutput), got {:?}", other),
        }
    }

    /// `SendError::WriteFailed` likewise — write hit the OS but the
    /// device rejected; the coordinator never started waiting.
    #[test]
    fn probe_returns_err_send_failed_with_reason_on_write_err() {
        let coord = ProbeCoordinator::new().with_timeout(Duration::from_millis(20));
        let result = coord.probe("write-fail-port", |_| {
            Err(SendError::WriteFailed("wire is down".to_string()))
        });
        match result {
            Err(ProbeStartError::SendFailed { port_name, reason }) => {
                assert_eq!(port_name, "write-fail-port");
                assert_eq!(reason, "wire is down");
            }
            other => panic!("expected Err(SendFailed), got {:?}", other),
        }
    }

    /// `MultipleIdentified` variant exists on `ProbeResult`. Phase
    /// 3.B.1 defines the variant; Phase 3.B.2 wires the cross-port
    /// correlation that produces it. The variant carries the full
    /// reply set so callers can show "we saw N replies — pick one".
    #[test]
    fn probe_result_has_multiple_identified_variant() {
        // Construct the variant directly — proves the type exists with
        // the documented field shape. Doesn't need a probe() round-trip.
        let identity = SysExIdentity {
            manufacturer_id: vec![0x42],
            family: 0x0034,
            model: 0x0001,
            version: [0, 0, 0, 1],
        };
        let result = ProbeResult::MultipleIdentified {
            identities: vec![identity.clone(), identity],
            rtt_ms: 42,
            probed_at_unix_ms: 1_700_000_000_000,
        };
        match result {
            ProbeResult::MultipleIdentified { identities, .. } => {
                assert_eq!(identities.len(), 2);
            }
            other => panic!("expected MultipleIdentified, got {:?}", other),
        }
    }

    /// Wire format parity: serialising the Result branches must
    /// produce the same flat `{"status": "..."}` JSON shape the old
    /// `ProbeOutcome` produced. Callers (MCP / GUI) parse on `status`
    /// — changing it would break LLM tool schemas. We use a
    /// `ProbeOutcomeWire` enum wrapper that holds either branch of
    /// `Result<ProbeResult, ProbeStartError>` and serialises flat via
    /// `#[serde(untagged)]` over the two inner enums (each tagged
    /// by `status`).
    #[test]
    fn probe_outcome_wire_format_keeps_flat_status_shape() {
        // Identified
        let ok_identified: Result<ProbeResult, ProbeStartError> = Ok(ProbeResult::Identified {
            identity: SysExIdentity {
                manufacturer_id: vec![0x42],
                family: 0x0034,
                model: 0x0001,
                version: [0, 0, 0, 1],
            },
            confidence: IdentityConfidence::DirectPairedPort,
            rtt_ms: 5,
            probed_at_unix_ms: 1_700_000_000_000,
        });
        let wire = ProbeOutcomeWire::from(ok_identified);
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(
            json.get("status").and_then(|v| v.as_str()),
            Some("Identified")
        );
        assert!(json.get("identity").is_some());
        // Phase 3.C.2: Identified now carries a confidence label.
        // Pin the wire format (snake_case via the IdentityConfidence
        // serde attr) so future MCP/GUI clients can render the
        // badge without a second cache lookup.
        assert_eq!(
            json.get("confidence").and_then(|v| v.as_str()),
            Some("direct_paired_port")
        );

        // Err side: RateLimited still serialises with the same flat
        // status discriminator — same shape, different tag value.
        let err_rl: Result<ProbeResult, ProbeStartError> = Err(ProbeStartError::RateLimited {
            retry_after_ms: 5000,
        });
        let wire = ProbeOutcomeWire::from(err_rl);
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(
            json.get("status").and_then(|v| v.as_str()),
            Some("RateLimited")
        );
        assert_eq!(
            json.get("retry_after_ms").and_then(|v| v.as_u64()),
            Some(5000)
        );

        // NoReply, on the Ok side
        let ok_noreply: Result<ProbeResult, ProbeStartError> =
            Ok(ProbeResult::NoReply { timeout_ms: 1000 });
        let wire = ProbeOutcomeWire::from(ok_noreply);
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("NoReply"));
    }

    // ─────────────────────────────────────────────────────────────────
    // Phase 3.B.2 — cross-port reply correlation
    // ─────────────────────────────────────────────────────────────────

    /// Reply on the target port only → `Ok(Identified)` with
    /// `DirectPairedPort` cache. Pins the single-cable case after the
    /// 3.B.2 refactor (cross-port collection logic must NOT change
    /// the simple-case semantics).
    #[test]
    fn probe_single_port_reply_returns_identified_with_direct_paired_confidence() {
        let coord = Arc::new(
            ProbeCoordinator::new()
                .with_timeout(Duration::from_millis(200))
                .with_correlation_window(Duration::from_millis(50)),
        );
        let coord_bg = Arc::clone(&coord);
        let (registered_tx, registered_rx) = bounded::<()>(1);
        let reply_handle = thread::spawn(move || {
            registered_rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap();
            coord_bg.observe_reply("port-A", &korg_reply());
        });
        let result = coord.probe("port-A", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().unwrap();

        match result {
            Ok(ProbeResult::Identified { identity, .. }) => {
                assert_eq!(identity.manufacturer_id, vec![0x42]);
            }
            other => panic!("expected Ok(Identified), got {:?}", other),
        }
        // Cache: single port → DirectPairedPort.
        assert_eq!(
            coord.cached("port-A").map(|(_, c)| c),
            Some(IdentityConfidence::DirectPairedPort)
        );
    }

    /// Same identity arrives on a *different* input port within the
    /// correlation window after the first reply → `MultipleIdentified`
    /// with both replies. Both ports get `SharedRoute` cache entries.
    /// This is the canonical thru-box / merger scenario.
    #[test]
    fn probe_cross_port_replies_promote_to_shared_route_and_multiple_identified() {
        let coord = Arc::new(
            ProbeCoordinator::new()
                // Generous timing so the test isn't a race against CI
                // scheduler jitter — the spaced `thread::sleep` calls
                // below can oversleep significantly under load on
                // GitHub macOS runners, pushing the 2nd reply past a
                // tight window (PR #1207). Production probe windows
                // are a separate, smaller value.
                .with_timeout(Duration::from_millis(1000))
                .with_correlation_window(Duration::from_millis(500)),
        );
        let coord_bg = Arc::clone(&coord);
        let (registered_tx, registered_rx) = bounded::<()>(1);
        let reply_handle = thread::spawn(move || {
            registered_rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap();
            // First reply on the target input port.
            coord_bg.observe_reply("port-A", &korg_reply());
            // Second reply on a sibling port within the correlation
            // window — simulates a thru-box echoing the device's
            // Identity Reply onto a second input.
            thread::sleep(Duration::from_millis(20));
            coord_bg.observe_reply("port-B", &korg_reply());
        });
        let result = coord.probe("port-A", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().unwrap();

        match &result {
            Ok(ProbeResult::MultipleIdentified { identities, .. }) => {
                assert_eq!(identities.len(), 2);
                for id in identities {
                    assert_eq!(id.manufacturer_id, vec![0x42]);
                }
            }
            other => panic!("expected Ok(MultipleIdentified), got {:?}", other),
        }
        // Both ports cached with SharedRoute confidence.
        assert_eq!(
            coord.cached("port-A").map(|(_, c)| c),
            Some(IdentityConfidence::SharedRoute),
            "port-A confidence",
        );
        assert_eq!(
            coord.cached("port-B").map(|(_, c)| c),
            Some(IdentityConfidence::SharedRoute),
            "port-B confidence",
        );
    }

    /// Late reply (after the correlation window has closed) is
    /// dropped — the probe already returned. The cache for the late
    /// port stays absent and the original `Identified` result is
    /// unaffected. Pins the wait-window contract.
    #[test]
    fn probe_late_cross_port_reply_is_ignored() {
        let coord = Arc::new(
            ProbeCoordinator::new()
                .with_timeout(Duration::from_millis(200))
                .with_correlation_window(Duration::from_millis(30)),
        );
        let coord_bg = Arc::clone(&coord);
        let (registered_tx, registered_rx) = bounded::<()>(1);
        let reply_handle = thread::spawn(move || {
            registered_rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap();
            coord_bg.observe_reply("port-A", &korg_reply());
            // Wait WELL beyond the correlation window before the
            // sibling-port reply. The probe call should have returned
            // before this fires, so the late reply must be dropped.
            //
            // Bumped from 150ms → 500ms (PR #1211): under macOS CI
            // scheduler jitter the main probe thread can be preempted
            // for >150ms after seeing port-A, leaving its 30ms
            // correlation window still open by the time port-B's reply
            // lands and producing a false `MultipleIdentified`. 500ms
            // gives ample margin without changing the test's premise
            // (the contract here is "outside the window = dropped").
            thread::sleep(Duration::from_millis(500));
            coord_bg.observe_reply("port-B", &korg_reply());
        });
        let result = coord.probe("port-A", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().unwrap();

        // Result is single Identified — the late reply doesn't promote.
        match result {
            Ok(ProbeResult::Identified { .. }) => {}
            other => panic!("expected Ok(Identified), got {:?}", other),
        }
        // port-A cached with DirectPairedPort.
        assert_eq!(
            coord.cached("port-A").map(|(_, c)| c),
            Some(IdentityConfidence::DirectPairedPort)
        );
        // port-B's late reply was dropped — no cache entry.
        assert!(
            coord.cached("port-B").is_none(),
            "late reply on port-B must not populate cache"
        );
    }

    /// `invalidate()` firing AFTER the first reply has been collected
    /// (i.e. during the correlation-window sleep) must NOT lose that
    /// already-recorded reply. Prior design dropped `current_probe`
    /// → dropped the only `Arc<InFlightProbe>` → InFlightProbe and
    /// its collector dropped → take_in_flight returned None → probe
    /// returned `NoReply` despite the device having actually replied.
    ///
    /// Fix: probe() captures a local Arc clone *after* the first
    /// wake. Pre-wake invalidate still works (probe holds no local
    /// Arc yet, Sender drops, recv_timeout returns Disconnected,
    /// existing `invalidate_wakes_in_flight_probe_immediately`
    /// behaviour preserved). Post-wake invalidate now finds the
    /// collector still alive via the local clone, so the drain sees
    /// the recorded reply and returns `Identified`.
    #[test]
    fn invalidate_during_correlation_window_preserves_first_reply() {
        let coord = Arc::new(
            ProbeCoordinator::new()
                .with_timeout(Duration::from_secs(2))
                // Long correlation window so we have a generous gap
                // for invalidate to fire post-wake but pre-drain.
                .with_correlation_window(Duration::from_millis(200)),
        );
        let coord_bg = Arc::clone(&coord);
        let (registered_tx, registered_rx) = bounded::<()>(1);
        let bg_handle = thread::spawn(move || {
            registered_rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap();
            // Reply quickly so the wake fires.
            coord_bg.observe_reply("port-A", &korg_reply());
            // Wait until the wake has definitely been processed,
            // then invalidate during the correlation-window sleep.
            thread::sleep(Duration::from_millis(50));
            coord_bg.invalidate("port-A");
        });
        let result = coord.probe("port-A", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
        bg_handle.join().unwrap();

        match result {
            Ok(ProbeResult::Identified { identity, .. }) => {
                assert_eq!(identity.manufacturer_id, vec![0x42]);
            }
            other => panic!(
                "post-wake invalidate must NOT clobber the collected reply; got {:?}",
                other
            ),
        }
    }

    /// Race fix: a reply observed AFTER probe()'s drain ran must not
    /// be silently lost into a forgotten collector. Prior design
    /// allowed observe_reply to clone the `Arc<InFlightProbe>`, then
    /// probe() to drain via `mem::take`, then observe_reply (still
    /// holding its earlier clone) to push into the now-empty vec —
    /// that push went into a vec nobody reads. Refactor to
    /// `CollectorState::Open | Closed` makes this race
    /// unrepresentable: after `drain_and_close`, further observes hit
    /// the `Closed` branch and drop. This test pins the contract
    /// directly on the InFlightProbe type.
    #[test]
    fn in_flight_probe_observe_after_close_is_dropped() {
        let identity = SysExIdentity {
            manufacturer_id: vec![0x42],
            family: 0x0034,
            model: 0x0001,
            version: [0, 0, 0, 1],
        };
        let (wake_tx, _wake_rx) = bounded::<()>(1);
        let in_flight = InFlightProbe::new("port-A".to_string(), wake_tx);

        // Pre-close observation accumulates.
        in_flight.observe("port-A".to_string(), identity.clone());
        let drained = in_flight.drain_and_close();
        assert_eq!(drained.len(), 1);

        // Post-close observation must be dropped — the collector is
        // gone, so a late push (the race scenario) doesn't leak into
        // a successor probe and doesn't change cache state.
        in_flight.observe("port-A".to_string(), identity);
        let drained_again = in_flight.drain_and_close();
        assert!(
            drained_again.is_empty(),
            "post-close observe must drop, not accumulate"
        );
    }

    /// Single reply landing on a port other than the probe target
    /// (i.e. unexpected routing — typically a thru-box echoing the
    /// device's reply onto a sibling input only) is NOT
    /// `DirectPairedPort` because the docstring requires the reply to
    /// land on the *expected* input. Phase 3.B.2 must label this
    /// case `SharedRoute` so auto-promotion (Phase 3.C) can flag it
    /// for user confirmation rather than silently binding the wrong
    /// port.
    #[test]
    fn probe_single_reply_on_non_target_port_caches_as_shared_route() {
        let coord = Arc::new(
            ProbeCoordinator::new()
                .with_timeout(Duration::from_millis(500))
                .with_correlation_window(Duration::from_millis(50)),
        );
        let coord_bg = Arc::clone(&coord);
        let (registered_tx, registered_rx) = bounded::<()>(1);
        let reply_handle = thread::spawn(move || {
            registered_rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap();
            // Reply lands on port-B even though we sent the request
            // for port-A — simulates a thru-box that only forwards
            // the device's reply onto a sibling input.
            coord_bg.observe_reply("port-B", &korg_reply());
        });
        let result = coord.probe("port-A", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().unwrap();

        match &result {
            Ok(ProbeResult::Identified { .. }) => {
                // Single reply, so still Identified — but the cache
                // entry is on port-B with SharedRoute, not
                // DirectPairedPort. port-A has no cache because we
                // never observed a reply on it.
                assert_eq!(
                    coord.cached("port-B").map(|(_, c)| c),
                    Some(IdentityConfidence::SharedRoute),
                    "non-target reply must cache as SharedRoute, not DirectPairedPort"
                );
                assert!(
                    coord.cached("port-A").is_none(),
                    "target port with no observed reply must not cache"
                );
            }
            other => panic!("expected Ok(Identified), got {:?}", other),
        }
    }

    /// MultipleIdentified merger case: same input port observes two
    /// *distinct* identities during the correlation window. Caching
    /// either one would lie about the routing. Phase 3.B.2 policy:
    /// skip the cache entry for ambiguous ports (those with >1
    /// distinct identity in the window). Other ports with a single
    /// distinct identity still get their entries.
    #[test]
    fn probe_same_port_multiple_distinct_identities_skips_cache_for_that_port() {
        // Two replies on port-A with different manufacturers (Korg +
        // Roland), one on port-B with Korg. port-A is the ambiguous
        // port; port-B is unambiguous.
        fn roland_reply() -> Vec<u8> {
            vec![
                0xF0, 0x7E, 0x00, 0x06, 0x02, 0x41, 0x10, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
                0xF7,
            ]
        }

        // correlation_window is generous (500ms) so the test isn't a
        // race against CI scheduler jitter — the spaced `thread::sleep`
        // calls below can oversleep significantly under load on
        // GitHub macOS runners, pushing the 3rd reply past a tight
        // window and causing flake (PR #1207 surfaced this once
        // `--workspace` was added to the macOS test step). Production
        // probe windows are a separate, smaller value.
        let coord = Arc::new(
            ProbeCoordinator::new()
                .with_timeout(Duration::from_millis(1000))
                .with_correlation_window(Duration::from_millis(500)),
        );
        let coord_bg = Arc::clone(&coord);
        let (registered_tx, registered_rx) = bounded::<()>(1);
        let reply_handle = thread::spawn(move || {
            registered_rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap();
            coord_bg.observe_reply("port-A", &korg_reply());
            thread::sleep(Duration::from_millis(10));
            coord_bg.observe_reply("port-A", &roland_reply());
            thread::sleep(Duration::from_millis(10));
            coord_bg.observe_reply("port-B", &korg_reply());
        });
        let result = coord.probe("port-A", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().unwrap();

        match &result {
            Ok(ProbeResult::MultipleIdentified { identities, .. }) => {
                assert_eq!(identities.len(), 3);
            }
            other => panic!("expected Ok(MultipleIdentified), got {:?}", other),
        }
        assert!(
            coord.cached("port-A").is_none(),
            "port-A produced 2 distinct identities — must not cache (ambiguous)"
        );
        assert_eq!(
            coord.cached("port-B").map(|(_, c)| c),
            Some(IdentityConfidence::SharedRoute),
            "port-B produced 1 distinct identity — caches SharedRoute"
        );
    }

    /// `rtt_ms` is the request → first-reply round-trip, NOT including
    /// the correlation window's hold-open sleep. Phase 3.B.2 extended
    /// the wait by `correlation_window` after the first reply lands;
    /// without explicit accounting, the elapsed-at-aggregation time
    /// would inflate `rtt_ms` by the full window (e.g. +80 ms in
    /// production). This test pins the fix: a fast reply against a
    /// long correlation window must still report a sub-window RTT.
    #[test]
    fn rtt_ms_excludes_correlation_window_sleep() {
        let coord = Arc::new(
            ProbeCoordinator::new()
                .with_timeout(Duration::from_millis(500))
                // Long correlation window so the bug is unmissable —
                // if rtt_ms includes the sleep it will be >= 200 ms.
                .with_correlation_window(Duration::from_millis(200)),
        );
        let coord_bg = Arc::clone(&coord);
        let (registered_tx, registered_rx) = bounded::<()>(1);
        let reply_handle = thread::spawn(move || {
            registered_rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap();
            // Reply immediately — real RTT is near-zero.
            coord_bg.observe_reply("port-A", &korg_reply());
        });
        let result = coord.probe("port-A", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().unwrap();

        match result {
            Ok(ProbeResult::Identified { rtt_ms, .. }) => {
                assert!(
                    rtt_ms < 200,
                    "rtt_ms must exclude the correlation_window sleep (200ms); got {}",
                    rtt_ms
                );
            }
            other => panic!("expected Ok(Identified), got {:?}", other),
        }
    }

    /// Two distinct identities replying on different ports within the
    /// correlation window → `MultipleIdentified` carrying both. This is
    /// the merger case (two devices on a shared input route, both
    /// answering the broadcast Identity Request).
    #[test]
    fn probe_distinct_identities_on_different_ports_returns_multiple_identified() {
        // Synthesize a second Identity Reply with a different
        // manufacturer ID. korg_reply() uses 0x42; this one uses 0x41
        // (Roland) so the parsed identities are distinct.
        fn roland_reply() -> Vec<u8> {
            vec![
                0xF0, 0x7E, 0x00, 0x06, 0x02, 0x41, // mfr = Roland
                0x10, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04, 0xF7,
            ]
        }

        let coord = Arc::new(
            ProbeCoordinator::new()
                // Generous timing so the test isn't a race against CI
                // scheduler jitter — the spaced `thread::sleep` calls
                // below can oversleep significantly under load on
                // GitHub macOS runners, pushing the 2nd reply past a
                // tight window (PR #1207). Production probe windows
                // are a separate, smaller value.
                .with_timeout(Duration::from_millis(1000))
                .with_correlation_window(Duration::from_millis(500)),
        );
        let coord_bg = Arc::clone(&coord);
        let (registered_tx, registered_rx) = bounded::<()>(1);
        let reply_handle = thread::spawn(move || {
            registered_rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap();
            coord_bg.observe_reply("port-A", &korg_reply());
            thread::sleep(Duration::from_millis(20));
            coord_bg.observe_reply("port-B", &roland_reply());
        });
        let result = coord.probe("port-A", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
        reply_handle.join().unwrap();

        match &result {
            Ok(ProbeResult::MultipleIdentified { identities, .. }) => {
                assert_eq!(identities.len(), 2);
                let mfrs: Vec<&Vec<u8>> = identities.iter().map(|i| &i.manufacturer_id).collect();
                assert!(mfrs.iter().any(|m| **m == vec![0x42]));
                assert!(mfrs.iter().any(|m| **m == vec![0x41]));
            }
            other => panic!("expected Ok(MultipleIdentified), got {:?}", other),
        }
    }
}
