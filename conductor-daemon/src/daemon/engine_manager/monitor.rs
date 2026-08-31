// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `EngineManager` methods extracted from `engine_manager::mod` (refactor #2073).

use super::*;
use crate::midi_bytes::extract_raw_midi;

/// Canonical MIDI wire bytes for a channel-voice event, but only when the
/// channel is known (#840). `extract_raw_midi` folds `channel: None` into
/// channel 0, which would misrepresent a channel-less event — the Channel row
/// shows "—" for those, so the Raw row stays hidden to match.
fn raw_bytes_if_channelled(channel: Option<u8>, event: &InputEvent) -> Option<Vec<u8>> {
    channel.and(extract_raw_midi(event))
}

impl EngineManager {
    /// Emit an action/status event to the event monitor buffer (R897-R898)
    pub(crate) fn emit_action_event(&self, event_type: &str, detail: Option<&str>) {
        if !self.event_monitor_active.load(Ordering::Relaxed) || !self.capture_actions {
            return;
        }
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let event = MonitorEvent {
            timestamp_ms,
            event_type: event_type.to_string(),
            detail: detail.map(|d| d.to_string()),
            ..Default::default()
        };
        self.push_monitor_event(event);
    }

    /// Push a MonitorEvent to the buffer (DRY helper for all emit methods)
    pub(crate) fn push_monitor_event(&self, mut event: MonitorEvent) {
        // Fixed-window rate limiting (R924): drop events exceeding max_events_per_second
        if let Some(ref limiter) = self.monitor_rate_limiter
            && !limiter.allow(event.timestamp_ms)
        {
            return;
        }

        // #2410: stamp a monotonic emission sequence so the GUI can reconstruct
        // true daemon order across both Tauri channels. `mapping_fired` is pushed
        // from a different run-loop select arm than the raw event, so push order
        // — not webview arrival order — is authoritative. Stamped after the
        // rate-limit drop so surviving events stay gap-free. One relaxed atomic.
        event.seq = self
            .event_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Latency tracking (R919: track_latency)
        if self.track_latency
            && event.processing_us.is_none()
            && let Ok(duration) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        {
            let now_us = duration.as_micros() as u64;
            let event_ts_us = event.timestamp_ms.saturating_mul(1000);
            event.processing_us = Some(now_us.saturating_sub(event_ts_us));
        }

        // Memory profiling (R920: track_memory)
        if self.track_memory
            && let Some(rss) = get_resident_memory_bytes()
        {
            event.memory_bytes = Some(rss);
        }

        // Check triggers (R915-R917)
        // Collect fired triggers under lock, then log outside to avoid
        // holding the mutex during I/O (council review: logging under lock).
        let fired_triggers = if let Ok(mut engine) = self.trigger_engine.lock().or_else(|p| {
            tracing::error!("Trigger engine mutex poisoned — recovering");
            Ok::<_, ()>(p.into_inner())
        }) {
            engine.check_event(&event.event_type)
        } else {
            Vec::new()
        };

        for fired in fired_triggers {
            use conductor_core::config::types::TriggerAction;
            match &fired.action {
                TriggerAction::Log { message } => {
                    tracing::warn!(
                        trigger = %fired.name,
                        count = fired.count,
                        "Event trigger: {}",
                        message
                    );
                }
                TriggerAction::Notification { message } => {
                    tracing::warn!(
                        trigger = %fired.name,
                        count = fired.count,
                        "Event trigger notification: {}",
                        message
                    );
                }
            }
        }

        if let Ok(mut buffer) = self.event_monitor_buffer.try_lock() {
            while buffer.len() >= self.event_monitor_max {
                buffer.pop_front();
            }
            buffer.push_back(event.clone());
        }

        // Push to broadcast channel for subscribers (#394)
        // send() fails silently if no receivers — zero cost when nobody is subscribed
        let _ = self.event_broadcast_tx.send(event);
    }

    /// Persist mode change to config file and update in-memory state (Phase 2 - Issue #321)
    ///
    /// Council review fix: Updates in-memory state FIRST for backwards compatibility,
    /// then persists to disk asynchronously via spawn_blocking, then does a targeted
    /// config update (not full overwrite) to avoid TOCTOU race conditions.
    ///
    /// # Arguments
    /// * `mode_name` - Name of the mode to switch to
    ///
    /// # Returns
    /// * `Ok(())` - Mode change persisted successfully (or in-memory only if disk fails)
    ///
    /// # Unknown modes
    ///
    /// Returns `Ok(())` for unknown mode names (with a warning log). This preserves
    /// backwards compatibility with gamepad chords that may reference modes from a
    /// different config version. Callers should not assume `Ok` means the mode was found.
    pub(crate) async fn persist_mode_change(&self, mode_name: String) -> Result<()> {
        // 1. Validate mode exists and get index
        let mode_index = self
            .live_config
            .load()
            .config
            .modes
            .iter()
            .position(|m| m.name == mode_name);
        let mode_index = match mode_index {
            Some(idx) => idx,
            None => {
                warn!("ModeChange requested unknown mode '{}'", mode_name);
                return Ok(());
            }
        };

        // 2. Update in-memory state FIRST (backwards compat: mode switches even if persist fails)
        self.current_mode.store(Arc::new(ModeState {
            index: mode_index,
            name: mode_name.clone(),
        }));

        // 3. Persist async via spawn_blocking (avoid blocking tokio runtime).
        //    Reload from disk to avoid overwriting concurrent changes — only patch last_selected_mode.
        let config_path = self.config_path.clone();
        let mode_name_for_save = mode_name.clone();

        // Suppress config watcher reload before writing
        {
            let mut guard = self.config_write_suppress.lock().await;
            *guard = Some(Instant::now());
        }

        let save_result = tokio::task::spawn_blocking(move || {
            let path_str = cfg_path_str(&config_path)?;
            let mut cfg = Config::load(path_str)
                .map_err(|e| DaemonError::Ipc(format!("Config reload for patch failed: {}", e)))?;
            cfg.last_selected_mode = Some(mode_name_for_save);
            cfg.save(path_str)
                .map_err(|e| DaemonError::Ipc(format!("Config save failed: {}", e)))
        })
        .await
        .map_err(|e| DaemonError::Ipc(e.to_string()))?;

        let persisted = if let Err(e) = save_result {
            warn!(
                "Failed to persist mode change (runtime state updated anyway): {}",
                e
            );
            false
        } else {
            // 4. Targeted update only — don't overwrite entire config with stale clone.
            //    D4.A.3.2 migration via LiveConfig.
            let mode_for_closure = mode_name.clone();
            self.live_config
                .mutate_replace_whole(self.default_cli_provenance(), move |cfg| {
                    cfg.last_selected_mode = Some(mode_for_closure);
                })
                .await
                .expect("D4.A.3.2: only engine_manager writes LiveConfig — no CAS contention");
            true
        };

        if persisted {
            info!(
                "Mode changed to '{}' (index {}) and persisted to config",
                mode_name, mode_index
            );
        } else {
            info!(
                "Mode changed to '{}' (index {}) in memory (disk persist failed)",
                mode_name, mode_index
            );
        }
        Ok(())
    }

    /// Persist LED config changes to disk (reload-from-disk-then-patch, consistent with persist_mode_change)
    pub(crate) async fn persist_led_config(&self) -> Result<()> {
        let led_config = self.live_config.load().config.led.clone();
        let config_path = self.config_path.clone();

        // Suppress config watcher reload before writing
        {
            let mut guard = self.config_write_suppress.lock().await;
            *guard = Some(Instant::now());
        }

        tokio::task::spawn_blocking(move || {
            let path_str = cfg_path_str(&config_path)?;
            let mut cfg = Config::load(path_str).map_err(|e| {
                DaemonError::Ipc(format!("Config reload for LED patch failed: {}", e))
            })?;
            cfg.led = led_config;
            cfg.save(path_str)
                .map_err(|e| DaemonError::Ipc(format!("Config save failed: {}", e)))
        })
        .await
        .map_err(|e| DaemonError::Ipc(e.to_string()))?
    }

    /// Create a MonitorEvent from an InputEvent (Issue #326)
    pub(crate) fn create_monitor_event(
        input_event: &InputEvent,
        device_id: Option<&str>,
    ) -> Option<MonitorEvent> {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let dev = device_id.map(|s| s.to_string());

        match input_event {
            InputEvent::PadPressed {
                pad,
                velocity,
                channel,
                ..
            } => {
                if *pad >= 128 {
                    // Gamepad path — semantic separation from MIDI; channel
                    // intentionally not propagated even if the InputEvent set one.
                    Some(MonitorEvent {
                        timestamp_ms,
                        event_type: "gamepad_button".to_string(),
                        device_id: dev,
                        button: Some(*pad),
                        velocity: Some(*velocity),
                        ..Default::default()
                    })
                } else {
                    Some(MonitorEvent {
                        timestamp_ms,
                        event_type: "note_on".to_string(),
                        device_id: dev,
                        channel: *channel,
                        note: Some(*pad),
                        velocity: Some(*velocity),
                        raw_bytes: raw_bytes_if_channelled(*channel, input_event),
                        ..Default::default()
                    })
                }
            }
            InputEvent::PadReleased { pad, channel, .. } => {
                if *pad >= 128 {
                    Some(MonitorEvent {
                        timestamp_ms,
                        event_type: "gamepad_button_release".to_string(),
                        device_id: dev,
                        button: Some(*pad),
                        ..Default::default()
                    })
                } else {
                    Some(MonitorEvent {
                        timestamp_ms,
                        event_type: "note_off".to_string(),
                        device_id: dev,
                        channel: *channel,
                        note: Some(*pad),
                        velocity: Some(0),
                        raw_bytes: raw_bytes_if_channelled(*channel, input_event),
                        ..Default::default()
                    })
                }
            }
            InputEvent::ControlChange {
                control,
                value,
                channel,
                ..
            } => Some(MonitorEvent {
                timestamp_ms,
                event_type: "cc".to_string(),
                device_id: dev,
                channel: *channel,
                cc: Some(*control),
                value: Some(*value as u16),
                raw_bytes: raw_bytes_if_channelled(*channel, input_event),
                ..Default::default()
            }),
            InputEvent::EncoderTurned {
                encoder,
                value,
                channel,
                analog,
                ..
            } => {
                if *encoder >= 132 {
                    Some(MonitorEvent {
                        timestamp_ms,
                        event_type: "gamepad_trigger".to_string(),
                        device_id: dev,
                        axis: Some(*encoder),
                        value: Some(*value as u16),
                        analog_value: *analog,
                        ..Default::default()
                    })
                } else if *encoder >= 128 {
                    Some(MonitorEvent {
                        timestamp_ms,
                        event_type: "gamepad_axis".to_string(),
                        device_id: dev,
                        axis: Some(*encoder),
                        value: Some(*value as u16),
                        analog_value: *analog,
                        ..Default::default()
                    })
                } else {
                    Some(MonitorEvent {
                        timestamp_ms,
                        event_type: "encoder".to_string(),
                        device_id: dev,
                        channel: *channel,
                        cc: Some(*encoder),
                        value: Some(*value as u16),
                        ..Default::default()
                    })
                }
            }
            InputEvent::PitchBend { value, channel, .. } => Some(MonitorEvent {
                timestamp_ms,
                event_type: "pitch_bend".to_string(),
                device_id: dev,
                channel: *channel,
                value: Some(*value),
                raw_bytes: raw_bytes_if_channelled(*channel, input_event),
                ..Default::default()
            }),
            InputEvent::Aftertouch {
                pressure, channel, ..
            } => Some(MonitorEvent {
                timestamp_ms,
                event_type: "aftertouch".to_string(),
                device_id: dev,
                channel: *channel,
                value: Some(*pressure as u16),
                raw_bytes: raw_bytes_if_channelled(*channel, input_event),
                ..Default::default()
            }),
            InputEvent::PolyPressure {
                pad,
                pressure,
                channel,
                ..
            } => Some(MonitorEvent {
                timestamp_ms,
                event_type: "poly_pressure".to_string(),
                device_id: dev,
                channel: *channel,
                note: Some(*pad),
                value: Some(*pressure as u16),
                raw_bytes: raw_bytes_if_channelled(*channel, input_event),
                ..Default::default()
            }),
            InputEvent::ProgramChange {
                program, channel, ..
            } => Some(MonitorEvent {
                timestamp_ms,
                event_type: "program_change".to_string(),
                device_id: dev,
                channel: *channel,
                // The GUI bridge reads the program number from `value` and the
                // existing ADR-025 `program_change` handling (events.rs).
                value: Some(*program as u16),
                raw_bytes: raw_bytes_if_channelled(*channel, input_event),
                ..Default::default()
            }),
        }
    }

    /// Rebuild the device_id → port_name cache from the current device statuses.
    /// Called during initial connect and hot-plug rescan.
    pub(crate) fn rebuild_port_name_cache(&self, statuses: &[DevicePortStatus]) {
        self.device_port_name_cache.clear();
        for dps in statuses {
            if dps.device_id != dps.port_name {
                self.device_port_name_cache
                    .insert(dps.device_id.clone(), dps.port_name.clone());
            }
        }
    }

    /// Record event into per-device fingerprint stats (ADR-022 D7).
    /// Called inline in the hot path (~25ns: DashMap shard lock + HashMap insert).
    /// Timestamp (`now_ms`) is computed once by the caller and shared across keys.
    /// Entries capped at MAX_FINGERPRINT_ENTRIES to prevent unbounded growth.
    const MAX_FINGERPRINT_ENTRIES: usize = 512;

    pub(crate) fn record_event_fingerprint(
        &self,
        device_key: &str,
        event: &InputEvent,
        now_ms: Option<u64>,
    ) {
        // Use get_mut (borrowed key) to avoid String allocation on every event.
        // Only allocate when inserting a new device for the first time.
        let mut stats = if let Some(existing) = self.event_stats.get_mut(device_key) {
            existing
        } else {
            // Cap total tracked entries (keys) to prevent unbounded growth.
            // Use trace! to avoid log spam — cap is generous (512) and only
            // reached with many transient ports.
            if self.event_stats.len() >= Self::MAX_FINGERPRINT_ENTRIES {
                trace!(
                    "Fingerprint entries cap reached; '{}' not tracked",
                    device_key
                );
                return;
            }
            self.event_stats.entry(device_key.to_string()).or_default()
        };
        match event {
            InputEvent::PadPressed { pad, velocity, .. } => {
                if *pad >= 128 {
                    stats.record_gamepad();
                } else {
                    stats.record_note(*pad, *velocity);
                }
            }
            InputEvent::ControlChange { control, value, .. } => {
                // #1451: forward the CC value so the fingerprinter can tell a
                // relative encoder from a repeatedly-moved absolute fader.
                stats.record_cc(*control, *value);
            }
            InputEvent::EncoderTurned { encoder, value, .. } => {
                if *encoder >= 128 {
                    stats.record_gamepad();
                } else {
                    stats.record_cc(*encoder, *value);
                }
            }
            _ => {} // PadReleased, Aftertouch, PitchBend, etc. don't help classification
        }
        // Update last event timestamp for health reporting (#747)
        if let Some(ts) = now_ms {
            stats.touch(ts);
        }
    }
}
