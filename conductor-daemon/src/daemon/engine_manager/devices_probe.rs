// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `EngineManager` SysEx probe methods (`run_probe_device_identity`,
//! `dispatch_probe_on_connect_for_new_ports`), extracted from
//! `engine_manager::devices` (refactor #2073).

use super::*;

impl EngineManager {
    /// Run a SysEx Identity probe (ADR-026 Phase 2).
    ///
    /// `port_name` is the **MIDI input port name** as reported by midir
    /// — the same string the daemon's ingress callback passes to
    /// `ProbeCoordinator::observe_reply` (Phase 1.B). The coordinator's
    /// pending-slot key is therefore consistent across send + observe
    /// (the original Phase 2 implementation keyed on alias instead and
    /// every probe timed out — caught in PR review).
    ///
    /// Internally we resolve that input port → device alias (via the
    /// active `InputManager`'s bindings) and use the alias to look up
    /// the paired output in `device_output_map`. The indirection is
    /// necessary because the OUTPUT mapping (ADR-021) is keyed by alias.
    ///
    /// The actual `coord.probe()` call is sync, so we wrap it in
    /// `spawn_blocking` to avoid holding the tokio runtime for the
    /// 1-second timeout window.
    pub(crate) async fn run_probe_device_identity(
        &self,
        port_name: &str,
    ) -> std::result::Result<
        conductor_core::device_intelligence::probe::ProbeResult,
        conductor_core::device_intelligence::probe::ProbeStartError,
    > {
        use conductor_core::device_intelligence::probe::{ProbeStartError, SendError};

        // Steps 1+2: resolve input port → alias → paired output port.
        // Extracted to a pure helper for unit testability — the live
        // bindings + ArcSwap snapshot are gathered here, then handed
        // to `resolve_probe_output_port` for the lookup logic. The
        // helper distinguishes "no output mapping" (config concern)
        // from "input disconnected" (runtime concern); we map each to
        // a different `ProbeStartError` so callers don't have to guess.
        let bindings = {
            let mgr_guard = self.input_manager.lock().await;
            mgr_guard
                .as_ref()
                .map(|mgr| mgr.get_device_bindings())
                .unwrap_or_default()
        };
        let device_output_map = self.device_output_map.load();
        let output_port = match resolve_probe_output_port(port_name, &bindings, &device_output_map)
        {
            Ok(port) => port,
            Err(ProbeResolveError::NoPairedOutput) => {
                return Err(ProbeStartError::NoPairedOutput {
                    port_name: port_name.to_string(),
                });
            }
            Err(ProbeResolveError::InputDisconnected) => {
                return Err(ProbeStartError::SendFailed {
                    port_name: port_name.to_string(),
                    reason: "input port is currently disconnected — re-probe after reconnect"
                        .to_string(),
                });
            }
        };

        // Step 3: spawn_blocking the sync probe with a send_fn that
        // writes through the action executor's MidiOutputManager.
        let coord = Arc::clone(&self.probe_coordinator);
        let action_executor = Arc::clone(&self.action_executor);
        let port_name_owned = port_name.to_string();

        match tokio::task::spawn_blocking(move || {
            coord.probe(&port_name_owned, |bytes| {
                let mut exec = action_executor.blocking_lock();
                exec.send_raw_bytes(&output_port, bytes)
                    .map_err(SendError::WriteFailed)
            })
        })
        .await
        {
            Ok(probe_result) => probe_result,
            Err(join_err) => {
                // Distinguish panic from cancellation. Cancellations
                // happen during graceful daemon shutdown; we don't
                // want to misclassify them as task panics in logs /
                // outcomes. Both surface to the caller as a
                // `ProbeStartError::SendFailed` because the wait
                // never completed normally.
                let (kind, reason) = if join_err.is_panic() {
                    (
                        "panicked",
                        format!("internal: spawn_blocking task panicked: {}", join_err),
                    )
                } else if join_err.is_cancelled() {
                    (
                        "cancelled",
                        "internal: spawn_blocking task cancelled (likely daemon shutdown)"
                            .to_string(),
                    )
                } else {
                    (
                        "join error",
                        format!("internal: spawn_blocking join error: {}", join_err),
                    )
                };
                tracing::error!("probe spawn_blocking task {}: {}", kind, join_err);
                Err(ProbeStartError::SendFailed {
                    port_name: port_name.to_string(),
                    reason,
                })
            }
        }
    }

    /// Probe-on-connect dispatcher (ADR-026 Phase 3.C.2). Computes
    /// the set of newly-bound configured input ports vs the
    /// previously-known set, gates on `should_probe_on_connect`,
    /// and spawns a fire-and-forget tokio task per eligible port.
    ///
    /// Each task:
    /// 1. Sends `DaemonCommand::ProbeDeviceIdentity` (the existing
    ///    Phase 2 path — `EngineManager`'s command loop handles it)
    ///    and awaits the outcome via the oneshot channel.
    /// 2. Calls `classify_probe_outcome` to map the outcome to a
    ///    discrete `ProbeOnConnectAction`.
    /// 3. Applies the action: `AutoPromote` sends a
    ///    `DaemonCommand::HotPlugCheck` so `PortInfo` re-resolves
    ///    against the freshly-cached identity (any
    ///    `SysExIdentity` matchers fire); `SurfaceConfirmation` /
    ///    `LogNoReply` / `LogStartError` emit `tracing` records.
    ///    Phase 3.C.3 will replace the `SurfaceConfirmation`
    ///    `tracing::warn!` placeholder with a real
    ///    `IdentityNeedsConfirmation` Daemon→GUI event.
    ///
    /// Idempotent: the `last_known_configured_ports` guard ensures
    /// steady-state rescans don't re-probe ports already seen.
    /// Safe to call from multiple sites (initial connect + every
    /// hot-plug check).
    pub(crate) async fn dispatch_probe_on_connect_for_new_ports(&self, config: &Config) {
        // Snapshot bindings under the input_manager lock, then
        // release before doing any further work — the dispatch
        // path doesn't need to hold the InputManager lock.
        let bindings = {
            let mgr_guard = self.input_manager.lock().await;
            mgr_guard
                .as_ref()
                .map(|mgr| mgr.get_device_bindings())
                .unwrap_or_default()
        };

        // Update last_known atomically with the eligibility check:
        // delegate to `process_dispatch_tick` so the bug-fix pin
        // (don't pollute last_known when probing is off) lives next
        // to the gating logic and is testable in isolation. Holding
        // the lock for the full read+update keeps two concurrent
        // dispatches from racing each other into spawning duplicate
        // probes.
        let eligible: HashSet<String> = {
            let mut last_known = self.last_known_configured_ports.lock().await;
            crate::daemon::probe_on_connect::process_dispatch_tick(
                config,
                &bindings,
                &mut last_known,
            )
        };

        if eligible.is_empty() {
            return;
        }

        for port_name in eligible {
            let cmd_tx = self.command_tx.clone();
            // Phase 3.C.3 — broadcast IdentityNeedsConfirmation events
            // to subscribed GUI clients via the existing MonitorEvent
            // channel. Replaces the tracing::warn! placeholder.
            let event_tx = self.event_broadcast_tx.clone();
            tokio::spawn(async move {
                use crate::daemon::probe_on_connect::{
                    ProbeOnConnectAction, build_identity_needs_confirmation_event,
                    classify_probe_outcome,
                };

                // Step 1: dispatch the probe through the existing
                // command-channel path. Reuses Phase 2's
                // `DaemonCommand::ProbeDeviceIdentity` handler.
                let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                if let Err(e) = cmd_tx
                    .send(crate::daemon::types::DaemonCommand::ProbeDeviceIdentity {
                        port_name: port_name.clone(),
                        response_tx: resp_tx,
                    })
                    .await
                {
                    tracing::debug!(
                        port = %port_name,
                        error = %e,
                        "probe-on-connect: command channel closed"
                    );
                    return;
                }

                // Step 2: bound the wait. 30 s outer cap matches
                // the MCP probe path — covers ~25-device hot-plug
                // bursts (each waits ~1 s in the global queue) plus
                // scheduling slack.
                let outcome =
                    match tokio::time::timeout(std::time::Duration::from_secs(30), resp_rx).await {
                        Ok(Ok(o)) => o,
                        Ok(Err(_)) => {
                            tracing::debug!(
                                port = %port_name,
                                "probe-on-connect: response channel closed"
                            );
                            return;
                        }
                        Err(_) => {
                            tracing::warn!(
                                port = %port_name,
                                "probe-on-connect: timed out waiting for daemon response"
                            );
                            return;
                        }
                    };

                // Step 3: classify + apply.
                let action = classify_probe_outcome(outcome, &port_name);
                match action {
                    ProbeOnConnectAction::AutoPromote {
                        port_name,
                        identity,
                    } => {
                        tracing::info!(
                            port = %port_name,
                            manufacturer = ?identity.manufacturer_id,
                            family = identity.family,
                            model = identity.model,
                            "probe-on-connect: auto-promoting binding (DirectPairedPort)"
                        );
                        // Trigger a re-resolve so PortInfo carries
                        // the freshly-cached identity and any
                        // `SysExIdentity` matchers that should now
                        // match actually fire.
                        if let Err(e) = cmd_tx
                            .send(crate::daemon::types::DaemonCommand::HotPlugCheck)
                            .await
                        {
                            tracing::debug!(
                                error = %e,
                                "probe-on-connect: failed to dispatch HotPlugCheck"
                            );
                        }
                    }
                    ProbeOnConnectAction::SurfaceConfirmation {
                        port_name,
                        candidates,
                    } => {
                        // Phase 3.C.3 — broadcast the confirmation
                        // request to subscribed GUI clients. The log
                        // line stays for ops visibility; the wire
                        // event is the new user-actionable signal.
                        tracing::info!(
                            port = %port_name,
                            candidates_count = candidates.len(),
                            "probe-on-connect: identity needs confirmation \
                             (SharedRoute or MultipleIdentified) — emitting \
                             identity_needs_confirmation event"
                        );
                        let timestamp_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let event = build_identity_needs_confirmation_event(
                            &port_name,
                            &candidates,
                            timestamp_ms,
                        );
                        // Direct broadcast bypasses `push_monitor_event`'s
                        // ring buffer / rate limiter / trigger pipeline
                        // intentionally (Copilot #970 review). Rationale:
                        //   - This is an action-required notification,
                        //     not telemetry — the GUI's
                        //     `identityConfirmations` store dedupes
                        //     pending entries by port_name and renders
                        //     them in the banner. A second, ring-buffer-
                        //     backed history of "probe asked us to
                        //     confirm at T+5s" provides no value users
                        //     can act on.
                        //   - Probe outcomes are already rare (one per
                        //     newly-bound port); the rate-limiter would
                        //     never gate them.
                        //   - We're inside a `tokio::spawn` task that
                        //     captured `event_tx` only — no `&self`,
                        //     no access to `push_monitor_event` from
                        //     here without restructuring as a
                        //     command-channel round-trip. Not worth
                        //     the extra indirection.
                        // send returns Err only when there are no
                        // active subscribers — normal when the GUI
                        // hasn't yet started monitoring; drop and
                        // continue. The probe-coordinator cache on
                        // the daemon side keeps the outcome available
                        // for a later GUI-initiated probe / status read.
                        let _ = event_tx.send(event);
                    }
                    ProbeOnConnectAction::LogNoReply {
                        port_name,
                        timeout_ms,
                    } => {
                        tracing::debug!(
                            port = %port_name,
                            timeout_ms,
                            "probe-on-connect: no SysEx Identity Reply (device may not support \
                             Universal SysEx — binding stays in pre-probe state)"
                        );
                    }
                    ProbeOnConnectAction::LogStartError { port_name, error } => {
                        tracing::debug!(
                            port = %port_name,
                            ?error,
                            "probe-on-connect: probe could not be started"
                        );
                    }
                }
            });
        }
    }
}
