// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

impl EngineManager {
    pub(crate) fn handle_list_devices(&mut self, id: String) -> IpcResponse {
        // Enumerate both MIDI and HID devices
        let mut response_data = json!({});
        let mut has_data = false;

        // Get MIDI devices
        match Self::enumerate_midi_devices() {
            Ok(midi_devices) => {
                response_data["midi_devices"] = json!(midi_devices);
                has_data = true;
            }
            Err(e) => {
                warn!("Failed to enumerate MIDI devices: {}", e);
            }
        }

        // Get HID/gamepad devices
        match HidDeviceManager::list_gamepads() {
            Ok(gamepads) => {
                let gamepad_list: Vec<serde_json::Value> = gamepads
                    .iter()
                    .enumerate()
                    .map(|(idx, (_id, name, uuid))| {
                        json!({
                            "index": idx,
                            "name": name,
                            "uuid": uuid,
                        })
                    })
                    .collect();
                response_data["hid_devices"] = json!(gamepad_list);
                has_data = true;
            }
            Err(e) => {
                warn!("Failed to enumerate HID devices: {}", e);
            }
        }

        if has_data {
            create_success_response(&id, Some(response_data))
        } else {
            IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InternalError.as_u16(),
                    message: "Failed to enumerate any input devices".to_string(),
                    details: None,
                }),
            }
        }
    }

    pub(crate) async fn handle_set_device(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        warn!(
            "IPC command SET_DEVICE is deprecated. Use SET_DEVICE_ENABLED or [[devices]] config instead."
        );
        // Extract port index from args
        match request.args.get("port").and_then(|v| v.as_u64()) {
            Some(port_index) => {
                let port_index = port_index as usize;
                info!("Device switch requested to port {}", port_index);

                // Attempt device switch
                match self.switch_device(port_index).await {
                    Ok((port_name, actual_port)) => create_success_response(
                        &id,
                        Some(json!({
                            "message": "Device switched successfully",
                            "port": actual_port,
                            "port_name": port_name,
                            "deprecated": "SET_DEVICE is deprecated. Use SET_DEVICE_ENABLED or [[devices]] config.",
                        })),
                    ),
                    Err(e) => IpcResponse {
                        id,
                        status: ResponseStatus::Error,
                        data: None,
                        error: Some(ErrorDetails {
                            code: IpcErrorCode::InternalError.as_u16(),
                            message: format!("Device switch failed: {}", e),
                            details: None,
                        }),
                    },
                }
            }
            None => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing 'port' parameter".to_string(),
                    details: Some(json!({"example": {"port": 0}})),
                }),
            },
        }
    }

    pub(crate) async fn handle_get_device(&mut self, id: String) -> IpcResponse {
        let device_status = self.device_status.read().await.clone();
        create_success_response(
            &id,
            Some(json!({
                "device": device_status
            })),
        )
    }

    pub(crate) async fn handle_set_device_enabled(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let device_id_str = request.args.get("device_id").and_then(|v| v.as_str());
        let enabled = request.args.get("enabled").and_then(|v| v.as_bool());

        match (device_id_str, enabled) {
            (Some(device_id), Some(enabled)) => {
                if let Err(e) = self
                    .command_tx
                    .send(DaemonCommand::SetDeviceEnabled {
                        device_id: device_id.to_string(),
                        enabled,
                    })
                    .await
                {
                    return IpcResponse {
                        id,
                        status: ResponseStatus::Error,
                        data: None,
                        error: Some(ErrorDetails {
                            code: IpcErrorCode::InternalError.as_u16(),
                            message: format!("Failed to send command: {}", e),
                            details: None,
                        }),
                    };
                }
                create_success_response(
                    &id,
                    Some(json!({
                        "device_id": device_id,
                        "enabled": enabled
                    })),
                )
            }
            _ => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing 'device_id' or 'enabled' argument".to_string(),
                    details: Some(json!({"example": {"device_id": "pads", "enabled": false}})),
                }),
            },
        }
    }

    pub(crate) fn handle_get_led_status(&mut self, id: String) -> IpcResponse {
        let led_config = self.live_config.load().config.led.clone();
        create_success_response(
            &id,
            led_config.map(|led| {
                json!({
                    "enabled": led.enabled,
                    "brightness": led.brightness,
                    "scheme": led.scheme,
                    "idle_timeout_secs": led.idle_timeout_secs,
                    "mode_colors": led.mode_colors,
                })
            }),
        )
    }

    pub(crate) async fn handle_set_led_scheme(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let Some(scheme_str) = request.args.get("scheme").and_then(|v| v.as_str()) else {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing required 'scheme' argument".to_string(),
                    details: None,
                }),
            };
        };
        if conductor_core::feedback::LightingScheme::parse(scheme_str).is_none() {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: format!(
                        "Unknown scheme '{}'. Valid: {}",
                        scheme_str,
                        conductor_core::feedback::LightingScheme::list_all().join(", ")
                    ),
                    details: None,
                }),
            };
        }
        // Normalize to lowercase for consistent config storage
        let scheme_normalized = scheme_str.to_lowercase();
        // Update config via LiveConfig (ADR-034 §D1 — sole
        // write seam since D4.A.3.2; legacy bridge retired in
        // D4.A.3.3.A).
        let scheme_for_closure = scheme_normalized.clone();
        self.live_config
            .mutate_replace_whole(self.default_cli_provenance(), move |cfg| {
                let led = cfg.led.get_or_insert_with(Default::default);
                led.scheme = scheme_for_closure;
            })
            .await
            .expect("D4.A.3.2: only engine_manager writes LiveConfig — no CAS contention");
        // TODO: Apply to FeedbackManager when hardware integration is added
        // Persist to disk
        if let Err(e) = self.persist_led_config().await {
            warn!("Failed to persist LED scheme change: {}", e);
        }
        info!("LED scheme set to '{}'", scheme_normalized);
        create_success_response(&id, Some(json!({ "scheme": scheme_normalized })))
    }

    pub(crate) async fn handle_set_led_brightness(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let Some(raw_brightness) = request.args.get("brightness").and_then(|v| v.as_u64()) else {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: "Missing required 'brightness' argument".to_string(),
                    details: None,
                }),
            };
        };
        if raw_brightness > 127 {
            return IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: format!("Brightness must be 0-127, got {}", raw_brightness),
                    details: None,
                }),
            };
        }
        let brightness = raw_brightness as u8;
        // D4.A.3.2 migration — same pattern as LED scheme above.
        self.live_config
            .mutate_replace_whole(self.default_cli_provenance(), move |cfg| {
                let led = cfg.led.get_or_insert_with(Default::default);
                led.brightness = brightness;
            })
            .await
            .expect("D4.A.3.2: only engine_manager writes LiveConfig — no CAS contention");
        // TODO: Apply to FeedbackManager when hardware integration is added
        // Persist to disk
        if let Err(e) = self.persist_led_config().await {
            warn!("Failed to persist LED brightness change: {}", e);
        }
        info!("LED brightness set to {}", brightness);
        create_success_response(&id, Some(json!({ "brightness": brightness })))
    }
}
