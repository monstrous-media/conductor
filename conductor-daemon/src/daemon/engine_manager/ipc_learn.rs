// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

impl EngineManager {
    pub(crate) fn handle_start_midi_learn(&mut self, id: String) -> IpcResponse {
        self.midi_learn_active.store(true, Ordering::SeqCst);
        // Use the dedicated Learn chord window
        // (`advanced_settings.chord_learn_timeout_ms`, default 150) — the single
        // source of truth the Settings panel displays — replacing the prior
        // `chord_timeout_ms × 3` (clamped) heuristic, so this IPC-Learn path, the
        // event-pump's new-processor path (`events_device.rs`), and the GUI label
        // all agree on one value.
        let learn_ms = self
            .live_config
            .load()
            .config
            .advanced_settings
            .chord_learn_timeout_ms;
        let learn_timeout = std::time::Duration::from_millis(learn_ms);
        for mut entry in self.event_processors.iter_mut() {
            entry.value_mut().set_chord_timeout(learn_timeout);
        }
        info!(
            "MIDI Learn mode started (chord timeout extended to {}ms)",
            learn_ms
        );
        create_success_response(
            &id,
            Some(json!({
                "learning": true
            })),
        )
    }

    pub(crate) async fn handle_stop_midi_learn(&mut self, id: String) -> IpcResponse {
        self.midi_learn_active.store(false, Ordering::SeqCst);
        // Restore chord timeout from config
        let default_ms = self
            .live_config
            .load()
            .config
            .advanced_settings
            .chord_timeout_ms;
        let default_timeout = std::time::Duration::from_millis(default_ms);
        for mut entry in self.event_processors.iter_mut() {
            entry.value_mut().set_chord_timeout(default_timeout);
        }
        // Flush any pending chord before clearing
        self.flush_pending_chord().await;
        // Clear the event buffer
        let mut events = self.midi_learn_events.lock().await;
        events.clear();
        info!("MIDI Learn mode stopped");
        create_success_response(
            &id,
            Some(json!({
                "learning": false
            })),
        )
    }

    pub(crate) async fn handle_get_midi_learn_events(&mut self, id: String) -> IpcResponse {
        // Flush any pending chord that has been waiting >150ms
        self.flush_pending_chord_if_stale().await;
        // Drain the event buffer and return events
        let mut events = self.midi_learn_events.lock().await;
        let result: Vec<MidiLearnEvent> = events.drain(..).collect();
        create_success_response(
            &id,
            Some(json!({
                "events": result
            })),
        )
    }

    pub(crate) fn handle_start_event_monitor(&mut self, id: String) -> IpcResponse {
        self.event_monitor_active.store(true, Ordering::SeqCst);
        // Clear buffer on start for fresh capture
        let mut buffer = self.event_monitor_buffer.lock().unwrap();
        buffer.clear();
        info!("Event monitor started");
        create_success_response(&id, Some(json!({ "monitoring": true })))
    }

    pub(crate) fn handle_stop_event_monitor(&mut self, id: String) -> IpcResponse {
        self.event_monitor_active.store(false, Ordering::SeqCst);
        info!("Event monitor stopped");
        create_success_response(&id, Some(json!({ "monitoring": false })))
    }

    pub(crate) fn handle_subscribe_events(&mut self, id: String) -> IpcResponse {
        create_success_response(&id, Some(json!({"subscribed": true})))
    }

    pub(crate) fn handle_get_monitor_events(&mut self, id: String) -> IpcResponse {
        let mut buffer = self.event_monitor_buffer.lock().unwrap();
        let result: Vec<MonitorEvent> = buffer.drain(..).collect();
        create_success_response(&id, Some(json!({ "events": result })))
    }

    pub(crate) async fn handle_simulate_mapping(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        use conductor_core::dispatch::SimulateOptions;

        let mode = request
            .args
            .get("mode_name")
            .and_then(|v| v.as_str())
            .map(String::from);
        let index = match request.args.get("mapping_index").and_then(|v| v.as_u64()) {
            Some(v) => match usize::try_from(v) {
                Ok(u) => Some(u),
                Err(_) => {
                    return IpcResponse {
                        id,
                        status: ResponseStatus::Error,
                        data: None,
                        error: Some(ErrorDetails {
                            code: IpcErrorCode::InvalidRequest.as_u16(),
                            message: format!("Invalid 'mapping_index': {} out of range", v),
                            details: None,
                        }),
                    };
                }
            },
            None => None,
        };
        let execute = request
            .args
            .get("execute")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let value = match request.args.get("value").and_then(|v| v.as_u64()) {
            Some(v) if v <= 127 => Some(v as u8),
            Some(v) => {
                return IpcResponse {
                    id,
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code: IpcErrorCode::InvalidRequest.as_u16(),
                        message: format!("Invalid 'value' argument: {} exceeds range 0-127", v),
                        details: Some(json!({
                            "received": v,
                            "allowed_range": [0, 127],
                        })),
                    }),
                };
            }
            None => None,
        };

        match (mode, index) {
            (Some(mode), Some(index)) => {
                let options = SimulateOptions {
                    mode,
                    index,
                    execute,
                    value,
                };
                match self.simulate_mapping(options).await {
                    Ok(result) => match serde_json::to_value(&result) {
                        Ok(data) => create_success_response(&id, Some(data)),
                        Err(e) => IpcResponse {
                            id,
                            status: ResponseStatus::Error,
                            data: None,
                            error: Some(ErrorDetails {
                                code: IpcErrorCode::InternalError.as_u16(),
                                message: format!("Failed to serialize SimulateResult: {}", e),
                                details: None,
                            }),
                        },
                    },
                    Err(e) => IpcResponse {
                        id,
                        status: ResponseStatus::Error,
                        data: None,
                        error: Some(ErrorDetails {
                            code: IpcErrorCode::InvalidRequest.as_u16(),
                            message: e.to_string(),
                            details: None,
                        }),
                    },
                }
            }
            _ => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing 'mode_name' or 'mapping_index' argument".to_string(),
                    details: Some(json!({
                        "example": {
                            "mode_name": "Mix",
                            "mapping_index": 0,
                            "execute": true,
                            "value": null
                        }
                    })),
                }),
            },
        }
    }
}
