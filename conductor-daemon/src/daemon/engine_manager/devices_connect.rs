// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `EngineManager::connect_multi_device`, extracted from
//! `engine_manager::devices`.

use super::*;

impl EngineManager {
    /// Connect in multi-device mode (ADR-009 Phase 2)
    pub(crate) async fn connect_multi_device(&mut self, config: &Config) -> Result<()> {
        info!("Connecting in multi-device mode (ADR-009 Phase 2)");

        let input_mode = config.advanced_settings.input_mode;

        let stick_deadzone = config.advanced_settings.stick_deadzone;
        let trigger_deadzone = config.advanced_settings.trigger_deadzone;

        let mut manager =
            InputManager::with_deadzone(None, false, input_mode, stick_deadzone, trigger_deadzone);

        // Auto-exclude virtual output ports from input scanning (ADR-009 D21)
        {
            let executor = self.action_executor.lock().await;
            let virtual_names = executor.virtual_port_names();
            if !virtual_names.is_empty() {
                info!(
                    "Auto-excluding {} virtual output port(s) from input",
                    virtual_names.len()
                );
                manager.set_exclude_port_names(virtual_names);
            }
        }

        // Share MIDI Learn flag for Ambiguous port handling (ADR-009 D11)
        manager.set_midi_learn_flag(Arc::clone(&self.midi_learn_active));

        // Surface ambiguous-port resolver decisions to GUI / MCP / CLI
        // through the existing daemon event broadcast (in addition to
        // `tracing::warn!` which stays as the durable log record).
        manager.set_event_broadcast_tx(self.event_broadcast_tx.clone());

        // ADR-026 Phase 1.B: plumb the probe coordinator so every port
        // the InputManager opens can observe SysEx Identity Replies.
        manager.set_probe_coordinator(Arc::clone(&self.probe_coordinator));

        let bindings = manager
            .listen_to_all_ports(
                config,
                self.device_event_tx.clone(),
                self.command_tx.clone(),
            )
            .map_err(|e| DaemonError::Ipc(format!("Multi-device connection failed: {}", e)))?;

        // `multi_device_active` removed — every config now uses
        // the multi-device dispatcher unconditionally.

        // Build device port statuses from bindings
        let device_bindings = manager.get_device_bindings();

        // ADR-021 Phase 1B / ADR-035: enumerate output ports and
        // build the output map from the unified endpoint set. `build_output_map`
        // subsumes the legacy device + connector builders (including the
        // output/bidirectional alias resolution for `MidiForward`).
        let output_ports = crate::daemon::output_resolver::enumerate_output_ports_async().await;
        let input_bindings: Vec<(String, String)> = device_bindings
            .iter()
            .filter(|(_, _, _, is_configured)| *is_configured)
            .map(|(device_id, port_name, _, _)| (device_id.as_str().to_string(), port_name.clone()))
            .collect();
        let (endpoints, _findings) =
            conductor_core::config::loader::normalize_to_endpoints(config)?;
        let output_map = crate::daemon::output_resolver::build_output_map(
            &endpoints,
            &input_bindings,
            &output_ports,
        );

        // ADR-021 Phase 2A: Update shared device_output_map for ActionExecutor alias resolution
        let flat_map: HashMap<String, String> = output_map
            .iter()
            .map(|(alias, res)| (alias.clone(), res.port_name.clone()))
            .collect();
        self.device_output_map.store(Arc::new(flat_map));

        // Create the OS virtual MIDI ports declared by MidiVirtualPort
        // endpoints so routes resolve and external apps can see them.
        self.sync_virtual_ports(&endpoints);

        let device_statuses: Vec<DevicePortStatus> = device_bindings
            .iter()
            .map(|(device_id, port_name, connected, is_configured)| {
                let alias = device_id.as_str();
                let io = resolve_device_io(alias, *is_configured, &endpoints, &output_map);
                DevicePortStatus {
                    device_id: alias.to_string(),
                    port_name: port_name.clone(),
                    port_index: 0,
                    connected: *connected,
                    enabled: manager.is_device_enabled(device_id),
                    last_event_at: None,
                    is_configured: *is_configured,
                    direction: io.direction,
                    output_connected: io.output_connected,
                    output_port_name: io.output_port_name,
                    output_auto_paired: io.output_auto_paired,
                    // TODO: Currently MIDI-only. Derive from binding source
                    // when HID/OSC devices are added to the binding path.
                    protocol: "midi".to_string(),
                }
            })
            .collect();

        self.rebuild_port_name_cache(&device_statuses);

        // Update device status
        let any_connected = device_bindings.iter().any(|(_, _, c, _)| *c);
        let first_name = device_bindings.first().map(|(_, name, _, _)| name.clone());

        {
            let mut status = self.device_status.write().await;
            status.connected = any_connected;
            status.name = first_name;
            status.port = Some(0);
            status.devices = device_statuses;
            if any_connected {
                status.last_event_at = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                );
            }
        }

        *self.input_manager.lock().await = Some(manager);

        // ADR-026 Phase 3.C.2: fire probe-on-connect for every
        // freshly-bound configured port. Idempotent — first call
        // populates `last_known_configured_ports`, so subsequent
        // hot-plug ticks only probe newly-arrived ports.
        self.dispatch_probe_on_connect_for_new_ports(config).await;

        // Spawn the daemon-lifetime timer-tick (D12) + hot-plug (Phase 4)
        // background tasks exactly ONCE. `connect_multi_device` re-runs on every
        // MIDI `DeviceReconnected`; these tasks only poll `command_tx` for the
        // daemon's life (never tied to a connect session) and must survive
        // disconnect (the hot-plug loop is what detects the replug), so respawning
        // them here leaked a fresh tick+hot-plug pair per reconnect.
        self.spawn_background_tasks_once();

        info!(
            "Multi-device mode active: {} bindings resolved",
            bindings.len()
        );

        // ADR-042 Phase A: start network (OSC/Art-Net) listeners for any
        // loopback Input endpoints. A malformed network ACL fails startup here.
        self.start_network_listeners(config).await?;

        Ok(())
    }

    /// Spawn the daemon-lifetime timer-tick (D12, 50ms) and hot-plug
    /// (ADR-009 Phase 4, 5s) background tasks **exactly once** per `EngineManager`
    /// (one exists per daemon process).
    ///
    /// Both tasks only send on `command_tx` and break solely when its receiver
    /// is dropped (daemon shutdown) — they are NOT scoped to a connect session,
    /// and the hot-plug loop must keep running across disconnects so it can
    /// detect a replug (current-system-spec: "zero ports → idle so later
    /// hot-plug rescans still fire"). ADR-009 D12 likewise specifies a single
    /// `timer_tick_loop` per `EngineManager`. Guarded by `background_tasks_spawned`
    /// so re-entry from `DeviceReconnected` (which re-runs `connect_multi_device`)
    /// can't leak a duplicate pair.
    ///
    /// Returns `true` if this call spawned the tasks, `false` if they were
    /// already running.
    pub(crate) fn spawn_background_tasks_once(&self) -> bool {
        if self
            .background_tasks_spawned
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return false; // already spawned on an earlier connect — do not duplicate
        }

        // Timer tick task (D12) — 50ms interval for hold detection.
        let tick_command_tx = self.command_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
            loop {
                interval.tick().await;
                if tick_command_tx
                    .send(DaemonCommand::TimerTick)
                    .await
                    .is_err()
                {
                    break; // Channel closed, daemon shutting down
                }
            }
        });

        // Hot-plug detection loop (ADR-009 Phase 4) — 5s interval.
        let hotplug_command_tx = self.command_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            // Skip the first immediate tick (ports were just opened)
            interval.tick().await;
            loop {
                interval.tick().await;
                if hotplug_command_tx
                    .send(DaemonCommand::HotPlugCheck)
                    .await
                    .is_err()
                {
                    break; // Channel closed, daemon shutting down
                }
            }
        });

        true
    }
}
