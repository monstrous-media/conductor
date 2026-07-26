// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! `EngineManager::dispatch_route_outputs`, extracted from
//! `engine_manager::events` (refactor #2073).

use super::*;

impl EngineManager {
    /// Dispatch a batch of route outputs (ADR-031 § 4.4 / ADR-036 D4).
    /// Applies MIDI / OSC / Art-Net handling, executor wiring, and
    /// `route_forwarded` monitor emission for post-mapping routes. `raw` is
    /// the original input event (used only for the `bytes_in` metric).
    pub(crate) async fn dispatch_route_outputs(
        &self,
        destinations: Vec<crate::route_engine::RouteOutput>,
        raw: &[u8],
        device_id: &DeviceId,
        current_mode_name: &Option<String>,
        processed_events: &[conductor_core::event_processor::ProcessedEvent],
        // ADR-038 §2: how the event reached the route stage. Supplies the
        // trace's matched_mapping + let_through so a let-through route entry is
        // distinguishable from a no-match one.
        disposition: RouteDisposition,
    ) {
        // ADR-036 §8 / Slice 9: record this dispatch decision in the
        // bounded trace ring (read by `conductor_get_dispatch_trace`).
        // Only non-empty dispatches are recorded — the vast majority of
        // events never route anywhere, so recording empties would flood
        // the ring with noise and evict the useful entries.
        if !destinations.is_empty() {
            let (matched_mapping, let_through) = disposition.trace_fields();
            let entry = crate::daemon::dispatch_trace::DispatchTraceEntry {
                timestamp_ms: crate::daemon::dispatch_trace::now_ms(),
                device_id: device_id.as_str().to_string(),
                active_mode: current_mode_name.clone().unwrap_or_default(),
                event: crate::daemon::dispatch_trace::summarize_midi(raw),
                destinations: destinations.iter().map(|o| o.to_alias.clone()).collect(),
                matched_mapping,
                let_through,
            };
            // ADR-037 D4: when CONDUCTOR_TRACE_LOG=1 (or CONDUCTOR_TRACE=1),
            // also emit this entry as a structured-JSON line to stderr.
            // No-op (single atomic load) when the env gate is unset.
            crate::daemon::dispatch_trace::emit_stderr_trace(&entry);
            self.dispatch_trace.record(entry);
        }

        for output in destinations {
            let crate::route_engine::RouteOutput {
                to_alias,
                bytes,
                kind,
            } = output;
            let bytes_in = raw.len();
            let bytes_out = bytes.len();

            // Branch on output protocol — slices 1-4 of P5.
            //
            // **MIDI** (the original slice-1-of-#1301 path): dispatch
            // via the action executor's `Action::MidiForward`. The
            // executor owns port resolution + recursion-guard
            // registration. `try_dispatch` returns synchronously
            // on queue-accept; actual MIDI write happens in the
            // executor thread.
            //
            // **OSC** (slice 3 of P5): bytes are already an encoded
            // OSC packet (slice 1's `midi_to_osc::apply`). Send
            // directly through `ConnectorRegistry::send_osc` —
            // no action executor, no recursion guard (OSC is
            // outbound-only today; OscToMidi input is slice 8).
            //
            // Both branches converge on the same metric/monitor
            // emission below (`route_forwarded` event with the
            // enriched payload). Differences:
            // - `protocol` field added to payload so the GUI badge
            //   can render protocol-specific styling
            // - OSC's `bound_port` is always null (OSC connectors
            //   don't have a `bound_port`; the destination is a
            //   socket address, not a hot-plug port)
            let (dispatched, bound_port_name) = match kind {
                crate::route_engine::RouteOutputKind::Midi => {
                    let invocation_id = self.action_dispatcher.next_invocation_id();
                    let dispatch = crate::daemon::executor_thread::ActionDispatch {
                        invocation_id,
                        action: conductor_core::actions::Action::MidiForward {
                            target: to_alias.clone(),
                            transform: None,
                        },
                        context: Some(TriggerContext {
                            velocity: None,
                            current_mode: current_mode_name.clone(),
                            raw_midi: Some(bytes),
                            device_id: Some(device_id.as_str().to_string()),
                            input_event: None,
                            osc_message: None,
                        }),
                        provenance: crate::daemon::executor_thread::ActionProvenance {
                            device_id: Some(device_id.as_str().to_string()),
                            matched_rule: None,
                            mode_name: current_mode_name.clone(),
                            action_type: "MidiForward".to_string(),
                            action_summary: format!("[route] -> {}", to_alias),
                            trigger_info: Self::extract_trigger_info(processed_events, device_id),
                            mapping_label: Some(format!("route:{}->{}", device_id, to_alias)),
                            let_through: false,
                        },
                        dispatch_time: Instant::now(),
                        // ADR-042 D17: MIDI/HID dispatch path — never network-tainted.
                        network_origin: None,
                    };

                    if self.action_dispatcher.try_dispatch(dispatch).is_err() {
                        warn!(
                            device_id = %device_id,
                            to_alias = %to_alias,
                            "Action executor queue full, dropping routed forward"
                        );
                        let mut registry = self.connector_registry.write().await;
                        registry.record_error(&to_alias);
                        continue;
                    }

                    let bound = {
                        let mut registry = self.connector_registry.write().await;
                        registry.record_activity(&to_alias);
                        registry.resolve_output(&to_alias).map(str::to_string)
                    };
                    (true, bound)
                }
                crate::route_engine::RouteOutputKind::Osc => {
                    // P5 slice 3: synchronous OSC send via the
                    // registry's lazy-init UDP socket. `send_osc`
                    // returns Err on bind failure, unknown alias,
                    // or send_to syscall failure — record_error
                    // and skip the monitor emit so the user sees
                    // a non-zero error_count via P4's metrics tool.
                    let send_result = {
                        let mut registry = self.connector_registry.write().await;
                        registry.send_osc(&to_alias, &bytes)
                    };
                    match send_result {
                        Ok(()) => {
                            let mut registry = self.connector_registry.write().await;
                            registry.record_activity(&to_alias);
                            // OSC connectors have no `bound_port`
                            // (destination is host:port, not a
                            // hot-plug port name). Always None.
                            (true, None)
                        }
                        Err(e) => {
                            warn!(
                                device_id = %device_id,
                                to_alias = %to_alias,
                                error = %e,
                                "OSC send failed for routed forward"
                            );
                            let mut registry = self.connector_registry.write().await;
                            registry.record_error(&to_alias);
                            continue;
                        }
                    }
                }
                crate::route_engine::RouteOutputKind::ArtNet => {
                    // P5 slice 8: Art-Net dispatch. `bytes` is the
                    // 3-byte serialized `DmxUpdate` produced by
                    // `route_destinations`: [chan_high, chan_low,
                    // value], channel as BE u16. The registry's
                    // `send_artnet` writes into the persistent
                    // 512-channel frame and emits one OpDmx UDP
                    // packet per call.
                    //
                    // The 3-byte length is a `route_engine` invariant
                    // — if `bytes.len() != 3` here, the producer
                    // changed shape without updating the consumer.
                    // Drop the route's contribution (record_error,
                    // skip monitor emit, continue) rather than panic
                    // on the hot path.
                    if bytes.len() != 3 {
                        warn!(
                            device_id = %device_id,
                            to_alias = %to_alias,
                            got_bytes = bytes.len(),
                            "Art-Net route output is not 3 bytes — \
                             route_destinations producer/consumer skew; dropping"
                        );
                        let mut registry = self.connector_registry.write().await;
                        registry.record_error(&to_alias);
                        continue;
                    }
                    let update = crate::transforms::midi_to_artnet::DmxUpdate {
                        channel: u16::from_be_bytes([bytes[0], bytes[1]]),
                        value: bytes[2],
                    };
                    let send_result = {
                        let mut registry = self.connector_registry.write().await;
                        registry.send_artnet(&to_alias, update)
                    };
                    match send_result {
                        Ok(()) => {
                            let mut registry = self.connector_registry.write().await;
                            registry.record_activity(&to_alias);
                            // Art-Net connectors, like OSC, have no
                            // hot-plug `bound_port` — destination
                            // is host:port.
                            (true, None)
                        }
                        Err(e) => {
                            warn!(
                                device_id = %device_id,
                                to_alias = %to_alias,
                                error = %e,
                                "Art-Net send failed for routed forward"
                            );
                            let mut registry = self.connector_registry.write().await;
                            registry.record_error(&to_alias);
                            continue;
                        }
                    }
                }
            };

            debug_assert!(
                dispatched,
                "every continue above must skip the monitor emit; \
                     if we reach this point, the dispatch was accepted"
            );

            // `route_forwarded` monitor event — slice 3b's
            // EventStreamPanel badge consumes the payload. New
            // `protocol` field (this slice) lets the badge
            // render protocol-specific styling.
            if self.event_monitor_active.load(Ordering::Relaxed) {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let protocol = match kind {
                    crate::route_engine::RouteOutputKind::Midi => "Midi",
                    crate::route_engine::RouteOutputKind::Osc => "Osc",
                    crate::route_engine::RouteOutputKind::ArtNet => "ArtNet",
                };
                self.push_monitor_event(MonitorEvent {
                    timestamp_ms: timestamp,
                    event_type: "route_forwarded".to_string(),
                    device_id: Some(device_id.as_str().to_string()),
                    detail: Some(format!(
                        "[route/{}] {} -> {} ({} -> {} bytes)",
                        protocol, device_id, to_alias, bytes_in, bytes_out
                    )),
                    payload: Some(serde_json::json!({
                        "from": device_id.as_str(),
                        "to": to_alias,
                        "bytes_in": bytes_in,
                        "bytes_out": bytes_out,
                        "bound_port": bound_port_name,
                        "protocol": protocol,
                    })),
                    ..Default::default()
                });
            }
        }
    }
}
