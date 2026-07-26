// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Bring-up paths for [`super::InputManager`].
//!
//! Holds the legacy single-device `connect()` path, the multi-device
//! `listen_to_all_ports` path with its gamepad helpers, and the
//! mode-based `classify_gamepad_connect_result` decision used to share
//! gamepad-failure semantics across them.

use super::{InputManager, InputMode};
use crate::daemon::DaemonCommand;
use crate::input_source::{InputSource, InputSourceMetricsHandle, spawn_input_pump};
use conductor_core::config::types::{ConnectorDirection, ConnectorProtocol, EndpointConfig};
use conductor_core::event_processor::MidiEvent;
use conductor_core::events::{InputEvent, ProtocolEvent};
use conductor_core::identity::{DeviceEvent, DeviceId};
use conductor_core::resolver::{BindingResult, PortInfo, PortResolver};
use conductor_core::{Config, ListenMode};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

impl InputManager {
    /// Connect to input devices based on mode (legacy single-device path)
    ///
    /// For multi-device mode, use `listen_to_all_ports()` instead.
    pub fn connect(
        &mut self,
        event_tx: mpsc::Sender<InputEvent>,
        command_tx: mpsc::Sender<DaemonCommand>,
    ) -> Result<String, String> {
        let mut status_messages = Vec::new();

        // Connect MIDI device (with MidiEvent → InputEvent conversion)
        if let Some(ref mut midi_mgr) = self.midi_manager {
            // Create intermediate channel for MIDI events
            let (midi_event_tx, mut midi_event_rx) = mpsc::channel::<MidiEvent>(1024);

            match midi_mgr.connect(midi_event_tx, command_tx.clone()) {
                Ok((port_idx, port_name)) => {
                    info!(
                        device_type = "MIDI",
                        port = %port_name,
                        "Connected to MIDI device (port {})",
                        port_idx
                    );
                    status_messages.push(format!("MIDI: {} (port {})", port_name, port_idx));

                    // Spawn converter task: MidiEvent → InputEvent
                    let event_tx_clone = event_tx.clone();
                    tokio::spawn(async move {
                        while let Some(midi_event) = midi_event_rx.recv().await {
                            let input_event: InputEvent = midi_event.into();
                            if let Err(e) = event_tx_clone.send(input_event).await {
                                warn!(error = %e, "Failed to send converted InputEvent");
                                break;
                            }
                        }
                        debug!("MIDI-to-Input converter task exited");
                    });
                }
                Err(e) => {
                    if self.mode == InputMode::MidiOnly {
                        return Err(format!("Failed to connect MIDI device: {}", e));
                    }
                    warn!(error = %e, "Failed to connect MIDI device (continuing with gamepad)");
                }
            }
        }

        // Connect gamepad device (native InputEvent). This legacy single-device
        // path feeds the raw `InputEvent` channel directly (no unified pump), so
        // it drives the wrapped manager via `manager_mut()` rather than the
        // `InputSource::start` cutover used by `connect_gamepad_multi_device`.
        if let Some(ref mut gamepad_mgr) = self.gamepad_manager {
            match gamepad_mgr
                .manager_mut()
                .connect(event_tx.clone(), command_tx.clone())
            {
                Ok((gamepad_id, gamepad_name)) => {
                    info!(
                        device_type = "Gamepad",
                        name = %gamepad_name,
                        "Connected to gamepad (ID {:?})",
                        gamepad_id
                    );
                    status_messages
                        .push(format!("Gamepad: {} (ID {:?})", gamepad_name, gamepad_id));
                }
                Err(e) => {
                    if self.mode == InputMode::GamepadOnly {
                        return Err(format!("Failed to connect gamepad: {}", e));
                    }
                    warn!(error = %e, "Failed to connect gamepad (continuing with MIDI)");
                }
            }
        }

        if status_messages.is_empty() {
            return Err("No input devices could be connected".to_string());
        }

        Ok(status_messages.join(" | "))
    }

    /// Open all MIDI ports simultaneously for multi-device operation (v4.20.0 - ADR-009 Phase 2)
    ///
    /// 1. Enumerates available MIDI ports
    /// 2. Filters by `ignore_ports` (D4)
    /// 3. Caps at `max_midi_ports` (D1)
    /// 4. Resolves ports via `PortResolver` against `config.devices` identities
    /// 5. For each port: creates `MidiDeviceManager`, connects with DeviceId-tagged channel
    /// 6. Logs warnings for failed ports (D10 partial success)
    ///
    /// Gamepad connect is attempted alongside MIDI bring-up when `mode`
    /// is `Both` or `GamepadOnly`:
    ///
    /// - `Both`: best-effort — gamepad failures are logged and swallowed
    ///   so a missing controller doesn't bring down a working MIDI setup.
    /// - `GamepadOnly`: required — `listen_to_all_ports` early-returns
    ///   without enumerating MIDI ports (the user's mode choice excludes
    ///   them). Gamepad-connect failures propagate as `Err` so the
    ///   daemon's bring-up surfaces a user-visible error rather than
    ///   coming up "successfully" with zero input devices (#974).
    /// - `MidiOnly`: gamepad connect is skipped entirely.
    ///
    /// Mode-based propagation lives in
    /// [`classify_gamepad_connect_result`].
    pub fn listen_to_all_ports(
        &mut self,
        config: &Config,
        event_tx: mpsc::Sender<DeviceEvent<ProtocolEvent>>,
        command_tx: mpsc::Sender<DaemonCommand>,
    ) -> Result<Vec<BindingResult>, String> {
        let advanced = &config.advanced_settings;

        // ADR-039-B #1762 (step 4c): resolve the HID input endpoint alias (if
        // any) up-front so BOTH the GamepadOnly early-return below and the
        // normal MIDI+gamepad bring-up tag the gamepad's events with it.
        // `connect_gamepad_multi_device` falls back to "gamepad" when None.
        self.hid_input_alias = resolve_hid_input_alias(&config.endpoints);
        // ADR-047 §D2: also resolve any ControllerGuid matcher(s) on the HID
        // input endpoint so connect prefers that controller over first-available.
        self.hid_preferred_guids = resolve_hid_preferred_guids(&config.endpoints);

        // #974: GamepadOnly mode skips MIDI bring-up entirely. Opening
        // MIDI ports would contradict the user's mode choice. The
        // gamepad-connect attempt itself is required (not best-effort)
        // — `try_connect_gamepad` propagates Err for GamepadOnly so a
        // missing controller surfaces as a clean startup failure rather
        // than the daemon coming up "successfully" with zero devices.
        // Bonus: avoids `midir::MidiInput::new()` on this path entirely,
        // which is desirable on Linux without ALSA.
        if self.mode == InputMode::GamepadOnly {
            return self
                .enter_multi_device_idle_state(
                    &event_tx,
                    &command_tx,
                    "GamepadOnly mode skips MIDI bring-up",
                )
                .map(|()| Vec::new());
        }

        // Step 1: Enumerate ports
        let midi_in = midir::MidiInput::new("Conductor Multi-Device Enum")
            .map_err(|e| format!("Failed to create MIDI input for enumeration: {}", e))?;
        let ports = midi_in.ports();

        if ports.is_empty() {
            // No MIDI ports at all — enter idle multi-device state so
            // hot-plug rescans can detect devices connected later, and
            // best-effort gamepad connect (#969). For GamepadOnly mode
            // we already early-returned above.
            self.enter_multi_device_idle_state(&event_tx, &command_tx, "no MIDI ports enumerated")?;
            if self.mode == InputMode::MidiOnly {
                return Err("No MIDI input ports available".to_string());
            }
            return Ok(Vec::new());
        }

        // Collect port info
        let mut port_infos: Vec<PortInfo> = Vec::new();
        for (index, port) in ports.iter().enumerate() {
            let name = midi_in
                .port_name(port)
                .unwrap_or_else(|_| format!("Port {}", index));
            port_infos.push(PortInfo::new(name, index));
        }

        // Resolve the unified endpoint set up-front (ADR-035 Slice 9.5).
        // `normalize_to_endpoints` folds `[[bindings]]` + `[[connectors]]` +
        // authored `[[endpoints]]` into one set, so an authored input endpoint
        // binds here exactly like a legacy binding. Needed BEFORE filtering so
        // Conductor's own MidiVirtualPort outputs are excluded from input
        // scanning (#2054/#2216). Guaranteed-Ok post-load; `?` defensive.
        let endpoints = conductor_core::config::loader::normalize_to_endpoints(config)
            .map_err(|e| format!("normalize endpoints for resolve: {e}"))?
            .0;

        // Step 2-3: Filter by ignore_ports (D4), virtual output ports (D21),
        // and cap at max_midi_ports (D1).
        let ignore =
            super::build_input_ignore(&advanced.ignore_ports, &self.exclude_port_names, &endpoints);
        let capped = super::filter_ports(port_infos, &ignore, advanced.max_midi_ports);

        if capped.is_empty() {
            // All ports filtered out — same idle-state semantics as
            // ports.is_empty() above. Sharing the helper means future
            // changes can't accidentally set the flag at one site and
            // miss the other (#969 review).
            self.enter_multi_device_idle_state(
                &event_tx,
                &command_tx,
                "all MIDI ports filtered out",
            )?;
            return Ok(Vec::new());
        }

        // Step 4: Resolve ports against the unified endpoint set.
        let bindings = PortResolver::resolve(&capped, &endpoints);

        // v4.26.0 (D19): Track which DeviceIds are from configured identities
        self.configured_devices.clear();
        for binding in &bindings {
            if let BindingResult::Bound { device_id, .. } = binding {
                self.configured_devices.insert(device_id.clone());
            }
        }

        // Step 5: Apply listen_mode filter with instance disambiguation for duplicate port names
        let mut unbound_instance_counts: HashMap<String, usize> = HashMap::new();
        let ports_to_open: Vec<(&PortInfo, DeviceId)> = capped
            .iter()
            .zip(bindings.iter())
            .filter_map(|(port, binding)| {
                match binding {
                    BindingResult::Bound { device_id, .. } => {
                        Some((port, device_id.clone()))
                    }
                    BindingResult::Unbound { port_name, .. } => {
                        match advanced.listen_mode {
                            ListenMode::All => {
                                // Use instance counting to avoid DeviceId collision
                                // when multiple devices share the same port name
                                let instance = unbound_instance_counts
                                    .entry(port_name.clone())
                                    .or_insert(0);
                                let device_id = DeviceId::from_port_instance(port_name, *instance);
                                *instance += 1;
                                Some((port, device_id))
                            }
                            ListenMode::Configured => {
                                info!(port = %port_name, "Skipping unconfigured port (listen_mode=Configured)");
                                None
                            }
                        }
                    }
                    BindingResult::Ambiguous { port_name, claimed_by, .. } => {
                        // v4.26.0 (D11): Open Ambiguous ports during MIDI Learn for device discovery
                        let learn_active = self.midi_learn_active.as_ref()
                            .is_some_and(|flag| flag.load(Ordering::SeqCst));
                        if learn_active {
                            info!(
                                port = %port_name,
                                "Opening ambiguous port for MIDI Learn discovery"
                            );
                            let device_id = DeviceId::raw(port_name);
                            Some((port, device_id))
                        } else {
                            warn!(
                                port = %port_name,
                                claimed_by = %claimed_by,
                                "Port ambiguous (identity already claimed), skipping"
                            );
                            // #943: surface this beyond the log so the GUI / MCP /
                            // CLI can show the user a "refine matcher" hint
                            // instead of silently dropping the port.
                            self.emit_ambiguous_port_event(
                                port_name,
                                port.index,
                                claimed_by.as_str(),
                            );
                            None
                        }
                    }
                }
            })
            .collect();

        // Step 6: Open each port
        // We need a fresh MidiInput per connection (midir consumes it)
        let mut opened_count = 0;
        for (port_info, device_id) in &ports_to_open {
            match self.open_port_with_device_id(
                port_info.index,
                &port_info.name,
                device_id.clone(),
                event_tx.clone(),
                command_tx.clone(),
            ) {
                Ok(()) => {
                    opened_count += 1;
                    info!(
                        device_id = %device_id,
                        port = %port_info.name,
                        port_index = port_info.index,
                        "Opened MIDI port for multi-device"
                    );
                }
                Err(e) => {
                    // D6/D10: Log and continue on failure
                    warn!(
                        port = %port_info.name,
                        port_index = port_info.index,
                        error = %e,
                        "Failed to open MIDI port (continuing with remaining ports)"
                    );
                }
            }
        }

        if opened_count == 0 && (self.mode == InputMode::MidiOnly) {
            return Err("Failed to open any MIDI ports".to_string());
        }

        self.multi_device_active = true;

        // Connect gamepad if mode includes it (#969 / #974).
        // GamepadOnly mode would have early-returned above, so reaching
        // here means mode is `Both` and try_connect_gamepad uses
        // best-effort semantics (failures logged, not propagated).
        self.try_connect_gamepad(&event_tx, &command_tx, "MIDI ports opened")?;

        info!(
            "Multi-device mode active: {} MIDI ports opened, {} endpoints configured",
            opened_count,
            config.endpoints.len()
        );

        Ok(bindings)
    }

    /// Mark multi-device active and best-effort connect any gamepad.
    /// Used by both empty-ports paths in `listen_to_all_ports` (Sites 1
    /// and 2) and the GamepadOnly early-return, where there's no MIDI
    /// port to listen on but we still want the manager "active" so
    /// hot-plug rescans detect later device connections (#969 review
    /// 3167308646).
    ///
    /// Setting the flag and attempting gamepad-connect together via
    /// this helper makes the pair a single atomic transition — it's
    /// impossible to add a future call site that sets one without the
    /// other.
    ///
    /// Returns `Err` only when `try_connect_gamepad` does — i.e. mode
    /// is `GamepadOnly` and gamepad-connect failed (#974).
    pub(crate) fn enter_multi_device_idle_state(
        &mut self,
        event_tx: &mpsc::Sender<DeviceEvent<ProtocolEvent>>,
        command_tx: &mpsc::Sender<DaemonCommand>,
        context: &str,
    ) -> Result<(), String> {
        self.multi_device_active = true;
        self.try_connect_gamepad(event_tx, command_tx, context)
    }

    /// Attempt gamepad connect with mode-aware error semantics.
    ///
    /// - `MidiOnly`: no-op, returns Ok.
    /// - `Both`: best-effort — connect failures are logged + swallowed,
    ///   so a missing gamepad doesn't bring down a working MIDI setup.
    /// - `GamepadOnly`: required — connect failure propagates as Err
    ///   so the daemon's bring-up fails with a user-visible error
    ///   instead of starting "successfully" with zero input devices
    ///   (#974).
    ///
    /// The mode-based decision is delegated to the pure
    /// `classify_gamepad_connect_result` so it can be exercised
    /// exhaustively by unit tests without touching gilrs / requiring a
    /// platform-specific runtime.
    pub(crate) fn try_connect_gamepad(
        &mut self,
        event_tx: &mpsc::Sender<DeviceEvent<ProtocolEvent>>,
        command_tx: &mpsc::Sender<DaemonCommand>,
        context: &str,
    ) -> Result<(), String> {
        if !matches!(self.mode, InputMode::Both | InputMode::GamepadOnly) {
            return Ok(());
        }
        let connect_result = self.connect_gamepad_multi_device(event_tx, command_tx);
        classify_gamepad_connect_result(self.mode, connect_result, context)
    }

    /// Connect gamepad in multi-device mode (wraps events in DeviceEvent)
    fn connect_gamepad_multi_device(
        &mut self,
        event_tx: &mpsc::Sender<DeviceEvent<ProtocolEvent>>,
        command_tx: &mpsc::Sender<DaemonCommand>,
    ) -> Result<(), String> {
        // ADR-039-B #1762 (step 4c): tag the gamepad's events with the
        // configured `[[endpoints]]` Input/Hid alias (the route `from` key), or
        // the historical "gamepad" DeviceId when no HID input endpoint is
        // declared. Resolved before the mutable borrow below.
        let device_id = self
            .hid_input_alias
            .as_deref()
            .map(DeviceId::raw)
            .unwrap_or_else(|| DeviceId::raw("gamepad"));

        if let Some(ref mut gamepad_src) = self.gamepad_manager {
            // ADR-039-B #1762 (step 4c): drive the HID `InputSource` substrate
            // (#1758/#1760) — `connect` stages the intermediate `InputEvent`
            // channel; `start(tx)` spawns the §4.3 shed-load pump (try_send +
            // drop-newest) onto the unified `DeviceEvent<ProtocolEvent>` channel.
            // Behaviour-identical to the prior inline pump; the source now owns
            // the wiring so every substrate line has a live consumer.
            gamepad_src.set_device_id(device_id);
            // ADR-047 §D2: prefer a connected controller whose GUID matches the
            // configured `ControllerGuid` endpoint over first-available.
            gamepad_src
                .manager_mut()
                .set_preferred_guids(self.hid_preferred_guids.clone());
            let (gamepad_id, gamepad_name) = gamepad_src.connect(command_tx.clone())?;

            info!(
                device_type = "Gamepad",
                name = %gamepad_name,
                alias = %gamepad_src.device_id(),
                "Connected to gamepad (ID {:?}) in multi-device mode",
                gamepad_id
            );

            // Retain the source's metrics handle so `input_source_metrics()`
            // surfaces this source — `start()` increments the same handle.
            self.gamepad_source_metrics = Some(gamepad_src.metrics_handle());
            gamepad_src.start(event_tx.clone())?;

            Ok(())
        } else {
            Ok(()) // No gamepad manager, nothing to do
        }
    }

    /// ADR-039-B #2293: gamepad hot-plug rescan. Connects a gamepad that was
    /// switched on / paired AFTER daemon start, mirroring MIDI's 5s hot-plug
    /// rescan (the gamepad was previously only connected at startup, so a
    /// controller powered on later was never picked up).
    ///
    /// Returns `true` iff a gamepad newly connected on this call.
    ///
    /// No-ops (returns `false`) when a gamepad is already connected or the mode
    /// excludes gamepads — see [`should_rescan_gamepad`]. The actual connect is
    /// best-effort regardless of mode (unlike startup's `GamepadOnly`-fatal
    /// `try_connect_gamepad`): a periodic rescan must never bring the daemon
    /// down. **Caller contract:** because `connect_gamepad_multi_device` uses
    /// the full `GAMEPAD_DISCOVERY_WINDOW_MS` window, the caller should
    /// presence-check first (a cheap off-lock `HidDeviceManager::list_gamepads`)
    /// and only invoke this when a controller is actually present, so the
    /// discovery window returns immediately instead of blocking the hot-plug
    /// loop while holding the input-manager lock.
    pub(crate) fn rescan_gamepad(
        &mut self,
        event_tx: &mpsc::Sender<DeviceEvent<ProtocolEvent>>,
        command_tx: &mpsc::Sender<DaemonCommand>,
    ) -> bool {
        if !should_rescan_gamepad(self.mode, self.get_status().1) {
            return false;
        }
        match self.connect_gamepad_multi_device(event_tx, command_tx) {
            Ok(()) => {
                let connected = self.get_status().1;
                if connected {
                    info!("Hot-plug: gamepad connected via rescan (#2293)");
                }
                connected
            }
            Err(e) => {
                // Best-effort: any error — no controller present, OR a gilrs
                // init / connect / start failure — just means "didn't connect
                // this tick"; the next hot-plug interval retries. Don't presume
                // the cause in the message (the real error is in `error`).
                debug!(error = %e, "Hot-plug gamepad rescan: connect attempt did not succeed");
                false
            }
        }
    }

    /// ADR-039-B #2293: whether the hot-plug loop should run a gamepad rescan
    /// this tick (gamepad-capable mode + no gamepad currently connected). Lets
    /// the caller skip the off-lock presence probe entirely in the common
    /// `MidiOnly` / already-connected cases — see [`should_rescan_gamepad`].
    pub(crate) fn needs_gamepad_rescan(&self) -> bool {
        should_rescan_gamepad(self.mode, self.get_status().1)
    }

    /// Reconnect the MIDI device to a specific port by index (legacy single-device).
    ///
    /// #885: emits `DeviceEvent<ProtocolEvent>` on the unified channel using a
    /// `DeviceId` derived from the port name, so the daemon's
    /// `process_device_event` hot path is the only consumer (the parallel
    /// `process_input_event` path was removed). For configs without
    /// `[[bindings]]`, this synthesises a single binding from the connected
    /// port — trigger-matching with no `device` filter still matches because
    /// the engine treats a missing `device` field as "match any device".
    pub fn reconnect_midi_port(
        &mut self,
        port_index: usize,
        device_event_tx: mpsc::Sender<DeviceEvent<ProtocolEvent>>,
        command_tx: mpsc::Sender<DaemonCommand>,
    ) -> Result<(usize, String), String> {
        let midi_mgr = self
            .midi_manager
            .as_mut()
            .ok_or("MIDI device manager not available (gamepad-only mode?)")?;

        // Disconnect current MIDI device if connected
        if midi_mgr.is_connected() {
            midi_mgr.disconnect();
            debug!("Disconnected from current MIDI device");
        }

        // Create intermediate channel for MIDI events
        let (midi_event_tx, midi_event_rx) = mpsc::channel::<MidiEvent>(1024);

        // Connect to the specified port
        let (port_idx, port_name) =
            midi_mgr.connect_to_port(port_index, midi_event_tx, command_tx)?;

        info!(
            device_type = "MIDI",
            port = %port_name,
            "Reconnected to MIDI device (port {})",
            port_idx
        );

        // Synthesise a DeviceId from the connected port so legacy configs flow
        // through the same DeviceEvent channel as `[[bindings]]`-configured
        // setups. `process_device_event` will see a real DeviceId rather than
        // the previous `"default"` sentinel.
        let device_id = DeviceId::raw(&port_name);

        // ADR-039 §4.3 (#1760): non-blocking shed-load push (try_send +
        // drop-newest) onto the unified pump, replacing the old blocking
        // `send().await`. Retain the metrics handle for `input_source_metrics()`.
        let metrics = InputSourceMetricsHandle::new();
        self.midi_source_metrics
            .insert(device_id.clone(), metrics.clone());
        spawn_input_pump(device_id, midi_event_rx, device_event_tx, metrics);

        Ok((port_idx, port_name))
    }
}

/// Decide whether a gamepad-connect failure should propagate or be
/// swallowed, based on the active input mode (#974).
///
/// - `MidiOnly`: gamepad connect isn't relevant; result is collapsed to
///   `Ok(())` regardless. (`try_connect_gamepad` already early-returns
///   for this mode, but classify is robust to either input so it can be
///   tested independently.)
/// - `Both`: best-effort — failures are logged and swallowed so a
///   missing gamepad doesn't bring down a working MIDI setup.
/// - `GamepadOnly`: failures propagate, because there's no other input
///   source for the daemon to fall back on. The error message bundles
///   the call-site context with the underlying gilrs error so logs
///   identify both the failure point and the cause.
///
/// **Determinism for tests**: the *return value* depends only on
/// `(mode, connect_result)` — no `&self`, no hardware I/O, no gilrs/midir
/// calls. That's what makes the four `classify_*` cases exercisable by
/// platform-independent unit tests.
///
/// **Side-effect**: the `Both + Err` arm emits a `warn!` log so the
/// failure is observable in daemon logs (the daemon continues but the
/// operator should know). This is a side-effect, not a strict pure
/// function — but it's not part of the return-value contract that the
/// unit tests assert.
/// ADR-039-B #1762 (step 4c): resolve the alias a live gamepad's events should
/// be tagged with — the route `from` key the route engine matches against in
/// [`route_destinations_ctx`](crate::route_engine::RouteEngine::route_destinations_ctx).
///
/// Returns the `alias` of the first enabled `[[endpoints]]` entry that is HID
/// (`effective_protocol() == Hid`) with `direction = Input`. `None` when the
/// config declares no such endpoint — callers then fall back to the historical
/// `DeviceId::raw("gamepad")` tag, so existing gamepad setups keep routing
/// unchanged.
///
/// Reads `config.endpoints` directly (not the normalized endpoint set): HID is
/// input-only (ADR-039 D7) and only ever authored as `[[endpoints]]` (legacy
/// `[[bindings]]`/`[[connectors]]` are MIDI constructs), so no normalization is
/// needed — which also lets the `GamepadOnly` early-return path (which skips
/// endpoint normalization) resolve the alias.
pub(crate) fn resolve_hid_input_alias(endpoints: &[EndpointConfig]) -> Option<String> {
    endpoints
        .iter()
        .find(|ep| {
            ep.enabled
                && ep.direction == ConnectorDirection::Input
                && ep.effective_protocol() == ConnectorProtocol::Hid
        })
        .map(|ep| ep.alias.clone())
}

/// ADR-047 §D2: collect SDL GUIDs from `ControllerGuid` matchers on the enabled
/// HID input endpoint(s), so the gamepad connect path can prefer that
/// controller over first-available. Empty when none are configured (preserving
/// historical first-available behaviour).
pub(crate) fn resolve_hid_preferred_guids(endpoints: &[EndpointConfig]) -> Vec<[u8; 16]> {
    use conductor_core::identity::DeviceMatcher;
    endpoints
        .iter()
        .filter(|ep| {
            ep.enabled
                && ep.direction == ConnectorDirection::Input
                && ep.effective_protocol() == ConnectorProtocol::Hid
        })
        .flat_map(|ep| ep.kind.effective_matchers(ConnectorDirection::Input))
        .filter_map(|m| match m {
            DeviceMatcher::ControllerGuid { value } => Some(*value),
            _ => None,
        })
        .collect()
}

/// ADR-039-B #2293: should the gamepad hot-plug rescan attempt a connect this
/// tick? Pure decision so it's exhaustively unit-testable without gilrs.
///
/// Attempt only when the mode includes a gamepad (`Both`/`GamepadOnly`) AND no
/// gamepad is currently connected — once connected, the polling thread owns it
/// and a rescan would be wasted work (and, given the discovery window, would
/// needlessly hold the input-manager lock).
pub(crate) fn should_rescan_gamepad(mode: InputMode, gamepad_connected: bool) -> bool {
    matches!(mode, InputMode::Both | InputMode::GamepadOnly) && !gamepad_connected
}

pub(crate) fn classify_gamepad_connect_result(
    mode: InputMode,
    connect_result: Result<(), String>,
    context: &str,
) -> Result<(), String> {
    match (mode, connect_result) {
        (InputMode::MidiOnly, _) => Ok(()),
        (_, Ok(())) => Ok(()),
        (InputMode::Both, Err(e)) => {
            warn!(error = %e, context = context, "Failed to connect gamepad (best-effort)");
            Ok(())
        }
        (InputMode::GamepadOnly, Err(e)) => Err(format!(
            "Gamepad connect required for GamepadOnly mode (context: {}): {}",
            context, e
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::Config;

    // ========================================================================
    // #969: gamepad-connect error handling consistency in listen_to_all_ports
    // ========================================================================
    //
    // Three sites in `listen_to_all_ports` call `connect_gamepad_multi_device`.
    // Pre-fix: Sites 1 & 2 used `?` (fatal — kills the whole bring-up); Site 3
    // used `if let Err(e)` (best-effort — logs and continues). Whether a
    // failed gamepad connect crashed the daemon depended on whether MIDI ports
    // happened to be enumerable.
    //
    // Fix: extract `try_connect_gamepad` helper that returns `()` and swallows
    // the error with a warn-level log. The `()` return type makes propagation
    // a compile-time impossibility; if anyone reverts to `?` inside the
    // helper, the compiler rejects it.

    // `DaemonCommand` is brought in by `use super::*;` above — no extra
    // import needed (Copilot review 3167308772).

    #[tokio::test]
    async fn try_connect_gamepad_does_not_propagate_failure_in_both_mode() {
        // Helper must be best-effort in Both mode: the typical CI (and
        // many real user setups) has no gamepads available. Pre-#969
        // this would propagate at Sites 1 & 2 of `listen_to_all_ports`
        // and bring down the whole bring-up. Post-#974 the helper now
        // returns Result; for Both mode + connect failure, classify
        // collapses to Ok.
        //
        // `#[tokio::test]` (not `#[test]`) because connect_gamepad_multi_device
        // calls tokio::spawn when a gamepad IS connected — without a
        // runtime the test would panic on dev machines with controllers.
        let mut manager = InputManager::new(None, true, InputMode::Both);
        let (event_tx, _event_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(16);
        let (command_tx, _command_rx) = mpsc::channel::<DaemonCommand>(16);

        let result = manager.try_connect_gamepad(&event_tx, &command_tx, "test no-MIDI path");
        assert!(
            result.is_ok(),
            "Both mode must swallow connect failures; got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn try_connect_gamepad_is_a_noop_in_midi_only_mode() {
        // MidiOnly mode has no gamepad_manager — the helper must early-return
        // without invoking gilrs at all. (gilrs init can take measurable
        // time, especially on macOS, so the noop matters for startup
        // latency too.)
        let mut manager = InputManager::new(Some("Test".to_string()), true, InputMode::MidiOnly);
        let (event_tx, _event_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(16);
        let (command_tx, _command_rx) = mpsc::channel::<DaemonCommand>(16);

        let result = manager.try_connect_gamepad(&event_tx, &command_tx, "midi-only test");
        assert!(
            result.is_ok(),
            "MidiOnly must early-return Ok; got: {:?}",
            result
        );
    }

    // ========================================================================
    // #974: GamepadOnly hard-fail when gamepad-connect fails
    // ========================================================================
    //
    // Pre-fix: `try_connect_gamepad` always returned `()` — even in
    // `GamepadOnly` mode where there's no other input source to fall back
    // on. The daemon came up "successfully" with zero input devices.
    //
    // Fix (Option C+): pure `classify_gamepad_connect_result(mode, result, context)`
    // function centralises the mode-based decision; `try_connect_gamepad`
    // delegates to it. All call sites use `?` — consistency by construction
    // is preserved (the property #969 established).

    #[test]
    fn classify_gamepad_only_failure_propagates_err() {
        // The headline contract: GamepadOnly + connect failure must surface
        // the error so listen_to_all_ports can return Err and the daemon's
        // bring-up fails cleanly with a user-visible message.
        let result = classify_gamepad_connect_result(
            InputMode::GamepadOnly,
            Err("No game controllers connected".to_string()),
            "test context",
        );
        assert!(result.is_err(), "GamepadOnly + Err must propagate");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("Gamepad connect required for GamepadOnly mode"),
            "error must explain why it's fatal: {}",
            msg
        );
        assert!(msg.contains("test context"), "context preserved: {}", msg);
        assert!(
            msg.contains("No game controllers connected"),
            "underlying gilrs error preserved: {}",
            msg
        );
    }

    #[test]
    fn classify_both_failure_swallowed_to_ok() {
        // Both mode keeps the legacy best-effort behaviour: a missing
        // gamepad must not bring down a working MIDI setup.
        let result = classify_gamepad_connect_result(
            InputMode::Both,
            Err("gilrs init failed".to_string()),
            "test context",
        );
        assert!(result.is_ok(), "Both + Err must be swallowed (best-effort)");
    }

    #[test]
    fn classify_midi_only_skips_regardless_of_result() {
        // Defensive: try_connect_gamepad shouldn't even invoke connect in
        // MidiOnly mode, but the classify function should be robust to
        // either input. (`MidiOnly + Ok` is the realistic path; testing
        // `MidiOnly + Err` documents that classify treats MidiOnly as a
        // no-op rather than propagating.)
        assert!(classify_gamepad_connect_result(InputMode::MidiOnly, Ok(()), "ctx").is_ok());
        assert!(
            classify_gamepad_connect_result(InputMode::MidiOnly, Err("ignored".to_string()), "ctx")
                .is_ok()
        );
    }

    #[test]
    fn classify_success_passes_through_for_all_modes() {
        // Successful gamepad connect must always produce Ok regardless of
        // mode — there's nothing to propagate.
        for mode in [InputMode::Both, InputMode::GamepadOnly, InputMode::MidiOnly] {
            assert!(
                classify_gamepad_connect_result(mode, Ok(()), "ctx").is_ok(),
                "Ok must pass through for mode {:?}",
                mode
            );
        }
    }

    // ====================================================================
    // ADR-039-B #2293: gamepad hot-plug rescan decision (should_rescan_gamepad)
    // ====================================================================

    #[test]
    fn should_rescan_gamepad_only_when_gamepad_mode_and_disconnected() {
        // Attempt a rescan ONLY in a gamepad-capable mode with no gamepad
        // currently connected — i.e. the exact state where a controller
        // switched on after startup should be picked up.
        assert!(should_rescan_gamepad(InputMode::Both, false));
        assert!(should_rescan_gamepad(InputMode::GamepadOnly, false));
    }

    #[test]
    fn should_rescan_gamepad_skips_when_already_connected() {
        // Once connected, the polling thread owns the pad — rescanning would be
        // wasted work and (via the discovery window) needless lock contention.
        assert!(!should_rescan_gamepad(InputMode::Both, true));
        assert!(!should_rescan_gamepad(InputMode::GamepadOnly, true));
    }

    #[test]
    fn should_rescan_gamepad_skips_midi_only_mode() {
        // MidiOnly never has a gamepad manager — never rescan, regardless of
        // the (irrelevant) connected flag.
        assert!(!should_rescan_gamepad(InputMode::MidiOnly, false));
        assert!(!should_rescan_gamepad(InputMode::MidiOnly, true));
    }

    // (Removed: `try_connect_gamepad_propagates_err_in_gamepad_only` —
    // it was hardware-dependent (passes when no controller is connected,
    // fails when one is) and only duplicated coverage already provided
    // by the pure `classify_*` tests above. Per Copilot review on PR
    // #978, the pure-function tests are the authoritative regression
    // guards for the mode-based decision.)

    #[tokio::test]
    async fn listen_to_all_ports_skips_midi_in_gamepad_only_mode() {
        // #974: in GamepadOnly mode, opening MIDI ports contradicts the
        // user's mode choice. The early-return at the top of
        // listen_to_all_ports must skip the MIDI bring-up entirely; we
        // verify by asserting `midi_managers` stays empty regardless of
        // platform MIDI state.
        //
        // Bonus: this also avoids `midir::MidiInput::new()` on Linux CI
        // where ALSA/`/dev/snd/seq` is missing — the early-return makes
        // the call unreachable for GamepadOnly.
        let mut manager = InputManager::new(None, true, InputMode::GamepadOnly);
        let config = Config::default_config();
        let (event_tx, _event_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(16);
        let (command_tx, _command_rx) = mpsc::channel::<DaemonCommand>(16);

        // listen_to_all_ports may return Ok or Err depending on whether
        // gilrs can connect to a gamepad — what matters is that no MIDI
        // ports were opened either way.
        let _ = manager.listen_to_all_ports(&config, event_tx, command_tx);

        assert!(
            manager.midi_managers.is_empty(),
            "GamepadOnly mode must not open MIDI ports (#974)"
        );
    }

    #[tokio::test]
    async fn enter_multi_device_idle_state_sets_active_flag() {
        // #969 review (Copilot 3167308646): the Site 1 early-return path
        // (no MIDI ports enumerated) was missing
        // `self.multi_device_active = true`, unlike Site 2
        // (capped.is_empty()) and the normal path. Without the flag,
        // subsequent `rescan_ports` calls no-op forever — devices plugged
        // in later would never be detected.
        //
        // Fix: extract `enter_multi_device_idle_state` helper that bundles
        // `multi_device_active = true` with the best-effort gamepad
        // connect, then call it from both empty-ports branches. This test
        // exercises the helper directly so it works cross-platform — an
        // earlier attempt to test via `listen_to_all_ports` failed on
        // Linux CI where `midir::MidiInput::new()` errors out before any
        // flag-setting branch is reached (`/dev/snd/seq` missing in
        // GitHub-runner sandboxes).
        let mut manager = InputManager::new(None, true, InputMode::Both);
        let (event_tx, _event_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(16);
        let (command_tx, _command_rx) = mpsc::channel::<DaemonCommand>(16);

        assert!(!manager.is_multi_device(), "precondition: not yet active");
        let result = manager.enter_multi_device_idle_state(&event_tx, &command_tx, "test");
        assert!(
            result.is_ok(),
            "Both mode swallows gamepad failures; got: {:?}",
            result
        );
        assert!(
            manager.is_multi_device(),
            "helper must set multi_device_active so hot-plug rescans run \
             when devices connect later (#969 review)"
        );
    }

    // ========================================================================
    // ADR-039-B #1762 (step 4c): HID input endpoint alias resolution
    // ========================================================================
    //
    // A live gamepad's events are tagged with a `DeviceId` that the route
    // engine keys on (`route_destinations_ctx(device_id.as_str(), …)`). When
    // the config declares the gamepad as an `[[endpoints]]` Input/Hid endpoint,
    // its alias becomes that tag so a catch-all route `from = "<alias>"`
    // matches. With no HID input endpoint, callers fall back to the historical
    // `"gamepad"` tag so existing setups keep routing unchanged.

    use conductor_core::config::types::{
        ConnectorDirection as Dir, ConnectorProtocol as Proto, EndpointConfig, EndpointKind,
    };

    fn endpoint(alias: &str, direction: Dir, protocol: Option<Proto>) -> EndpointConfig {
        EndpointConfig {
            alias: alias.to_string(),
            direction,
            protocol,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers: vec![],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }
    }

    #[test]
    fn resolve_hid_input_alias_none_when_no_endpoints() {
        assert_eq!(resolve_hid_input_alias(&[]), None);
    }

    #[test]
    fn resolve_hid_input_alias_none_when_only_midi_endpoints() {
        // A MIDI input endpoint (protocol inferred from Matcher kind) must not
        // be mistaken for a HID input — the gamepad falls back to "gamepad".
        let endpoints = vec![endpoint("Mikro", Dir::Input, None)];
        assert_eq!(resolve_hid_input_alias(&endpoints), None);
    }

    #[test]
    fn resolve_hid_input_alias_finds_the_hid_input_endpoint() {
        let endpoints = vec![
            endpoint("Mikro", Dir::Input, None),
            endpoint("MyPad", Dir::Input, Some(Proto::Hid)),
        ];
        assert_eq!(
            resolve_hid_input_alias(&endpoints).as_deref(),
            Some("MyPad")
        );
    }

    #[test]
    fn resolve_hid_input_alias_skips_disabled_hid_endpoint() {
        // A disabled HID input endpoint contributes no alias — the gamepad
        // falls back to the default "gamepad" tag rather than routing under a
        // name the operator has switched off.
        let mut ep = endpoint("MyPad", Dir::Input, Some(Proto::Hid));
        ep.enabled = false;
        assert_eq!(resolve_hid_input_alias(&[ep]), None);
    }

    // ── ADR-047 §D2: resolve_hid_preferred_guids ──

    fn hid_endpoint_with_guid(alias: &str, guid: [u8; 16]) -> EndpointConfig {
        use conductor_core::config::types::EndpointKind;
        use conductor_core::identity::DeviceMatcher;
        let mut ep = endpoint(alias, Dir::Input, Some(Proto::Hid));
        if let EndpointKind::Matcher { matchers, .. } = &mut ep.kind {
            matchers.push(DeviceMatcher::controller_guid(guid));
        }
        ep
    }

    #[test]
    fn resolve_hid_preferred_guids_extracts_controller_guid_from_hid_endpoint() {
        let guid = [7u8; 16];
        let endpoints = vec![
            endpoint("Mikro", Dir::Input, None), // MIDI — ignored
            hid_endpoint_with_guid("MyPad", guid),
        ];
        assert_eq!(resolve_hid_preferred_guids(&endpoints), vec![guid]);
    }

    #[test]
    fn resolve_hid_preferred_guids_empty_without_controller_guid() {
        // A HID endpoint with no ControllerGuid matcher → first-available.
        let endpoints = vec![endpoint("MyPad", Dir::Input, Some(Proto::Hid))];
        assert!(resolve_hid_preferred_guids(&endpoints).is_empty());
    }

    #[test]
    fn resolve_hid_preferred_guids_ignores_controller_guid_on_midi_endpoint() {
        // A ControllerGuid on a MIDI (non-Hid) endpoint is not a gamepad
        // preference — the protocol filter excludes it.
        use conductor_core::config::types::EndpointKind;
        use conductor_core::identity::DeviceMatcher;
        let mut midi = endpoint("Mikro", Dir::Input, None);
        if let EndpointKind::Matcher { matchers, .. } = &mut midi.kind {
            matchers.push(DeviceMatcher::controller_guid([1u8; 16]));
        }
        assert!(resolve_hid_preferred_guids(&[midi]).is_empty());
    }

    #[test]
    fn resolve_hid_preferred_guids_skips_disabled_endpoint() {
        let mut ep = hid_endpoint_with_guid("MyPad", [9u8; 16]);
        ep.enabled = false;
        assert!(resolve_hid_preferred_guids(&[ep]).is_empty());
    }
}
