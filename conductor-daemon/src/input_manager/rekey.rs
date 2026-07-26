// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Two-phase rekey apply for [`super::InputManager`] (#955, council bug_003 / bug_005).
//!
//! Split out from [`super::rescan`] so each submodule stays under the
//! LLM Council per-file ceiling (#1684). Holds `compute_rekeys` (pure
//! helper), the `StagedRekey` work item, and the `drain_rekeys_for_apply`
//! impl method that phase 2 of `rescan_ports` consumes.

use super::InputManager;
use super::rescan::DesiredPort;
use conductor_core::identity::DeviceId;
use std::collections::HashMap;

/// Work item emitted by `drain_rekeys_for_apply` for phase 2 to reapply.
/// Carries everything the reapply needs — the new DeviceId to insert
/// under, the OS port info, and whether the device was muted before
/// the rekey so phase 2 can reinsert the mute under the new key.
#[derive(Debug)]
pub(crate) struct StagedRekey {
    pub(crate) new_key: DeviceId,
    pub(crate) port_name: String,
    pub(crate) port_index: usize,
    pub(crate) was_muted: bool,
}

impl InputManager {
    /// Phase 1 of the rekey: drain the old DeviceId entries from
    /// `midi_managers` and migrate `muted_devices` entries to follow
    /// the device. Returns staged work items for the caller to reapply
    /// via `open_port_with_device_id` in phase 2.
    ///
    /// Two-phase apply matters because:
    ///
    /// 1. **Cycles / swaps**: a single-pass loop calling
    ///    `HashMap::insert` would silently overwrite a live entry on
    ///    `alias_a ↔ alias_b` swap; `MidiDeviceManager::Drop` then
    ///    closes the orphaned midir port. By draining all old keys
    ///    *before* any reapply, no collision is possible. (Council
    ///    bug_005, PR #960 review.)
    /// 2. **Hot-swap not possible**: the converter task spawned in
    ///    `open_port_with_device_id` captures DeviceId by clone at
    ///    task-start time and can't be hot-swapped — so close+reopen
    ///    is required regardless.
    /// 3. **Mute state lives in a separate set** (`muted_devices`)
    ///    keyed by DeviceId. Without explicit migration the mute
    ///    orphans under the old key and `is_device_enabled(&new_key)`
    ///    silently returns true, subverting the user's choice.
    ///    (Council bug_003.)
    pub(crate) fn drain_rekeys_for_apply(
        &mut self,
        rekeys: Vec<(DeviceId, DeviceId)>,
        current_port_names: &HashMap<DeviceId, String>,
        desired_ports: &[DesiredPort],
    ) -> Vec<StagedRekey> {
        let mut staged = Vec::new();
        for (old_key, new_key) in rekeys {
            let Some(port_name) = current_port_names.get(&old_key).cloned() else {
                continue;
            };
            // #1478: look up the port_index by the *new* DeviceId, not by
            // port_name — duplicate names would otherwise return the wrong
            // sibling's index.
            let Some(port_index) = desired_ports
                .iter()
                .find(|d| d.device_id == new_key)
                .map(|d| d.port_index)
            else {
                continue;
            };
            let was_muted = self.muted_devices.remove(&old_key);
            self.remove_disconnected_manager(&old_key, Some(&port_name));
            staged.push(StagedRekey {
                new_key,
                port_name,
                port_index,
                was_muted,
            });
        }
        staged
    }
}

/// Given the current (DeviceId → port_name) state of `midi_managers`
/// and the freshly-built `desired_ports` map from `rescan_ports`'s
/// step 4, compute the set of (old_key, new_key) pairs that need
/// re-keying.
///
/// Consulting `desired_ports` directly (rather than re-resolving via
/// `PortResolver` inside this helper) keeps DeviceId disambiguation
/// consistent with the rest of `rescan_ports`. The map's values are
/// `from_port_instance(name, n)` for unbound ports — including the
/// `#2`/`#3` suffix used to keep duplicate port names from colliding
/// — so the rekeys this returns will never propose collapsing
/// `"X #2"` onto `"X"`. (Copilot #960 review.)
///
/// Used by `rescan_ports` and by `reload_config` to handle the case
/// where a port stays open but its desired DeviceId changes — e.g.
/// adding a `[[bindings]]` for an existing raw port flips it from
/// `DeviceId::raw("X")` to `DeviceId::from_alias("alias")`. Without
/// this re-key step, `midi_managers` keeps the stale key and
/// `get_device_bindings` returns the wrong DeviceId (#955).
///
/// Returns only entries where the DeviceId actually changed. Ports
/// missing from `desired_ports` (e.g. unplugged between rescans) are
/// dropped — `rescan_ports` handles those via its own remove path.
/// Match current managers to desired ports and emit `(old_id, new_id)`
/// pairs for ports whose DeviceId must change.
///
/// #1478: matching is two-pass so duplicate port names don't collapse:
///
///   1. **Exact DeviceId match → stable.** A current port whose id is
///      already in the desired set keeps it (no rekey). This is what
///      makes an unchanged duplicate pair (`X`, `X #2`) a no-op instead
///      of collapsing both onto one id.
///   2. **Name match → rekey.** Each remaining current port is matched,
///      greedily in order, to an as-yet-unclaimed desired entry with the
///      same `port_name`; if the desired DeviceId differs, that's a
///      rekey (e.g. raw `X` → alias after a binding is added).
///
/// Each desired entry is claimed at most once, so two current ports
/// sharing a name can never both rekey onto the same desired id.
pub(crate) fn compute_rekeys(
    current: &[(DeviceId, &str)],
    desired_ports: &[DesiredPort],
) -> Vec<(DeviceId, DeviceId)> {
    let mut claimed = vec![false; desired_ports.len()];
    let mut current_matched = vec![false; current.len()];
    let mut rekeys = Vec::new();

    // Pass 1: exact DeviceId matches are stable — claim both sides.
    for (ci, (cur_id, _)) in current.iter().enumerate() {
        for (di, dp) in desired_ports.iter().enumerate() {
            if !claimed[di] && dp.device_id == *cur_id {
                claimed[di] = true;
                current_matched[ci] = true;
                break;
            }
        }
    }

    // Pass 2: remaining current ports match by name to an unclaimed
    // desired entry; emit a rekey when the DeviceId differs.
    for (ci, (cur_id, port_name)) in current.iter().enumerate() {
        if current_matched[ci] {
            continue;
        }
        for (di, dp) in desired_ports.iter().enumerate() {
            if !claimed[di] && dp.port_name == *port_name {
                claimed[di] = true;
                current_matched[ci] = true;
                if dp.device_id != *cur_id {
                    rekeys.push((cur_id.clone(), dp.device_id.clone()));
                }
                break;
            }
        }
    }

    rekeys
}

#[cfg(test)]
mod tests {
    use super::super::InputMode;
    use super::super::rescan::DesiredPort;
    use super::*;
    use crate::midi_device::MidiDeviceManager;
    use conductor_core::ListenMode;
    use conductor_core::identity::DeviceId;
    use conductor_core::resolver::{PortInfo, PortResolver};
    use std::collections::{HashMap, HashSet};

    // -------------------------------------------------------------------
    // compute_rekeys — pure helper backing the #955 fix.
    //
    // The bug: when `[[bindings]]` is added/removed/edited at runtime,
    // `rescan_ports` re-runs PortResolver but only opens new and closes
    // removed ports. Existing ports keep their old keys in midi_managers
    // (e.g. raw "X" stays raw even when a binding now resolves it to
    // alias "y"). Re-keying is the missing step.
    // -------------------------------------------------------------------

    fn name_contains_matcher(value: &str) -> conductor_core::identity::DeviceMatcher {
        conductor_core::identity::DeviceMatcher::NameContains {
            value: value.to_string(),
        }
    }

    // ADR-035 Slice 9.5: PortResolver consumes the unified endpoint set; build
    // an input `EndpointConfig` (Matcher/Input) — the shape a legacy binding
    // lowers to.
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

    /// Build the desired-port set the way `rescan_ports` does, by calling
    /// the real `build_rescan_desired`, so `compute_rekeys` tests run
    /// against production output. Uses `ListenMode::All` with MIDI Learn
    /// inactive — bound ports get the alias DeviceId, unbound ports get
    /// `from_port_instance(name, n)` for instance disambiguation.
    fn desired_for_test(
        port_names: &[&str],
        bindings: &[conductor_core::config::types::EndpointConfig],
    ) -> Vec<DesiredPort> {
        let port_infos: Vec<PortInfo> = port_names
            .iter()
            .enumerate()
            .map(|(i, n)| PortInfo::new(n.to_string(), i))
            .collect();
        let resolved = PortResolver::resolve(&port_infos, bindings);
        let (desired, _ambiguous) =
            InputManager::build_rescan_desired(&port_infos, &resolved, ListenMode::All, false);
        desired
    }

    #[test]
    fn rekeys_raw_port_to_configured_alias_when_binding_added() {
        // Pre-state: port open under raw DeviceId (no binding existed).
        // New config adds a binding matching the same port name.
        // Expectation: re-key to alias.
        let port = "Komplete Audio 6 MK2";
        let current = vec![(DeviceId::raw(port), port)];
        let desired = desired_for_test(&[port], &[binding_for("fab", port)]);

        let rekeys = compute_rekeys(&current, &desired);
        assert_eq!(rekeys.len(), 1);
        assert_eq!(rekeys[0].0, DeviceId::raw(port));
        assert_eq!(rekeys[0].1, DeviceId::from_alias("fab"));
    }

    #[test]
    fn rekeys_alias_to_new_alias_when_binding_renamed() {
        let port = "Mikro";
        let current = vec![(DeviceId::from_alias("old_name"), port)];
        let desired = desired_for_test(&[port], &[binding_for("new_name", port)]);

        let rekeys = compute_rekeys(&current, &desired);
        assert_eq!(rekeys.len(), 1);
        assert_eq!(rekeys[0].0, DeviceId::from_alias("old_name"));
        assert_eq!(rekeys[0].1, DeviceId::from_alias("new_name"));
    }

    #[test]
    fn rekeys_alias_back_to_raw_when_binding_removed() {
        // Binding deleted: port that was bound to alias should drop
        // back to raw DeviceId so events still flow (listen_mode=All).
        let port = "TouchOSC";
        let current = vec![(DeviceId::from_alias("touchosc"), port)];
        let desired = desired_for_test(&[port], &[]); // no bindings

        let rekeys = compute_rekeys(&current, &desired);
        assert_eq!(rekeys.len(), 1);
        assert_eq!(rekeys[0].0, DeviceId::from_alias("touchosc"));
        assert_eq!(rekeys[0].1, DeviceId::raw(port));
    }

    #[test]
    fn no_rekey_when_resolution_unchanged() {
        let port = "X";
        let current = vec![(DeviceId::from_alias("x"), port)];
        let desired = desired_for_test(&[port], &[binding_for("x", port)]);

        let rekeys = compute_rekeys(&current, &desired);
        assert!(
            rekeys.is_empty(),
            "stable resolution must not produce rekey entries: {:?}",
            rekeys
        );
    }

    #[test]
    fn handles_multiple_ports_with_mixed_changes() {
        // Three ports: A renamed, B unchanged, C newly bound.
        let current = vec![
            (DeviceId::from_alias("a_old"), "A"),
            (DeviceId::from_alias("b"), "B"),
            (DeviceId::raw("C"), "C"),
        ];
        let desired = desired_for_test(
            &["A", "B", "C"],
            &[
                binding_for("a_new", "A"),
                binding_for("b", "B"),
                binding_for("c_alias", "C"),
            ],
        );

        let rekeys = compute_rekeys(&current, &desired);
        // A renamed + C newly bound = 2 changes; B unchanged.
        assert_eq!(
            rekeys.len(),
            2,
            "expected exactly 2 rekeys, got {:?}",
            rekeys
        );
        let pairs: HashSet<(String, String)> = rekeys
            .iter()
            .map(|(o, n)| (o.as_str().to_string(), n.as_str().to_string()))
            .collect();
        assert!(pairs.contains(&("a_old".to_string(), "a_new".to_string())));
        assert!(pairs.iter().any(|(o, n)| o == "C" && n == "c_alias"));
    }

    #[test]
    fn drops_ports_not_in_resolved_set() {
        // Port unplugged between rescan and rekey: handled by the
        // remove path, not by us. Just don't crash.
        let current = vec![(DeviceId::from_alias("x"), "X")];
        let desired: Vec<DesiredPort> = Vec::new(); // no ports

        let rekeys = compute_rekeys(&current, &desired);
        assert!(rekeys.is_empty());
    }

    #[test]
    fn rekeys_use_disambiguated_device_id_for_duplicate_port_names() {
        // #1478 (was a #960-review workaround): two ports named "X" under
        // listen_mode=All resolve to instance-disambiguated DeviceIds
        // (`X` instance 0, `X #2` instance 1). With the duplicate-
        // preserving Vec representation, `desired_for_test` mints both
        // entries (pre-#1478 the name-keyed map kept only the second).
        // A stable pair already open under those ids must emit ZERO
        // rekeys — exact-DeviceId matches are recognised as unchanged,
        // so neither port collapses onto the other's id.
        let desired = desired_for_test(&["X", "X"], &[]);
        assert_eq!(desired.len(), 2, "duplicate names preserved: {desired:?}");

        // Current managers are already open under the same disambiguated
        // ids the rescan derived.
        let current: Vec<(DeviceId, &str)> = desired
            .iter()
            .map(|d| (d.device_id.clone(), d.port_name.as_str()))
            .collect();

        let rekeys = compute_rekeys(&current, &desired);
        assert!(
            rekeys.is_empty(),
            "stable duplicate pair must not rekey: {rekeys:?}"
        );
    }

    // -------------------------------------------------------------------
    // drain_rekeys_for_apply — phase 1 of the two-phase rekey apply.
    //
    // Council found two correctness bugs in the original single-pass
    // rekey loop (PR #960 review):
    //
    //   bug_003 — `muted_devices: HashSet<DeviceId>` not migrated when
    //             DeviceId changes. User mutes a port → adds binding →
    //             reload silently unmutes (and `device_status` reports
    //             enabled:true, masking the regression).
    //
    //   bug_005 — alias swap A↔B: the loop's `HashMap::insert` overwrites
    //             a live entry, and `MidiDeviceManager::Drop` closes the
    //             midir port. Silent data loss until the next 5s hot-plug
    //             rescan reopens the orphaned port.
    //
    // Fix: drain all old keys first (closing their managers and migrating
    // mute state in lockstep), then phase 2 reopens under the new keys.
    // No collision possible because every old key is removed before any
    // new key gets inserted.
    // -------------------------------------------------------------------

    fn raw_mgr() -> MidiDeviceManager {
        MidiDeviceManager::new(String::new(), false)
    }

    #[test]
    fn drain_migrates_muted_devices_to_new_key() {
        // bug_003: mute state must follow the device across a rekey.
        let mut mgr = InputManager::new(Some("Test".to_string()), false, InputMode::MidiOnly);
        let old_key = DeviceId::raw("LPD8");
        let new_key = DeviceId::from_alias("lpd");
        mgr.midi_managers.insert(old_key.clone(), raw_mgr());
        mgr.muted_devices.insert(old_key.clone());

        let mut current_port_names = HashMap::new();
        current_port_names.insert(old_key.clone(), "LPD8".to_string());

        let desired_ports = vec![DesiredPort {
            device_id: new_key.clone(),
            port_name: "LPD8".to_string(),
            port_index: 0,
        }];

        let staged = mgr.drain_rekeys_for_apply(
            vec![(old_key.clone(), new_key.clone())],
            &current_port_names,
            &desired_ports,
        );

        assert_eq!(staged.len(), 1);
        assert!(
            staged[0].was_muted,
            "drain must capture pre-rekey mute state"
        );
        assert!(
            !mgr.muted_devices.contains(&old_key),
            "old_key must be removed from muted_devices after drain"
        );
        // The new_key entry isn't inserted by drain itself — phase 2
        // does that — but `was_muted: true` carries the signal forward.
        assert!(!mgr.midi_managers.contains_key(&old_key));
    }

    #[test]
    fn drain_does_not_touch_muted_devices_when_old_key_was_unmuted() {
        // Defensive: if the user never muted, the drain shouldn't
        // accidentally insert a stale entry.
        let mut mgr = InputManager::new(Some("Test".to_string()), false, InputMode::MidiOnly);
        let old_key = DeviceId::raw("X");
        let new_key = DeviceId::from_alias("x_alias");
        mgr.midi_managers.insert(old_key.clone(), raw_mgr());

        let mut current_port_names = HashMap::new();
        current_port_names.insert(old_key.clone(), "X".to_string());
        let desired_ports = vec![DesiredPort {
            device_id: new_key.clone(),
            port_name: "X".to_string(),
            port_index: 0,
        }];

        let staged = mgr.drain_rekeys_for_apply(
            vec![(old_key, new_key)],
            &current_port_names,
            &desired_ports,
        );
        assert!(!staged[0].was_muted);
    }

    #[test]
    fn drain_handles_alias_swap_without_collision() {
        // bug_005: swap of (alias_a ↔ alias_b) must not drop either
        // port. Pre-fix the single-pass loop overwrote the second
        // alias's live manager via HashMap::insert; with two-phase
        // apply, both old keys are drained before any reapply, so
        // no collision is possible.
        let mut mgr = InputManager::new(Some("Test".to_string()), false, InputMode::MidiOnly);
        let alias_a = DeviceId::from_alias("alias_a");
        let alias_b = DeviceId::from_alias("alias_b");
        mgr.midi_managers.insert(alias_a.clone(), raw_mgr());
        mgr.midi_managers.insert(alias_b.clone(), raw_mgr());

        let mut current_port_names = HashMap::new();
        current_port_names.insert(alias_a.clone(), "port_a".to_string());
        current_port_names.insert(alias_b.clone(), "port_b".to_string());

        let desired_ports = vec![
            DesiredPort {
                device_id: alias_b.clone(),
                port_name: "port_a".to_string(),
                port_index: 0,
            },
            DesiredPort {
                device_id: alias_a.clone(),
                port_name: "port_b".to_string(),
                port_index: 1,
            },
        ];

        let staged = mgr.drain_rekeys_for_apply(
            vec![
                (alias_a.clone(), alias_b.clone()),
                (alias_b.clone(), alias_a.clone()),
            ],
            &current_port_names,
            &desired_ports,
        );

        // Both staged correctly with port info preserved.
        assert_eq!(staged.len(), 2);
        let staged_for: HashMap<&str, &str> = staged
            .iter()
            .map(|s| (s.new_key.as_str(), s.port_name.as_str()))
            .collect();
        assert_eq!(staged_for.get("alias_b"), Some(&"port_a"));
        assert_eq!(staged_for.get("alias_a"), Some(&"port_b"));

        // Both old managers drained — phase 2 reapply will insert
        // fresh entries with no live entry to overwrite.
        assert!(mgr.midi_managers.is_empty());
    }

    #[test]
    fn drain_skips_rekey_when_port_name_lookup_fails() {
        // Defensive: if `current_port_names` doesn't have the old_key
        // (manager already removed in step 6, or never present), drain
        // skips that pair without crashing or staging garbage.
        let mut mgr = InputManager::new(Some("Test".to_string()), false, InputMode::MidiOnly);
        let staged = mgr.drain_rekeys_for_apply(
            vec![(DeviceId::raw("ghost"), DeviceId::from_alias("ghost_alias"))],
            &HashMap::new(),
            &[],
        );
        assert!(staged.is_empty());
    }
}
