// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Hot-plug rescan + port-open helpers for [`super::InputManager`].
//!
//! Holds `rescan_ports`, its pure `build_rescan_desired` helper, and the
//! shared `open_port_with_device_id` / `remove_disconnected_manager`
//! building blocks used by both [`super::listen`] and the rekey path.
//!
//! The two-phase rekey apply itself lives in [`super::rekey`] so this
//! file stays under the per-file LLM Council size ceiling (#1684).

use super::InputManager;
use crate::daemon::DaemonCommand;
use crate::input_source::{InputSourceMetricsHandle, spawn_input_pump};
use crate::midi_device::MidiDeviceManager;
use conductor_core::event_processor::MidiEvent;
use conductor_core::events::ProtocolEvent;
use conductor_core::identity::{DeviceEvent, DeviceId};
use conductor_core::resolver::{BindingResult, PortInfo, PortResolver};
use conductor_core::{Config, ListenMode};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// One entry in the desired open-port set produced by a rescan.
///
/// #1478: the desired set is a `Vec`, not a `HashMap` keyed by
/// `port_name`. Two OS ports can share a name (e.g. two identical
/// controllers) yet must stay distinct — they resolve to distinct
/// `device_id`s (`from_port_instance` mints `X`, `X #2`, …). Keying by
/// the bare name collapsed them, so one port was dropped or clobbered
/// on the next rescan/rekey pass. Carrying every port as its own entry,
/// matched downstream by `device_id`, preserves duplicate-name support.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DesiredPort {
    /// The DeviceId this port should be open under. Unique within a
    /// rescan in every normal case (bound alias, instance-disambiguated
    /// unbound). This is the stable identity used for open/remove/rekey.
    pub(crate) device_id: DeviceId,
    /// OS port name (may be shared by sibling ports).
    pub(crate) port_name: String,
    /// OS enumeration index for this rescan.
    pub(crate) port_index: usize,
}

/// Desired open-port set produced by a rescan. Order follows OS
/// enumeration order. See [`InputManager::build_rescan_desired`].
pub(crate) type DesiredPorts = Vec<DesiredPort>;

/// Ports skipped on rescan because their identity is already claimed and
/// MIDI Learn is inactive: (port_name, port_index, claimed_by_alias).
pub(crate) type AmbiguousSkips = Vec<(String, usize, String)>;

impl InputManager {
    /// Open a single MIDI port and spawn a converter task that tags events with DeviceId
    pub(crate) fn open_port_with_device_id(
        &mut self,
        port_index: usize,
        _port_name: &str,
        device_id: DeviceId,
        event_tx: mpsc::Sender<DeviceEvent<ProtocolEvent>>,
        command_tx: mpsc::Sender<DaemonCommand>,
    ) -> Result<(), String> {
        let mut mgr = MidiDeviceManager::new(String::new(), false);
        mgr.set_device_id(device_id.clone());
        // ADR-026 Phase 1.B: forward the shared probe coordinator so
        // this port's midir callback can observe Identity Replies.
        if let Some(coord) = self.probe_coordinator.as_ref() {
            mgr.set_probe_coordinator(coord.clone());
        }

        // Create intermediate channel for raw MIDI events
        let (midi_event_tx, midi_event_rx) = mpsc::channel::<MidiEvent>(1024);

        mgr.connect_to_port(port_index, midi_event_tx, command_tx)?;

        // ADR-039 §4.3 (#1760): push MidiEvents onto the unified pump via the
        // shared shed-load policy (try_send + drop-newest), replacing the old
        // blocking `send().await`. The per-device metrics handle is retained so
        // `input_source_metrics()` can surface this source's counters.
        let metrics = InputSourceMetricsHandle::new();
        self.midi_source_metrics
            .insert(device_id.clone(), metrics.clone());
        spawn_input_pump(device_id.clone(), midi_event_rx, event_tx, metrics);

        self.midi_managers.insert(device_id, mgr);
        Ok(())
    }

    /// Rescan MIDI ports for hot-plug detection (v4.22.0 - ADR-009 Phase 4)
    ///
    /// Compares currently available ports against open ports in `midi_managers`.
    /// Opens newly discovered ports and marks removed ports as disconnected.
    /// Returns `(opened, removed, rekeyed)` — `rekeyed` is the count of
    /// existing ports whose DeviceId changed because of a `[[bindings]]`
    /// edit (see `compute_rekeys`, #955).
    /// Compute the desired open-port set for a rescan pass (#1476).
    ///
    /// Pure given its inputs so the `BindingResult::Ambiguous` × MIDI
    /// Learn interaction can be unit-tested without real hardware.
    /// Returns `(desired_ports, ambiguous_skipped)` where:
    ///
    /// - `desired_ports` maps port name → `(DeviceId, port_index)` for
    ///   every port that should be open after this rescan.
    /// - `ambiguous_skipped` lists `(port_name, port_index, claimed_by)`
    ///   for ports skipped because their identity is already claimed and
    ///   MIDI Learn is *not* active — the caller emits the
    ///   `ambiguous_port_detected` event for each (needs `&self`).
    ///
    /// The Ambiguous arm mirrors `listen_to_all_ports`: while
    /// `learn_active`, the port is opened under `DeviceId::raw(port_name)`
    /// so the device can be discovered; otherwise it is skipped and
    /// surfaced for the refine-matcher CTA.
    pub(crate) fn build_rescan_desired(
        filtered: &[PortInfo],
        bindings: &[BindingResult],
        listen_mode: ListenMode,
        learn_active: bool,
    ) -> (DesiredPorts, AmbiguousSkips) {
        let mut unbound_instance_counts: HashMap<String, usize> = HashMap::new();
        let mut desired_ports: DesiredPorts = Vec::new();
        let mut ambiguous_skipped: AmbiguousSkips = Vec::new();

        for (port, binding) in filtered.iter().zip(bindings.iter()) {
            match binding {
                BindingResult::Bound { device_id, .. } => {
                    desired_ports.push(DesiredPort {
                        device_id: device_id.clone(),
                        port_name: port.name.clone(),
                        port_index: port.index,
                    });
                }
                BindingResult::Unbound { port_name, .. } => match listen_mode {
                    ListenMode::All => {
                        // #1478: instance-disambiguate duplicate names so
                        // each physical port keeps a distinct DeviceId
                        // (`X`, `X #2`, …) instead of collapsing.
                        let instance = unbound_instance_counts
                            .entry(port_name.clone())
                            .or_insert(0);
                        let device_id = DeviceId::from_port_instance(port_name, *instance);
                        *instance += 1;
                        desired_ports.push(DesiredPort {
                            device_id,
                            port_name: port_name.clone(),
                            port_index: port.index,
                        });
                    }
                    ListenMode::Configured => {}
                },
                BindingResult::Ambiguous {
                    port_name,
                    port_index,
                    claimed_by,
                } => {
                    if learn_active {
                        // Mirror listen_to_all_ports (D11): open ambiguous
                        // ports for MIDI Learn discovery under a raw id.
                        desired_ports.push(DesiredPort {
                            device_id: DeviceId::raw(port_name),
                            port_name: port_name.clone(),
                            port_index: *port_index,
                        });
                    } else {
                        ambiguous_skipped.push((
                            port_name.clone(),
                            *port_index,
                            claimed_by.as_str().to_string(),
                        ));
                    }
                }
            }
        }

        (desired_ports, ambiguous_skipped)
    }

    /// Enumerate current MIDI input ports (name + index). #2390: extracted from
    /// `rescan_ports` so the blocking CoreMIDI call (`MidiInput::new()` +
    /// `.ports()` + per-port `port_name()`) can run OFF the run-loop via
    /// [`Self::enumerate_input_ports_async`]. Returns `Err` if the MIDI client
    /// can't be created — callers MUST skip the rescan on `Err` rather than treat
    /// it as "no ports" (which would close every open port).
    pub(crate) fn enumerate_input_ports() -> Result<Vec<PortInfo>, String> {
        let midi_in = midir::MidiInput::new("Conductor Hot-Plug Rescan")
            .map_err(|e| format!("Failed to create MIDI input for rescan: {}", e))?;
        let ports = midi_in.ports();
        let mut port_infos: Vec<PortInfo> = Vec::with_capacity(ports.len());
        for (index, port) in ports.iter().enumerate() {
            let name = midi_in
                .port_name(port)
                .unwrap_or_else(|_| format!("Port {}", index));
            port_infos.push(PortInfo::new(name, index));
        }
        Ok(port_infos)
    }

    /// Run [`Self::enumerate_input_ports`] on the blocking thread pool so the
    /// ~500ms CoreMIDI enumeration never stalls the async run-loop (#2390).
    /// Mirrors `output_resolver::enumerate_output_ports_async`.
    pub(crate) async fn enumerate_input_ports_async() -> Result<Vec<PortInfo>, String> {
        match tokio::task::spawn_blocking(Self::enumerate_input_ports).await {
            Ok(result) => result,
            Err(e) => Err(format!("Input port enumeration task panicked: {e}")),
        }
    }

    /// Hot-plug rescan diff/open, given a pre-enumerated port list.
    ///
    /// #2390: the blocking CoreMIDI enumeration (`MidiInput::new()` + `.ports()`)
    /// is NO LONGER done here — it scaled with the number of open ports (~500ms
    /// with 22+ ports) and ran inline on the run-loop while holding the
    /// `input_manager` lock, stalling MIDI forwarding every 5s (stuck notes).
    /// Callers now enumerate OFF the run-loop via
    /// [`InputManager::enumerate_input_ports_async`] and pass `port_infos` in, so
    /// the lock is held only for the cheap diff/open below. Taking `port_infos`
    /// also makes this function unit-testable without real hardware.
    pub fn rescan_ports(
        &mut self,
        port_infos: Vec<PortInfo>,
        config: &Config,
        event_tx: &mpsc::Sender<DeviceEvent<ProtocolEvent>>,
        command_tx: &mpsc::Sender<DaemonCommand>,
    ) -> Result<(usize, usize, usize), String> {
        if !self.multi_device_active {
            return Ok((0, 0, 0));
        }

        // #974: GamepadOnly mode skipped MIDI bring-up at startup, but
        // hot-plug rescan would still re-enumerate MIDI on every tick
        // (multi_device_active is set so gamepad-state machinery stays
        // active). Mirror the listen_to_all_ports early-return — no
        // MIDI rescan in GamepadOnly mode. Gamepad hot-plug runs via
        // its own gilrs polling loop and is unaffected.
        if self.mode == super::InputMode::GamepadOnly {
            return Ok((0, 0, 0));
        }

        let advanced = &config.advanced_settings;

        // Step 1 (port enumeration) moved off the run-loop — `port_infos` is
        // passed in already enumerated (#2390). See `enumerate_input_ports`.

        // Resolve the unified endpoint set up-front (ADR-035 Slice 9.5) — same
        // normalized source as listen_to_all_ports, so authored `[[endpoints]]`
        // bind on hot-plug rescan too. Needed BEFORE filtering so Conductor's
        // own MidiVirtualPort outputs are excluded from input scanning even when
        // created via reload/hot-plug (#2054/#2216). Guaranteed-Ok post-load;
        // `?` defensive.
        let endpoints = conductor_core::config::loader::normalize_to_endpoints(config)
            .map_err(|e| format!("normalize endpoints for resolve: {e}"))?
            .0;

        // Step 2: Filter ports (reuse same logic as listen_to_all_ports,
        // including virtual port exclusion D21 — now derived from current config).
        let ignore =
            super::build_input_ignore(&advanced.ignore_ports, &self.exclude_port_names, &endpoints);
        let filtered = super::filter_ports(port_infos, &ignore, advanced.max_midi_ports);

        // Step 3: Resolve ports against the unified endpoint set.
        let bindings = PortResolver::resolve(&filtered, &endpoints);

        // v4.26.0 (D19): Rebuild configured_devices from fresh bindings
        self.configured_devices.clear();
        for binding in &bindings {
            if let BindingResult::Bound { device_id, .. } = binding {
                self.configured_devices.insert(device_id.clone());
            }
        }

        // Build set of port names we should have open, with instance
        // disambiguation. #1476: the Ambiguous arm now honours MIDI Learn
        // exactly like `listen_to_all_ports` — see `build_rescan_desired`.
        let learn_active = self
            .midi_learn_active
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst));
        let (desired_ports, ambiguous_skipped) =
            Self::build_rescan_desired(&filtered, &bindings, advanced.listen_mode, learn_active);

        // #943: surface each skipped-ambiguous port beyond the log so the
        // GUI / MCP / CLI can offer a "refine matcher" CTA. Emission stays
        // in the caller (it needs `&self`); the desired-set computation is
        // pure so it can be unit-tested without hardware.
        for (port_name, port_index, claimed_by) in &ambiguous_skipped {
            warn!(
                port = %port_name,
                claimed_by = %claimed_by,
                "Port ambiguous (identity already claimed) on rescan, skipping"
            );
            self.emit_ambiguous_port_event(port_name, *port_index, claimed_by);
        }

        // Step 4: Snapshot current managers by DeviceId (#1478: identity,
        // not bare port name — two ports can share a name).
        let current_port_names: HashMap<DeviceId, String> = self
            .midi_managers
            .iter()
            .filter_map(|(id, mgr)| mgr.device_info().map(|(_, name)| (id.clone(), name)))
            .collect();
        let current_ids: HashSet<DeviceId> = current_port_names.keys().cloned().collect();

        // Step 5 prep: compute rekeys FIRST so open (Step 6) and remove
        // (Step 7) can exclude rekey endpoints — a rekeyed port is drained
        // from its old key and reopened under its new key by Step 8, so it
        // must not also be opened-new or removed.
        let current_refs: Vec<(DeviceId, &str)> = current_port_names
            .iter()
            .map(|(id, name)| (id.clone(), name.as_str()))
            .collect();
        let rekeys = super::rekey::compute_rekeys(&current_refs, &desired_ports);
        let rekey_old: HashSet<DeviceId> = rekeys.iter().map(|(o, _)| o.clone()).collect();
        let rekey_new: HashSet<DeviceId> = rekeys.iter().map(|(_, n)| n.clone()).collect();
        let desired_ids: HashSet<&DeviceId> = desired_ports.iter().map(|d| &d.device_id).collect();

        // Step 6: Open new ports — desired DeviceId with no current manager
        // and not a rekey target. Keying by DeviceId preserves duplicate
        // port names (each instance has a distinct id like `X`, `X #2`).
        let mut newly_opened = 0;
        for dp in &desired_ports {
            if current_ids.contains(&dp.device_id) || rekey_new.contains(&dp.device_id) {
                continue;
            }
            match self.open_port_with_device_id(
                dp.port_index,
                &dp.port_name,
                dp.device_id.clone(),
                event_tx.clone(),
                command_tx.clone(),
            ) {
                Ok(()) => {
                    newly_opened += 1;
                    info!(
                        device_id = %dp.device_id,
                        port = %dp.port_name,
                        "Hot-plug: opened new MIDI port"
                    );
                }
                Err(e) => {
                    warn!(
                        port = %dp.port_name,
                        error = %e,
                        "Hot-plug: failed to open new MIDI port"
                    );
                }
            }
        }

        // Step 7: Remove ports whose DeviceId is no longer desired and
        // isn't a rekey source (rekey sources are drained by Step 8).
        let mut removed = 0;
        let removed_entries: Vec<(DeviceId, Option<String>)> = current_port_names
            .iter()
            .filter(|(device_id, _)| {
                !desired_ids.contains(device_id) && !rekey_old.contains(*device_id)
            })
            .map(|(device_id, name)| (device_id.clone(), Some(name.clone())))
            .collect();

        for (device_id, port_name) in removed_entries {
            if self.remove_disconnected_manager(&device_id, port_name.as_deref()) {
                removed += 1;
                info!(
                    device_id = %device_id,
                    "Hot-plug: removed disconnected MIDI port"
                );
            }
        }

        // Step 8: Re-key existing ports whose desired DeviceId changed (#955).
        // Two-phase apply (council bug_003 + bug_005, PR #960 review):
        //   - Phase 1 (`drain_rekeys_for_apply`): drain all old keys
        //     from `midi_managers` and migrate `muted_devices` entries
        //     onto the new keys before any reapply.
        //   - Phase 2 (below): reopen each port under its new key.
        // Draining all old keys before any reapply makes alias swaps
        // (A↔B) collision-safe — there's no live entry to overwrite —
        // and migrating mute state in lockstep stops `is_device_enabled`
        // from silently flipping to true after a binding rename.
        let staged = self.drain_rekeys_for_apply(rekeys, &current_port_names, &desired_ports);

        let mut rekeyed = 0usize;
        for entry in staged {
            // Stage mute BEFORE the converter task starts — events on
            // the new task are dropped if `muted_devices.contains(&new_key)`.
            // If the open fails, leaving the entry costs nothing: a
            // hot-plug rescan ≤5s later reopens the port under the same
            // new_key and the mute correctly applies to the new manager.
            if entry.was_muted {
                self.muted_devices.insert(entry.new_key.clone());
            }
            match self.open_port_with_device_id(
                entry.port_index,
                &entry.port_name,
                entry.new_key.clone(),
                event_tx.clone(),
                command_tx.clone(),
            ) {
                Ok(()) => {
                    rekeyed += 1;
                    info!(
                        to = %entry.new_key,
                        port = %entry.port_name,
                        was_muted = entry.was_muted,
                        "Hot-plug: re-keyed MIDI port DeviceId after binding change"
                    );
                }
                Err(e) => {
                    warn!(
                        to = %entry.new_key,
                        port = %entry.port_name,
                        error = %e,
                        "Hot-plug: failed to re-key MIDI port; next hot-plug rescan will recover"
                    );
                }
            }
        }

        Ok((newly_opened, removed, rekeyed))
    }

    /// ADR-026 Phase 1.C: drop a `MidiDeviceManager` from the multi-device
    /// set and notify the probe coordinator so a replug on the same port
    /// name starts from a clean slate.
    ///
    /// Returns `true` when a manager was actually removed (device_id was
    /// present); `false` when the key wasn't in the map.
    ///
    /// Takes `port_name` explicitly (rather than deriving from
    /// `mgr.device_info()`) so hardware-free tests can populate
    /// `midi_managers` with a manager that hasn't actually connected to a
    /// physical port — `device_info()` only returns `Some` after a real
    /// `connect()`, which we can't call in unit tests.
    ///
    /// Ordering note: `probe_coordinator.invalidate(port_name)` MUST fire
    /// before `mgr.disconnect()` so any in-flight probe waiter wakes via
    /// the pending-slot drop immediately rather than waiting out the full
    /// 1-second probe timeout.
    pub(crate) fn remove_disconnected_manager(
        &mut self,
        device_id: &DeviceId,
        port_name: Option<&str>,
    ) -> bool {
        let Some(mut mgr) = self.midi_managers.remove(device_id) else {
            return false;
        };
        // ADR-039 #1760: drop this source's metrics handle alongside its manager
        // so `input_source_metrics()` doesn't report stale counters for a
        // removed device.
        self.midi_source_metrics.remove(device_id);
        if let Some(coord) = self.probe_coordinator.as_ref()
            && let Some(name) = port_name
        {
            coord.invalidate(name);
        }
        mgr.disconnect();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_device::MidiDeviceManager;
    use conductor_core::Config;
    use conductor_core::identity::DeviceId;
    use conductor_core::resolver::{BindingResult, PortInfo, PortResolver};
    use std::collections::HashSet;
    use std::sync::Arc;

    // ----------------------------------------------------------------
    // ADR-026 Phase 1.C — hot-plug invalidate wiring
    // ----------------------------------------------------------------

    #[test]
    fn remove_disconnected_manager_invalidates_probe_cache() {
        // The hot-plug disconnect path must clear the probe
        // coordinator's cached identity for the removed port, so a
        // replug on the same port name runs a fresh probe rather than
        // serving a stale cache hit.
        //
        // Hardware-free setup: seed the coordinator's cache via a
        // successful probe + canned observe_reply, then manually
        // insert a MidiDeviceManager into `midi_managers` and call
        // the extracted helper.
        use conductor_core::device_intelligence::probe::ProbeCoordinator;
        use crossbeam_channel::bounded;
        use std::time::Duration;

        const PORT_NAME: &str = "hot-plug-test-port";

        // Step 1: seed the cache via a real probe cycle. Timeouts are
        // generous (2 s) so CI scheduler jitter doesn't flake the test;
        // locally this completes in < 10 ms.
        let coord = Arc::new(ProbeCoordinator::new().with_timeout(Duration::from_secs(2)));
        let coord_bg = coord.clone();
        let (registered_tx, registered_rx) = bounded::<()>(1);
        let probe_handle = std::thread::spawn(move || {
            coord_bg.probe(PORT_NAME, |_| {
                registered_tx.send(()).unwrap();
                Ok(())
            })
        });
        registered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("probe should register pending slot");

        // KORG Identity Reply — canned, lands via observe_reply and
        // populates the session cache for PORT_NAME.
        let korg_reply: &[u8] = &[
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0xF7,
        ];
        coord.observe_reply(PORT_NAME, korg_reply);
        let probe_outcome = probe_handle
            .join()
            .expect("probe thread should complete without panicking");
        assert!(
            matches!(
                probe_outcome,
                Ok(conductor_core::device_intelligence::probe::ProbeResult::Identified { .. })
            ),
            "probe should return Ok(Identified) after observe_reply; got {:?}",
            probe_outcome
        );
        assert!(
            coord.cached(PORT_NAME).is_some(),
            "cache should be populated after successful probe"
        );

        // Step 2: construct InputManager, wire the coordinator, insert
        // a fresh MidiDeviceManager keyed on a fake device_id.
        let mut mgr = InputManager::new(
            Some("Test".to_string()),
            false,
            super::super::InputMode::MidiOnly,
        );
        mgr.set_probe_coordinator(coord.clone());
        let device_id = DeviceId::from_alias("hot-plug-test");
        mgr.midi_managers.insert(
            device_id.clone(),
            MidiDeviceManager::new(String::new(), false),
        );

        // Step 3: call the extracted helper with the real port_name.
        let removed = mgr.remove_disconnected_manager(&device_id, Some(PORT_NAME));

        // Step 4: assertions.
        assert!(removed, "helper must report that a manager was removed");
        assert!(
            !mgr.midi_managers.contains_key(&device_id),
            "manager must be removed from the map"
        );
        assert!(
            coord.cached(PORT_NAME).is_none(),
            "probe coordinator cache must be cleared for the removed port"
        );
    }

    #[test]
    fn remove_disconnected_manager_no_port_name_still_drops_manager() {
        // Safety: if port_name is None (manager never connected), the
        // helper still removes the manager from the map. The coord
        // can't be notified without a port name, but the cleanup
        // shouldn't fail.
        use conductor_core::device_intelligence::probe::ProbeCoordinator;

        let coord = Arc::new(ProbeCoordinator::new());
        let mut mgr = InputManager::new(
            Some("Test".to_string()),
            false,
            super::super::InputMode::MidiOnly,
        );
        mgr.set_probe_coordinator(coord);
        let device_id = DeviceId::from_alias("no-port-name-test");
        mgr.midi_managers.insert(
            device_id.clone(),
            MidiDeviceManager::new(String::new(), false),
        );

        let removed = mgr.remove_disconnected_manager(&device_id, None);
        assert!(removed);
        assert!(!mgr.midi_managers.contains_key(&device_id));
    }

    #[test]
    fn remove_disconnected_manager_drops_source_metrics() {
        // ADR-039 #1760: a per-device source metrics handle is retained when a
        // port opens and surfaced via `input_source_metrics()`; it must be
        // dropped together with the manager so a removed device leaves no stale
        // counters behind.
        use conductor_core::device_intelligence::probe::ProbeCoordinator;

        let mut mgr = InputManager::new(
            Some("Test".to_string()),
            false,
            super::super::InputMode::MidiOnly,
        );
        mgr.set_probe_coordinator(Arc::new(ProbeCoordinator::new()));
        let device_id = DeviceId::from_alias("metrics-cleanup");
        mgr.midi_managers.insert(
            device_id.clone(),
            MidiDeviceManager::new(String::new(), false),
        );

        let handle = InputSourceMetricsHandle::new();
        handle.record_event();
        mgr.midi_source_metrics.insert(device_id.clone(), handle);

        assert!(
            mgr.input_source_metrics()
                .iter()
                .any(|(id, m)| id == &device_id && m.events_in == 1),
            "accessor surfaces the source's live counters"
        );

        assert!(mgr.remove_disconnected_manager(&device_id, None));
        assert!(
            !mgr.input_source_metrics()
                .iter()
                .any(|(id, _)| id == &device_id),
            "metrics handle is dropped with the manager"
        );
    }

    // NB: the bulk-`disconnect()` clears-source-metrics case is covered by
    // `mod tests::test_disconnect_clears_source_metrics_handles` in mod.rs
    // (PR #2177 — copilot-swe-agent landed that test); this module keeps the
    // per-device hot-plug removal coverage above.

    #[test]
    fn remove_disconnected_manager_returns_false_for_missing_device_id() {
        use conductor_core::device_intelligence::probe::ProbeCoordinator;

        let mut mgr = InputManager::new(
            Some("Test".to_string()),
            false,
            super::super::InputMode::MidiOnly,
        );
        mgr.set_probe_coordinator(Arc::new(ProbeCoordinator::new()));

        let nonexistent = DeviceId::from_alias("never-inserted");
        assert!(
            !mgr.remove_disconnected_manager(&nonexistent, Some("p1")),
            "helper should return false when device_id is not in the map"
        );
    }

    // -------------------------------------------------------------------
    // build_rescan_desired — pure helper exercised below by way of the
    // BindingResult::Ambiguous × MIDI Learn matrix. The compute_rekeys
    // and drain_rekeys_for_apply tests live alongside their helpers in
    // `super::rekey`.
    // -------------------------------------------------------------------

    fn name_contains_matcher(value: &str) -> conductor_core::identity::DeviceMatcher {
        conductor_core::identity::DeviceMatcher::NameContains {
            value: value.to_string(),
        }
    }

    // ADR-035 Slice 9.5: PortResolver now consumes the unified endpoint set,
    // so test fixtures build an input `EndpointConfig` (Matcher/Input) — the
    // shape a legacy binding lowers to.
    fn binding_for(
        alias: &str,
        port_name_substr: &str,
    ) -> conductor_core::config::types::EndpointConfig {
        use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
        EndpointConfig {
            alias: alias.to_string(),
            direction: ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers: vec![name_contains_matcher(port_name_substr)],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }
    }

    #[test]
    fn rescan_opens_ambiguous_port_under_raw_id_when_learn_active() {
        // #1476: rescan must mirror listen_to_all_ports — while MIDI Learn
        // is active, an ambiguous port (identity already claimed by another
        // port) is opened under a raw DeviceId for discovery, not skipped.
        let ports = ["LPD8 One", "LPD8 Two"];
        let port_infos: Vec<PortInfo> = ports
            .iter()
            .enumerate()
            .map(|(i, n)| PortInfo::new(n.to_string(), i))
            .collect();
        let bindings = PortResolver::resolve(&port_infos, &[binding_for("lpd8", "LPD8")]);
        assert!(
            matches!(bindings[1], BindingResult::Ambiguous { .. }),
            "second LPD8 port should resolve Ambiguous; got {:?}",
            bindings[1]
        );

        let (desired, ambiguous_skipped) =
            InputManager::build_rescan_desired(&port_infos, &bindings, ListenMode::All, true);

        assert_eq!(
            desired
                .iter()
                .find(|d| d.port_name == "LPD8 Two")
                .map(|d| d.device_id.clone()),
            Some(DeviceId::raw("LPD8 Two")),
            "ambiguous port must open under raw DeviceId during MIDI Learn"
        );
        assert!(
            ambiguous_skipped.is_empty(),
            "no ambiguous-skip events while learn active: {ambiguous_skipped:?}"
        );
    }

    #[test]
    fn rescan_skips_ambiguous_port_when_learn_inactive() {
        // Learn inactive: ambiguous port stays skipped and is surfaced for
        // the `ambiguous_port_detected` event (existing #943 behaviour).
        let ports = ["LPD8 One", "LPD8 Two"];
        let port_infos: Vec<PortInfo> = ports
            .iter()
            .enumerate()
            .map(|(i, n)| PortInfo::new(n.to_string(), i))
            .collect();
        let bindings = PortResolver::resolve(&port_infos, &[binding_for("lpd8", "LPD8")]);

        let (desired, ambiguous_skipped) =
            InputManager::build_rescan_desired(&port_infos, &bindings, ListenMode::All, false);

        assert!(
            !desired.iter().any(|d| d.port_name == "LPD8 Two"),
            "ambiguous port must be skipped when learn inactive"
        );
        assert_eq!(ambiguous_skipped.len(), 1, "one ambiguous port to emit for");
        assert_eq!(ambiguous_skipped[0].0, "LPD8 Two");
        assert_eq!(ambiguous_skipped[0].2, "lpd8", "claimed_by alias surfaced");
    }

    #[test]
    fn rescan_learn_active_ambiguous_keeps_bound_and_adds_raw_for_duplicate_name() {
        // Two physical ports share the name "LPD8": the first binds the
        // alias, the second resolves Ambiguous. #1478: the desired set is
        // a Vec, so BOTH are preserved — the Bound entry keeps its alias
        // AND the ambiguous port is added under a raw id for MIDI Learn
        // discovery (#1476). Pre-#1478 the name-keyed map collapsed them
        // to one entry.
        let ports = ["LPD8", "LPD8"];
        let port_infos: Vec<PortInfo> = ports
            .iter()
            .enumerate()
            .map(|(i, n)| PortInfo::new(n.to_string(), i))
            .collect();
        let bindings = PortResolver::resolve(&port_infos, &[binding_for("lpd8", "LPD8")]);
        assert!(
            matches!(bindings[0], BindingResult::Bound { .. }),
            "first duplicate-name port should resolve Bound; got {:?}",
            bindings[0]
        );
        assert!(
            matches!(bindings[1], BindingResult::Ambiguous { .. }),
            "second duplicate-name port should resolve Ambiguous; got {:?}",
            bindings[1]
        );

        let (desired, ambiguous_skipped) =
            InputManager::build_rescan_desired(&port_infos, &bindings, ListenMode::All, true);

        let ids: Vec<DeviceId> = desired
            .iter()
            .filter(|d| d.port_name == "LPD8")
            .map(|d| d.device_id.clone())
            .collect();
        assert_eq!(ids.len(), 2, "both duplicate-name ports preserved: {ids:?}");
        assert!(
            ids.contains(&DeviceId::from_alias("lpd8")),
            "Bound alias entry preserved: {ids:?}"
        );
        assert!(
            ids.contains(&DeviceId::raw("LPD8")),
            "ambiguous port added under raw id for learn discovery: {ids:?}"
        );
        assert!(
            ambiguous_skipped.is_empty(),
            "ambiguous ports are not surfaced while learn active"
        );
    }

    #[test]
    fn rescan_preserves_duplicate_unbound_port_names() {
        // #1478 core: two OS ports named "X" under ListenMode::All must
        // yield two distinct desired entries (instance-disambiguated
        // DeviceIds) with distinct port indices — not collapse to one,
        // which previously dropped/clobbered the first port on rescan.
        let port_infos: Vec<PortInfo> = ["X", "X"]
            .iter()
            .enumerate()
            .map(|(i, n)| PortInfo::new(n.to_string(), i))
            .collect();
        let bindings = PortResolver::resolve(&port_infos, &[]);
        let (desired, _ambiguous) =
            InputManager::build_rescan_desired(&port_infos, &bindings, ListenMode::All, false);

        let x: Vec<&DesiredPort> = desired.iter().filter(|d| d.port_name == "X").collect();
        assert_eq!(
            x.len(),
            2,
            "both duplicate-name ports preserved: {desired:?}"
        );
        assert_ne!(
            x[0].device_id, x[1].device_id,
            "duplicate-name ports get distinct DeviceIds"
        );
        assert_ne!(
            x[0].port_index, x[1].port_index,
            "duplicate-name ports keep distinct port indices"
        );
        let ids: HashSet<DeviceId> = x.iter().map(|d| d.device_id.clone()).collect();
        assert!(
            ids.contains(&DeviceId::from_port_instance("X", 0)),
            "first instance present: {ids:?}"
        );
        assert!(
            ids.contains(&DeviceId::from_port_instance("X", 1)),
            "second instance ('X #2') present: {ids:?}"
        );
    }

    // The `compute_rekeys` and `drain_rekeys_for_apply` tests live in
    // `super::rekey::tests` alongside the helpers themselves; see #1684
    // for the per-file size split.

    #[tokio::test]
    async fn rescan_ports_is_noop_in_gamepad_only_mode() {
        // #974 review (Copilot 3168507626): the GamepadOnly early-return
        // in `listen_to_all_ports` sets `multi_device_active = true` so
        // gamepad hot-plug works. But `rescan_ports` was gated only on
        // that flag — once set, every hot-plug tick called
        // `midir::MidiInput::new()` and re-enumerated MIDI ports,
        // reintroducing exactly the MIDI bring-up the GamepadOnly mode
        // is supposed to avoid (and emitting ALSA errors on Linux CI).
        //
        // Fix: rescan_ports must early-return for GamepadOnly. This test
        // simulates the post-listen state (flag set) and asserts the
        // rescan is a no-op. Deterministic on every platform — pre-fix,
        // Linux CI would fail with the ALSA error; macOS dev would
        // return non-zero counts depending on real MIDI ports.
        use crate::daemon::DaemonCommand;
        let mut manager = InputManager::new(None, true, super::super::InputMode::GamepadOnly);
        // Simulate the post-listen state where the flag was set by
        // `enter_multi_device_idle_state` during the GamepadOnly path.
        manager.multi_device_active = true;
        let config = Config::default_config();
        let (event_tx, _event_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(16);
        let (command_tx, _command_rx) = mpsc::channel::<DaemonCommand>(16);

        // #2390: rescan_ports now takes pre-enumerated ports. GamepadOnly
        // early-returns before touching them, so the injected list is unused.
        let result = manager.rescan_ports(vec![], &config, &event_tx, &command_tx);
        assert_eq!(
            result,
            Ok((0, 0, 0)),
            "GamepadOnly mode must skip MIDI rescan — got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn rescan_ports_with_injected_ports_opens_nothing_when_unconfigured_and_configured_mode()
    {
        // #2390 testability win: rescan_ports no longer enumerates hardware, so
        // the diff path can be exercised with injected ports. With
        // `listen_mode=Configured` and no matching endpoints, every port is
        // Unbound → skipped → nothing opened/removed/rekeyed. Deterministic on
        // every platform (no MidiInput::new, no real ports opened).
        use crate::daemon::DaemonCommand;
        use conductor_core::config::types::ListenMode;
        let mut manager = InputManager::new(None, true, super::super::InputMode::MidiOnly);
        manager.multi_device_active = true;
        let mut config = Config::default_config();
        config.advanced_settings.listen_mode = ListenMode::Configured;
        let (event_tx, _event_rx) = mpsc::channel::<DeviceEvent<ProtocolEvent>>(16);
        let (command_tx, _command_rx) = mpsc::channel::<DaemonCommand>(16);

        let injected = vec![
            PortInfo::new("Some Random Synth".to_string(), 0),
            PortInfo::new("Another Device".to_string(), 1),
        ];
        let result = manager.rescan_ports(injected, &config, &event_tx, &command_tx);
        assert_eq!(
            result,
            Ok((0, 0, 0)),
            "unconfigured ports under listen_mode=Configured must not be opened — got: {:?}",
            result
        );
    }
}
