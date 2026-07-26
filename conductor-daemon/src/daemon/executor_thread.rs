// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Dedicated executor thread for non-blocking action execution (ADR-015)
//!
//! Decouples action execution from the event loop by running `ActionExecutor`
//! on a dedicated `std::thread`. The event loop dispatches actions via a bounded
//! channel and receives completions via a tokio mpsc channel.
//!
//! Key design decisions (from ADR-015):
//! - D1: Single long-lived `std::thread` (not `spawn_blocking`)
//! - D2: Bounded(32) dispatch channel, unbounded completion channel
//! - D3: `try_send` semantics — full channel = action dropped (not backpressure)
//! - D4: Completions processed as highest-priority `biased` select branch

use crate::action_executor::{
    ActionExecutor, SharedActionConfig, TriggerContext, compute_midi_forward_bytes,
    compute_midi_output_bytes,
};
use arc_swap::ArcSwap;
use conductor_core::actions::Action;
use conductor_core::control_state::PhysicalControlStateStore;
use conductor_core::dispatch::{DispatchOutcome, DispatchResult};
use conductor_core::event_types::FiredTriggerInfo;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tracing::{debug, error, info, trace, warn};

/// Capacity of the bounded dispatch channel (D3)
pub const DISPATCH_CHANNEL_CAPACITY: usize = 32;

/// Action dispatch sent from event loop to executor thread
#[derive(Debug)]
pub struct ActionDispatch {
    /// Monotonically increasing ID for correlation
    pub invocation_id: u64,
    /// The action to execute
    pub action: Action,
    /// Trigger context (velocity, mode, raw MIDI)
    pub context: Option<TriggerContext>,
    /// Provenance metadata for monitor events
    pub provenance: ActionProvenance,
    /// When the dispatch was created (for latency measurement)
    pub dispatch_time: Instant,
    /// ADR-042 D17 network-origin taint (ADR-039-A Slice 2, #2325).
    /// `Some(listener_alias)` when the triggering event came from a network
    /// listener (OSC — including loopback); `None` for MIDI/gamepad origins.
    /// The executor threads this per-dispatch (set unconditionally before
    /// execution, so a tainted dispatch can never leak its taint into the
    /// next one) and the action-class gate refuses sensitive actions from a
    /// tainted origin unless the listener set `allow_sensitive_actions`.
    pub network_origin: Option<String>,
}

/// Provenance metadata attached to each dispatch (ADR-009 Gap K extension)
#[derive(Debug, Clone)]
pub struct ActionProvenance {
    /// Device that triggered the event
    pub device_id: Option<String>,
    /// Description from the matched CompiledRule
    pub matched_rule: Option<String>,
    /// Active mode when the rule matched
    pub mode_name: Option<String>,
    /// Action type string (e.g., "keystroke", "sequence")
    pub action_type: String,
    /// Human-readable action summary (e.g., "Cmd+C", "Sequence (25)")
    pub action_summary: String,
    /// Trigger info for MappingFiredPayload
    pub trigger_info: FiredTriggerInfo,
    /// Optional mapping label/description
    pub mapping_label: Option<String>,
    /// ADR-038: whether the matched mapping set `let_through = true`.
    /// Carried through to `MappingFiredPayload` so the GUI can badge the row.
    pub let_through: bool,
}

/// Completion message sent from executor thread back to event loop
#[derive(Debug)]
pub struct ActionCompletion {
    /// Correlates with `ActionDispatch.invocation_id`
    pub invocation_id: u64,
    /// Result of the action execution
    pub result: DispatchResult,
    /// Wall-clock execution time in microseconds
    pub execution_time_us: u64,
    /// Provenance carried through from the dispatch
    pub provenance: ActionProvenance,
    /// Original dispatch time (for end-to-end latency)
    pub dispatch_time: Instant,
    /// Raw MIDI bytes sent by SendMidi/MidiForward (for recursion guard)
    pub sent_midi: Option<Vec<u8>>,
    /// Output port names captured by the executor during this dispatch
    /// (issue #555). Populated on the success path of every nested
    /// `SendMidi` and `MidiForward` (post `_source` resolution), so:
    ///
    /// - Wrapper actions (`Sequence`, `Repeat`, `Conditional`,
    ///   `ContextSwitchTable`) that nest one or more MIDI sends produce
    ///   one entry per successful inner send (Copilot review on
    ///   PR #1211).
    /// - `MidiForward { target: "_source" }` produces the *resolved*
    ///   port (the originating device's bound output), not the literal
    ///   `"_source"` placeholder.
    ///
    /// Empty for non-MIDI actions, failed sends, and actions that
    /// produced no MIDI output (e.g. a `Conditional` whose branch
    /// didn't fire).
    ///
    /// `EngineManager::handle_action_completion` opens a per-port
    /// blanket-suppression window on each entry when
    /// `advanced_settings.allow_cascade = false`.
    pub output_ports: Vec<String>,
    /// ADR-025 Phase 3.A: breadcrumbs collected by the executor
    /// whenever a dispatch resolves through a state-bearing route
    /// (a `ContextSwitchTable` branch or a state-bearing
    /// `Conditional` that matched). Empty for non-context-switch
    /// actions. Propagated into `MappingFiredPayload.routing_trace`
    /// by `EngineManager::handle_action_completion`.
    pub routing_trace: Vec<String>,
}

/// Handle for the executor thread, held by the event loop (D2)
pub struct ActionDispatcher {
    /// Bounded sender for dispatching actions
    dispatch_tx: crossbeam_channel::Sender<ActionDispatch>,
    /// Receiver for completions (tokio-compatible)
    pub completion_rx: tokio::sync::mpsc::UnboundedReceiver<ActionCompletion>,
    /// Shutdown signal
    shutdown: Arc<AtomicBool>,
    /// Thread join handle (taken during shutdown)
    thread_handle: Option<JoinHandle<()>>,
    /// Monotonically increasing invocation counter
    invocation_counter: AtomicU64,
}

impl ActionDispatcher {
    /// Spawn the executor thread and return the dispatcher handle.
    ///
    /// The `ActionExecutor` is constructed *inside* the thread (D1: thread affinity
    /// for Enigo on some platforms).
    ///
    /// `device_output_map` is shared with EngineManager for lock-free alias resolution
    /// (ADR-021 Phase 2A).
    pub fn spawn(device_output_map: Arc<ArcSwap<HashMap<String, String>>>) -> Self {
        Self::spawn_with_state(device_output_map, None)
    }

    /// Production-path constructor that threads the physical control-state
    /// store into the worker's `ActionExecutor` so `Conditional` actions
    /// with ADR-025 state conditions (`ActivePcIs`, `CcValueInRange`,
    /// `NoteHeld`) can evaluate against live state.
    ///
    /// `control_state` is `None` in tests that don't need state condition
    /// coverage — those paths behave as before (state conditions return
    /// `false`, falling through to `else_action`).
    pub fn spawn_with_state(
        device_output_map: Arc<ArcSwap<HashMap<String, String>>>,
        control_state: Option<Arc<PhysicalControlStateStore>>,
    ) -> Self {
        // Test/legacy convenience: private empty shared config (DENY) + a
        // virtual-port watch whose sender is immediately dropped (no updates).
        // The worker tolerates a dropped sender (treats has_changed Err as no
        // change). Production uses `spawn_with_config` to inject the SHARED
        // config + a live watch receiver (#2396).
        let shared_config = Arc::new(ArcSwap::from_pointee(SharedActionConfig::default()));
        let (_vport_tx, vport_rx) = watch::channel(Vec::<String>::new());
        Self::spawn_with_config(device_output_map, control_state, shared_config, vport_rx)
    }

    /// #2396: production spawn — inject the SHARED read-mostly config `ArcSwap`
    /// (OSC endpoints + D17 allow-map) and the virtual-port-names `watch`
    /// receiver. The worker attaches `shared_config` to its `ActionExecutor`
    /// (so EngineManager's `store`s reach the dispatch path) and applies
    /// virtual-port changes between actions on this thread (midir affinity,
    /// ADR-009 D1). `shared_config` starts at its default (empty maps = DENY)
    /// BEFORE the worker loop runs, so there is no fail-open window
    /// (ADR-042 D17).
    pub fn spawn_with_config(
        device_output_map: Arc<ArcSwap<HashMap<String, String>>>,
        control_state: Option<Arc<PhysicalControlStateStore>>,
        shared_config: Arc<ArcSwap<SharedActionConfig>>,
        vport_rx: watch::Receiver<Vec<String>>,
    ) -> Self {
        let (dispatch_tx, dispatch_rx) =
            crossbeam_channel::bounded::<ActionDispatch>(DISPATCH_CHANNEL_CAPACITY);
        let (completion_tx, completion_rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);

        let thread_handle = thread::Builder::new()
            .name("action-executor".into())
            .spawn(move || {
                let mut worker = ExecutorWorker::new(
                    dispatch_rx,
                    completion_tx,
                    shutdown_clone,
                    device_output_map,
                    control_state,
                    shared_config,
                    vport_rx,
                );
                worker.run();
            })
            .expect("Failed to spawn action-executor thread");

        Self {
            dispatch_tx,
            completion_rx,
            shutdown,
            thread_handle: Some(thread_handle),
            invocation_counter: AtomicU64::new(1),
        }
    }

    /// Allocate a new invocation ID (monotonically increasing)
    pub fn next_invocation_id(&self) -> u64 {
        self.invocation_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Try to dispatch an action (non-blocking, D3).
    ///
    /// Returns `Ok(invocation_id)` on success, or `Err(dispatch)` if the channel is full.
    #[allow(clippy::result_large_err)]
    pub fn try_dispatch(&self, dispatch: ActionDispatch) -> Result<u64, ActionDispatch> {
        let id = dispatch.invocation_id;
        match self.dispatch_tx.try_send(dispatch) {
            Ok(()) => Ok(id),
            Err(crossbeam_channel::TrySendError::Full(d)) => Err(d),
            Err(crossbeam_channel::TrySendError::Disconnected(d)) => {
                warn!("Executor thread disconnected");
                Err(d)
            }
        }
    }

    /// Dispatch an action and wait for its completion.
    ///
    /// Sends the dispatch, then polls `completion_rx` until we see the matching
    /// `invocation_id`. Other completions received in the interim are returned
    /// as well so the caller can process them.
    ///
    /// **Note:** Uses blocking `crossbeam::Sender::send()` which can block the
    /// tokio worker if the bounded channel is full. Currently only used in tests.
    /// Production simulation uses `try_dispatch()` (non-blocking).
    pub async fn dispatch_and_wait(
        &mut self,
        dispatch: ActionDispatch,
    ) -> Result<(ActionCompletion, Vec<ActionCompletion>), String> {
        let target_id = dispatch.invocation_id;

        // Blocking send: only safe in test usage. Do NOT call from production tokio tasks.
        self.dispatch_tx
            .send(dispatch)
            .map_err(|_| "Executor thread disconnected".to_string())?;

        // Collect completions until we find our target
        let mut other_completions = Vec::new();
        loop {
            match self.completion_rx.recv().await {
                Some(completion) if completion.invocation_id == target_id => {
                    return Ok((completion, other_completions));
                }
                Some(completion) => {
                    other_completions.push(completion);
                }
                None => {
                    return Err("Completion channel closed".to_string());
                }
            }
        }
    }

    /// Signal the executor thread to shut down and join it (D11).
    ///
    /// In-flight actions are cancelled via the shutdown flag (interruptible sleep
    /// checks every 10ms); queued actions are drained and returned as Cancelled.
    /// Join is blocking — the thread should exit promptly once the shutdown flag
    /// is set, but non-interruptible actions (e.g., Shell) may delay it.
    pub fn shutdown(&mut self) -> Vec<ActionCompletion> {
        info!("Shutting down action executor thread");
        self.shutdown.store(true, Ordering::Release);

        let mut cancelled = Vec::new();

        // Join the thread (blocking — no timeout; worker exits promptly via shutdown flag)
        if let Some(handle) = self.thread_handle.take() {
            // The worker thread checks shutdown between actions and during recv_timeout,
            // so it should exit promptly
            match handle.join() {
                Ok(()) => {
                    debug!("Action executor thread joined cleanly");
                }
                Err(e) => {
                    error!("Action executor thread panicked: {:?}", e);
                }
            }
        }

        // Drain any remaining completions
        while let Ok(completion) = self.completion_rx.try_recv() {
            cancelled.push(completion);
        }

        cancelled
    }
}

impl Drop for ActionDispatcher {
    fn drop(&mut self) {
        if self.thread_handle.is_some() {
            self.shutdown.store(true, Ordering::Release);
            if let Some(handle) = self.thread_handle.take() {
                let _ = handle.join();
            }
        }
    }
}

/// Worker running on the dedicated executor thread (private)
struct ExecutorWorker {
    executor: ActionExecutor,
    dispatch_rx: crossbeam_channel::Receiver<ActionDispatch>,
    completion_tx: tokio::sync::mpsc::UnboundedSender<ActionCompletion>,
    shutdown: Arc<AtomicBool>,
    /// #2396: latest-wins desired virtual-port names. Applied on this thread
    /// (midir create/teardown is thread-affine, ADR-009 D1) both at the idle
    /// top-of-loop AND right before each dispatch.
    vport_rx: watch::Receiver<Vec<String>>,
    /// #2396: the desired set most recently applied. `apply_pending_virtual_ports`
    /// diffs the latest published set against this instead of consuming the
    /// watch's edge-triggered `has_changed` flag, so it is idempotent and safe
    /// to call from both apply sites per loop iteration (PR #2403 review).
    applied_vports: Vec<String>,
}

impl ExecutorWorker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        dispatch_rx: crossbeam_channel::Receiver<ActionDispatch>,
        completion_tx: tokio::sync::mpsc::UnboundedSender<ActionCompletion>,
        shutdown: Arc<AtomicBool>,
        device_output_map: Arc<ArcSwap<HashMap<String, String>>>,
        control_state: Option<Arc<PhysicalControlStateStore>>,
        shared_config: Arc<ArcSwap<SharedActionConfig>>,
        vport_rx: watch::Receiver<Vec<String>>,
    ) -> Self {
        // D1: Construct ActionExecutor *inside* the thread for Enigo thread affinity.
        // ADR-025 Phase 2: attach the shared control-state store so Conditional
        // actions can evaluate `ActivePcIs` / `CcValueInRange` / `NoteHeld` against
        // live state on this, the production dispatch path.
        // #2396: attach the SHARED read-mostly config so OscForward target
        // resolution and the ADR-042 D17 gate read config that EngineManager
        // actually updates (the bug: it was set on a different executor).
        let executor =
            ActionExecutor::new(device_output_map).with_shared_action_config(shared_config);
        let executor = match control_state {
            Some(store) => executor.with_control_state(store),
            None => executor,
        };
        Self {
            executor,
            dispatch_rx,
            completion_tx,
            shutdown,
            vport_rx,
            applied_vports: Vec::new(),
        }
    }

    fn run(&mut self) {
        info!("Action executor thread started");

        loop {
            // Check shutdown before blocking on recv
            if self.shutdown.load(Ordering::Acquire) {
                debug!("Executor thread received shutdown signal");
                break;
            }

            // #2396: apply any pending virtual-port change on this thread
            // (midir create/teardown is thread-affine, ADR-009 D1) during the
            // idle/between-actions window, so a port created on reload/hot-plug
            // becomes visible even when no action is dispatched. Idempotent
            // (diffs against the last-applied set), so the second apply below is
            // a cheap no-op when nothing changed.
            self.apply_pending_virtual_ports();

            // recv_timeout so we periodically check shutdown (50ms granularity)
            match self
                .dispatch_rx
                .recv_timeout(std::time::Duration::from_millis(50))
            {
                Ok(dispatch) => {
                    // #2396 (Copilot review): a vport update may have landed
                    // while we were blocked in recv_timeout above. Re-apply
                    // BEFORE executing so this action sees current ports, not
                    // next-iteration-stale ones (the one-dispatch "port not
                    // found" window right after connect/reload/hot-plug).
                    self.apply_pending_virtual_ports();
                    self.execute_dispatch(dispatch);
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    // Normal idle cycle — check shutdown on next iteration
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    info!("Dispatch channel disconnected, executor thread exiting");
                    break;
                }
            }
        }

        // D13: Drain remaining queued dispatches and send Cancelled completions
        self.drain_queued_dispatches();

        info!("Action executor thread exiting");
    }

    /// #2396: apply the latest desired virtual-port set if it changed.
    ///
    /// Clones the names out and DROPS the watch `Ref` before the (potentially
    /// slow) midir OS work, so a concurrent `send` is never blocked behind port
    /// creation. `sync_virtual_ports` itself diffs desired-vs-current and only
    /// creates/tears down the delta (close-then-open), so a no-op tick is cheap
    /// and unchanged ports keep their handles (no in-flight-route breakage).
    fn apply_pending_virtual_ports(&mut self) {
        // #2396 review hardening (PR #2403): do NOT gate on the watch's
        // consumable `has_changed()` edge flag. This method runs from TWO sites
        // per loop iteration — the idle top-of-loop and right before each
        // dispatch (to close the recv_timeout race where a port update lands
        // while we're blocked — Copilot review) — and an edge consumed by the
        // first call would be lost to the second (cloud-review · blocking).
        // Instead diff the latest published set against the set we last
        // applied: idempotent, and safe to call any number of times. `borrow()`
        // never errors on a dropped sender (unlike `has_changed()`); it just
        // returns the last value. The clone drops the watch guard before the
        // potentially-slow midir work, so a concurrent `send` is never blocked.
        let desired = self.vport_rx.borrow().clone();
        if desired == self.applied_vports {
            return; // nothing changed since the last apply — cheap fast path
        }
        let started = Instant::now();
        let report = self.executor.sync_virtual_ports(&desired);
        // Record the attempt regardless of per-port creation success, matching
        // the prior edge-triggered no-retry semantics: a failed port is
        // surfaced via the warning below, not retried on every 50ms tick.
        self.applied_vports = desired;
        let elapsed = started.elapsed();
        if !report.created.is_empty() {
            info!(ports = ?report.created, "Created virtual MIDI port(s) on executor thread");
        }
        if !report.removed.is_empty() {
            info!(ports = ?report.removed, "Removed virtual MIDI port(s) on executor thread");
        }
        for (name, err) in &report.failed {
            warn!(port = %name, error = %err, "Failed to create virtual MIDI port");
        }
        if elapsed > Duration::from_millis(200) {
            warn!(
                elapsed_ms = elapsed.as_millis() as u64,
                "Virtual-port sync was slow on the executor thread (delays queued action dispatch)"
            );
        }
    }

    fn execute_dispatch(&mut self, dispatch: ActionDispatch) {
        let exec_start = Instant::now();
        let invocation_id = dispatch.invocation_id;

        // ADR-042 D17 (ADR-039-A Slice 2, #2325): thread the network-origin
        // taint into the executor for THIS dispatch. Set unconditionally
        // (Some or None) so a prior tainted dispatch can never bleed its
        // taint — or its absence — into this one. The gate itself runs
        // inside the executor before any (partial) execution.
        self.executor
            .set_network_origin(dispatch.network_origin.clone());

        trace!(
            invocation_id = invocation_id,
            action_type = dispatch.provenance.action_type,
            "Executing action"
        );

        // For SendMidi/MidiForward, compute the actual OUTPUT bytes for recursion guard (D8).
        // Compute expected MIDI output bytes from the Action params (not context.raw_midi,
        // which is the trigger's incoming bytes). Computed before execution for simplicity;
        // the completion handler only records these in the recursion guard on success.
        let sent_midi = match &dispatch.action {
            Action::SendMidi {
                message_type,
                channel,
                params,
                ..
            } => {
                compute_midi_output_bytes(message_type, *channel, params, dispatch.context.as_ref())
                    .ok()
            }
            Action::MidiForward { transform, .. } => dispatch
                .context
                .as_ref()
                .and_then(|c| c.raw_midi.as_ref())
                .map(|raw| compute_midi_forward_bytes(raw, transform.as_ref())),
            _ => None,
        };

        // Execute with interruptibility for Sequence/Delay/Repeat
        let result = self.execute_interruptible(dispatch.action, dispatch.context);
        let execution_time_us = exec_start.elapsed().as_micros() as u64;
        // ADR-025 Phase 3.A: drain the trace the executor accumulated
        // during this dispatch. Always done here regardless of
        // result so a cancelled / errored dispatch doesn't leak
        // breadcrumbs into the next one.
        let routing_trace = self.executor.take_routing_trace();

        // Issue #555: drain the per-port send trail collected by the
        // executor (one entry per successful nested SendMidi /
        // MidiForward, post `_source` resolution). Has to be drained
        // unconditionally for the same reason as `routing_trace`: a
        // failed/cancelled dispatch can't leak ports into the next
        // dispatch's window. See `ActionExecutor::sent_ports` for why
        // capture happens inside the executor rather than here.
        let output_ports = self.executor.take_sent_ports();

        let completion = ActionCompletion {
            invocation_id,
            result,
            execution_time_us,
            provenance: dispatch.provenance,
            dispatch_time: dispatch.dispatch_time,
            sent_midi,
            output_ports,
            routing_trace,
        };

        if let Err(e) = self.completion_tx.send(completion) {
            warn!(
                invocation_id = invocation_id,
                "Failed to send completion (receiver dropped): {}", e
            );
        }
    }

    /// Execute an action with shutdown awareness (D11, D13)
    ///
    /// For Sequence/Delay/Repeat, checks the shutdown flag between steps
    /// and returns Cancelled if set.
    fn execute_interruptible(
        &mut self,
        action: Action,
        context: Option<TriggerContext>,
    ) -> DispatchResult {
        match &action {
            Action::Sequence(_) => self.execute_sequence_interruptible(action, context),
            Action::Delay(ms) => {
                let ms = *ms;
                if self.interruptible_sleep(std::time::Duration::from_millis(ms)) {
                    Ok(DispatchOutcome::Cancelled)
                } else {
                    Ok(DispatchOutcome::Completed)
                }
            }
            Action::Repeat { .. } => self.execute_repeat_interruptible(action, context),
            _ => {
                // Non-compound actions: execute directly
                self.executor.execute(action, context)
            }
        }
    }

    /// Execute a Sequence with shutdown checks between steps
    fn execute_sequence_interruptible(
        &mut self,
        action: Action,
        context: Option<TriggerContext>,
    ) -> DispatchResult {
        let actions = match action {
            Action::Sequence(a) => a,
            _ => unreachable!(),
        };

        for (i, act) in actions.into_iter().enumerate() {
            // Check shutdown before each step
            if self.shutdown.load(Ordering::Acquire) {
                debug!(step = i, "Sequence cancelled by shutdown");
                return Ok(DispatchOutcome::Cancelled);
            }

            // Recursively handle nested sequences/delays
            let result = self.execute_interruptible(act, context.clone())?;

            match &result {
                DispatchOutcome::ModeChangeRequested { .. } => return Ok(result),
                DispatchOutcome::Cancelled => return Ok(result),
                DispatchOutcome::Completed => {}
            }

            // Inter-action delay (50ms) — interruptible
            if self.interruptible_sleep(std::time::Duration::from_millis(50)) {
                return Ok(DispatchOutcome::Cancelled);
            }
        }

        Ok(DispatchOutcome::Completed)
    }

    /// Execute a Repeat with shutdown checks between iterations
    fn execute_repeat_interruptible(
        &mut self,
        action: Action,
        context: Option<TriggerContext>,
    ) -> DispatchResult {
        let (inner, count, delay_ms) = match action {
            Action::Repeat {
                action,
                count,
                delay_ms,
            } => (*action, count, delay_ms),
            _ => unreachable!(),
        };

        for i in 0..count {
            if self.shutdown.load(Ordering::Acquire) {
                debug!(iteration = i, "Repeat cancelled by shutdown");
                return Ok(DispatchOutcome::Cancelled);
            }

            let result = self.execute_interruptible(inner.clone(), context.clone())?;
            match &result {
                DispatchOutcome::ModeChangeRequested { .. } => return Ok(result),
                DispatchOutcome::Cancelled => return Ok(result),
                DispatchOutcome::Completed => {}
            }

            // Inter-iteration delay
            if i < count - 1
                && let Some(delay) = delay_ms
                && self.interruptible_sleep(std::time::Duration::from_millis(delay))
            {
                return Ok(DispatchOutcome::Cancelled);
            }
        }

        Ok(DispatchOutcome::Completed)
    }

    /// Sleep in 10ms increments, checking shutdown flag.
    /// Returns `true` if shutdown was requested (i.e., sleep was interrupted).
    fn interruptible_sleep(&self, duration: std::time::Duration) -> bool {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if self.shutdown.load(Ordering::Acquire) {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let chunk = remaining.min(std::time::Duration::from_millis(10));
            if chunk.is_zero() {
                break;
            }
            thread::sleep(chunk);
        }
        false
    }

    /// Drain queued dispatches on shutdown, sending Cancelled completions (D13)
    fn drain_queued_dispatches(&self) {
        let mut count = 0;
        while let Ok(dispatch) = self.dispatch_rx.try_recv() {
            let completion = ActionCompletion {
                invocation_id: dispatch.invocation_id,
                result: Ok(DispatchOutcome::Cancelled),
                execution_time_us: 0,
                provenance: dispatch.provenance,
                dispatch_time: dispatch.dispatch_time,
                sent_midi: None,
                // Cancelled before the executor ran — no MIDI sent,
                // so no port-level cascade suppression to set up.
                output_ports: Vec::new(),
                // Cancelled before the executor ran — no trace.
                routing_trace: Vec::new(),
            };
            let _ = self.completion_tx.send(completion);
            count += 1;
        }
        if count > 0 {
            debug!(count, "Drained queued dispatches as Cancelled on shutdown");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::event_types::FiredTriggerInfo;

    fn test_provenance() -> ActionProvenance {
        ActionProvenance {
            device_id: None,
            matched_rule: None,
            mode_name: Some("Default".to_string()),
            action_type: "delay".to_string(),
            action_summary: "Delay 1ms".to_string(),
            trigger_info: FiredTriggerInfo {
                trigger_type: "note".to_string(),
                device: None,
                channel: None,
                number: Some(36),
                value: Some(100),
            },
            mapping_label: None,
            let_through: false,
        }
    }

    #[tokio::test]
    async fn test_spawn_and_shutdown() {
        let mut dispatcher =
            ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
        // Give the thread a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let cancelled = dispatcher.shutdown();
        // No dispatches, so no cancelled items
        assert!(cancelled.is_empty());
    }

    #[tokio::test]
    async fn test_dispatch_delay_and_receive_completion() {
        let mut dispatcher =
            ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
        let id = dispatcher.next_invocation_id();

        let dispatch = ActionDispatch {
            invocation_id: id,
            action: Action::Delay(1),
            context: None,
            provenance: test_provenance(),
            dispatch_time: Instant::now(),
            network_origin: None,
        };

        let result = dispatcher.try_dispatch(dispatch);
        assert!(result.is_ok());

        // Wait for completion
        let completion = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            dispatcher.completion_rx.recv(),
        )
        .await
        .expect("Timed out waiting for completion")
        .expect("Channel closed");

        assert_eq!(completion.invocation_id, id);
        assert!(matches!(completion.result, Ok(DispatchOutcome::Completed)));
        assert!(completion.execution_time_us > 0);

        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_dispatch_mode_change() {
        let mut dispatcher =
            ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
        let id = dispatcher.next_invocation_id();

        let dispatch = ActionDispatch {
            invocation_id: id,
            action: Action::ModeChange {
                mode: "Live".to_string(),
            },
            context: None,
            provenance: ActionProvenance {
                action_type: "mode_change".to_string(),
                action_summary: "Switch to Live".to_string(),
                ..test_provenance()
            },
            dispatch_time: Instant::now(),
            network_origin: None,
        };

        dispatcher.try_dispatch(dispatch).unwrap();

        let completion = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            dispatcher.completion_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(completion.invocation_id, id);
        assert!(matches!(
            completion.result,
            Ok(DispatchOutcome::ModeChangeRequested { ref mode }) if mode == "Live"
        ));

        dispatcher.shutdown();
    }

    #[test]
    fn test_channel_full_returns_error() {
        // Test the bounded channel semantics directly without executor thread.
        // crossbeam bounded(N) blocks or returns Full when N items are queued.
        let (tx, _rx) = crossbeam_channel::bounded::<ActionDispatch>(DISPATCH_CHANNEL_CAPACITY);

        // Fill the channel
        for i in 0..DISPATCH_CHANNEL_CAPACITY {
            let dispatch = ActionDispatch {
                invocation_id: i as u64 + 1,
                action: Action::Delay(1),
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            };
            tx.try_send(dispatch).unwrap();
        }

        // (DISPATCH_CHANNEL_CAPACITY + 1)th dispatch should fail
        let overflow = ActionDispatch {
            invocation_id: 999,
            action: Action::Delay(1),
            context: None,
            provenance: test_provenance(),
            dispatch_time: Instant::now(),
            network_origin: None,
        };

        let result = tx.try_send(overflow);
        assert!(
            matches!(result, Err(crossbeam_channel::TrySendError::Full(_))),
            "Expected channel full error"
        );
    }

    #[tokio::test]
    async fn test_invocation_ids_monotonic() {
        let dispatcher = ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
        let id1 = dispatcher.next_invocation_id();
        let id2 = dispatcher.next_invocation_id();
        let id3 = dispatcher.next_invocation_id();
        assert!(id2 > id1);
        assert!(id3 > id2);
        drop(dispatcher);
    }

    #[tokio::test]
    async fn test_execution_time_populated() {
        let mut dispatcher =
            ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
        let id = dispatcher.next_invocation_id();

        let dispatch = ActionDispatch {
            invocation_id: id,
            action: Action::Delay(10), // 10ms delay
            context: None,
            provenance: test_provenance(),
            dispatch_time: Instant::now(),
            network_origin: None,
        };

        dispatcher.try_dispatch(dispatch).unwrap();

        let completion = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            dispatcher.completion_rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        // Execution time should be at least 10ms = 10000us
        assert!(
            completion.execution_time_us >= 5000,
            "Expected >= 5000us, got {}us",
            completion.execution_time_us
        );

        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_delay_cancelled_on_shutdown() {
        let mut dispatcher =
            ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
        let id = dispatcher.next_invocation_id();

        let dispatch = ActionDispatch {
            invocation_id: id,
            action: Action::Delay(5000), // 5 second delay
            context: None,
            provenance: test_provenance(),
            dispatch_time: Instant::now(),
            network_origin: None,
        };

        dispatcher.try_dispatch(dispatch).unwrap();

        // Wait a bit, then shut down
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let start = Instant::now();
        dispatcher.shutdown();
        let elapsed = start.elapsed();

        // Should complete much faster than 5s (within ~120ms)
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "Shutdown took too long: {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_sequence_cancelled_midway() {
        let mut dispatcher =
            ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
        let id = dispatcher.next_invocation_id();

        // 10-step sequence with 100ms delays
        let actions: Vec<Action> = (0..10).map(|_| Action::Delay(100)).collect();
        let dispatch = ActionDispatch {
            invocation_id: id,
            action: Action::Sequence(actions),
            context: None,
            provenance: ActionProvenance {
                action_type: "sequence".to_string(),
                action_summary: "Sequence (10)".to_string(),
                ..test_provenance()
            },
            dispatch_time: Instant::now(),
            network_origin: None,
        };

        dispatcher.try_dispatch(dispatch).unwrap();

        // Let it start executing some steps, then cancel
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let cancelled = dispatcher.shutdown();

        // The in-flight sequence should have been cancelled.
        // The completion appears in the cancelled vec (drained from completion_rx
        // during shutdown).
        let sequence_completion = cancelled.iter().find(|c| c.invocation_id == id);
        assert!(
            sequence_completion.is_some(),
            "Expected completion for in-flight sequence invocation {}",
            id
        );
        let completion = sequence_completion.unwrap();
        assert!(
            matches!(completion.result, Ok(DispatchOutcome::Cancelled)),
            "Expected Cancelled outcome, got {:?}",
            completion.result
        );
    }

    #[tokio::test]
    async fn test_dispatch_and_wait() {
        let mut dispatcher =
            ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
        let id = dispatcher.next_invocation_id();

        let dispatch = ActionDispatch {
            invocation_id: id,
            action: Action::Delay(1),
            context: None,
            provenance: test_provenance(),
            dispatch_time: Instant::now(),
            network_origin: None,
        };

        let (completion, others) = dispatcher.dispatch_and_wait(dispatch).await.unwrap();
        assert_eq!(completion.invocation_id, id);
        assert!(others.is_empty());

        dispatcher.shutdown();
    }

    // ─── ADR-025 Phase 2: state conditions on the dispatcher hot path ──
    //
    // Regression for the bug surfaced in chat session b8d0bc01: the
    // dispatcher owns its own ActionExecutor (Enigo thread affinity) so
    // attaching `control_state` to the EngineManager's Mutex-wrapped
    // executor only covered the simulation path, not the production
    // ingress path. Every state condition evaluated through this
    // dispatcher returned `false` → `else_action` always fired, even
    // when `conductor_get_active_pc` agreed with the condition.
    //
    // These two tests pin the contract:
    //   - `spawn_with_state(_, Some(store))` must produce an executor
    //     that matches state conditions against the store.
    //   - `spawn_with_state(_, None)` (and the forwarding `spawn`)
    //     must continue working — state conditions fall through to
    //     else_action instead of panicking.
    #[tokio::test]
    async fn test_dispatcher_state_condition_matches_with_store() {
        use conductor_core::Condition;
        use conductor_core::control_state::PhysicalControlStateStore;
        use conductor_core::event_processor::MidiEvent;

        let store = Arc::new(PhysicalControlStateStore::default());
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 12,
                channel: 0,
                time: Instant::now(),
            },
        );

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );
        let id = dispatcher.next_invocation_id();

        let action = Action::Conditional {
            condition: Condition::ActivePcIs {
                pc: 12,
                channel: 0,
                device: "fcb1010".into(),
            },
            then_action: Box::new(Action::ModeChange {
                mode: "Lead".into(),
            }),
            else_action: Some(Box::new(Action::ModeChange {
                mode: "Rhythm".into(),
            })),
        };

        let dispatch = ActionDispatch {
            invocation_id: id,
            action,
            context: None,
            provenance: test_provenance(),
            dispatch_time: Instant::now(),
            network_origin: None,
        };

        let (completion, _others) = dispatcher.dispatch_and_wait(dispatch).await.unwrap();
        assert_eq!(
            completion.result.as_ref().ok(),
            Some(&DispatchOutcome::ModeChangeRequested {
                mode: "Lead".into(),
            }),
            "condition should match store state → then branch (Lead), not else (Rhythm)"
        );

        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_dispatcher_state_condition_without_store_falls_through_to_else() {
        use conductor_core::Condition;

        let mut dispatcher =
            ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
        let id = dispatcher.next_invocation_id();

        let action = Action::Conditional {
            condition: Condition::ActivePcIs {
                pc: 12,
                channel: 0,
                device: "fcb1010".into(),
            },
            then_action: Box::new(Action::ModeChange {
                mode: "ShouldNotRun".into(),
            }),
            else_action: Some(Box::new(Action::ModeChange {
                mode: "Fallback".into(),
            })),
        };

        let dispatch = ActionDispatch {
            invocation_id: id,
            action,
            context: None,
            provenance: test_provenance(),
            dispatch_time: Instant::now(),
            network_origin: None,
        };

        let (completion, _) = dispatcher.dispatch_and_wait(dispatch).await.unwrap();
        assert_eq!(
            completion.result.as_ref().ok(),
            Some(&DispatchOutcome::ModeChangeRequested {
                mode: "Fallback".into(),
            })
        );

        dispatcher.shutdown();
    }

    // ─── ADR-025 Phase 2.F: ContextSwitchTable dispatch ────────────────
    //
    // Same wiring as the state-condition tests above — the dispatcher
    // owns its own ActionExecutor and needs `control_state` attached
    // via `spawn_with_state`. These tests exercise the new
    // Action::ContextSwitchTable arm end-to-end.

    fn context_switch_branch_modechange(mode: &str) -> Box<Action> {
        Box::new(Action::ModeChange { mode: mode.into() })
    }

    #[tokio::test]
    async fn test_dispatcher_pc_context_switch_table_matches_active_pc() {
        use conductor_core::actions::{ContextBranchTable, ContextKind, LoweringSource};
        use conductor_core::control_state::PhysicalControlStateStore;
        use conductor_core::event_processor::MidiEvent;

        let store = Arc::new(PhysicalControlStateStore::default());
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 12,
                channel: 0,
                time: Instant::now(),
            },
        );

        let mut branches = HashMap::new();
        branches.insert(11, context_switch_branch_modechange("Rhythm"));
        branches.insert(12, context_switch_branch_modechange("Lead"));
        branches.insert(13, context_switch_branch_modechange("Clean"));

        let action = Action::ContextSwitchTable {
            kind: ContextKind::Pc,
            channel: 0,
            device: "fcb1010".into(),
            branches: ContextBranchTable::Pc(branches),
            default: Some(context_switch_branch_modechange("Fallback")),
            source: LoweringSource {
                origin: "test".into(),
                branch_index: None,
            },
        };

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );
        let id = dispatcher.next_invocation_id();
        let (completion, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();

        assert_eq!(
            completion.result.as_ref().ok(),
            Some(&DispatchOutcome::ModeChangeRequested {
                mode: "Lead".into()
            }),
            "PC 12 branch must fire (not Fallback)"
        );
        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_dispatcher_pc_context_switch_table_falls_back_to_default() {
        // Store has a PC that isn't in the branch table → default fires.
        use conductor_core::actions::{ContextBranchTable, ContextKind, LoweringSource};
        use conductor_core::control_state::PhysicalControlStateStore;
        use conductor_core::event_processor::MidiEvent;

        let store = Arc::new(PhysicalControlStateStore::default());
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 99,
                channel: 0,
                time: Instant::now(),
            },
        );

        let mut branches = HashMap::new();
        branches.insert(12, context_switch_branch_modechange("Lead"));
        let action = Action::ContextSwitchTable {
            kind: ContextKind::Pc,
            channel: 0,
            device: "fcb1010".into(),
            branches: ContextBranchTable::Pc(branches),
            default: Some(context_switch_branch_modechange("Fallback")),
            source: LoweringSource {
                origin: "test".into(),
                branch_index: None,
            },
        };

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );
        let id = dispatcher.next_invocation_id();
        let (completion, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();

        assert_eq!(
            completion.result.as_ref().ok(),
            Some(&DispatchOutcome::ModeChangeRequested {
                mode: "Fallback".into()
            }),
            "unmatched PC must fall through to default"
        );
        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_dispatcher_pc_context_switch_table_no_observed_pc_and_no_default_noops() {
        // Store is wired, but no PC has been observed for (device, channel)
        // and the action has no default → Completed with no side effect.
        use conductor_core::actions::{ContextBranchTable, ContextKind, LoweringSource};
        use conductor_core::control_state::PhysicalControlStateStore;

        let store = Arc::new(PhysicalControlStateStore::default());
        let mut branches = HashMap::new();
        branches.insert(12, context_switch_branch_modechange("Lead"));
        let action = Action::ContextSwitchTable {
            kind: ContextKind::Pc,
            channel: 0,
            device: "fcb1010".into(),
            branches: ContextBranchTable::Pc(branches),
            default: None,
            source: LoweringSource {
                origin: "test".into(),
                branch_index: None,
            },
        };

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );
        let id = dispatcher.next_invocation_id();
        let (completion, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();

        assert_eq!(
            completion.result.as_ref().ok(),
            Some(&DispatchOutcome::Completed),
            "no state + no default must be a clean Completed no-op, not a panic"
        );
        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_dispatcher_cc_context_switch_table_matches_range() {
        use conductor_core::actions::{ContextBranchTable, ContextKind, LoweringSource};
        use conductor_core::control_state::PhysicalControlStateStore;
        use conductor_core::event_processor::MidiEvent;

        let store = Arc::new(PhysicalControlStateStore::default());
        // CC 1 value = 50 → should match the mid range [32, 95].
        store.observe_event(
            "keyboard",
            &MidiEvent::ControlChange {
                cc: 1,
                value: 50,
                channel: 0,
                time: Instant::now(),
            },
        );

        let ranges = vec![
            (0u8, 31u8, context_switch_branch_modechange("Low")),
            (32, 95, context_switch_branch_modechange("Mid")),
            (96, 127, context_switch_branch_modechange("High")),
        ];
        let action = Action::ContextSwitchTable {
            kind: ContextKind::Cc,
            channel: 0,
            device: "keyboard".into(),
            branches: ContextBranchTable::Cc { cc: 1, ranges },
            default: Some(context_switch_branch_modechange("Fallback")),
            source: LoweringSource {
                origin: "test".into(),
                branch_index: None,
            },
        };

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );
        let id = dispatcher.next_invocation_id();
        let (completion, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();

        assert_eq!(
            completion.result.as_ref().ok(),
            Some(&DispatchOutcome::ModeChangeRequested { mode: "Mid".into() }),
            "CC value 50 must match range [32,95] → Mid"
        );
        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_dispatcher_cc_context_switch_table_boundary_matches() {
        // Range endpoints are inclusive. 64 matches the low range
        // [0,64], 65 matches [65,127] — validates the dispatcher's
        // inclusive comparisons match CcValueInRange semantics.
        use conductor_core::actions::{ContextBranchTable, ContextKind, LoweringSource};
        use conductor_core::control_state::PhysicalControlStateStore;
        use conductor_core::event_processor::MidiEvent;

        for (value, expected) in [(64u8, "Low"), (65u8, "High")] {
            let store = Arc::new(PhysicalControlStateStore::default());
            store.observe_event(
                "keyboard",
                &MidiEvent::ControlChange {
                    cc: 7,
                    value,
                    channel: 0,
                    time: Instant::now(),
                },
            );

            let ranges = vec![
                (0u8, 64u8, context_switch_branch_modechange("Low")),
                (65, 127, context_switch_branch_modechange("High")),
            ];
            let action = Action::ContextSwitchTable {
                kind: ContextKind::Cc,
                channel: 0,
                device: "keyboard".into(),
                branches: ContextBranchTable::Cc { cc: 7, ranges },
                default: None,
                source: LoweringSource {
                    origin: "test".into(),
                    branch_index: None,
                },
            };

            let mut dispatcher = ActionDispatcher::spawn_with_state(
                Arc::new(ArcSwap::from_pointee(HashMap::new())),
                Some(Arc::clone(&store)),
            );
            let id = dispatcher.next_invocation_id();
            let (completion, _) = dispatcher
                .dispatch_and_wait(ActionDispatch {
                    invocation_id: id,
                    action,
                    context: None,
                    provenance: test_provenance(),
                    dispatch_time: Instant::now(),
                    network_origin: None,
                })
                .await
                .unwrap();

            assert_eq!(
                completion.result.as_ref().ok(),
                Some(&DispatchOutcome::ModeChangeRequested {
                    mode: expected.into()
                }),
                "CC value {} should match range containing that boundary → {}",
                value,
                expected
            );
            dispatcher.shutdown();
        }
    }

    #[tokio::test]
    async fn test_dispatcher_context_switch_table_without_store_uses_default() {
        // No control_state wired → dispatcher cannot look up state,
        // falls through to default. Mirrors the Conditional arm's
        // no-store behaviour from Phase 2.A+B.
        use conductor_core::actions::{ContextBranchTable, ContextKind, LoweringSource};

        let mut branches = HashMap::new();
        branches.insert(12, context_switch_branch_modechange("Lead"));
        let action = Action::ContextSwitchTable {
            kind: ContextKind::Pc,
            channel: 0,
            device: "fcb1010".into(),
            branches: ContextBranchTable::Pc(branches),
            default: Some(context_switch_branch_modechange("NoStore")),
            source: LoweringSource {
                origin: "test".into(),
                branch_index: None,
            },
        };

        // spawn (not spawn_with_state) → no store
        let mut dispatcher =
            ActionDispatcher::spawn(Arc::new(ArcSwap::from_pointee(HashMap::new())));
        let id = dispatcher.next_invocation_id();
        let (completion, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();

        assert_eq!(
            completion.result.as_ref().ok(),
            Some(&DispatchOutcome::ModeChangeRequested {
                mode: "NoStore".into()
            })
        );
        dispatcher.shutdown();
    }

    // ─── ADR-025 Phase 3.A — routing_trace ──────────────────────
    //
    // The executor records breadcrumbs on dispatches that resolve
    // through a state-bearing route. EventStreamPanel will render
    // these on the row ("via PC 12 → Lead"). Tests below pin the
    // emission surface across ContextSwitchTable + Conditional
    // paths, plus the no-trace-on-default negative case.

    #[tokio::test]
    async fn test_context_switch_table_pc_emits_routing_trace() {
        use conductor_core::actions::{ContextBranchTable, ContextKind, LoweringSource};
        use conductor_core::control_state::PhysicalControlStateStore;
        use conductor_core::event_processor::MidiEvent;

        let store = Arc::new(PhysicalControlStateStore::default());
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 12,
                channel: 0,
                time: Instant::now(),
            },
        );
        let mut branches = HashMap::new();
        branches.insert(12, context_switch_branch_modechange("Lead"));
        let action = Action::ContextSwitchTable {
            kind: ContextKind::Pc,
            channel: 0,
            device: "fcb1010".into(),
            branches: ContextBranchTable::Pc(branches),
            default: Some(context_switch_branch_modechange("Fallback")),
            source: LoweringSource {
                origin: "test".into(),
                branch_index: None,
            },
        };

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );
        let id = dispatcher.next_invocation_id();
        let (completion, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();

        assert_eq!(completion.routing_trace.len(), 1);
        assert!(
            completion.routing_trace[0].contains("PC 12"),
            "trace should name the observed PC, got: {:?}",
            completion.routing_trace
        );
        assert!(completion.routing_trace[0].contains("fcb1010"));
        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_context_switch_table_cc_emits_routing_trace() {
        use conductor_core::actions::{ContextBranchTable, ContextKind, LoweringSource};
        use conductor_core::control_state::PhysicalControlStateStore;
        use conductor_core::event_processor::MidiEvent;

        let store = Arc::new(PhysicalControlStateStore::default());
        store.observe_event(
            "keyboard",
            &MidiEvent::ControlChange {
                cc: 1,
                value: 50,
                channel: 0,
                time: Instant::now(),
            },
        );
        let ranges = vec![
            (0u8, 63u8, context_switch_branch_modechange("Low")),
            (64u8, 127u8, context_switch_branch_modechange("High")),
        ];
        let action = Action::ContextSwitchTable {
            kind: ContextKind::Cc,
            channel: 0,
            device: "keyboard".into(),
            branches: ContextBranchTable::Cc { cc: 1, ranges },
            default: None,
            source: LoweringSource {
                origin: "test".into(),
                branch_index: None,
            },
        };

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );
        let id = dispatcher.next_invocation_id();
        let (completion, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();

        assert_eq!(completion.routing_trace.len(), 1);
        let entry = &completion.routing_trace[0];
        assert!(entry.contains("CC 1"));
        assert!(entry.contains("50")); // observed value
        assert!(entry.contains("[0, 63]")); // matched range
        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_context_switch_table_default_fallback_no_trace() {
        // Fallthrough to default isn't a "via ..." route the user
        // would want annotated on the row — the default isn't the
        // context-switched path, it's the fallback. No trace.
        use conductor_core::actions::{ContextBranchTable, ContextKind, LoweringSource};
        use conductor_core::control_state::PhysicalControlStateStore;

        let store = Arc::new(PhysicalControlStateStore::default()); // empty
        let mut branches = HashMap::new();
        branches.insert(12, context_switch_branch_modechange("Lead"));
        let action = Action::ContextSwitchTable {
            kind: ContextKind::Pc,
            channel: 0,
            device: "fcb1010".into(),
            branches: ContextBranchTable::Pc(branches),
            default: Some(context_switch_branch_modechange("Fallback")),
            source: LoweringSource {
                origin: "test".into(),
                branch_index: None,
            },
        };

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );
        let id = dispatcher.next_invocation_id();
        let (completion, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();

        assert!(
            completion.routing_trace.is_empty(),
            "default-fallback path must not emit a trace entry, got: {:?}",
            completion.routing_trace
        );
        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_conditional_active_pc_is_emits_routing_trace() {
        // Phase 2.E lowering of ≤MAX_LINEAR_BRANCHES PcContextSwitch
        // collapses to a nested `Conditional` chain with
        // `ActivePcIs` conditions. This test pins the Conditional-
        // path breadcrumb so the common-case lowering emits the
        // same "via ..." entry as the oversize ContextSwitchTable.
        use conductor_core::Condition;
        use conductor_core::control_state::PhysicalControlStateStore;
        use conductor_core::event_processor::MidiEvent;

        let store = Arc::new(PhysicalControlStateStore::default());
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 7,
                channel: 0,
                time: Instant::now(),
            },
        );
        let action = Action::Conditional {
            condition: Condition::ActivePcIs {
                pc: 7,
                channel: 0,
                device: "fcb1010".into(),
            },
            then_action: context_switch_branch_modechange("Match"),
            else_action: Some(context_switch_branch_modechange("Miss")),
        };

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );
        let id = dispatcher.next_invocation_id();
        let (completion, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();

        assert_eq!(
            completion.result.as_ref().ok(),
            Some(&DispatchOutcome::ModeChangeRequested {
                mode: "Match".into()
            })
        );
        assert_eq!(completion.routing_trace.len(), 1);
        let entry = &completion.routing_trace[0];
        assert!(entry.contains("PC 7"));
        assert!(entry.contains("fcb1010"));
        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_conditional_unmatched_condition_no_trace() {
        // Condition didn't match → else branch ran → no trace.
        // The trace only records the route that actually fired.
        use conductor_core::Condition;
        use conductor_core::control_state::PhysicalControlStateStore;

        let store = Arc::new(PhysicalControlStateStore::default()); // no observed PC
        let action = Action::Conditional {
            condition: Condition::ActivePcIs {
                pc: 7,
                channel: 0,
                device: "fcb1010".into(),
            },
            then_action: context_switch_branch_modechange("Match"),
            else_action: Some(context_switch_branch_modechange("Miss")),
        };

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );
        let id = dispatcher.next_invocation_id();
        let (completion, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();

        assert_eq!(
            completion.result.as_ref().ok(),
            Some(&DispatchOutcome::ModeChangeRequested {
                mode: "Miss".into()
            })
        );
        assert!(completion.routing_trace.is_empty());
        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn test_routing_trace_cleared_between_dispatches() {
        // Dispatch a context-switch that DOES populate the trace,
        // then dispatch a plain action on the same dispatcher and
        // confirm the second completion carries an empty trace.
        // Without `take_routing_trace` draining per dispatch, a
        // stale breadcrumb from the first fire would leak into the
        // second event's MappingFiredPayload.
        use conductor_core::actions::{ContextBranchTable, ContextKind, LoweringSource};
        use conductor_core::control_state::PhysicalControlStateStore;
        use conductor_core::event_processor::MidiEvent;

        let store = Arc::new(PhysicalControlStateStore::default());
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 5,
                channel: 0,
                time: Instant::now(),
            },
        );
        let mut branches = HashMap::new();
        branches.insert(5, context_switch_branch_modechange("Lead"));
        let context_switch = Action::ContextSwitchTable {
            kind: ContextKind::Pc,
            channel: 0,
            device: "fcb1010".into(),
            branches: ContextBranchTable::Pc(branches),
            default: None,
            source: LoweringSource {
                origin: "test".into(),
                branch_index: None,
            },
        };

        let mut dispatcher = ActionDispatcher::spawn_with_state(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            Some(Arc::clone(&store)),
        );

        // First dispatch: context-switched, populates the trace.
        let id1 = dispatcher.next_invocation_id();
        let (first, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id1,
                action: context_switch,
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();
        assert_eq!(
            first.routing_trace.len(),
            1,
            "first dispatch should have populated the trace"
        );

        // Second dispatch on the SAME dispatcher — a plain action
        // whose execution doesn't touch routing_trace at all. Must
        // NOT inherit the breadcrumb from the first dispatch.
        let id2 = dispatcher.next_invocation_id();
        let (second, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id2,
                action: Action::Delay(1),
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: None,
            })
            .await
            .unwrap();
        assert!(
            second.routing_trace.is_empty(),
            "second dispatch must start with a clean trace, got: {:?}",
            second.routing_trace
        );
        dispatcher.shutdown();
    }

    // ── #2396: executor-config-reaches-the-dispatch-thread integration tests ──
    // These drive config through the REAL propagation channels (the shared
    // `ArcSwap` + the dispatch thread) and assert a DISPATCHED action observes
    // it — the coverage gap that let the original bug ship (unit tests had only
    // ever set config on a standalone executor). Config is stored AFTER spawn,
    // exactly as EngineManager does it on connect / reload / listener-bind.

    #[tokio::test]
    async fn osc_forward_resolves_endpoint_stored_via_shared_arcswap_after_spawn() {
        use std::net::UdpSocket;

        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();

        let shared = Arc::new(ArcSwap::from_pointee(SharedActionConfig::default()));
        let (_vtx, vrx) = watch::channel(Vec::<String>::new());
        let mut dispatcher = ActionDispatcher::spawn_with_config(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            None,
            Arc::clone(&shared),
            vrx,
        );

        // EngineManager-style store AFTER the thread is already running.
        shared.store(Arc::new(SharedActionConfig {
            network_sensitive_allow: HashMap::new(),
            osc_output_endpoints: HashMap::from([(
                "out".to_string(),
                ("127.0.0.1".to_string(), port),
            )]),
        }));

        let id = dispatcher.next_invocation_id();
        let dispatch = ActionDispatch {
            invocation_id: id,
            action: Action::OscForward {
                target: "out".to_string(),
                transform: None,
            },
            context: Some(TriggerContext {
                osc_message: Some(conductor_core::events::OscInbound {
                    address: "/x".to_string(),
                    args: vec![conductor_core::OscArg::Float(1.0)],
                    time: Instant::now(),
                }),
                ..Default::default()
            }),
            provenance: test_provenance(),
            dispatch_time: Instant::now(),
            network_origin: None,
        };
        let (completion, _) = dispatcher.dispatch_and_wait(dispatch).await.unwrap();
        assert!(
            matches!(completion.result, Ok(DispatchOutcome::Completed)),
            "OscForward must resolve the endpoint stored via the shared ArcSwap \
             AFTER spawn (the #2396 propagation path), got: {:?}",
            completion.result
        );
        // End-to-end: the message actually reached the UDP endpoint.
        let mut buf = [0u8; 1024];
        listener
            .recv_from(&mut buf)
            .expect("forwarded OSC packet should arrive at the resolved endpoint");

        dispatcher.shutdown();
    }

    #[tokio::test]
    async fn d17_gate_reads_allow_map_propagated_via_shared_arcswap() {
        use conductor_core::dispatch::DispatchError;

        let shared = Arc::new(ArcSwap::from_pointee(SharedActionConfig::default()));
        let (_vtx, vrx) = watch::channel(Vec::<String>::new());
        let mut dispatcher = ActionDispatcher::spawn_with_config(
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            None,
            Arc::clone(&shared),
            vrx,
        );

        // Default (empty) config on the thread = fail-safe DENY for a
        // network-origin sensitive action.
        let id = dispatcher.next_invocation_id();
        let (denied, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action: Action::Text("x".to_string()),
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: Some("osc_in".to_string()),
            })
            .await
            .unwrap();
        assert!(
            matches!(
                denied.result,
                Err(DispatchError::NetworkActionClassBlocked { .. })
            ),
            "empty allow-map on the dispatch thread must DENY a network-origin \
             sensitive action (no fail-open), got: {:?}",
            denied.result
        );

        // Store an allow-map (EngineManager style) → the SAME origin is now
        // permitted past the gate (it may fail later for unrelated reasons, but
        // NOT with the gate block).
        shared.store(Arc::new(SharedActionConfig {
            network_sensitive_allow: HashMap::from([("osc_in".to_string(), true)]),
            osc_output_endpoints: HashMap::new(),
        }));
        let id = dispatcher.next_invocation_id();
        let (allowed, _) = dispatcher
            .dispatch_and_wait(ActionDispatch {
                invocation_id: id,
                action: Action::Text("x".to_string()),
                context: None,
                provenance: test_provenance(),
                dispatch_time: Instant::now(),
                network_origin: Some("osc_in".to_string()),
            })
            .await
            .unwrap();
        assert!(
            !matches!(
                allowed.result,
                Err(DispatchError::NetworkActionClassBlocked { .. })
            ),
            "after storing allow_sensitive_actions=true via the shared ArcSwap, \
             the gate must NOT block the network-origin action, got: {:?}",
            allowed.result
        );

        dispatcher.shutdown();
    }

    // #2396 review hardening (PR #2403): the virtual-port apply must be
    // idempotent and latest-wins, NOT gated on the watch's consumable
    // `has_changed()` edge flag.
    //   - Copilot: apply must run before each dispatch (not only at the idle
    //     top-of-loop), so a port update that lands while the worker is blocked
    //     in `recv_timeout` is reflected for the very next action — closing the
    //     one-dispatch "port not found" window after connect/reload/hot-plug.
    //   - cloud-review (blocking): consuming the edge flag in one call would
    //     starve the other call site of the notification; a diff against the
    //     last-applied set is safe to call any number of times.
    #[test]
    fn apply_pending_virtual_ports_is_idempotent_and_latest_wins() {
        let (_dtx, drx) = crossbeam_channel::bounded::<ActionDispatch>(DISPATCH_CHANNEL_CAPACITY);
        let (ctx, _crx) = tokio::sync::mpsc::unbounded_channel::<ActionCompletion>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let (vtx, vrx) = watch::channel(Vec::<String>::new());
        let mut worker = ExecutorWorker::new(
            drx,
            ctx,
            shutdown,
            Arc::new(ArcSwap::from_pointee(HashMap::new())),
            None,
            Arc::new(ArcSwap::from_pointee(SharedActionConfig::default())),
            vrx,
        );

        // Empty desired set → applying is a no-op.
        worker.apply_pending_virtual_ports();
        assert!(worker.applied_vports.is_empty());

        // A desired set is published, then reflected in a SINGLE apply call —
        // this is what lets the apply run right before dispatch and still see
        // the latest set (the Copilot race fix).
        let desired = vec!["conductor-test-vport-2396".to_string()];
        vtx.send(desired.clone()).unwrap();
        worker.apply_pending_virtual_ports();
        assert_eq!(
            worker.applied_vports, desired,
            "a single apply must reflect the latest published set so it can run \
             before dispatch (closes the recv_timeout staleness race)"
        );

        // Idempotent: a second apply with no new publish neither loses nor
        // duplicates state (no reliance on a consumed edge flag).
        worker.apply_pending_virtual_ports();
        assert_eq!(worker.applied_vports, desired);

        // Latest-wins: a subsequent teardown (empty) is picked up.
        vtx.send(Vec::new()).unwrap();
        worker.apply_pending_virtual_ports();
        assert!(
            worker.applied_vports.is_empty(),
            "apply must track the latest published set, not a stale edge"
        );
    }
}
