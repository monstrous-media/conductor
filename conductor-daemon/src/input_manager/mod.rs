// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Unified Input Management (v3.0, multi-device v4.20.0)
//!
//! This module provides a unified interface for managing both MIDI and gamepad input devices.
//! It combines MidiDeviceManager and GamepadDeviceManager into a single manager that outputs
//! a unified stream of InputEvents, with multi-device support via DeviceEvent tagging.
//!
//! # Multi-Device Architecture (v4.20.0 - ADR-009 Phase 2)
//!
//! In multi-device mode, the InputManager opens all MIDI ports simultaneously and tags each
//! event with a `DeviceId` via the `DeviceEvent<ProtocolEvent>` channel type. Events flow through
//! per-device EventProcessors in the EngineManager.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │  InputManager                                    │
//! │  ┌────────────────────────────────────────────┐  │
//! │  │  Multi-Device MIDI (v4.20.0)               │  │
//! │  │  - HashMap<DeviceId, MidiDeviceManager>    │  │
//! │  │  - Device mute/unmute                      │  │
//! │  │  - Port filtering (ignore_ports, max)      │  │
//! │  └────────────────────────────────────────────┘  │
//! │  ┌────────────────────────────────────────────┐  │
//! │  │  Legacy single MIDI (backward compat)      │  │
//! │  │  - MidiDeviceManager                       │  │
//! │  └────────────────────────────────────────────┘  │
//! │  ┌────────────────────────────────────────────┐  │
//! │  │  HidDeviceManager                          │  │
//! │  │  - Outputs: InputEvent (native)            │  │
//! │  └────────────────────────────────────────────┘  │
//! │  ┌────────────────────────────────────────────┐  │
//! │  │  Unified DeviceEvent<ProtocolEvent> Stream    │  │
//! │  │  - Tagged with DeviceId per source         │  │
//! │  └────────────────────────────────────────────┘  │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! ## Module layout (#1684)
//!
//! The manager grew past the LLM Council `verify` size ceiling, so the
//! cohesive subsystems live in submodules; `mod.rs` keeps the
//! `InputManager` struct, its constructors/setters/accessors, and the
//! pure `filter_ports` helper:
//! - [`listen`] — `connect` (legacy), `listen_to_all_ports`, gamepad
//!   bring-up + `classify_gamepad_connect_result`
//! - [`rescan`] — hot-plug `rescan_ports` + its pure
//!   `build_rescan_desired` helper, plus the shared
//!   `open_port_with_device_id` / `remove_disconnected_manager`
//!   building blocks and the rescan-side `DesiredPort` /
//!   `DesiredPorts` / `AmbiguousSkips` types.
//! - [`rekey`] — two-phase rekey apply (`compute_rekeys` pure helper +
//!   `drain_rekeys_for_apply` impl + `StagedRekey` work item),
//!   consumed by `rescan_ports` step 8.
//! - [`devices`] — per-device mute/enable + status accessors.

use crate::daemon::MonitorEvent;
use crate::gamepad_device::{HidDeviceManager, HidInputSource};
use crate::input_source::{InputSourceMetrics, InputSourceMetricsHandle};
use crate::midi_device::MidiDeviceManager;
use conductor_core::gamepad_events::{DEFAULT_STICK_DEADZONE, DEFAULT_TRIGGER_DEADZONE};
use conductor_core::identity::DeviceId;
use conductor_core::resolver::PortInfo;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::broadcast;

mod devices;
mod listen;
mod rekey;
mod rescan;

/// Input device selection mode (v3.0)
// Re-export InputMode from conductor-core (v3.0)
pub use conductor_core::InputMode;

/// Unified input device manager (v3.0, multi-device v4.20.0)
///
/// Manages connections to MIDI and/or gamepad devices, providing a unified
/// stream of DeviceEvent<ProtocolEvent> for processing by the event engine.
///
/// # Multi-Device Mode (v4.20.0)
///
/// When `listen_to_all_ports()` is called, the manager opens all available MIDI ports
/// (filtered by `ignore_ports` and capped at `max_midi_ports`), resolves port→device
/// bindings via `PortResolver`, and tags each event with the appropriate `DeviceId`.
///
/// # Legacy Mode
///
/// When `connect()` is called directly, the manager operates in single-device legacy mode
/// for backward compatibility with existing configs using `[device]`.
pub struct InputManager {
    /// Legacy single MIDI device manager (used by connect())
    pub(super) midi_manager: Option<MidiDeviceManager>,

    /// Multi-device MIDI managers (v4.20.0 - ADR-009 Phase 2)
    /// Populated by listen_to_all_ports()
    pub(super) midi_managers: HashMap<DeviceId, MidiDeviceManager>,

    /// ADR-039 §4.3 (#1760): per-device source observability counters,
    /// incremented by each MIDI converter's [`spawn_input_pump`] push task
    /// (`events_in` / `dropped`). Keyed by the same `DeviceId` as
    /// `midi_managers`; an entry is dropped alongside its manager in
    /// `remove_disconnected_manager`. Surfaced via [`input_source_metrics`](Self::input_source_metrics).
    ///
    /// [`spawn_input_pump`]: crate::input_source::spawn_input_pump
    pub(super) midi_source_metrics: HashMap<DeviceId, InputSourceMetricsHandle>,

    /// ADR-039 §4.3 (#1760): gamepad source observability counters,
    /// incremented by the gamepad converter's push task. `None` until a
    /// gamepad connects.
    pub(super) gamepad_source_metrics: Option<InputSourceMetricsHandle>,

    /// Muted device IDs (v4.20.0 - ADR-009 Phase 2, D8)
    pub(super) muted_devices: HashSet<DeviceId>,

    /// Device IDs bound to a configured `[[devices]]` identity (v4.26.0 - ADR-009 D19)
    pub(super) configured_devices: HashSet<DeviceId>,

    /// HID input source (optional). ADR-039-B #1762 (step 4c): the live gamepad
    /// path is now expressed through the [`HidInputSource`] substrate (#1758)
    /// rather than a bare [`HidDeviceManager`], so every `InputSource` line has
    /// a live consumer. Wraps the same manager and push-based gilrs delivery —
    /// behaviour is preserved; `connect_gamepad_multi_device` drives it via
    /// `connect` + `start(tx)`.
    pub(super) gamepad_manager: Option<HidInputSource>,

    /// ADR-039-B #1762 (step 4c): alias the live gamepad's events are tagged
    /// with — resolved from the config's `[[endpoints]]` Input/Hid declaration
    /// (`resolve_hid_input_alias`) at `listen_to_all_ports` time. `None` until
    /// resolved, in which case the gamepad falls back to the historical
    /// `DeviceId::raw("gamepad")` tag (backward compatible).
    pub(super) hid_input_alias: Option<String>,

    /// ADR-047 §D2: SDL GUIDs from the configured HID input endpoint's
    /// `ControllerGuid` matcher(s), resolved at `listen_to_all_ports` time
    /// (`resolve_hid_preferred_guids`). Passed to the gamepad manager before
    /// connect so a configured controller is selected over first-available.
    /// Empty when no `ControllerGuid` is configured (first-available, as before).
    pub(super) hid_preferred_guids: Vec<[u8; 16]>,

    /// Input mode selection
    pub(super) mode: InputMode,

    /// Whether multi-device mode is active
    pub(super) multi_device_active: bool,

    /// Port names to auto-exclude from input scanning (v4.26.0 - ADR-009 D21)
    /// Populated with virtual output port names to prevent feedback loops
    pub(super) exclude_port_names: Vec<String>,

    /// MIDI Learn active flag — shared with EngineManager (v4.26.0 - ADR-009 D11)
    /// When true, Ambiguous ports are opened temporarily for device discovery
    pub(super) midi_learn_active: Option<Arc<AtomicBool>>,

    /// Shared `ProbeCoordinator` for SysEx Identity Request dispatch
    /// (ADR-026 Phase 1.B). Cloned into every `MidiDeviceManager` this
    /// `InputManager` opens, so each port's midir callback can observe
    /// Identity Replies. `None` in tests / contexts that don't probe.
    pub(super) probe_coordinator:
        Option<Arc<conductor_core::device_intelligence::probe::ProbeCoordinator>>,

    /// #943: Daemon → GUI event channel for surfacing
    /// `BindingResult::Ambiguous` (and other resolver-side conditions)
    /// beyond the existing `tracing::warn!`. `None` in tests; set by
    /// `EngineManager::new` after construction. `broadcast::Sender::send`
    /// is sync and never blocks, so this is safe to call from the
    /// non-async resolver paths.
    pub(super) event_broadcast_tx: Option<broadcast::Sender<MonitorEvent>>,
}

impl InputManager {
    /// Create a new unified input manager
    pub fn new(midi_device_name: Option<String>, auto_reconnect: bool, mode: InputMode) -> Self {
        Self::with_deadzone(
            midi_device_name,
            auto_reconnect,
            mode,
            DEFAULT_STICK_DEADZONE,
            DEFAULT_TRIGGER_DEADZONE,
        )
    }

    /// Create a new InputManager with configurable gamepad dead zones
    pub fn with_deadzone(
        midi_device_name: Option<String>,
        auto_reconnect: bool,
        mode: InputMode,
        stick_deadzone: f32,
        trigger_deadzone: f32,
    ) -> Self {
        let midi_manager = if mode == InputMode::MidiOnly || mode == InputMode::Both {
            Some(MidiDeviceManager::new(
                midi_device_name.unwrap_or_default(),
                auto_reconnect,
            ))
        } else {
            None
        };

        let gamepad_manager = if mode == InputMode::GamepadOnly || mode == InputMode::Both {
            // ADR-039-B #1762 (step 4c): wrap the manager as the HID
            // `InputSource`. The pumped event tag (`DeviceId`) defaults to
            // "gamepad" (backward compatible) and is updated via
            // `set_device_id` in `connect_gamepad_multi_device` when the
            // config declares a `[[endpoints]]` Input/Hid alias.
            Some(HidInputSource::new(
                "gamepad",
                HidDeviceManager::with_deadzone(auto_reconnect, stick_deadzone, trigger_deadzone),
            ))
        } else {
            None
        };

        Self {
            midi_manager,
            midi_managers: HashMap::new(),
            midi_source_metrics: HashMap::new(),
            gamepad_source_metrics: None,
            muted_devices: HashSet::new(),
            configured_devices: HashSet::new(),
            gamepad_manager,
            hid_input_alias: None,
            hid_preferred_guids: Vec::new(),
            mode,
            multi_device_active: false,
            exclude_port_names: Vec::new(),
            midi_learn_active: None,
            probe_coordinator: None,
            event_broadcast_tx: None,
        }
    }

    /// ADR-039 §4.1/§4.3 (#1760): snapshot every connected input source's
    /// baseline observability counters (`events_in`, `dropped`, `errors`,
    /// `last_activity`). The MIDI/HID converter push tasks increment these via
    /// the shared shed-load [`enqueue`] policy, so this is the live view of how
    /// many events each source ingested and shed under backpressure.
    ///
    /// Returned in no particular order; the gamepad source (when present) uses
    /// the same `DeviceId` as the pumped event tag — the configured HID input
    /// endpoint alias when one is declared, or `"gamepad"` (backward compatible).
    ///
    /// [`enqueue`]: crate::input_source::enqueue
    pub fn input_source_metrics(&self) -> Vec<(DeviceId, InputSourceMetrics)> {
        let mut out: Vec<(DeviceId, InputSourceMetrics)> = self
            .midi_source_metrics
            .iter()
            .map(|(id, h)| (id.clone(), h.snapshot()))
            .collect();
        if let Some(h) = &self.gamepad_source_metrics {
            let gamepad_id = self
                .hid_input_alias
                .as_deref()
                .map(DeviceId::raw)
                .unwrap_or_else(|| DeviceId::raw("gamepad"));
            out.push((gamepad_id, h.snapshot()));
        }
        out
    }

    /// Set the MIDI Learn active flag for Ambiguous port handling (v4.26.0 - ADR-009 D11).
    pub fn set_midi_learn_flag(&mut self, flag: Arc<AtomicBool>) {
        self.midi_learn_active = Some(flag);
    }

    /// #943: Wire the daemon's event broadcast channel so resolver-side
    /// surface conditions (Ambiguous ports, etc.) can be raised to the
    /// GUI / MCP / CLI surfaces beyond the `tracing::warn!` log line.
    pub fn set_event_broadcast_tx(&mut self, tx: broadcast::Sender<MonitorEvent>) {
        self.event_broadcast_tx = Some(tx);
    }

    /// #943: emit a `MonitorEvent` over the broadcast channel (no-op if
    /// the channel is unset). Failures (no subscribers, lagging) are
    /// intentionally swallowed — the `tracing::warn!` companion at the
    /// call site is the durable record; this channel exists to push the
    /// same condition into surfaces that can render it interactively.
    pub(crate) fn emit_monitor_event(&self, event: MonitorEvent) {
        if let Some(ref tx) = self.event_broadcast_tx {
            let _ = tx.send(event);
        }
    }

    /// #943: dedicated helper for `BindingResult::Ambiguous` so both
    /// resolver call sites (initialize_multi_device + rescan_ports)
    /// share a single payload shape. The event_type string
    /// `"ambiguous_port_detected"` is what GUI / MCP / CLI consumers
    /// will subscribe on; the `payload` carries enough detail to render
    /// a "refine matcher" CTA without another round-trip.
    pub(crate) fn emit_ambiguous_port_event(
        &self,
        port_name: &str,
        port_index: usize,
        claimed_by_alias: &str,
    ) {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let payload = serde_json::json!({
            "port_name": port_name,
            "port_index": port_index,
            "claimed_by_alias": claimed_by_alias,
        });
        self.emit_monitor_event(MonitorEvent {
            timestamp_ms,
            event_type: "ambiguous_port_detected".to_string(),
            detail: Some(format!(
                "Port '{}' matches alias '{}' which is already bound to another port",
                port_name, claimed_by_alias
            )),
            payload: Some(payload),
            ..Default::default()
        });
    }

    /// Wire the shared `ProbeCoordinator` into the MIDI ingress path so
    /// SysEx Identity Replies can be observed by pending probes
    /// (ADR-026 Phase 1.B). Must be called BEFORE connecting MIDI
    /// devices — applies to:
    ///
    /// - the legacy single-device `MidiDeviceManager` (`connect()` path)
    /// - every per-port `MidiDeviceManager` spawned later by
    ///   `listen_to_all_ports` → `open_port_with_device_id` (stored on
    ///   the `InputManager` itself, cloned into each port manager at
    ///   open time)
    ///
    /// No-op for gamepad-only devices.
    pub fn set_probe_coordinator(
        &mut self,
        coordinator: Arc<conductor_core::device_intelligence::probe::ProbeCoordinator>,
    ) {
        self.probe_coordinator = Some(coordinator.clone());
        if let Some(mgr) = self.midi_manager.as_mut() {
            mgr.set_probe_coordinator(coordinator);
        }
    }

    /// Set port names to auto-exclude from input scanning (v4.26.0 - ADR-009 D21).
    ///
    /// Used to exclude Conductor's own virtual output ports from input,
    /// preventing feedback loops.
    pub fn set_exclude_port_names(&mut self, names: Vec<String>) {
        self.exclude_port_names = names;
    }

    /// Whether multi-device mode is active
    pub fn is_multi_device(&self) -> bool {
        self.multi_device_active
    }

    /// Check if any input device is connected
    pub fn is_connected(&self) -> bool {
        if self.multi_device_active {
            let midi_connected = self.midi_managers.values().any(|m| m.is_connected());
            let gamepad_connected = self
                .gamepad_manager
                .as_ref()
                .map(|g| g.manager().is_connected())
                .unwrap_or(false);
            midi_connected || gamepad_connected
        } else {
            let midi_connected = self
                .midi_manager
                .as_ref()
                .map(|m| m.is_connected())
                .unwrap_or(false);
            let gamepad_connected = self
                .gamepad_manager
                .as_ref()
                .map(|g| g.manager().is_connected())
                .unwrap_or(false);
            midi_connected || gamepad_connected
        }
    }

    /// Get connection status for both devices
    pub fn get_status(&self) -> (bool, bool) {
        let midi_connected = if self.multi_device_active {
            self.midi_managers.values().any(|m| m.is_connected())
        } else {
            self.midi_manager
                .as_ref()
                .map(|m| m.is_connected())
                .unwrap_or(false)
        };

        let gamepad_connected = self
            .gamepad_manager
            .as_ref()
            .map(|g| g.manager().is_connected())
            .unwrap_or(false);

        (midi_connected, gamepad_connected)
    }

    /// Disconnect all input devices
    pub fn disconnect(&mut self) {
        // Disconnect legacy single device
        if let Some(ref mut midi_mgr) = self.midi_manager {
            midi_mgr.disconnect();
            tracing::info!("MIDI device disconnected (legacy)");
        }

        // Disconnect multi-device managers
        for (device_id, mut mgr) in self.midi_managers.drain() {
            mgr.disconnect();
            tracing::info!(device_id = %device_id, "MIDI device disconnected (multi-device)");
        }

        if let Some(ref mut gamepad_src) = self.gamepad_manager {
            gamepad_src.manager_mut().disconnect();
            tracing::info!("Gamepad device disconnected");
        }

        self.multi_device_active = false;

        // ADR-039 #1760 (PR #2177 review): the source-metrics handles track live
        // converter push tasks; clear them alongside the managers so
        // `input_source_metrics()` doesn't report stale counters for devices that
        // are no longer connected. (The hot-plug single-device removal path
        // clears its entry in `remove_disconnected_manager`; this is the bulk
        // teardown counterpart.)
        self.midi_source_metrics.clear();
        self.gamepad_source_metrics = None;
    }

    /// Get current input mode
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    /// Get a reference to the legacy MIDI device manager (if available)
    pub fn get_midi_manager(&self) -> Option<&MidiDeviceManager> {
        self.midi_manager.as_ref()
    }

    /// Get a mutable reference to the legacy MIDI device manager (if available)
    pub fn get_midi_manager_mut(&mut self) -> Option<&mut MidiDeviceManager> {
        self.midi_manager.as_mut()
    }

    /// Get connected gamepad devices
    pub fn get_connected_gamepads(&self) -> Vec<(String, String)> {
        if let Some(ref gamepad_src) = self.gamepad_manager {
            if let Some(info) = gamepad_src.manager().get_connected_gamepad_info() {
                vec![info]
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    }

    /// List available gamepad devices
    pub fn list_gamepads() -> Result<Vec<(gilrs::GamepadId, String, String)>, String> {
        HidDeviceManager::list_gamepads()
    }
}

// v4.10.9: Removed convert_midi_to_input() - now uses From<MidiEvent> for InputEvent
// from conductor-core/src/events.rs (single source of truth, preserves original timestamps)

/// Filter ports by ignore list and max cap (v4.20.0 - ADR-009 Phase 2)
///
/// Pure function for testability. Used by `listen_to_all_ports`.
pub fn filter_ports(
    ports: Vec<PortInfo>,
    ignore_ports: &[String],
    max_midi_ports: usize,
) -> Vec<PortInfo> {
    let filtered: Vec<PortInfo> = ports
        .into_iter()
        .filter(|p| {
            !ignore_ports
                .iter()
                .any(|pattern| p.name.contains(pattern.as_str()))
        })
        .collect();

    if filtered.len() > max_midi_ports {
        filtered.into_iter().take(max_midi_ports).collect()
    } else {
        filtered
    }
}

/// Whether MIDI input-port enumeration is worth running for `mode` (#2393).
///
/// `InputMode` gates which *input* protocols are active (MIDI *output* —
/// SendMidi, virtual ports — is unaffected and still works in GamepadOnly). In
/// [`InputMode::GamepadOnly`] the daemon brings up no MIDI *input* stack and
/// `rescan_ports` early-returns `(0, 0, 0)`, discarding any enumerated input
/// ports — so the off-loop hot-plug task ([`crate::daemon::engine_manager`]
/// run-loop) and the reload path skip the blocking CoreMIDI/ALSA *input* scan
/// entirely. This is cosmetic/efficiency only (the scan is already off the
/// run-loop), and it silences spurious MIDI debug warnings on hosts with no MIDI
/// input stack (e.g. Linux CI without ALSA `/dev/snd`).
pub(crate) fn midi_enumeration_enabled(mode: InputMode) -> bool {
    mode != InputMode::GamepadOnly
}

/// Build the input-scan ignore list (#2054 / #2216).
///
/// Combines, in order: the user's `ignore_ports`, any externally-set
/// `exclude_port_names`, and Conductor's own enabled `MidiVirtualPort`
/// outputs — the last derived from the *current* config via
/// [`crate::daemon::output_resolver::desired_virtual_port_names`].
///
/// Deriving the virtual-port exclusions from config (not from
/// already-created ports) is the fix: the old path set the exclusion once at
/// initial connect from `ActionExecutor::virtual_port_names()` — read *before*
/// the ports were created and never refreshed on reload/hot-plug. So a
/// `MidiVirtualPort` endpoint added in the GUI after startup was re-discovered
/// as an orphaned, unbound *input* port (its OS port, by raw name), showing up
/// mislabeled in the EVENTS pills and the LLM's device-bindings view instead of
/// as the configured output endpoint. Folding it in here, at every scan, keeps
/// the exclusion correct regardless of when the port was created.
pub(crate) fn build_input_ignore(
    ignore_ports: &[String],
    exclude_port_names: &[String],
    endpoints: &[conductor_core::config::types::EndpointConfig],
) -> Vec<String> {
    let mut ignore: Vec<String> = ignore_ports.to_vec();
    ignore.extend(exclude_port_names.iter().cloned());
    ignore.extend(crate::daemon::output_resolver::desired_virtual_port_names(
        endpoints,
    ));
    ignore
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::event_processor::MidiEvent;
    use conductor_core::events::InputEvent;
    use conductor_core::identity::DeviceId;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    #[test]
    fn test_input_manager_creation_midi_only() {
        let manager = InputManager::new(Some("Test Device".to_string()), true, InputMode::MidiOnly);
        assert!(manager.midi_manager.is_some());
        assert!(manager.gamepad_manager.is_none());
        assert_eq!(manager.mode(), InputMode::MidiOnly);
        assert!(!manager.is_multi_device());
    }

    #[test]
    fn test_input_manager_creation_gamepad_only() {
        let manager = InputManager::new(None, true, InputMode::GamepadOnly);
        assert!(manager.midi_manager.is_none());
        assert!(manager.gamepad_manager.is_some());
        assert_eq!(manager.mode(), InputMode::GamepadOnly);
    }

    #[test]
    fn test_input_manager_creation_both() {
        let manager = InputManager::new(Some("Test Device".to_string()), true, InputMode::Both);
        assert!(manager.midi_manager.is_some());
        assert!(manager.gamepad_manager.is_some());
        assert_eq!(manager.mode(), InputMode::Both);
    }

    #[test]
    fn midi_enumeration_enabled_skips_gamepad_only() {
        // #2393: in GamepadOnly mode `rescan_ports` discards any enumerated MIDI
        // ports, so the off-loop hot-plug task and the reload path must skip the
        // CoreMIDI/ALSA enumeration (cosmetic/efficiency — avoids wasted scans
        // and spurious MIDI warnings on no-MIDI hosts). MIDI must still
        // enumerate in MidiOnly and Both.
        assert!(!midi_enumeration_enabled(InputMode::GamepadOnly));
        assert!(midi_enumeration_enabled(InputMode::MidiOnly));
        assert!(midi_enumeration_enabled(InputMode::Both));
    }

    #[test]
    fn test_convert_midi_note_on() {
        let midi_event = MidiEvent::NoteOn {
            note: 60,
            velocity: 100,
            channel: 0,
            time: Instant::now(),
        };
        let input_event: InputEvent = midi_event.into();

        match input_event {
            InputEvent::PadPressed { pad, velocity, .. } => {
                assert_eq!(pad, 60);
                assert_eq!(velocity, 100);
            }
            _ => panic!("Expected PadPressed"),
        }
    }

    #[test]
    fn test_convert_midi_note_off() {
        let midi_event = MidiEvent::NoteOff {
            note: 60,
            channel: 0,
            time: Instant::now(),
        };
        let input_event: InputEvent = midi_event.into();

        match input_event {
            InputEvent::PadReleased { pad, .. } => {
                assert_eq!(pad, 60);
            }
            _ => panic!("Expected PadReleased"),
        }
    }

    #[test]
    fn test_convert_midi_cc() {
        let midi_event = MidiEvent::ControlChange {
            cc: 7,
            value: 64,
            channel: 0,
            time: Instant::now(),
        };
        let input_event: InputEvent = midi_event.into();

        match input_event {
            InputEvent::ControlChange { control, value, .. } => {
                assert_eq!(control, 7);
                assert_eq!(value, 64);
            }
            _ => panic!("Expected ControlChange"),
        }
    }

    #[test]
    fn test_get_midi_manager_returns_some_when_midi_only() {
        let manager =
            InputManager::new(Some("Test Device".to_string()), false, InputMode::MidiOnly);
        assert!(manager.get_midi_manager().is_some());
    }

    #[test]
    fn test_get_midi_manager_returns_none_when_gamepad_only() {
        let manager = InputManager::new(None, false, InputMode::GamepadOnly);
        assert!(manager.get_midi_manager().is_none());
    }

    #[test]
    fn test_get_midi_manager_returns_some_when_both() {
        let manager = InputManager::new(Some("Test Device".to_string()), false, InputMode::Both);
        assert!(manager.get_midi_manager().is_some());
    }

    #[test]
    fn test_get_midi_manager_mut_works() {
        let mut manager =
            InputManager::new(Some("Test Device".to_string()), false, InputMode::MidiOnly);
        assert!(manager.get_midi_manager_mut().is_some());
    }

    // Multi-device tests (v4.20.0)

    #[test]
    fn test_filter_ports_ignore() {
        let ports = vec![
            PortInfo::new("IAC Driver Bus 1".to_string(), 0),
            PortInfo::new("Mikro MK3".to_string(), 1),
            PortInfo::new("MIDI Through".to_string(), 2),
        ];

        let result = filter_ports(ports, &["IAC Driver".to_string()], 32);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "Mikro MK3");
        assert_eq!(result[1].name, "MIDI Through");
    }

    #[test]
    fn test_filter_ports_max_cap() {
        let ports = vec![
            PortInfo::new("Port A".to_string(), 0),
            PortInfo::new("Port B".to_string(), 1),
            PortInfo::new("Port C".to_string(), 2),
            PortInfo::new("Port D".to_string(), 3),
            PortInfo::new("Port E".to_string(), 4),
        ];

        let result = filter_ports(ports, &[], 3);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].name, "Port A");
        assert_eq!(result[2].name, "Port C");
    }

    #[test]
    fn test_filter_ports_excludes_virtual_output_names() {
        let ports = vec![
            PortInfo::new("Mikro MK3".to_string(), 0),
            PortInfo::new("Conductor Virtual Out".to_string(), 1),
            PortInfo::new("Launchpad".to_string(), 2),
        ];

        // Virtual output names should be excluded via the ignore_ports mechanism
        let ignore = vec!["Conductor Virtual Out".to_string()];
        let result = filter_ports(ports, &ignore, 32);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "Mikro MK3");
        assert_eq!(result[1].name, "Launchpad");
    }

    #[test]
    fn build_input_ignore_excludes_config_virtual_ports() {
        use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
        // A MidiVirtualPort OUTPUT endpoint: Conductor creates "Virtual Test Port"
        // as an OS port (#2063); it must NOT be re-scanned as an input, else it's
        // re-discovered as an orphaned unbound input (#2054 / #2216).
        let endpoints = vec![EndpointConfig {
            alias: "virtual-test-out".to_string(),
            direction: ConnectorDirection::Output,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::MidiVirtualPort {
                port_name: "Virtual Test Port".to_string(),
            },
        }];

        let ignore = build_input_ignore(&[], &[], &endpoints);
        assert!(
            ignore.contains(&"Virtual Test Port".to_string()),
            "config MidiVirtualPort output must be folded into the input ignore list"
        );

        // …and it actually filters the port out of an input scan.
        let ports = vec![
            PortInfo::new("Virtual Test Port".to_string(), 0),
            PortInfo::new("TouchOSC".to_string(), 1),
        ];
        let filtered = filter_ports(ports, &ignore, 32);
        assert_eq!(filtered.len(), 1, "the virtual output must be excluded");
        assert_eq!(filtered[0].name, "TouchOSC");
    }

    #[test]
    fn build_input_ignore_skips_disabled_virtual_ports() {
        use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
        // A disabled MidiVirtualPort is torn down (not created), so it should NOT
        // be excluded — mirrors desired_virtual_port_names' enabled filter.
        let endpoints = vec![EndpointConfig {
            alias: "off".to_string(),
            direction: ConnectorDirection::Output,
            protocol: None,
            description: None,
            enabled: false,
            channels: vec![],
            kind: EndpointKind::MidiVirtualPort {
                port_name: "Disabled Port".to_string(),
            },
        }];
        let ignore = build_input_ignore(&[], &[], &endpoints);
        assert!(!ignore.contains(&"Disabled Port".to_string()));
    }

    #[test]
    fn test_set_exclude_port_names() {
        let mut manager = InputManager::new(None, false, InputMode::MidiOnly);
        assert!(manager.exclude_port_names.is_empty());

        manager.set_exclude_port_names(vec!["Virtual Port 1".to_string()]);
        assert_eq!(manager.exclude_port_names.len(), 1);
        assert_eq!(manager.exclude_port_names[0], "Virtual Port 1");
    }

    #[test]
    fn test_disconnect_clears_source_metrics_handles() {
        let mut manager = InputManager::new(None, false, InputMode::Both);
        let midi_id = DeviceId::from_alias("midi-metrics");
        manager.midi_managers.insert(
            midi_id.clone(),
            crate::midi_device::MidiDeviceManager::new(String::new(), false),
        );
        let midi_metrics = crate::input_source::InputSourceMetricsHandle::new();
        midi_metrics.record_event();
        manager.midi_source_metrics.insert(midi_id, midi_metrics);

        let gamepad_metrics = crate::input_source::InputSourceMetricsHandle::new();
        gamepad_metrics.record_event();
        manager.gamepad_source_metrics = Some(gamepad_metrics);

        assert!(!manager.input_source_metrics().is_empty());
        manager.disconnect();
        assert!(manager.input_source_metrics().is_empty());
    }

    #[test]
    fn gamepad_metrics_key_defaults_to_gamepad_without_hid_endpoint() {
        // ADR-039-B #1762 (step 4c): with no HID input endpoint declared, the
        // gamepad source's metrics key matches the historical "gamepad" event
        // tag (backward compatible).
        let mut manager = InputManager::new(None, false, InputMode::Both);
        manager.gamepad_source_metrics = Some(crate::input_source::InputSourceMetricsHandle::new());

        let keys: Vec<DeviceId> = manager
            .input_source_metrics()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(keys, vec![DeviceId::raw("gamepad")]);
    }

    #[test]
    fn gamepad_metrics_key_uses_configured_hid_input_alias() {
        // ADR-039-B #1762 (step 4c): the gamepad source's metrics key must match
        // the configured `[[endpoints]]` Input/Hid alias used to tag its events
        // (`set_device_id` in `connect_gamepad_multi_device`) — otherwise
        // `input_source_metrics()` misattributes the source (e.g. events route
        // as "MyPad" but metrics show "gamepad"). Regression guard for the
        // metrics/event-tag consistency fix.
        let mut manager = InputManager::new(None, false, InputMode::Both);
        manager.hid_input_alias = Some("MyPad".to_string());
        manager.gamepad_source_metrics = Some(crate::input_source::InputSourceMetricsHandle::new());

        let keys: Vec<DeviceId> = manager
            .input_source_metrics()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            keys,
            vec![DeviceId::raw("MyPad")],
            "metrics key must match the configured HID input endpoint alias"
        );
    }

    #[test]
    fn test_midi_learn_flag_default_none() {
        let manager = InputManager::new(None, false, InputMode::MidiOnly);
        assert!(manager.midi_learn_active.is_none());
    }

    #[test]
    fn test_set_midi_learn_flag() {
        let mut manager = InputManager::new(None, false, InputMode::MidiOnly);
        let flag = Arc::new(AtomicBool::new(false));
        manager.set_midi_learn_flag(Arc::clone(&flag));

        assert!(manager.midi_learn_active.is_some());
        assert!(
            !manager
                .midi_learn_active
                .as_ref()
                .unwrap()
                .load(Ordering::SeqCst)
        );

        flag.store(true, Ordering::SeqCst);
        assert!(
            manager
                .midi_learn_active
                .as_ref()
                .unwrap()
                .load(Ordering::SeqCst)
        );
    }

    // ── #943: Ambiguous-port event emission ──────────────────────────
    //
    // The resolver's `BindingResult::Ambiguous` arm now emits a
    // `MonitorEvent` of type "ambiguous_port_detected" via the broadcast
    // channel set by `set_event_broadcast_tx`, in addition to the
    // existing `tracing::warn!`. The tests pin two invariants:
    //
    //   1. With no broadcast tx wired, `emit_ambiguous_port_event` is a
    //      no-op (the tracing log line is the only signal). This is the
    //      shape every test that doesn't care about this surface relies
    //      on — InputManager::new returns a manager with no tx.
    //   2. With a tx wired, the helper produces a MonitorEvent whose
    //      event_type, detail, and payload carry exactly the fields the
    //      GUI / MCP / CLI consumers need to render a "refine matcher"
    //      hint without another round-trip.

    #[tokio::test]
    async fn test_emit_ambiguous_port_event_noop_without_tx() {
        let manager = InputManager::new(None, false, InputMode::MidiOnly);
        // No tx wired → must not panic, no observable side effect to assert.
        manager.emit_ambiguous_port_event("Some Port", 0, "fcb");
    }

    #[tokio::test]
    async fn test_emit_ambiguous_port_event_carries_payload_when_tx_wired() {
        let (tx, mut rx) = broadcast::channel(8);
        let mut manager = InputManager::new(None, false, InputMode::MidiOnly);
        manager.set_event_broadcast_tx(tx);

        manager.emit_ambiguous_port_event("Komplete Audio 6 MK2 Port 2", 3, "fcb");

        let evt = rx.recv().await.expect("event must arrive");
        assert_eq!(evt.event_type, "ambiguous_port_detected");
        let detail = evt.detail.expect("detail must be present");
        assert!(
            detail.contains("Komplete Audio 6 MK2 Port 2") && detail.contains("fcb"),
            "detail must mention both port_name and claimed_by_alias for log-only consumers; got: {detail}"
        );
        let payload = evt.payload.expect("payload must be present");
        assert_eq!(
            payload.get("port_name").and_then(|v| v.as_str()),
            Some("Komplete Audio 6 MK2 Port 2")
        );
        assert_eq!(payload.get("port_index").and_then(|v| v.as_u64()), Some(3));
        assert_eq!(
            payload.get("claimed_by_alias").and_then(|v| v.as_str()),
            Some("fcb")
        );
    }
}
