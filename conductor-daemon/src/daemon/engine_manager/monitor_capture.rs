// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `EngineManager` learn/capture/simulate helpers (`simulate_mapping`,
//! `create_learn_event`, `capture_pattern_events`, `flush_pending_chord`,
//! `flush_pending_chord_if_stale`), extracted from `engine_manager::monitor`
//! (refactor #2073).

use super::*;

impl EngineManager {
    /// Simulate a mapping execution (ADR-014 Phase 5A — Issue #488)
    ///
    /// Looks up the mapping by mode name + index, compiles the action, optionally
    /// executes it, and emits a `mapping_fired` MonitorEvent so the GUI shows feedback.
    ///
    /// # Arguments
    /// * `options` — which mapping to simulate and whether to execute
    ///
    /// # Errors
    /// Returns `SimulateError::ModeNotFound` or `SimulateError::MappingIndexOutOfBounds`
    /// if the lookup fails.
    pub async fn simulate_mapping(
        &self,
        options: conductor_core::dispatch::SimulateOptions,
    ) -> std::result::Result<
        conductor_core::dispatch::SimulateResult,
        conductor_core::dispatch::SimulateError,
    > {
        use conductor_core::dispatch::{GLOBAL_MODE_SENTINEL, SimulateError, SimulateResult};

        // Read the active mode name for global mappings and TriggerContext
        let active_mode_name = self.current_mode.load().name.clone();

        let snap = self.live_config.load();
        let config = snap.config.as_ref();

        // Resolve the mapping
        let (mapping, mode_name) = if options.mode == GLOBAL_MODE_SENTINEL {
            let mappings = &config.global_mappings;
            if options.index >= mappings.len() {
                return Err(SimulateError::MappingIndexOutOfBounds {
                    mode: options.mode.clone(),
                    index: options.index,
                    count: mappings.len(),
                });
            }
            // Global mappings report the active mode name (matching real firing path)
            (&mappings[options.index], active_mode_name.clone())
        } else {
            let mode = config.modes.iter().find(|m| m.name == options.mode);
            match mode {
                None => return Err(SimulateError::ModeNotFound(options.mode.clone())),
                Some(m) => {
                    if options.index >= m.mappings.len() {
                        return Err(SimulateError::MappingIndexOutOfBounds {
                            mode: options.mode.clone(),
                            index: options.index,
                            count: m.mappings.len(),
                        });
                    }
                    (&m.mappings[options.index], m.name.clone())
                }
            }
        };

        // Compile the action
        let action: conductor_core::actions::Action = mapping.action.clone().into();
        let action_summary = summarize_action(&action);
        let description = mapping.description.clone();

        // Build trigger info from the config trigger, populated with default value
        let mut trigger_info = Self::trigger_info_from_trigger(&mapping.trigger);

        // Determine velocity/value: use options.value override, or default for this trigger type
        let effective_value = options
            .value
            .or_else(|| Self::default_value_for(&trigger_info));

        // Populate trigger_info.value so simulated events match real ones.
        // For pitch_bend, use a 14-bit default (8192 = center) since u8 doesn't fit.
        if trigger_info.value.is_none() {
            if trigger_info.trigger_type == "pitch_bend" {
                let pb_value = effective_value
                    .map(|v| ((v as u32) * 16383 / 127) as u16) // Map 0-127 → 0-16383
                    .unwrap_or(8192); // Center
                trigger_info.value = Some(pb_value);
            } else {
                trigger_info.value = effective_value.map(|v| v as u16);
            }
        }

        // Synthesize raw MIDI bytes for MidiForward actions
        let raw_midi = Self::synthesize_midi_bytes(&mapping.trigger, effective_value);

        // ADR-038: capture let_through (Copy) before `snap` is dropped below,
        // so the mapping_matched payload can carry it without extending the
        // `snap` borrow across the drop.
        let mapping_let_through = mapping.let_through;

        // D4.A.3.3.A: snap held implicitly by `config: &Config`. Drop it
        // explicitly before dispatching so the LiveConfig snapshot isn't
        // pinned across the dispatch I/O — matches the original
        // RwLock-guard `drop(config)` intent.
        drop(snap);

        let (executed, outcome, error, latency_us) = if options.execute {
            // Only set velocity for note-like triggers (matching real firing path)
            let velocity = match trigger_info.trigger_type.as_str() {
                "note" | "gamepad_button" => effective_value,
                _ => None,
            };

            let context = TriggerContext {
                velocity,
                current_mode: Some(mode_name.clone()),
                raw_midi,
                device_id: None, // Simulate path — no originating device
                input_event: None,
                osc_message: None,
            };

            // ADR-015: Dispatch to executor thread via try_dispatch() — non-blocking.
            // The executor thread runs the action (including thread::sleep for Sequence
            // inter-step delays) without stalling the event loop. The normal completion
            // handler (biased select first branch) emits mapping_fired when done.
            let invocation_id = self.action_dispatcher.next_invocation_id();
            let trigger_info_for_event = trigger_info.clone();
            let dispatch = crate::daemon::executor_thread::ActionDispatch {
                invocation_id,
                action: action.clone(),
                context: Some(context),
                provenance: crate::daemon::executor_thread::ActionProvenance {
                    device_id: None,
                    matched_rule: description.clone(),
                    mode_name: Some(mode_name.clone()),
                    action_type: action_type_string(&action).to_string(),
                    action_summary: action_summary.clone(),
                    trigger_info,
                    mapping_label: description.clone(),
                    let_through: mapping_let_through,
                },
                dispatch_time: Instant::now(),
                // ADR-042 D17: simulate path — operator-initiated, not network-tainted.
                network_origin: None,
            };

            match self.action_dispatcher.try_dispatch(dispatch) {
                Ok(id) => {
                    // Emit mapping_matched with structured payload (ADR-015)
                    if self.event_monitor_active.load(Ordering::Relaxed) && self.capture_actions {
                        let timestamp = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let matched_payload = conductor_core::MappingMatchedPayload {
                            invocation_id: id,
                            trigger: trigger_info_for_event,
                            action_type: action_type_string(&action).to_string(),
                            action_summary: action_summary.clone(),
                            mode: mode_name.clone(),
                            mapping_label: description.clone(),
                            let_through: mapping_let_through,
                            timestamp,
                        };
                        if let Ok(payload_value) = serde_json::to_value(&matched_payload) {
                            self.push_monitor_event(MonitorEvent {
                                timestamp_ms: timestamp,
                                event_type: "mapping_matched".to_string(),
                                detail: Some(format!(
                                    "[{}] {} (simulation, invocation {})",
                                    mode_name,
                                    action_type_string(&action),
                                    id
                                )),
                                payload: Some(payload_value),
                                ..Default::default()
                            });
                        }
                    }
                    // mapping_fired will be emitted by handle_action_completion()
                    // when the executor thread finishes.
                    // executed=false: action is dispatched/queued, not yet completed.
                    (false, Some(format!("dispatched:{}", id)), None, 0)
                }
                Err(_dispatch) => {
                    warn!("Executor queue full during simulation");
                    (
                        false,
                        Some("dropped".to_string()),
                        Some("Executor queue full".to_string()),
                        0,
                    )
                }
            }
        } else {
            // Dry-run: emit mapping_matched for GUI feedback without executing
            if self.event_monitor_active.load(Ordering::Relaxed) && self.capture_actions {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;

                let payload = MappingFiredPayload {
                    trigger: trigger_info,
                    action: FiredActionInfo {
                        action_type: conductor_core::action_type_string(&action).to_string(),
                        summary: action_summary.clone(),
                    },
                    result: conductor_core::FiredResult::Ok,
                    error: None,
                    latency_us: 0,
                    mode: mode_name.clone(),
                    mapping_label: description,
                    timestamp,
                    invocation_id: None,
                    execution_time_us: None,
                    // Dry-run simulate_mapping path never exercises
                    // the executor, so no breadcrumbs are collected.
                    routing_trace: Vec::new(),
                    // ADR-038: carry let_through so the GUI badge is correct on dry-runs.
                    let_through: mapping_let_through,
                };

                let detail = format!(
                    "Fired: {} → {} [ok, dry-run]",
                    payload.trigger.trigger_type, payload.action.summary,
                );

                if let Ok(payload_value) = serde_json::to_value(&payload) {
                    self.push_monitor_event(MonitorEvent {
                        timestamp_ms: timestamp,
                        event_type: "mapping_fired".to_string(),
                        detail: Some(detail),
                        payload: Some(payload_value),
                        ..Default::default()
                    });
                }
            }
            (false, None, None, 0)
        };

        Ok(SimulateResult {
            mode: options.mode,
            index: options.index,
            action_summary,
            executed,
            outcome,
            error,
            latency_us,
        })
    }
    /// Create a MidiLearnEvent from an InputEvent (shared helper for both pipelines)
    pub(crate) fn create_learn_event(
        &self,
        input_event: &InputEvent,
        timestamp: u64,
    ) -> Option<MidiLearnEvent> {
        match input_event {
            InputEvent::PadPressed { pad, velocity, .. } => {
                if *pad >= 128 {
                    Some(MidiLearnEvent {
                        event_type: EventType::GamepadButton,
                        button: Some(*pad),
                        velocity: Some(*velocity),
                        timestamp,
                        ..Default::default()
                    })
                } else {
                    Some(MidiLearnEvent {
                        event_type: EventType::NoteOn,
                        note: Some(*pad),
                        velocity: Some(*velocity),
                        timestamp,
                        ..Default::default()
                    })
                }
            }
            InputEvent::PadReleased { pad, .. } => {
                if *pad >= 128 {
                    Some(MidiLearnEvent {
                        event_type: EventType::GamepadButtonRelease,
                        button: Some(*pad),
                        timestamp,
                        ..Default::default()
                    })
                } else {
                    Some(MidiLearnEvent {
                        event_type: EventType::NoteOff,
                        note: Some(*pad),
                        velocity: Some(0),
                        timestamp,
                        ..Default::default()
                    })
                }
            }
            InputEvent::ControlChange { control, value, .. } => Some(MidiLearnEvent {
                event_type: EventType::Cc,
                cc: Some(*control),
                value: Some(*value),
                timestamp,
                ..Default::default()
            }),
            InputEvent::EncoderTurned { encoder, value, .. } => {
                if *encoder >= 132 {
                    Some(MidiLearnEvent {
                        event_type: EventType::GamepadTrigger,
                        trigger: Some(*encoder),
                        value: Some(*value),
                        timestamp,
                        ..Default::default()
                    })
                } else if *encoder >= 128 {
                    Some(MidiLearnEvent {
                        event_type: EventType::GamepadAxis,
                        axis: Some(*encoder),
                        value: Some(*value),
                        timestamp,
                        ..Default::default()
                    })
                } else {
                    Some(MidiLearnEvent {
                        event_type: EventType::Encoder,
                        cc: Some(*encoder),
                        value: Some(*value),
                        timestamp,
                        ..Default::default()
                    })
                }
            }
            InputEvent::PitchBend { value, .. } => Some(MidiLearnEvent {
                event_type: EventType::PitchBend,
                value: Some((*value >> 7) as u8),
                timestamp,
                ..Default::default()
            }),
            InputEvent::Aftertouch { pressure, .. } => Some(MidiLearnEvent {
                event_type: EventType::Aftertouch,
                value: Some(*pressure),
                timestamp,
                ..Default::default()
            }),
            InputEvent::PolyPressure { pad, pressure, .. } => Some(MidiLearnEvent {
                event_type: EventType::PolyPressure,
                note: Some(*pad),
                value: Some(*pressure),
                timestamp,
                ..Default::default()
            }),
            // ADR-025 Phase 1: PC capture for foot-controller bank stomps
            // and similar (multi-device path).
            InputEvent::ProgramChange {
                program, channel, ..
            } => Some(MidiLearnEvent {
                event_type: EventType::ProgramChange,
                pc: Some(*program),
                channel: channel.unwrap_or(0),
                timestamp,
                ..Default::default()
            }),
        }
    }

    /// Capture pattern events into MIDI Learn buffer (shared helper)
    ///
    /// Chord events are debounced: when a ChordDetected arrives, it is stored
    /// in `pending_chord_event` instead of being pushed immediately. If a larger
    /// chord supersedes it (same notes as a subset), we replace. Non-chord events
    /// flush the pending chord first. The pending chord is also flushed by
    /// `flush_pending_chord_if_stale()` called from the polling path.
    pub(crate) async fn capture_pattern_events(
        &self,
        processed_events: &[conductor_core::event_processor::ProcessedEvent],
        timestamp: u64,
        device_id: Option<&DeviceId>,
    ) {
        // #2486: the chord-detection window actually in effect, so the chord
        // pattern pill reflects reality (chord_timeout_ms normally /
        // chord_learn_timeout_ms during Learn) instead of a hardcoded 100ms that
        // matched neither configured value.
        let chord_window_ms = {
            let snap = self.live_config.load();
            super::helpers::active_chord_window_ms(
                self.midi_learn_active.load(Ordering::SeqCst),
                snap.config.advanced_settings.chord_timeout_ms,
                snap.config.advanced_settings.chord_learn_timeout_ms,
            )
        };
        for processed_event in processed_events {
            let (is_chord, pattern_event) = match processed_event {
                conductor_core::event_processor::ProcessedEvent::LongPress {
                    note,
                    duration_ms,
                    ..
                } => (
                    false,
                    Some(MidiLearnEvent {
                        event_type: EventType::NoteOn,
                        note: Some(*note),
                        timestamp,
                        device_id: device_id.map(|d| d.as_str().to_string()),
                        pattern_type: Some(PatternType::LongPress),
                        pattern_duration_ms: Some(*duration_ms as u64),
                        ..Default::default()
                    }),
                ),
                conductor_core::event_processor::ProcessedEvent::DoubleTap { note, .. } => (
                    false,
                    Some(MidiLearnEvent {
                        event_type: EventType::NoteOn,
                        note: Some(*note),
                        timestamp,
                        device_id: device_id.map(|d| d.as_str().to_string()),
                        pattern_type: Some(PatternType::DoubleTap),
                        pattern_timeout_ms: Some(300),
                        ..Default::default()
                    }),
                ),
                conductor_core::event_processor::ProcessedEvent::ChordDetected {
                    notes, ..
                } => {
                    let is_gamepad_chord = notes.iter().any(|n| *n >= 128);
                    if is_gamepad_chord {
                        (
                            true,
                            Some(MidiLearnEvent {
                                event_type: EventType::GamepadButton,
                                timestamp,
                                device_id: device_id.map(|d| d.as_str().to_string()),
                                pattern_type: Some(PatternType::GamepadChord),
                                pattern_buttons: Some(notes.clone()),
                                pattern_timeout_ms: Some(chord_window_ms),
                                ..Default::default()
                            }),
                        )
                    } else {
                        (
                            true,
                            Some(MidiLearnEvent {
                                event_type: EventType::NoteOn,
                                timestamp,
                                device_id: device_id.map(|d| d.as_str().to_string()),
                                pattern_type: Some(PatternType::Chord),
                                pattern_notes: Some(notes.clone()),
                                pattern_timeout_ms: Some(chord_window_ms),
                                ..Default::default()
                            }),
                        )
                    }
                }
                _ => (false, None),
            };

            if let Some(event) = pattern_event {
                if is_chord {
                    // Debounce: store as pending, replacing any smaller chord
                    let mut pending = self.pending_chord_event.lock().await;
                    let new_count = event
                        .pattern_notes
                        .as_ref()
                        .map(|n| n.len())
                        .or_else(|| event.pattern_buttons.as_ref().map(|b| b.len()))
                        .unwrap_or(0);
                    let should_replace = match &*pending {
                        Some((existing, _)) => {
                            let existing_count = existing
                                .pattern_notes
                                .as_ref()
                                .map(|n| n.len())
                                .or_else(|| existing.pattern_buttons.as_ref().map(|b| b.len()))
                                .unwrap_or(0);
                            new_count >= existing_count
                        }
                        None => true,
                    };
                    if should_replace {
                        *pending = Some((event, Instant::now()));
                    }
                } else {
                    // Non-chord event: flush any pending chord first, then push this event
                    self.flush_pending_chord().await;
                    let mut events = self.midi_learn_events.lock().await;
                    if events.len() >= MIDI_LEARN_MAX_EVENTS {
                        events.pop_front();
                    }
                    events.push_back(event);
                }
            }
        }
    }

    /// Flush pending chord event to the MIDI Learn buffer unconditionally.
    pub(crate) async fn flush_pending_chord(&self) {
        let mut pending = self.pending_chord_event.lock().await;
        if let Some((event, _)) = pending.take() {
            let mut events = self.midi_learn_events.lock().await;
            if events.len() >= MIDI_LEARN_MAX_EVENTS {
                events.pop_front();
            }
            events.push_back(event);
        }
    }

    /// Flush a pending chord once it has been waiting longer than the configured
    /// Learn chord window (`advanced_settings.chord_learn_timeout_ms`, default
    /// 150ms — #2386). Called from the event polling path so chords aren't stuck
    /// forever. Reads the SAME field the Learn `EventProcessor`s use, so the
    /// poll-side flush can't cap or split a chord before the configured window
    /// elapses (Copilot review on PR #2481 — previously hardcoded 150ms).
    pub(crate) async fn flush_pending_chord_if_stale(&self) {
        let learn_window = Duration::from_millis(
            self.live_config
                .load()
                .config
                .advanced_settings
                .chord_learn_timeout_ms,
        );
        let should_flush = {
            let pending = self.pending_chord_event.lock().await;
            match &*pending {
                Some((_, timestamp)) => timestamp.elapsed() > learn_window,
                None => false,
            }
        };
        if should_flush {
            self.flush_pending_chord().await;
        }
    }
}
