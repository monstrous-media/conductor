// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `EngineManager` methods extracted from `engine_manager::mod`.

use super::*;

impl EngineManager {
    /// Process timer tick — check holds on all per-device EventProcessors (D12)
    pub(crate) async fn process_timer_tick(&mut self) -> Result<()> {
        // Lock-free mode + rules read (ADR-009 Phase 3 / D4.A.3.3.A)
        let mode = self.current_mode.load();
        let snap = self.live_config.load();
        let rules = &snap.rules;

        let current_mode_name = if mode.name.is_empty() {
            None
        } else {
            Some(mode.name.clone())
        };

        // Collect hold events from all processors
        let mut hold_events: Vec<(
            DeviceId,
            Vec<conductor_core::event_processor::ProcessedEvent>,
        )> = Vec::new();

        for mut entry in self.event_processors.iter_mut() {
            let device_id = entry.key().clone();
            let processor = entry.value_mut();
            let events = processor.check_holds();
            if !events.is_empty() {
                hold_events.push((device_id, events));
            }
        }

        // Suppress dispatch when MIDI Learn is active. Symmetric
        // with the process_device_event guard. The timer-tick path produces
        // ProcessedEvent::HoldDetected events from `check_holds()`
        // (LongPress comes from a different path inside the raw event
        // processor) — without this guard, a hold-triggered mapping
        // during Learn would still fire
        // its action.
        // `check_holds()` itself has already run above; we only skip
        // the dispatch loop body.
        let suppress_during_learn = self.midi_learn_active.load(Ordering::SeqCst);

        // Process any hold events — dispatch to executor thread (ADR-015)
        for (device_id, events) in hold_events {
            for processed_event in &events {
                if suppress_during_learn {
                    trace!(device_id = %device_id, "Suppressing hold action dispatch during MIDI Learn (timer tick)");
                    continue;
                }
                if let Some(action) =
                    rules.match_event(processed_event, mode.index, Some(device_id.as_str()))
                {
                    let context = TriggerContext {
                        velocity: None,
                        current_mode: current_mode_name.clone(),
                        raw_midi: None,
                        device_id: Some(device_id.as_str().to_string()),
                        // Legacy single-device MIDI path; HidForward is gated to
                        // HID-triggered mappings (multi-device path) at load.
                        input_event: None,
                        osc_message: None,
                    };

                    let trigger_info = Self::extract_trigger_info(
                        std::slice::from_ref(processed_event),
                        &device_id,
                    );
                    let invocation_id = self.action_dispatcher.next_invocation_id();
                    let dispatch = crate::daemon::executor_thread::ActionDispatch {
                        invocation_id,
                        action: action.clone(),
                        context: Some(context),
                        provenance: crate::daemon::executor_thread::ActionProvenance {
                            device_id: Some(device_id.as_str().to_string()),
                            matched_rule: None,
                            mode_name: current_mode_name.clone(),
                            action_type: action_type_string(&action).to_string(),
                            action_summary: summarize_action(&action),
                            trigger_info,
                            mapping_label: None,
                            let_through: false,
                        },
                        dispatch_time: Instant::now(),
                        // ADR-042 D17: MIDI single-device path — never network-tainted.
                        network_origin: None,
                    };

                    if self.action_dispatcher.try_dispatch(dispatch).is_err() {
                        warn!(
                            device_id = %device_id,
                            "Action executor queue full on timer tick, dropping hold action"
                        );
                    }
                }
            }
        }

        Ok(())
    }
    /// Extract trigger info from processed events for MappingFiredPayload (ADR-014)
    pub(crate) fn extract_trigger_info(
        processed_events: &[conductor_core::event_processor::ProcessedEvent],
        device_id: &DeviceId,
    ) -> FiredTriggerInfo {
        use conductor_core::event_processor::ProcessedEvent;

        let device = Some(device_id.as_str().to_string());

        for pe in processed_events {
            match pe {
                ProcessedEvent::PadPressed {
                    note,
                    velocity,
                    channel,
                    ..
                } => {
                    let trigger_type = if *note >= 128 {
                        "gamepad_button"
                    } else {
                        "note"
                    };
                    return FiredTriggerInfo {
                        trigger_type: trigger_type.to_string(),
                        device: device.clone(),
                        channel: *channel,
                        number: Some(*note),
                        value: Some(*velocity as u16),
                    };
                }
                ProcessedEvent::CCReceived { cc, value, channel } => {
                    return FiredTriggerInfo {
                        trigger_type: "cc".to_string(),
                        device: device.clone(),
                        channel: *channel,
                        number: Some(*cc),
                        value: Some(*value as u16),
                    };
                }
                ProcessedEvent::EncoderTurned {
                    cc, value, channel, ..
                } => {
                    return FiredTriggerInfo {
                        trigger_type: "encoder".to_string(),
                        device: device.clone(),
                        channel: *channel,
                        number: Some(*cc),
                        value: Some(*value as u16),
                    };
                }
                ProcessedEvent::PitchBendMoved { value, channel, .. } => {
                    return FiredTriggerInfo {
                        trigger_type: "pitch_bend".to_string(),
                        device: device.clone(),
                        channel: *channel,
                        number: None,
                        value: Some(*value),
                    };
                }
                ProcessedEvent::AftertouchChanged {
                    pressure, channel, ..
                } => {
                    return FiredTriggerInfo {
                        trigger_type: "aftertouch".to_string(),
                        device: device.clone(),
                        channel: *channel,
                        number: None,
                        value: Some(*pressure as u16),
                    };
                }
                ProcessedEvent::ShortPress { note, channel, .. }
                | ProcessedEvent::MediumPress { note, channel, .. }
                | ProcessedEvent::LongPress { note, channel, .. } => {
                    let trigger_type = match pe {
                        ProcessedEvent::ShortPress { .. } => "short_press",
                        ProcessedEvent::MediumPress { .. } => "medium_press",
                        ProcessedEvent::LongPress { .. } => "long_press",
                        _ => unreachable!(),
                    };
                    return FiredTriggerInfo {
                        trigger_type: trigger_type.to_string(),
                        device: device.clone(),
                        channel: *channel,
                        number: Some(*note),
                        value: None,
                    };
                }
                ProcessedEvent::DoubleTap { note, channel, .. } => {
                    return FiredTriggerInfo {
                        trigger_type: "double_tap".to_string(),
                        device: device.clone(),
                        channel: *channel,
                        number: Some(*note),
                        value: None,
                    };
                }
                ProcessedEvent::ChordDetected { notes, channel, .. } => {
                    let trigger_type = if notes.first().copied().unwrap_or(0) >= 128 {
                        "gamepad_chord"
                    } else {
                        "chord"
                    };
                    return FiredTriggerInfo {
                        trigger_type: trigger_type.to_string(),
                        device: device.clone(),
                        channel: *channel,
                        number: notes.first().copied(),
                        value: Some(notes.len() as u16),
                    };
                }
                ProcessedEvent::HoldDetected { note, channel, .. } => {
                    return FiredTriggerInfo {
                        trigger_type: "hold".to_string(),
                        device: device.clone(),
                        channel: *channel,
                        number: Some(*note),
                        value: None,
                    };
                }
                _ => continue,
            }
        }

        // Fallback if no processed event matched
        FiredTriggerInfo {
            trigger_type: "unknown".to_string(),
            device,
            channel: None,
            number: None,
            value: None,
        }
    }
    /// Handle the result of an action dispatch (ADR-009 Gap 1)
    ///
    /// Processes `DispatchResult` from `ActionExecutor::execute()`:
    /// - `ModeChangeRequested`: Updates `ArcSwap<ModeState>` atomically
    /// - `Completed`: No-op
    /// - `Err`: Logs warning
    async fn handle_dispatch_result(&self, result: conductor_core::dispatch::DispatchResult) {
        match result {
            Ok(DispatchOutcome::ModeChangeRequested { mode }) => {
                // Emit action event to monitor (R897)
                self.emit_action_event("mode_change", Some(&format!("Mode → {}", mode)));
                // ADR-040 D4/§4.2: a `ModeChange` action is a *manual*
                // selection, so it pins the mode against auto-switch (origin
                // Action). `apply_modechange_action` locks via `set_mode_manual`
                // (which updates in-memory state first, then persists — backwards
                // compat holds even if the disk save fails) and stays lenient on
                // an unknown target.
                if let Err(e) = self.apply_modechange_action(mode).await {
                    warn!("Failed to apply mode change: {}", e);
                    self.emit_action_event("action_error", Some(&format!("Mode persist: {}", e)));
                }
            }
            Ok(DispatchOutcome::Completed) => {
                // Emit action success to monitor (R897)
                self.emit_action_event("action_executed", None);
            }
            Ok(DispatchOutcome::Cancelled) => {
                // ADR-015: Action was cancelled during shutdown
                debug!("Action cancelled during shutdown");
                self.emit_action_event("action_cancelled", Some("Shutdown"));
            }
            Err(e) => {
                warn!("Action dispatch error: {}", e);
                // Emit action error to monitor (R898)
                self.emit_action_event("action_error", Some(&e.to_string()));
            }
        }
    }
    /// Handle an action completion from the executor thread (ADR-015)
    ///
    /// Called from the highest-priority `biased` select branch. Processes the
    /// dispatch result (mode changes, errors), emits `mapping_fired` with
    /// both `invocation_id` and dual latency, and updates statistics.
    pub(crate) async fn handle_action_completion(
        &self,
        mut completion: crate::daemon::executor_thread::ActionCompletion,
    ) {
        let prov = &completion.provenance;

        trace!(
            invocation_id = completion.invocation_id,
            action_type = prov.action_type,
            execution_time_us = completion.execution_time_us,
            "Action completion received"
        );

        // Handle dispatch result (mode changes, error logging)
        self.handle_dispatch_result(completion.result.clone()).await;

        // ADR-015 D8: Record sent MIDI bytes in recursion guard — only on success.
        // Failed/cancelled actions may not have actually emitted MIDI bytes;
        // recording them would cause false positive echo suppression.
        // Uses try_lock to avoid blocking the async event loop.
        //
        // When `allow_cascade = false` (the default), ALSO open
        // a per-port blanket-suppression window for the output port so any
        // MIDI input arriving on that port within `cascade_ttl_ms` is
        // dropped — catches the cross-note cascade case the per-message
        // echo guard misses (mapping A sends note 63, mapping B fires on
        // note 63 looping back). Both layers are set under the same lock
        // acquisition to avoid taking the mutex twice on the hot path.
        if matches!(completion.result, Ok(DispatchOutcome::Completed))
            && let Some(ref raw) = completion.sent_midi
        {
            // Read settings under the config read lock and release it
            // BEFORE acquiring the recursion-guard mutex (no nested lock
            // hold across the hot path).
            //
            // `cascade_ports` is the list of resolved output ports the
            // executor wrote to during this dispatch. Wrapper actions like
            // `Sequence`/`Repeat` can produce multiple sends in a
            // single dispatch, and `MidiForward { target: "_source" }`
            // resolves to the originating device's bound output —
            // both reasons we now drain from the executor instead of
            // pattern-matching the top-level action.
            let (cascade_ports, cascade_ttl) = {
                let snap = self.live_config.load();
                let advanced = &snap.config.advanced_settings;
                let ports: Vec<String> = if advanced.allow_cascade {
                    Vec::new()
                } else {
                    completion.output_ports.clone()
                };
                (ports, advanced.cascade_ttl_ms)
            };
            match self.recursion_guard.try_lock() {
                Ok(mut guard) => {
                    // Source-aware recursion guard: attribute the send
                    // to its SOURCE device so the source's own repeated notes
                    // aren't false-suppressed as echoes (stuck-note bug). `None`
                    // source stays globally suppressible (unattributed sends).
                    guard.record(raw, prov.device_id.as_deref());
                    for port in &cascade_ports {
                        guard.set_blanket_suppression(port, cascade_ttl);
                    }
                }
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    error!("Recursion guard mutex poisoned; recovering for MIDI recording");
                    let mut guard = poisoned.into_inner();
                    // Source-aware recursion guard: attribute the send
                    // to its SOURCE device so the source's own repeated notes
                    // aren't false-suppressed as echoes (stuck-note bug). `None`
                    // source stays globally suppressible (unattributed sends).
                    guard.record(raw, prov.device_id.as_deref());
                    for port in &cascade_ports {
                        guard.set_blanket_suppression(port, cascade_ttl);
                    }
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    trace!("Recursion guard contended; skipping MIDI recording");
                }
            }
        }

        // Compute end-to-end latency (dispatch_time → now)
        let latency_us = completion.dispatch_time.elapsed().as_micros() as u64;

        // Determine fired result
        let (fired_result, fired_error) = match &completion.result {
            Ok(DispatchOutcome::Cancelled) => {
                // Emit mapping_cancelled event (respect monitoring/capture gating)
                if self.event_monitor_active.load(Ordering::Relaxed) && self.capture_actions {
                    let timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let cancelled_payload = conductor_core::MappingCancelledPayload {
                        invocation_id: completion.invocation_id,
                        reason: "Shutdown".to_string(),
                        timestamp,
                    };
                    if let Ok(payload_value) = serde_json::to_value(&cancelled_payload) {
                        let event = MonitorEvent {
                            timestamp_ms: timestamp,
                            event_type: "mapping_cancelled".to_string(),
                            device_id: prov.device_id.clone(),
                            detail: Some(format!(
                                "Cancelled: {} (invocation {})",
                                prov.action_summary, completion.invocation_id
                            )),
                            payload: Some(payload_value),
                            ..Default::default()
                        };
                        self.push_monitor_event(event);
                    }
                }
                // Update stats and return — no mapping_fired for cancelled
                let mut stats = self.statistics.write().await;
                stats.events_processed += 1;
                return;
            }
            Ok(_) => (conductor_core::FiredResult::Ok, None),
            Err(e) => (conductor_core::FiredResult::Error, Some(e.to_string())),
        };

        // Emit mapping_fired with invocation_id + dual latency (ADR-015 D12)
        if self.event_monitor_active.load(Ordering::Relaxed) && self.capture_actions {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let payload = MappingFiredPayload {
                trigger: prov.trigger_info.clone(),
                action: FiredActionInfo {
                    action_type: prov.action_type.clone(),
                    summary: prov.action_summary.clone(),
                },
                result: fired_result,
                error: fired_error,
                latency_us,
                mode: prov
                    .mode_name
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                mapping_label: prov.mapping_label.clone(),
                timestamp,
                invocation_id: Some(completion.invocation_id),
                execution_time_us: Some(completion.execution_time_us),
                // ADR-025 Phase 3.A — route breadcrumbs collected by
                // the executor (empty for non-context-switch actions).
                // Moved out of `completion` rather than cloned so the
                // context-switch fire path doesn't pay for a vec +
                // string deep-copy on every event.
                routing_trace: std::mem::take(&mut completion.routing_trace),
                // ADR-038: carry let_through so the GUI badge is correct.
                let_through: prov.let_through,
            };

            let detail = format!(
                "Fired: {} → {} [{}, {}µs exec, {}µs e2e]",
                payload.trigger.trigger_type,
                payload.action.summary,
                if payload.result == conductor_core::FiredResult::Ok {
                    "ok"
                } else {
                    "error"
                },
                completion.execution_time_us,
                latency_us,
            );

            match serde_json::to_value(&payload) {
                Ok(payload_value) => {
                    let event = MonitorEvent {
                        timestamp_ms: timestamp,
                        event_type: "mapping_fired".to_string(),
                        device_id: prov.device_id.clone(),
                        detail: Some(detail),
                        payload: Some(payload_value),
                        ..Default::default()
                    };
                    self.push_monitor_event(event);
                }
                Err(e) => {
                    warn!("Failed to serialize MappingFiredPayload: {}", e);
                }
            }
        }

        // Update statistics
        let mut stats = self.statistics.write().await;
        stats.events_processed += 1;
    }
}
