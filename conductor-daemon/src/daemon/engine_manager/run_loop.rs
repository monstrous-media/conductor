// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `EngineManager::run` main event loop, extracted from `engine_manager::mod`.

use super::*;

/// Run-loop select-arm latency guard.
///
/// The daemon run-loop is a single biased `tokio::select!`; any arm body that
/// `.await`s for more than a few ms parks the WHOLE loop and backs up MIDI in
/// `device_event_rx` (the stuck-note symptom — events flush in a burst once the
/// arm returns). This guard times each arm and `warn!`s when one exceeds the
/// threshold, naming the offending arm (and, for IPC, the `IpcCommand` variant)
/// so the blocking `.await` is identifiable from a trace without per-handler
/// instrumentation. It only logs on the slow path, so it is safe to leave on.
const SLOW_ARM_THRESHOLD: std::time::Duration = std::time::Duration::from_millis(150);

struct ArmTimer {
    label: std::borrow::Cow<'static, str>,
    start: std::time::Instant,
}

impl ArmTimer {
    fn new(label: impl Into<std::borrow::Cow<'static, str>>) -> Self {
        Self {
            label: label.into(),
            start: std::time::Instant::now(),
        }
    }
}

impl Drop for ArmTimer {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed();
        if elapsed >= SLOW_ARM_THRESHOLD {
            warn!(
                arm = %self.label,
                elapsed_ms = elapsed.as_millis() as u64,
                "run-loop select arm exceeded {}ms — parks MIDI processing while it runs",
                SLOW_ARM_THRESHOLD.as_millis(),
            );
        }
    }
}

/// Short, stable label for a `DaemonCommand` (no `Debug` derive needed on the
/// command, which carries oneshot senders and configs). For IPC requests the
/// inner `IpcCommand` is included so a slow handler is identifiable by request
/// type.
fn command_label(cmd: &DaemonCommand) -> std::borrow::Cow<'static, str> {
    match cmd {
        DaemonCommand::IpcRequest { request, .. } => {
            std::borrow::Cow::Owned(format!("Ipc:{:?}", request.command))
        }
        DaemonCommand::ConfigFileChanged(_) => "ConfigFileChanged".into(),
        DaemonCommand::SignalReload => "SignalReload".into(),
        DaemonCommand::TimerTick => "TimerTick".into(),
        DaemonCommand::HotPlugCheck => "HotPlugCheck".into(),
        DaemonCommand::HotPlugApply { .. } => "HotPlugApply".into(),
        DaemonCommand::SetDeviceEnabled { .. } => "SetDeviceEnabled".into(),
        DaemonCommand::ProbeDeviceIdentity { .. } => "ProbeDeviceIdentity".into(),
        DaemonCommand::ProfileSwitch { .. } => "ProfileSwitch".into(),
        DaemonCommand::RefreshAppMappings => "RefreshAppMappings".into(),
        DaemonCommand::ResolveContextMode { .. } => "ResolveContextMode".into(),
        _ => "other".into(),
    }
}

impl EngineManager {
    /// Run the engine manager loop
    pub async fn run(&mut self) -> Result<()> {
        info!("Engine manager starting");

        // Transition to Starting state
        self.transition_state(LifecycleState::Starting).await?;

        // Initialize input device connection
        // Transition to appropriate state based on connection result
        let connection_result = self.connect_input_devices().await;

        if let Err(e) = connection_result {
            warn!("Failed to connect to input device during startup: {}", e);
            self.log_error("InputConnectionFailed", e.to_string()).await;
            // Enter Degraded state - device may be connected later via IPC
            self.transition_state(LifecycleState::Degraded).await?;
            info!("Engine manager running in degraded mode (no input devices)");
        } else {
            self.transition_state(LifecycleState::Running).await?;
            info!("Engine manager running");
        }

        // ADR-034 §D8: if the audit outbox failed to open at
        // startup, surface it as the `AuditDegraded` lifecycle state. The
        // fail-closed mutation refusal is already enforced in
        // `LiveConfig::commit_locked`; this makes the condition visible to
        // operators (status / menu bar) rather than only in the boot log. Only
        // promoted from the clean Running branch — a device-`Degraded` boot keeps
        // its device-level state (the two are orthogonal, and the mutation gate
        // applies regardless of the label).
        if matches!(*self.state.read().await, LifecycleState::Running)
            && self.live_config.is_audit_unavailable()
        {
            warn!(
                "ADR-034 §D8: audit outbox unavailable at startup — entering \
                 AuditDegraded; config mutations are refused until it is restored"
            );
            self.transition_state(LifecycleState::AuditDegraded).await?;
        }

        // ADR-025 Phase 3.F runtime check: schedule the deferred
        // observed-vs-expected PC-tuple warning. The static inventory log
        // already fired in `new()` with context "startup"; this pairs the
        // same context label so the two log lines can be correlated.
        self.schedule_pc_observation_check("startup");

        // Preload profile cache from profiles directory at startup
        if let Some(config_dir) = dirs::config_dir() {
            let profiles_dir = config_dir.join("conductor").join("profiles");
            if profiles_dir.is_dir() {
                self.profile_cache.preload_directory(&profiles_dir);
            }
        }

        // Start app detector for automatic profile switching
        self.start_app_detector().await;

        // Main event loop: process input events and commands concurrently
        // ADR-015: biased select ensures completions are processed before new input events (D4, D14)
        loop {
            tokio::select! {
                biased;

                // ADR-015: Action completions — highest priority (D4)
                Some(completion) = self.action_dispatcher.completion_rx.recv() => {
                    let _arm = ArmTimer::new("completion");
                    self.handle_action_completion(completion).await;
                }

                // Unified input dispatch — every device (legacy single-device or
                // multi-device `[[bindings]]`) emits DeviceEvent<ProtocolEvent> here.
                // (Legacy `process_input_event` path removed.)
                //
                // ADR-039: unwrap the protocol tag back to `InputEvent`
                // before the (still `InputEvent`-shaped) `process_device_event`
                // stage. Only `ProtocolEvent::Input` is produced today; OSC/DMX
                // sources and a protocol-tag-aware route stage land later,
                // so a non-Input event here is unreachable and logged, not handled.
                Some(device_event) = self.device_event_rx.recv() => {
                    let _arm = ArmTimer::new("device_event");
                    match pump_event_into_input(device_event) {
                        Ok(input_device_event) => {
                            if let Err(e) = self.process_device_event(input_device_event).await {
                                error!("Failed to process device event: {}", e);
                                self.log_error("DeviceEventProcessingFailed", e.to_string()).await;
                            }
                        }
                        Err(other) => {
                            // ADR-039-A: OSC inbound is consumed by
                            // the ROUTE engine only — never the mapping engine (so
                            // there is no OSC→Action path; ADR-042 D17 holds by
                            // construction). It therefore cannot go through
                            // `process_device_event` (which unwraps to InputEvent);
                            // dispatch it here. Art-Net (`Dmx`) still has no consumer
                            // (ADR-039-C) — drop + log.
                            let (device_id, proto) = other.into_parts();
                            // Match on a reference so `proto` is never moved
                            // (the previous `matches!`
                            // borrowed correctly, but matching `&proto` makes that
                            // unambiguous).
                            match &proto {
                                ProtocolEvent::Osc(_) => {
                                    self.process_osc_event(&device_id, &proto).await;
                                }
                                other_proto => {
                                    error!(
                                        protocol = other_proto.protocol_label(),
                                        device = %device_id,
                                        "Dropping non-Input/OSC ProtocolEvent on the pump — \
                                         no consumer yet (ADR-039-C)"
                                    );
                                }
                            }
                        }
                    }
                }

                // Commands from IPC, config watcher, or reconnection thread
                Some(command) = self.command_rx.recv() => {
                    let _arm = ArmTimer::new(command_label(&command));
                    match command {
                        DaemonCommand::IpcRequest {
                            request,
                            caller_ctx,
                            response_tx,
                        } => {
                            let response = self.handle_ipc_request(request, caller_ctx).await;
                            let _ = response_tx.send(response);
                        }

                        DaemonCommand::ConfigFileChanged(path) => {
                            // Suppress reload when we just wrote the config ourselves.
                            // Use a window larger than the ConfigWatcher debounce (500ms) to ensure
                            // the debounced event is caught even if it arrives slightly late.
                            let suppress = {
                                let mut guard = self.config_write_suppress.lock().await;
                                // Non-consuming within the window — a single
                                // daemon write touches live.toml AND the profile, so
                                // both debounced events must be suppressed, not just
                                // the first.
                                is_self_write_suppressed(&mut guard, Duration::from_millis(1000))
                            };
                            if suppress {
                                debug!("Suppressing config reload triggered by our own write: {:?}", path);
                                // Still invalidate cache — the file contents changed even
                                // though we don't need to reload (we already have the new config).
                                self.profile_cache.invalidate(&path);
                            } else {
                                // Not our own write. Both branches invalidate the cache
                                // (the on-disk bytes changed either way); they differ in
                                // what happens NEXT — the own-write branch above stops
                                // there, while here the §D9 policy decides reload (legacy
                                // `file`) vs. drift-only-notify (managed) vs. nothing
                                // (`ignore`). Invalidate first so any later cache read
                                // can't serve the stale pre-edit config.
                                self.profile_cache.invalidate(&path);
                                self.handle_external_config_change(path).await;
                            }
                        }

                        DaemonCommand::SignalReload => {
                            // ADR-034 §D9: explicit operator reload (SIGHUP) —
                            // always reloads from the configured config_path,
                            // bypassing the passive-watcher demotion.
                            info!("Explicit config reload requested (SIGHUP)");
                            self.profile_cache.invalidate(&self.config_path);
                            if let Err(e) = self.reload_config().await {
                                error!("Config reload failed: {}", e);
                                self.log_error("ConfigReloadFailed", e.to_string()).await;
                            }
                        }

                        DaemonCommand::DeviceDisconnected => {
                            warn!("Device disconnected");
                            self.transition_state(LifecycleState::Degraded).await?;
                            self.update_device_status(false, None, None).await;
                            self.log_error("DeviceDisconnected", "Input device unplugged").await;

                            // Disconnect device manager
                            self.disconnect_input_devices().await;
                        }

                        DaemonCommand::DeviceReconnected => {
                            info!("Device reconnected - attempting to establish connection");
                            // State machine: Degraded → Reconnecting → Running
                            // First transition to Reconnecting state
                            let current_state = *self.state.read().await;
                            if current_state == LifecycleState::Degraded {
                                self.transition_state(LifecycleState::Reconnecting).await?;
                            }

                            if let Err(e) = self.connect_input_devices().await {
                                error!("Failed to reconnect to input device: {}", e);
                                self.log_error("InputReconnectionFailed", e.to_string()).await;
                                // Transition back to Degraded on failure
                                self.transition_state(LifecycleState::Degraded).await?;
                            } else {
                                // Successfully reconnected - transition to Running
                                self.transition_state(LifecycleState::Running).await?;
                            }
                        }

                        DaemonCommand::ReconnectGamepad => {
                            info!("Gamepad reconnection requested (v3.0 - not yet implemented)");
                            // Gamepad reconnection is deferred - gilrs handles hot-plug automatically
                            // See: https://gitlab.com/gilrs-project/gilrs/-/issues/95
                        }

                        DaemonCommand::DeviceReconnectionFailed => {
                            error!("Device reconnection failed after max attempts");
                            self.log_error(
                                "DeviceReconnectionFailed",
                                "Failed to reconnect after multiple attempts",
                            )
                            .await;
                        }

                        DaemonCommand::FatalError(msg) => {
                            error!("Fatal error: {}", msg);
                            self.log_error("FatalError", msg).await;
                            // ADR-025 Phase 3.F: abort *before* the
                            // awaited state transition and teardown so the
                            // 60s sleeper can't wake during shutdown.
                            self.abort_pending_pc_observation_check();
                            self.transition_state(LifecycleState::Stopping).await?;
                            break;
                        }

                        DaemonCommand::Shutdown => {
                            info!("Shutdown requested");
                            // ADR-025 Phase 3.F: abort *before* the
                            // awaited state transition and teardown so the
                            // 60s sleeper can't wake during shutdown.
                            self.abort_pending_pc_observation_check();
                            self.transition_state(LifecycleState::Stopping).await?;
                            break;
                        }

                        DaemonCommand::MenuBarAction(_) => {
                            // TODO: Handle menu bar actions
                            debug!("Menu bar action received (not yet implemented)");
                        }

                        // ADR-009 Phase 2
                        DaemonCommand::TimerTick => {
                            if let Err(e) = self.process_timer_tick().await {
                                trace!("Timer tick error: {}", e);
                            }
                        }

                        DaemonCommand::SetDeviceEnabled { device_id, enabled } => {
                            let dev_id = DeviceId::from_alias(&device_id);
                            let mut mgr_guard = self.input_manager.lock().await;
                            if let Some(ref mut mgr) = *mgr_guard {
                                mgr.set_device_enabled(&dev_id, enabled);
                            }
                            drop(mgr_guard);

                            // Update cached device_status so Status IPC reflects the change
                            {
                                let mut status = self.device_status.write().await;
                                for dev in status.devices.iter_mut() {
                                    if dev.device_id == device_id {
                                        dev.enabled = enabled;
                                    }
                                }
                            }

                            info!(device_id = %device_id, enabled = enabled, "Device enabled state changed");
                        }

                        // ADR-009 Phase 4: Hot-plug port rescan.
                        // The CoreMIDI port enumeration is slow (~500ms
                        // with many ports) and must NOT run on the run-loop —
                        // awaiting it inline parked this `select!` and stalled
                        // MIDI forwarding every 5s (stuck notes). Spawn the
                        // enumeration off-loop and re-deliver the result as
                        // `HotPlugApply`, which does only the cheap diff here.
                        // The in-flight guard drops a tick if a scan is already
                        // running so bursty senders can't pile up concurrent
                        // scans.
                        //
                        // The gamepad probe (`list_gamepads`, a FIXED
                        // ~500ms gilrs window when no controller is connected) is
                        // ALSO slow and was previously awaited inline in
                        // `process_hot_plug_apply` — re-parking the loop ~535ms
                        // every 5s whenever a gamepad endpoint was configured but
                        // no controller present. Run it in the same off-loop task
                        // (gated by a cheap `needs_gamepad_rescan` read on the
                        // loop) and pass only the boolean result back.
                        DaemonCommand::HotPlugCheck => {
                            if !self
                                .hot_plug_in_flight
                                .swap(true, std::sync::atomic::Ordering::AcqRel)
                            {
                                let cmd_tx = self.command_tx.clone();
                                let in_flight = std::sync::Arc::clone(&self.hot_plug_in_flight);
                                // Cheap (microseconds) lock read to decide whether
                                // the off-loop task should pay the gamepad probe.
                                let need_gamepad_probe = {
                                    let guard = self.input_manager.lock().await;
                                    guard
                                        .as_ref()
                                        .map(|mgr| mgr.needs_gamepad_rescan())
                                        .unwrap_or(false)
                                };
                                // Cheap lock-free mode read so the off-loop
                                // task skips the MIDI scan in GamepadOnly (where
                                // rescan_ports discards it — pure waste + spurious
                                // MIDI warnings on no-MIDI hosts).
                                let midi_enum = crate::input_manager::midi_enumeration_enabled(
                                    self.live_config.load().config.advanced_settings.input_mode,
                                );
                                tokio::spawn(async move {
                                    let result = if midi_enum {
                                        crate::input_manager::InputManager::enumerate_input_ports_async()
                                            .await
                                    } else {
                                        Ok(Vec::new())
                                    };
                                    // Probe gamepads OFF the run-loop. Only
                                    // when a rescan is warranted (gamepad mode +
                                    // not connected); when one is connected
                                    // `list_gamepads` returns immediately anyway.
                                    let gamepad_available = if need_gamepad_probe {
                                        match tokio::task::spawn_blocking(
                                            crate::input_manager::InputManager::list_gamepads,
                                        )
                                        .await
                                        {
                                            Ok(Ok(list)) => !list.is_empty(),
                                            Ok(Err(e)) => {
                                                debug!(error = %e, "Hot-plug gamepad probe: list_gamepads failed (gilrs init/enumeration?)");
                                                false
                                            }
                                            Err(join_err) => {
                                                warn!(error = %join_err, "Hot-plug gamepad probe task failed to join (panicked/cancelled)");
                                                false
                                            }
                                        }
                                    } else {
                                        false
                                    };
                                    match result {
                                        Ok(ports) => {
                                            if let Err(e) = cmd_tx
                                                .send(DaemonCommand::HotPlugApply {
                                                    port_infos: ports,
                                                    gamepad_available,
                                                })
                                                .await
                                            {
                                                debug!("Hot-plug: failed to deliver HotPlugApply: {}", e);
                                            }
                                        }
                                        Err(e) => {
                                            debug!("Hot-plug rescan: enumeration skipped this tick: {}", e)
                                        }
                                    }
                                    // Clear the in-flight guard only AFTER the result
                                    // has been delivered: if it
                                    // were cleared before the `send().await` and the
                                    // command channel were backpressured, the next 5s
                                    // tick could spawn a concurrent enumeration/probe
                                    // while this one is still trying to deliver.
                                    in_flight.store(false, std::sync::atomic::Ordering::Release);
                                });
                            }
                        }

                        // Fast on-loop apply of a pre-enumerated rescan.
                        // Gamepad probe result is precomputed off-loop.
                        DaemonCommand::HotPlugApply {
                            port_infos,
                            gamepad_available,
                        } => {
                            if let Err(e) = self
                                .process_hot_plug_apply(port_infos, gamepad_available)
                                .await
                            {
                                debug!("Hot-plug apply error: {}", e);
                            }
                        }

                        // ADR-026 Phase 2: SysEx identity probe execution path.
                        // The MCP executor dispatches this command rather than
                        // calling the probe coordinator directly because it
                        // doesn't own the daemon's `MidiOutputManager`.
                        DaemonCommand::ProbeDeviceIdentity {
                            port_name,
                            response_tx,
                        } => {
                            let outcome = self.run_probe_device_identity(&port_name).await;
                            let _ = response_tx.send(outcome);
                        }

                        // Profile switch with cache fast-path
                        DaemonCommand::ProfileSwitch { profile_name, config_path, profile_id, result_tx } => {
                            info!("Profile switch requested: '{}' (config: {})", profile_name, config_path);

                            // Validate profile path using shared helper
                            let validated_path = match crate::daemon::types::validate_profile_path(&config_path) {
                                Ok(path) => path,
                                Err(e) => {
                                    error!("Profile path validation failed: {}", e);
                                    self.log_error("ProfileSwitchFailed", e.clone()).await;
                                    if let Some(tx) = result_tx {
                                        let _ = tx.send(Err(e));
                                    }
                                    continue;
                                }
                            };

                            // Save old path and lifecycle state for rollback on failure
                            let old_config_path = self.config_path.clone();
                            let old_lifecycle_state = *self.state.read().await;
                            self.config_path = validated_path.clone();

                            // Try cache fast-path first, fall back to full reload
                            use crate::daemon::profile_cache::CacheLookup;
                            let result = match self.profile_cache.get(&validated_path) {
                                CacheLookup::Hit(cached_config) => {
                                    self.reload_from_cached_config(*cached_config).await
                                }
                                CacheLookup::Miss => {
                                    let r = self.reload_config().await;
                                    // On successful load, populate cache for next time.
                                    // Verify config_path still matches to avoid caching wrong config
                                    // (no concurrent mutation in single-threaded loop, but defensive).
                                    if r.is_ok() && self.config_path == validated_path {
                                        let config = (*self.live_config.load().config).clone();
                                        self.profile_cache.insert(&validated_path, config);
                                    }
                                    r
                                }
                            };

                            match result {
                                Ok(metrics) => {
                                    // Identity commit via the shared
                                    // choke point, LAST after the reload succeeded.
                                    self.commit_active_profile(ActiveProfileInfo {
                                        id: profile_id,
                                        name: profile_name.clone(),
                                        config_path: self.config_path.display().to_string(),
                                    });
                                    info!("Profile '{}' activated (reload: {}ms)", profile_name, metrics.duration_ms);

                                    // ADR-040 §4.2/§4.7: the new profile may not declare
                                    // the locked mode — drop the lock if so, before the
                                    // ResolveContextMode that follows this switch re-resolves.
                                    self.reconcile_lock_after_reload();

                                    // Re-target ConfigWatcher to the new profile's config.
                                    // Also move the Overwrite/§D9 write target (`user_file_path`)
                                    // to the same path so the two never diverge after a switch.
                                    self.retarget_watched_user_file(self.config_path.clone())
                                        .await;

                                    // Emit monitor event for profile switch
                                    self.push_monitor_event(MonitorEvent {
                                        timestamp_ms: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis() as u64,
                                        event_type: "profile_switch".to_string(),
                                        detail: Some(format!("Switched to '{}' ({}ms)", profile_name, metrics.duration_ms)),
                                        processing_us: Some(metrics.duration_ms * 1000),
                                        ..MonitorEvent::default()
                                    });

                                    if let Some(tx) = result_tx {
                                        let _ = tx.send(Ok(profile_name));
                                    }
                                }
                                Err(e) => {
                                    // Rollback config_path and lifecycle state on failure
                                    error!("Profile switch failed during reload, rolling back: {}", e);
                                    self.config_path = old_config_path;
                                    // Restore lifecycle state to what it was before the switch attempt
                                    if let Err(te) = self.transition_state(old_lifecycle_state).await {
                                        warn!("Failed to restore lifecycle state after profile rollback: {}", te);
                                    }
                                    let err_msg = e.to_string();
                                    self.log_error("ProfileSwitchFailed", err_msg.clone()).await;
                                    if let Some(tx) = result_tx {
                                        let _ = tx.send(Err(err_msg));
                                    }
                                }
                            }
                        }

                        // Profile query
                        DaemonCommand::ProfileQuery { response_tx } => {
                            let profile = (**self.active_profile.load()).clone();
                            let _ = response_tx.send(profile);
                        }

                        // Refresh app detector mappings
                        DaemonCommand::RefreshAppMappings => {
                            if let Some(ref detector) = self.app_detector {
                                if let Some(config_dir) = dirs::config_dir() {
                                    let profiles_dir = config_dir.join("conductor").join("profiles");
                                    // Use spawn_blocking to avoid stalling the command loop with sync I/O
                                    let mappings = tokio::task::spawn_blocking(move || {
                                        crate::daemon::app_detector::load_mappings_from_manifest(&profiles_dir)
                                    }).await.unwrap_or_default();
                                    detector.update_mappings(mappings).await;
                                    info!("App detector mappings refreshed");
                                } else {
                                    warn!("Cannot refresh app detector mappings: no config directory available");
                                }
                            }
                        }

                        // Mode change via MCP tool
                        DaemonCommand::ModeChange { mode } => {
                            debug!("Mode change requested via command: {}", mode);
                            if let Err(e) = self.persist_mode_change(mode).await {
                                warn!("Failed to persist mode change from command: {}", e);
                            }
                        }

                        // ADR-040 (§4.5/§4.7): app/window → mode auto-switch.
                        // Arrives AFTER any same-change ProfileSwitch (FIFO), so the
                        // resolve runs against the newly-loaded profile's config.
                        DaemonCommand::ResolveContextMode { app, window_title } => {
                            self.resolve_context_mode(app, window_title).await;
                        }

                        // ADR-040 D4 §4.2 — mode-lock commands from the
                        // MCP server / LLM executor (origin `Mcp`).
                        DaemonCommand::SetModeLocked {
                            mode,
                            lock,
                            response_tx,
                        } => {
                            let result = self
                                .set_mode_manual(mode.clone(), lock, super::LockSource::Mcp)
                                .await
                                .map_err(|e| match e {
                                    // Include the requested mode so the MCP error
                                    // matches the CLI/IPC messaging.
                                    super::SetModeError::UnknownMode => {
                                        format!("unknown mode '{mode}'")
                                    }
                                    super::SetModeError::Internal(err) => err.to_string(),
                                });
                            if response_tx.send(result).is_err() {
                                debug!("SetModeLocked reply dropped — MCP caller already gone");
                            }
                        }
                        DaemonCommand::ReleaseModeLock { response_tx } => {
                            if response_tx.send(self.unlock_mode().await).is_err() {
                                debug!("ReleaseModeLock reply dropped — MCP caller already gone");
                            }
                        }
                        DaemonCommand::QueryModeStatus { response_tx } => {
                            if response_tx.send(self.mode_status_json()).is_err() {
                                debug!("QueryModeStatus reply dropped — MCP caller already gone");
                            }
                        }
                    }
                }

                // Both channels closed - shutdown
                else => {
                    info!("All channels closed, shutting down");
                    break;
                }
            }
        }

        // ADR-039 §4.4 — graceful shutdown ordering: quiesce → drain →
        // flush/close. The order matters: draining BEFORE quiescing would hang
        // on a still-producing source; flushing the executor BEFORE the drain
        // would drop the actions the drained events dispatch.
        //
        // (1) Ingress quiesce — stop every input source enqueueing FIRST, so no
        //     new events are produced during the drain (R3). This is the
        //     existing CFRunLoopStop / gilrs-stop clean-shutdown path.
        self.disconnect_input_devices().await;

        // (2) Bounded-drain the pump — process input already queued (e.g. a
        //     trailing note-off, so a held modifier isn't stranded) rather than
        //     losing it on restart. Deadline-bounded; never hangs.
        self.drain_pump_on_shutdown().await;

        // (3) Flush + close — shut down the executor thread (ADR-015 D11),
        //     processing completions for any cancelled/in-flight actions
        //     (including those just dispatched by the drain) so terminal events
        //     (mapping_cancelled) and side effects (stats, recursion guard) fire.
        let cancelled = self.action_dispatcher.shutdown();
        if !cancelled.is_empty() {
            info!(
                "Shutdown cancelled {} queued/in-flight actions — processing completions",
                cancelled.len()
            );
            for completion in cancelled {
                self.handle_action_completion(completion).await;
            }
        }

        // ADR-025 Phase 3.F: defensive belt-and-braces — the
        // Shutdown / FatalError arms already abort before break, but if
        // a future control-flow path exits the loop without going through
        // those arms, this catches it.
        self.abort_pending_pc_observation_check();

        // Final state transition
        self.transition_state(LifecycleState::Stopped).await?;

        info!("Engine manager stopped");
        Ok(())
    }

    /// ADR-034 §D9: react to an external write to the watched config file
    /// according to the LIVE config's source mode and user-file policy.
    ///
    /// This is the demotion seam: pre-ADR-034 the watcher unconditionally
    /// auto-reloaded; now only legacy `source = "file"` does. In the managed
    /// default the daemon's in-memory tree stays authoritative — an external
    /// edit surfaces as drift (`config_drift_detected`) and is applied only
    /// via an explicit IPC reload. `user_file_policy = "ignore"` is authoritative
    /// (matches the startup gate that disables the watcher entirely): no action.
    ///
    /// Own-write suppression and cache invalidation are handled by the caller
    /// (the `ConfigFileChanged` arm); this method only decides reload-vs-surface.
    pub(crate) async fn handle_external_config_change(&mut self, path: PathBuf) {
        let (source, policy) = {
            let snap = self.live_config.load();
            (
                snap.config.config_meta.source,
                snap.config.config_meta.user_file_policy,
            )
        };

        match external_change_action(source, policy) {
            ExternalChangeAction::LegacyReload => {
                warn!(
                    ?path,
                    "Legacy config.source = \"file\": auto-reloading on external edit. \
                     Deprecated (ADR-034 §D9), removed in v6.0 — migrate to \
                     source = \"managed\" + explicit `conductorctl config reload`."
                );
                if let Err(e) = self.reload_config().await {
                    error!("Config reload failed: {}", e);
                    self.log_error("ConfigReloadFailed", e.to_string()).await;
                }
            }
            ExternalChangeAction::NotifyDrift => {
                info!(
                    ?path,
                    "External config edit detected; live config unchanged \
                     (notify-only, ADR-034 §D9)"
                );
                let timestamp_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                self.push_monitor_event(MonitorEvent {
                    timestamp_ms,
                    event_type: "config_drift_detected".to_string(),
                    detail: Some(format!(
                        "External edit to {}; live config unchanged. \
                         Apply via `conductorctl config reload`.",
                        path.display()
                    )),
                    ..MonitorEvent::default()
                });
            }
            ExternalChangeAction::Ignore => {
                debug!(
                    ?path,
                    "External config edit ignored (user_file_policy = ignore)"
                );
            }
        }
    }

    /// ADR-039 §4.4: bounded-drain the unified pump on shutdown — step 2
    /// of the shutdown ordering, run AFTER ingress quiesce
    /// (`disconnect_input_devices`) and BEFORE the executor flush.
    ///
    /// Processes input already queued in `device_event_rx` (e.g. a trailing
    /// note-off) so it isn't lost on restart, then returns. It can never hang:
    /// [`drain_queued_events`] is deadline-bounded, and quiesce-first guarantees
    /// no source keeps it busy past the channel going quiet.
    ///
    /// The quiesce-before-drain contract is enforced structurally rather than by
    /// a runtime assertion: this is a private method whose **sole caller** is
    /// `run`'s shutdown sequence, which always calls `disconnect_input_devices`
    /// immediately before it. Even if that ordering were ever violated, the
    /// `TOTAL_BUDGET` ceiling still bounds the call — correctness degrades to
    /// "may drop a few late events", never to a hang.
    async fn drain_pump_on_shutdown(&mut self) {
        /// Inter-event silence that signals the (quiesced) pump has gone quiet.
        const QUIET_GAP: Duration = Duration::from_millis(50);
        /// Hard ceiling on the whole drain, the backstop against any straggler.
        const TOTAL_BUDGET: Duration = Duration::from_millis(500);

        let queued = drain_queued_events(&mut self.device_event_rx, QUIET_GAP, TOTAL_BUDGET).await;
        if queued.is_empty() {
            return;
        }
        let count = queued.len();
        for device_event in queued {
            match pump_event_into_input(device_event) {
                Ok(input_device_event) => {
                    if let Err(e) = self.process_device_event(input_device_event).await {
                        error!("Shutdown drain: failed to process device event: {}", e);
                    }
                }
                Err(other) => {
                    // Unreachable until a network producer exists;
                    // mirror the run loop's drop-and-log rather than crash.
                    error!(
                        protocol = other.event().protocol_label(),
                        "Shutdown drain dropped non-Input ProtocolEvent"
                    );
                }
            }
        }
        info!("Shutdown drain processed {} queued input event(s)", count);
    }
}

/// ADR-039 §4.4: drain events already queued in the unified pump,
/// bounded so it can never hang. Returns the drained events in arrival order.
///
/// MUST be called only AFTER ingress is quiesced (input sources disconnected) —
/// a still-producing source would otherwise keep this busy until `total_budget`
/// every time (R3). Stops at the first of: the channel goes quiet (no event
/// within `quiet_gap`), the channel closes (all senders dropped → `recv` yields
/// `None`), or `total_budget` elapses. Kept a free fn so the bounded-drain
/// guarantee is unit-testable without spinning up a daemon.
async fn drain_queued_events(
    rx: &mut mpsc::Receiver<DeviceEvent<ProtocolEvent>>,
    quiet_gap: Duration,
    total_budget: Duration,
) -> Vec<DeviceEvent<ProtocolEvent>> {
    let deadline = tokio::time::Instant::now() + total_budget;
    let mut drained = Vec::new();
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let wait = quiet_gap.min(deadline - now);
        match tokio::time::timeout(wait, rx.recv()).await {
            Ok(Some(ev)) => drained.push(ev),
            Ok(None) => break,      // all senders dropped — channel closed
            Err(_elapsed) => break, // quiet gap with no event → pump is quiesced
        }
    }
    drained
}

/// ADR-034 §D9: the action the daemon takes when the ConfigWatcher reports an
/// external write to the watched config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalChangeAction {
    /// Legacy `source = "file"`: auto-reload (with a deprecation warning).
    LegacyReload,
    /// Managed + notify: surface drift (`config_drift_detected`), no reload.
    NotifyDrift,
    /// `user_file_policy = "ignore"`: do nothing.
    Ignore,
}

/// ADR-034 §D9 decision matrix. `ignore` is authoritative — it matches the
/// startup gate (which disables the watcher entirely on `ignore`), so even a
/// live policy flip to `ignore` post-startup is honoured as "do not react".
/// Otherwise (`notify`) the source mode decides: legacy `file` auto-reloads,
/// managed surfaces drift only. Pure so the matrix is unit-testable without a
/// daemon.
fn external_change_action(source: ConfigSource, policy: UserFilePolicy) -> ExternalChangeAction {
    match policy {
        UserFilePolicy::Ignore => ExternalChangeAction::Ignore,
        UserFilePolicy::Notify => match source {
            ConfigSource::File => ExternalChangeAction::LegacyReload,
            ConfigSource::Managed => ExternalChangeAction::NotifyDrift,
        },
    }
}

/// ADR-034 §D9: decide whether a `ConfigFileChanged` is the daemon's own
/// recent write (suppress the drift/reload) or a genuine external edit.
///
/// **Non-consuming** within the window. A single daemon mutation writes BOTH
/// `live.toml` (the `live_config` commit) and the active profile file (the
/// write-through, or plan-apply), producing two debounced watcher events.
/// Consuming the marker on the first event let the second escape and surface as
/// a false "external edit" drift → a banner/reload loop. So we keep the
/// marker set until the window elapses (both events suppressed), and clear it
/// only once expired so the next genuine external edit is honoured.
fn is_self_write_suppressed(marker: &mut Option<Instant>, window: Duration) -> bool {
    match *marker {
        Some(ts) if ts.elapsed() < window => true, // recent self-write — suppress, keep window open
        Some(_) => {
            *marker = None; // expired — clear; treat this (and future) as external
            false
        }
        None => false,
    }
}

/// ADR-039: unwrap a unified-pump `DeviceEvent<ProtocolEvent>` back to the
/// `DeviceEvent<InputEvent>` the `process_device_event` stage consumes.
///
/// MIDI/HID sources tag their events as `ProtocolEvent::Input(..)` at ingress;
/// this reverses that for the (still `InputEvent`-shaped) hot path. Returns
/// `Err(original)` for non-`Input` variants — no source produces OSC/DMX events
/// until the listeners land, so that arm is unreachable today and
/// the recv loop logs+drops rather than handling it. Kept as a free fn so the
/// recv-loop logic is unit-testable without spinning the daemon.
fn pump_event_into_input(
    device_event: DeviceEvent<ProtocolEvent>,
) -> std::result::Result<DeviceEvent<InputEvent>, DeviceEvent<ProtocolEvent>> {
    let (device_id, protocol_event) = device_event.into_parts();
    match protocol_event.into_input() {
        Ok(input) => Ok(DeviceEvent::new(device_id, input)),
        Err(other) => Err(DeviceEvent::new(device_id, other)),
    }
}

#[cfg(test)]
mod external_change_action_tests {
    use super::*;

    #[test]
    fn managed_notify_surfaces_drift_without_reload() {
        // The ADR-034 production default: external user.toml edits are
        // surfaced, never silently reloaded.
        assert_eq!(
            external_change_action(ConfigSource::Managed, UserFilePolicy::Notify),
            ExternalChangeAction::NotifyDrift
        );
    }

    #[test]
    fn legacy_file_source_auto_reloads() {
        // Pre-ADR-034 behaviour retained for `source = "file"` (deprecated).
        assert_eq!(
            external_change_action(ConfigSource::File, UserFilePolicy::Notify),
            ExternalChangeAction::LegacyReload
        );
    }

    #[test]
    fn ignore_policy_is_authoritative_over_source() {
        // `ignore` matches the startup gate (watcher disabled): the daemon
        // never reacts, even if a stray event arrives or source = file.
        assert_eq!(
            external_change_action(ConfigSource::Managed, UserFilePolicy::Ignore),
            ExternalChangeAction::Ignore
        );
        assert_eq!(
            external_change_action(ConfigSource::File, UserFilePolicy::Ignore),
            ExternalChangeAction::Ignore
        );
    }

    // Self-write suppression must be non-consuming within the window so a
    // single daemon mutation's TWO file writes (live.toml + profile) are both
    // suppressed instead of the second leaking out as false "external edit" drift.
    #[test]
    fn self_write_suppression_is_non_consuming_within_window() {
        let window = Duration::from_millis(1000);

        // A recent self-write is suppressed AND the marker persists, so a second
        // event from the same multi-file write is also suppressed.
        let mut recent = Some(Instant::now());
        assert!(is_self_write_suppressed(&mut recent, window));
        assert!(recent.is_some(), "marker must persist within the window");
        assert!(
            is_self_write_suppressed(&mut recent, window),
            "the second event of a two-file write must also be suppressed"
        );

        // An expired marker is not suppressed and is cleared.
        let mut expired = Some(Instant::now() - Duration::from_secs(5));
        assert!(!is_self_write_suppressed(&mut expired, window));
        assert!(expired.is_none(), "expired marker is cleared");

        // No marker → genuine external edit, never suppressed.
        let mut none: Option<Instant> = None;
        assert!(!is_self_write_suppressed(&mut none, window));
    }
}

#[cfg(test)]
mod pump_unwrap_tests {
    use super::*;
    use conductor_core::events::{DMX_UNIVERSE_SIZE, DmxFrame, DmxInbound, OscInbound};
    use std::time::Instant;

    #[test]
    fn input_event_round_trips_through_the_pump_wrapper() {
        let time = Instant::now();
        let input = InputEvent::PadPressed {
            pad: 36,
            velocity: 100,
            channel: Some(9),
            time,
        };
        let device_id = DeviceId::from_alias("pads");
        // Ingress wraps as the pump producers do.
        let wrapped = DeviceEvent::new(device_id.clone(), ProtocolEvent::Input(input.clone()));

        let unwrapped = pump_event_into_input(wrapped).expect("Input must unwrap");
        assert_eq!(unwrapped.device_id(), &device_id);
        assert_eq!(unwrapped.event(), &input);
    }

    #[test]
    fn non_input_variants_are_returned_for_drop() {
        let device_id = DeviceId::from_alias("eos");
        let osc = DeviceEvent::new(
            device_id.clone(),
            ProtocolEvent::Osc(OscInbound {
                address: "/1/fader1".to_string(),
                args: vec![],
                time: Instant::now(),
            }),
        );
        let err = pump_event_into_input(osc).expect_err("Osc must not unwrap to Input");
        assert_eq!(err.device_id(), &device_id);
        assert_eq!(err.event().protocol_label(), "Osc");

        let dmx = DeviceEvent::new(
            device_id.clone(),
            ProtocolEvent::Dmx(DmxFrame::new(DmxInbound {
                universe: 0,
                channels: [0u8; DMX_UNIVERSE_SIZE],
                time: Instant::now(),
            })),
        );
        let err = pump_event_into_input(dmx).expect_err("Dmx must not unwrap to Input");
        assert_eq!(err.event().protocol_label(), "Dmx");
    }
}

#[cfg(test)]
mod shutdown_drain_tests {
    use super::*;
    use std::time::Instant;

    fn sample(pad: u8) -> DeviceEvent<ProtocolEvent> {
        DeviceEvent::new(
            DeviceId::raw("d"),
            ProtocolEvent::Input(InputEvent::PadPressed {
                pad,
                velocity: 100,
                channel: Some(0),
                time: Instant::now(),
            }),
        )
    }

    #[tokio::test]
    async fn drain_collects_queued_events_then_stops() {
        // Already-queued events are returned in arrival order, then the drain
        // stops once the (sender-alive but idle) channel goes quiet.
        let (tx, mut rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(8);
        for pad in [36u8, 37, 38] {
            tx.try_send(sample(pad)).unwrap();
        }
        let drained = drain_queued_events(
            &mut rx,
            Duration::from_millis(20),
            Duration::from_millis(500),
        )
        .await;
        assert_eq!(drained.len(), 3, "all queued events drained");
        // tx still alive — proves termination came from the quiet-gap, not EOF.
        drop(tx);
    }

    #[tokio::test]
    async fn drain_of_idle_channel_terminates_without_hanging() {
        // Sender alive but silent: recv never yields None, so only the quiet-gap
        // timeout can end the drain. It must return promptly, not hang.
        let (_tx, mut rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(8);
        let start = Instant::now();
        let drained = drain_queued_events(
            &mut rx,
            Duration::from_millis(20),
            Duration::from_millis(200),
        )
        .await;
        assert!(drained.is_empty());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "idle drain must not hang (took {:?})",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn drain_does_not_hang_on_a_live_source() {
        // A source that keeps producing never lets the quiet-gap elapse, so the
        // total-budget ceiling is what must bound the drain (R3 — the whole
        // point of quiescing ingress first). Here we DON'T quiesce, to prove the
        // ceiling holds even against a live producer.
        let (tx, mut rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(64);
        let sender = tokio::spawn(async move {
            while tx.send(sample(36)).await.is_ok() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        let start = Instant::now();
        let drained = drain_queued_events(
            &mut rx,
            Duration::from_millis(20),
            Duration::from_millis(150),
        )
        .await;
        let elapsed = start.elapsed();
        sender.abort();
        assert!(
            elapsed < Duration::from_millis(600),
            "total budget must bound the drain even against a live source (took {elapsed:?})"
        );
        assert!(
            !drained.is_empty(),
            "should have drained events from the live source before the deadline"
        );
    }
}
