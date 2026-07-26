// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `EngineManager::process_device_event`, extracted from
//! `engine_manager::events_dispatch` (refactor #2073).

use super::*;

impl EngineManager {
    /// Process a device-tagged input event through the multi-device pipeline (v4.20.0)
    pub(crate) async fn process_device_event(
        &mut self,
        device_event: DeviceEvent<InputEvent>,
    ) -> Result<()> {
        let (device_id, input_event) = device_event.into_parts();

        // Check mute status (D8)
        {
            let mgr_guard = self.input_manager.lock().await;
            if let Some(ref mgr) = *mgr_guard
                && !mgr.is_device_enabled(&device_id)
            {
                trace!(device_id = %device_id, "Dropping event from muted device");
                return Ok(());
            }
        }

        // Per-device rate limiting (v4.26.0 - ADR-009 D9)
        if !self.device_rate_limiter.check(&device_id) {
            warn!(device_id = %device_id, "Rate limit exceeded — dropping event");
            return Ok(());
        }

        debug!(device_id = %device_id, "Processing multi-device input event: {:?}", input_event);

        // Latency tracking (R899/Issue #709) — used for both processing_us on raw monitor
        // events (capture_midi) and dispatch_time on ActionDispatch (capture_actions).
        let processing_start = if self.event_monitor_active.load(Ordering::Relaxed)
            && (self.capture_midi || self.capture_actions)
        {
            Some(std::time::Instant::now())
        } else {
            None
        };

        // MIDI Learn capture with device_id
        if self.midi_learn_active.load(Ordering::SeqCst) {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let learn_event = self.create_learn_event(&input_event, timestamp);
            if let Some(mut event) = learn_event {
                event.device_id = Some(device_id.as_str().to_string());
                let mut events = self.midi_learn_events.lock().await;
                if events.len() >= MIDI_LEARN_MAX_EVENTS {
                    events.pop_front();
                }
                events.push_back(event);
            }
        }

        // Extract raw MIDI bytes before processing consumes the event (v4.25.0 - ADR-009 Gap 2)
        let raw_midi = extract_raw_midi(&input_event);

        // ADR-015 D8 + Issue #555: Check recursion guard — both
        // per-message echo suppression (exact bytes within TTL) and
        // per-port blanket cascade suppression (when allow_cascade=false).
        // Both checks share one lock acquisition. Blanket suppression
        // takes precedence and is checked first because it's the broader
        // guard — if the port is in its window, every event drops
        // regardless of byte content.
        //
        // Uses try_lock to avoid blocking the async event loop on
        // contention; on WouldBlock, both checks are skipped (rare,
        // and the next event will get the chance — better than
        // serializing the hot path).
        //
        // The `allow_cascade` gate is read fresh per event so a config
        // reload that flips it to `true` takes effect immediately —
        // any windows still in `blanket_until` are simply ignored and
        // expire harmlessly within ≤ `BLANKET_TTL_MAX_MS`. This
        // addresses Copilot's review on PR #1211 where a stale window
        // could otherwise outlive the opt-out flip by up to 60 s.
        if let Some(ref raw) = raw_midi {
            let allow_cascade = self
                .live_config
                .load()
                .config
                .advanced_settings
                .allow_cascade;
            let mut suppressed_kind: Option<&'static str> = None;
            match self.recursion_guard.try_lock() {
                Ok(mut guard) => {
                    if !allow_cascade && guard.is_blanket_suppressed(device_id.as_str()) {
                        suppressed_kind = Some("midi_cascade_suppressed");
                    } else if guard.is_echo(raw, Some(device_id.as_str())) {
                        suppressed_kind = Some("midi_echo_suppressed");
                    }
                }
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    error!(device_id = %device_id, "Recursion guard mutex poisoned; recovering");
                    let mut guard = poisoned.into_inner();
                    if !allow_cascade && guard.is_blanket_suppressed(device_id.as_str()) {
                        suppressed_kind = Some("midi_cascade_suppressed");
                    } else if guard.is_echo(raw, Some(device_id.as_str())) {
                        suppressed_kind = Some("midi_echo_suppressed");
                    }
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    trace!(device_id = %device_id, "Recursion guard contended; skipping echo check");
                }
            }
            if let Some(event_type) = suppressed_kind {
                trace!(device_id = %device_id, kind = event_type, "Suppressed MIDI input");
                // #2397: the event is ALWAYS suppressed (we return below); only
                // the MonitorEvent telemetry is coalesced. Under a feedback loop
                // / chord storm this collapses a 1:1 flood into one summary per
                // kind per window, keeping the GUI events panel responsive.
                if self.event_monitor_active.load(Ordering::Relaxed)
                    && let Some(absorbed) = self.suppression_throttle.record(event_type)
                {
                    let detail = if absorbed == 0 {
                        format!("{} from {}", event_type, device_id)
                    } else {
                        // The throttle is keyed by kind only, so the coalesced
                        // count may span multiple devices — don't attribute the
                        // aggregate to this one device (Copilot review).
                        format!(
                            "{} ×{} suppressed across devices in last {}s",
                            event_type,
                            absorbed + 1,
                            crate::daemon::suppression_throttle::SuppressionThrottle::DEFAULT_INTERVAL
                                .as_secs()
                        )
                    };
                    self.emit_action_event(event_type, Some(&detail));
                }
                return Ok(());
            }
        }

        // ADR-025 Phase 3.D: snapshot the previous PC on this
        // (device, channel) tuple BEFORE the store write so we can surface
        // a ContextSwitch annotation when the processed event emits. See
        // `detect_pc_transition` for the contract.
        let pc_transition =
            Self::detect_pc_transition(&input_event, device_id.as_str(), &self.control_state);

        // ADR-025 Phase 1: observe the raw event into the physical control
        // state store. Placed AFTER mute, rate-limit, and recursion-guard
        // so muted / dropped / echoed events don't pollute state — but
        // BEFORE transforms, so the store reflects hardware reality, not
        // the transformed logical stream.
        self.control_state
            .observe_input_event(device_id.as_str(), &input_event);

        // Record event fingerprint for device classification (ADR-022 D7)
        // Placed after mute/rate-limit/recursion-guard checks so only accepted events are counted.
        // Keyed by the DeviceId string (either a configured alias or the raw port name as-is).
        // Also key by the base port name without an instance suffix like " #2"
        // (e.g., "My Controller #2" → "My Controller") so MCP tool lookups by
        // plain port_name can still match even when the OS appends an instance number.
        let device_key = device_id.as_str();
        // One SystemTime::now() per accepted event (~20ns), shared across all key variants.
        // Acceptable for health-report granularity (seconds, not sub-ms).
        let fingerprint_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64);
        self.record_event_fingerprint(device_key, &input_event, fingerprint_ts);
        // For configured devices, device_key is an alias (e.g., "pads") which won't match
        // MCP lookups by port_name. Use cached alias→port_name mapping (O(1) DashMap lookup).
        if let Some(port_name) = self.device_port_name_cache.get(device_key) {
            let pn = port_name.value();
            self.record_event_fingerprint(pn, &input_event, fingerprint_ts);
            // Also strip instance suffix from port_name (e.g., "Port #2" → "Port")
            if let Some((base, sfx)) = pn.rsplit_once(" #")
                && sfx.chars().all(|c| c.is_ascii_digit())
            {
                self.record_event_fingerprint(base, &input_event, fingerprint_ts);
            }
        }
        // Strip instance suffix from device_key itself too
        if let Some((base_name, suffix)) = device_key.rsplit_once(" #")
            && suffix.chars().all(|c| c.is_ascii_digit())
        {
            self.record_event_fingerprint(base_name, &input_event, fingerprint_ts);
        }

        // Event monitor capture with device_id (Issue #326, R926)
        // Create the raw MonitorEvent after echo check but defer push until after
        // processing so we can stamp processing_us on it (Issue #709).
        // Note: deferred push changes daemon buffer insertion order — the raw event
        // is inserted after mapping_matched/processed events from the same input.
        // Timestamps may differ by ~0-1ms (each calls SystemTime::now()). The UI
        // displays events in arrival order, but the entire pipeline completes in
        // <1ms so all events from one interaction appear as a contiguous group.
        let pending_monitor_event =
            if self.event_monitor_active.load(Ordering::Relaxed) && self.capture_midi {
                Self::create_monitor_event(&input_event, Some(device_id.as_str()))
            } else {
                None
            };

        // Get or create the per-device EventProcessor with ALL configurable
        // timing knobs currently in effect (#2386/#2486 chord + #2490 hold +
        // double-tap). The chord window is Learn-aware; hold/double-tap come
        // from config. Previously only chord was wired (the non-Learn branch
        // used `EventProcessor::new()`, hardcoding hold=2s / double-tap=300ms),
        // so the "Long Press Threshold" slider was decorative (#2490). Computed
        // before the `entry()` borrow so the closure captures a plain copy, not
        // `self`.
        let timings = {
            let snap = self.live_config.load();
            super::helpers::event_timings_from_config(
                self.midi_learn_active.load(Ordering::SeqCst),
                &snap.config.advanced_settings,
            )
        };
        let processed_events = {
            let mut processor = self
                .event_processors
                .entry(device_id.clone())
                .or_insert_with(|| EventProcessor::with_timings(timings));
            processor.process_input(input_event)
        };

        // MIDI Learn pattern detection for multi-device
        if self.midi_learn_active.load(Ordering::SeqCst) {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            self.capture_pattern_events(&processed_events, timestamp, Some(&device_id))
                .await;
        }

        // Map ProcessedEvents → Action — LOCK-FREE with device filter (v4.21.0 - ADR-009 Phase 3)
        // Uses match_event_with_provenance for ActionEnvelope with debug provenance (v4.26.0 - ADR-009 Gap K)
        // D4.A.3.3.A: rules sourced from `live_config.load().rules` — the
        // legacy `self.rule_set` field retired alongside the
        // bridge helper.
        let mode = self.current_mode.load();
        let snap = self.live_config.load();
        let rules = &snap.rules;
        let mut envelope = None;
        let mut matched_event_idx: usize = 0;

        let current_mode_name = if mode.name.is_empty() {
            None
        } else {
            Some(mode.name.clone())
        };

        // ADR-036 Phase 3: pre-mapping route dispatch removed. All routes are
        // post-mapping — they run after the rule-engine matcher below.

        for (idx, processed_event) in processed_events.iter().enumerate() {
            if let Some(found_envelope) = rules.match_event_with_provenance(
                processed_event,
                mode.index,
                Some(device_id.as_str()),
            ) {
                envelope = Some(found_envelope);
                matched_event_idx = idx;
                break;
            }
        }

        // #836: Suppress dispatch when MIDI Learn is active — symmetric
        // with the legacy path. The user is capturing input for a new
        // mapping; firing the existing mapping mid-learn surprises them
        // with side effects. Resetting `envelope` to None preserves the
        // rest of the function (raw + processed event monitor emissions
        // still happen).
        let envelope = if envelope.is_some() && self.midi_learn_active.load(Ordering::SeqCst) {
            trace!(device_id = %device_id, "Suppressing action dispatch during MIDI Learn (multi-device path)");
            None
        } else {
            envelope
        };

        // Dispatch action to executor thread (ADR-015)
        if let Some(ref env) = envelope {
            debug!(
                device_id = %device_id,
                mode = ?env.mode_name,
                rule = ?env.matched_rule,
                "Dispatching action for multi-device event"
            );

            let velocity = processed_events.iter().find_map(|e| match e {
                conductor_core::event_processor::ProcessedEvent::PadPressed {
                    velocity, ..
                } => Some(*velocity),
                _ => None,
            });

            // ADR-039-B #1762 step 4b: carry the structured event so a
            // `HidForward` action can translate the original gamepad event
            // (raw_midi is lossy for HID). Reuse the `ProcessedEvent::Raw`
            // clone the processor always emits first (same source the route
            // context reads), so this is one small InputEvent clone on a path
            // already cloning raw_midi/action.
            let input_event = match processed_events.first() {
                Some(conductor_core::event_processor::ProcessedEvent::Raw(ev)) => Some(ev.clone()),
                _ => None,
            };
            let context = TriggerContext {
                velocity,
                current_mode: current_mode_name.clone(),
                raw_midi: raw_midi.clone(),
                device_id: Some(device_id.as_str().to_string()),
                input_event,
                // MIDI/HID path — no inbound OSC message (OscForward is a no-op).
                osc_message: None,
            };

            // Extract trigger info from the specific event that matched the rule
            let matched_event = &processed_events[matched_event_idx..=matched_event_idx];
            let trigger_info = Self::extract_trigger_info(matched_event, &device_id);

            let invocation_id = self.action_dispatcher.next_invocation_id();
            let dispatch = crate::daemon::executor_thread::ActionDispatch {
                invocation_id,
                action: env.action.clone(),
                context: Some(context),
                provenance: crate::daemon::executor_thread::ActionProvenance {
                    device_id: Some(device_id.as_str().to_string()),
                    matched_rule: env.matched_rule.clone(),
                    mode_name: env.mode_name.clone(),
                    action_type: action_type_string(&env.action).to_string(),
                    action_summary: summarize_action(&env.action),
                    trigger_info,
                    mapping_label: env.matched_rule.clone(),
                    let_through: env.let_through,
                },
                dispatch_time: processing_start.unwrap_or_else(Instant::now),
                // ADR-042 D17: MIDI/HID multi-device path — never network-tainted.
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
                            trigger: Self::extract_trigger_info(matched_event, &device_id),
                            action_type: action_type_string(&env.action).to_string(),
                            action_summary: summarize_action(&env.action),
                            mode: env.mode_name.as_deref().unwrap_or("unknown").to_string(),
                            mapping_label: env.matched_rule.clone(),
                            let_through: env.let_through,
                            timestamp,
                        };
                        if let Ok(payload_value) = serde_json::to_value(&matched_payload) {
                            self.push_monitor_event(MonitorEvent {
                                timestamp_ms: timestamp,
                                event_type: "mapping_matched".to_string(),
                                device_id: Some(device_id.as_str().to_string()),
                                detail: Some(format!(
                                    "[{}] {} ({}) invocation {}",
                                    matched_payload.mode,
                                    env.matched_rule.as_deref().unwrap_or("(unnamed mapping)"),
                                    device_id,
                                    id
                                )),
                                payload: Some(payload_value),
                                ..Default::default()
                            });
                        }
                    }
                }
                Err(_dispatch) => {
                    warn!(
                        device_id = %device_id,
                        "Action executor queue full, dropping action"
                    );
                    if self.event_monitor_active.load(Ordering::Relaxed) && self.capture_actions {
                        let timestamp = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let dropped_payload = conductor_core::MappingDroppedPayload {
                            invocation_id,
                            reason: format!("Executor queue full ({})", device_id),
                            timestamp,
                        };
                        if let Ok(payload_value) = serde_json::to_value(&dropped_payload) {
                            self.push_monitor_event(MonitorEvent {
                                timestamp_ms: timestamp,
                                event_type: "mapping_dropped".to_string(),
                                device_id: Some(device_id.as_str().to_string()),
                                detail: Some(format!(
                                    "Executor queue full ({}) invocation {}",
                                    device_id, invocation_id
                                )),
                                payload: Some(payload_value),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }

        // Stage 9 (ADR-031 § 4.5 / #1301 slice 1): fan out via configured
        // routes when the event reaches the route stage AND we have raw MIDI
        // bytes.
        //
        // ADR-038 §2: the route gate is driven by an explicit `RouteDisposition`
        // (NOT a collapsed bool). Post-mapping routes fire on `NoMatch` (nothing
        // matched, the pre-ADR-038 behaviour) OR `LetThrough` (a mapping matched
        // and consented to let the event continue); they are skipped on
        // `Consumed` (a mapping matched and swallowed it). The matched action,
        // if any, was already dispatched fire-and-forget above (ADR-015), so the
        // route proceeds regardless of that action's speed or backpressure.
        //
        // Each `(to_alias, bytes)` pair from `route_destinations()` carries
        // bytes ALREADY transformed by the route's `transform` (route_engine.rs
        // applies it inside route_destinations). We dispatch each as an
        // `Action::MidiForward` with `transform: None` and the transformed
        // bytes in `raw_midi` context — that reuses the existing action
        // executor's MIDI output dispatcher (port resolution via
        // `resolve_output_port`, recursion-guard registration via
        // `RecursionGuard::register_outbound`, echo/cascade suppression on
        // any return event back into the engine — ADR-015 D8 / #555).
        //
        // Route fan-out is MIDI→MIDI only (compile() excludes cross-protocol
        // routes upfront) — an ADR-031 routing constraint, unrelated to the
        // ADR-038 gate below.
        let route_disposition = RouteDisposition::from_envelope(envelope.as_ref());
        if route_disposition.allows_route()
            && let Some(ref raw) = raw_midi
        {
            let route_engine = self.route_engine.load();
            // ADR-036 D1: post-mapping routes are mode-scoped; pass the
            // active mode so mode-ineligible routes are skipped.
            // ADR-039 #1759: pass the `raw` we already extracted at line 65 — no
            // re-extraction on the hot path.
            // ADR-039-B #1762: also thread the structured event so HID routes'
            // structured transforms (HidToArtNet) can recover gamepad-native
            // semantics the lossy byte form drops (§6.2.1). Mapping's
            // `process_input` consumed `input_event`, but it always emits a clone
            // as `ProcessedEvent::Raw` *first* (event_processor.rs) — reuse THAT
            // (zero extra alloc, so the MIDI byte path stays perf-neutral). Read
            // it O(1) via `first()`; the defensive non-`Raw` arm yields `None`
            // (structured transforms then skip, same as a byte-only caller).
            // MIDI routes never read `ctx.event`.
            let structured_event = match processed_events.first() {
                Some(conductor_core::event_processor::ProcessedEvent::Raw(ev)) => Some(ev),
                _ => None,
            };
            let ctx = crate::route_engine::RouteEvalContext {
                raw_midi: raw,
                // MIDI/HID path — OSC events route via process_osc_event
                // (ADR-039-A), which builds a RouteInput::Osc context.
                input: structured_event.map_or(crate::route_engine::RouteInput::None, |ev| {
                    crate::route_engine::RouteInput::Event(ev)
                }),
                mode: mode.name.as_str(),
            };
            let destinations = route_engine.route_destinations_ctx(device_id.as_str(), &ctx);
            if !destinations.is_empty() {
                trace!(
                    device_id = %device_id,
                    active_mode = %mode.name,
                    count = destinations.len(),
                    disposition = ?route_disposition,
                    "post-mapping routes dispatched (NoMatch or let-through)"
                );
            }
            self.dispatch_route_outputs(
                destinations,
                raw,
                &device_id,
                &current_mode_name,
                &processed_events,
                route_disposition,
            )
            .await;
        }

        // Emit processed events to monitor with device context (R893, R927)
        if !processed_events.is_empty()
            && self.event_monitor_active.load(Ordering::Relaxed)
            && self.capture_processed
        {
            trace!(
                device_id = %device_id,
                "Processed {} events from multi-device input",
                processed_events.len()
            );
            for pe in &processed_events {
                self.emit_processed_event_with_transition(
                    pe,
                    Some(device_id.as_str()),
                    pc_transition,
                );
            }
        }

        // Push deferred raw monitor event with processing latency stamped (Issue #709)
        if let Some(mut event) = pending_monitor_event {
            if let Some(start) = processing_start {
                event.processing_us = Some(start.elapsed().as_micros() as u64);
            }
            self.push_monitor_event(event);
        }

        Ok(())
    }

    /// ADR-039-A: process an inbound OSC message.
    ///
    /// Slice 1 (#1361) delivered OSC to the ROUTE engine only (D17 by
    /// construction). Slice 2 (#2325) opens the mapping path: typed OSC
    /// triggers are evaluated FIRST, and any dispatched action carries the
    /// network-origin taint (ADR-042 D17 — see the inline block below); the
    /// route-engine dispatch then runs unchanged. `raw_midi` stays empty on
    /// both paths (OSC carries no MIDI bytes); `RouteDisposition::NoMatch`
    /// remains the route-trace disposition (routes are post-mapping).
    pub(crate) async fn process_osc_event(&self, device_id: &DeviceId, event: &ProtocolEvent) {
        let mode = self.current_mode.load();
        let current_mode_name = if mode.name.is_empty() {
            None
        } else {
            Some(mode.name.clone())
        };

        // ── Mapping engine — typed OSC triggers (Slice 2, #2325) ─────────
        // Every action dispatched from this path carries the NETWORK-ORIGIN
        // TAINT (ADR-042 D17): `network_origin = Some(listener alias)`, so
        // the executor's action-class gate refuses sensitive actions
        // (Shell/Launch/Keystroke, incl. statically nested in Sequence/
        // Delay/Conditional) unless the listener set
        // `allow_sensitive_actions = true`. MIDI/HID dispatch paths set
        // `network_origin: None` and are never gated.
        if let ProtocolEvent::Osc(osc) = event {
            let processed = conductor_core::event_processor::ProcessedEvent::OscReceived {
                address: osc.address.clone(),
                args: osc.args.clone(),
            };

            // Monitor visibility for the mapping-engine-facing form.
            if self.event_monitor_active.load(Ordering::Relaxed) {
                self.emit_processed_event_with_transition(
                    &processed,
                    Some(device_id.as_str()),
                    None,
                );
            }

            let snap = self.live_config.load();
            let envelope = snap.rules.match_event_with_provenance(
                &processed,
                mode.index,
                Some(device_id.as_str()),
            );

            // #836 symmetry: suppress dispatch while MIDI Learn captures.
            let envelope = if envelope.is_some() && self.midi_learn_active.load(Ordering::SeqCst) {
                trace!(device_id = %device_id, "Suppressing OSC action dispatch during MIDI Learn");
                None
            } else {
                envelope
            };

            if let Some(env) = envelope {
                debug!(
                    device_id = %device_id,
                    mode = ?env.mode_name,
                    rule = ?env.matched_rule,
                    "Dispatching action for OSC event (network-tainted, ADR-042 D17)"
                );
                let context = TriggerContext {
                    velocity: None,
                    current_mode: current_mode_name.clone(),
                    raw_midi: None,
                    device_id: Some(device_id.as_str().to_string()),
                    input_event: None,
                    // #2326: carry the inbound OSC so an OscForward action on
                    // this mapping can re-send it to an OSC output endpoint.
                    osc_message: Some(osc.clone()),
                };
                let trigger_info = conductor_core::FiredTriggerInfo {
                    trigger_type: "osc".to_string(),
                    channel: None,
                    device: Some(device_id.as_str().to_string()),
                    number: None,
                    value: None,
                };
                let invocation_id = self.action_dispatcher.next_invocation_id();
                let dispatch = crate::daemon::executor_thread::ActionDispatch {
                    invocation_id,
                    action: env.action.clone(),
                    context: Some(context),
                    provenance: crate::daemon::executor_thread::ActionProvenance {
                        device_id: Some(device_id.as_str().to_string()),
                        matched_rule: env.matched_rule.clone(),
                        mode_name: env.mode_name.clone(),
                        action_type: action_type_string(&env.action).to_string(),
                        action_summary: summarize_action(&env.action),
                        trigger_info,
                        mapping_label: env.matched_rule.clone(),
                        let_through: env.let_through,
                    },
                    dispatch_time: Instant::now(),
                    // ADR-042 D17: THE taint — the OSC listener's endpoint
                    // alias; the executor's gate keys its per-listener
                    // allow_sensitive_actions lookup on this.
                    network_origin: Some(device_id.as_str().to_string()),
                };
                if let Err(e) = self.action_dispatcher.try_dispatch(dispatch) {
                    warn!(
                        device_id = %device_id,
                        "OSC action dispatch failed: {:?}", e
                    );
                }
            }
        }

        // ── Route engine (Slice 1, #1361) — unchanged ────────────────────
        let destinations = {
            let route_engine = self.route_engine.load();
            route_engine.route_destinations(device_id.as_str(), event, mode.name.as_str())
        };
        if destinations.is_empty() {
            return;
        }
        trace!(
            device_id = %device_id,
            active_mode = %mode.name,
            count = destinations.len(),
            "OSC inbound routed (ADR-039-A Slice 1)"
        );
        self.dispatch_route_outputs(
            destinations,
            &[],
            device_id,
            &current_mode_name,
            &[],
            RouteDisposition::NoMatch,
        )
        .await;
    }
}
