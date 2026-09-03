// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Static analysis of which physical control-state tuples a config reads.
//!
//! ADR-025 Phase 3.F diagnostic. Walks a [`crate::Config`] and returns the
//! set of `(device, channel)` tuples whose Program Change state will be
//! consulted at runtime — either via an `ActivePcIs` condition or a
//! `PcContextSwitch` action. Recurses through structural wrappers
//! (`Conditional`, `Sequence`, `Repeat`, `And`/`Or`/`Not`) including
//! branches nested inside a `CcContextSwitch` (the switch itself reads
//! CC state, but its inner actions may still depend on PC state).
//!
//! Callers surface the inventory at daemon startup, config reload, profile
//! switch, and plan-apply so a user can cross-check against `conductor-state`
//! output to confirm the expected stomps are actually reaching the daemon.
//!
//! The core entry point [`expected_pc_tuples`] is pure — no runtime state,
//! no I/O; safe to call on any `Config` snapshot. [`log_expected_pc_tuples`]
//! is an optional convenience helper that emits log output via `tracing`
//! as a side effect (`info` when the set is non-empty, `debug` otherwise).
//!
//! [`unobserved_pc_tuples`] (Phase 3.F runtime check) is the
//! companion that diffs the expected set against the live
//! [`crate::control_state::PhysicalControlStateStore`]. Its
//! [`log_unobserved_pc_tuples`] helper emits a `tracing::warn!` listing
//! the tuples currently absent from the store snapshot — used by the
//! daemon after a grace period following each config-swap to surface
//! missing hardware / wrong binding alias / unplugged cable conditions
//! proactively, including after an explicit store reset/clear.
//!
//! The `walk_action` / `walk_condition` matches here are deliberately
//! exhaustive (no `_` wildcard) so that adding a new `ActionConfig` or
//! `Condition` variant forces a compile-time decision here rather than
//! silently producing an incomplete inventory.
//!
//! This is deliberately narrower than "all state conditions": it only
//! inventories PC-dependent routing. A future phase can generalise to
//! `CcValueInRange` / `NoteHeld` if an equivalent diagnostic surface is
//! needed for those.

use std::collections::BTreeSet;

use crate::actions::Condition;
use crate::config::types::{ActionConfig, Config};
use crate::control_state::PhysicalControlStateStore;

/// A `(device, channel)` tuple whose PC state the config depends on.
///
/// Ordered first by device alias, then by channel, so log output is stable
/// across runs regardless of config authoring order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExpectedPcTuple {
    pub device: String,
    pub channel: u8,
}

/// Emit an info-level log line summarising the config's PC-state
/// dependencies. `context` is a short label like `"startup"` or
/// `"reload"` included in the log for grepping. No-ops silently
/// (debug-level instead) when the config reads no PC state.
///
/// Side-effect-only helper so callers don't each have to stitch
/// together the formatting + empty-set handling.
pub fn log_expected_pc_tuples(config: &Config, context: &str) {
    let tuples = expected_pc_tuples(config);
    if tuples.is_empty() {
        tracing::debug!(context, "config has no PC-state dependencies");
        return;
    }
    let formatted: Vec<String> = tuples
        .iter()
        // Channels stored 0-indexed; present 1-indexed to match MIDI spec.
        .map(|t| format!("{}:ch{}", t.device, t.channel.saturating_add(1)))
        .collect();
    tracing::info!(
        context,
        tuple_count = tuples.len(),
        "Config reads PC state from: {} — verify with `conductor-state` after exercising hardware",
        formatted.join(", ")
    );
}

/// Compare `expected_pc_tuples(config)` against the live
/// [`PhysicalControlStateStore`] and return the subset **currently absent
/// from the store snapshot** at call time. Used by the runtime
/// observed-vs-expected warning emitted after a grace period following
/// each config-swap (ADR-025 Phase 3.F runtime check).
///
/// "Currently absent" rather than "never observed since daemon start" —
/// the store can be cleared by `reset_control_state` / `drop_device` /
/// `clear_channel`, so an entry that previously existed can vanish from
/// the snapshot. In practice that's rare outside explicit user action.
///
/// Pure — reads the store's snapshot at call time; callers own any
/// timing / scheduling decisions.
pub fn unobserved_pc_tuples(
    config: &Config,
    store: &PhysicalControlStateStore,
) -> BTreeSet<ExpectedPcTuple> {
    filter_unobserved(expected_pc_tuples(config), store)
}

/// Shared predicate used by both [`unobserved_pc_tuples`] and the
/// [`log_unobserved_pc_tuples`] logger — single source of truth for
/// "which expected tuples are absent from the store right now" so the
/// two can't drift if the observation predicate ever changes.
///
/// Delegates the presence check to
/// [`PhysicalControlStateStore::has_program_change`] so the store
/// owns the key-shape / allocation behaviour. See that method's
/// docstring for the current allocation profile.
fn filter_unobserved(
    expected: BTreeSet<ExpectedPcTuple>,
    store: &PhysicalControlStateStore,
) -> BTreeSet<ExpectedPcTuple> {
    expected
        .into_iter()
        .filter(|t| !store.has_program_change(&t.device, t.channel))
        .collect()
}

/// Side-effect helper: run the `expected − observed` diff and emit a
/// `tracing::warn!` with the missing tuples, or a `debug!` message when
/// there's nothing to warn about (distinguishing "config has no PC
/// dependencies" from "all expected tuples observed"). `context` is the
/// same label used by [`log_expected_pc_tuples`] (`"startup"` /
/// `"reload"` / etc.) so the two log lines can be correlated in output.
pub fn log_unobserved_pc_tuples(
    config: &Config,
    store: &PhysicalControlStateStore,
    context: &str,
    grace_secs: u64,
) {
    // Compute expected first so we can distinguish "nothing to check" from
    // "everything observed" — both result in no warning, but readers
    // debugging a quiet log need to know which case they're in.
    let expected = expected_pc_tuples(config);
    let expected_count = expected.len();
    if expected_count == 0 {
        tracing::debug!(
            context,
            grace_secs,
            "config has no PC-state dependencies — nothing to check"
        );
        return;
    }

    // Delegate the actual diff to the shared helper so `unobserved_pc_tuples`
    // and this logger can't drift on the observation predicate.
    let unobserved = filter_unobserved(expected, store);

    if unobserved.is_empty() {
        tracing::debug!(
            context,
            grace_secs,
            expected_count,
            "all {} expected PC tuples observed within grace period",
            expected_count
        );
        return;
    }

    let formatted: Vec<String> = unobserved
        .iter()
        .map(|t| format!("{}:ch{}", t.device, t.channel.saturating_add(1)))
        .collect();
    tracing::warn!(
        context,
        expected_count,
        unobserved_count = unobserved.len(),
        grace_secs,
        "Expected PC state not yet observed after {}s ({}) — {}/{} unobserved: {} — check device connection / binding alias / cable (use `conductor-state` to inspect)",
        grace_secs,
        context,
        unobserved.len(),
        expected_count,
        formatted.join(", ")
    );
}

/// Walk `config` and collect every `(device, channel)` tuple whose PC
/// state is consulted. Includes both explicit `ActivePcIs` conditions
/// inside `Conditional` actions and implicit references from
/// `PcContextSwitch` sugar. Deduplicated.
pub fn expected_pc_tuples(config: &Config) -> BTreeSet<ExpectedPcTuple> {
    let mut out = BTreeSet::new();
    for mode in &config.modes {
        for mapping in &mode.mappings {
            walk_action(&mapping.action, &mut out);
        }
    }
    for mapping in &config.global_mappings {
        walk_action(&mapping.action, &mut out);
    }
    out
}

fn walk_action(action: &ActionConfig, out: &mut BTreeSet<ExpectedPcTuple>) {
    match action {
        ActionConfig::PcContextSwitch {
            channel,
            device,
            mappings,
            default,
            ..
        } => {
            out.insert(ExpectedPcTuple {
                device: device.clone(),
                channel: *channel,
            });
            for (_, branch) in mappings {
                walk_action(branch, out);
            }
            if let Some(d) = default {
                walk_action(d, out);
            }
        }
        ActionConfig::Conditional {
            condition,
            then_action,
            else_action,
        } => {
            walk_condition(condition, out);
            walk_action(then_action, out);
            if let Some(e) = else_action {
                walk_action(e, out);
            }
        }
        ActionConfig::Sequence { actions } => {
            for a in actions {
                walk_action(a, out);
            }
        }
        ActionConfig::Repeat { action, .. } => walk_action(action, out),
        // `CcContextSwitch` itself reads CC state (not PC) so the switch's
        // own `(device, channel)` isn't a PC dependency. But its branches
        // can wrap PC-dependent actions (e.g. a `PcContextSwitch` nested
        // inside a CC range), so we recurse into the inner actions without
        // inserting any tuple for the switch itself.
        ActionConfig::CcContextSwitch {
            ranges, default, ..
        } => {
            for range in ranges {
                walk_action(&range.action, out);
            }
            if let Some(d) = default {
                walk_action(d, out);
            }
        }
        // Leaf action variants — never wrap nested actions, so no recursion
        // and never contribute a PC tuple. Listed explicitly (no wildcard)
        // so adding a new variant to `ActionConfig` will fail to compile
        // here until someone classifies it as leaf vs. structural. A silent
        // miss of a future variant that *does* wrap nested actions would be
        // much worse than the one-line maintenance cost of listing leaves.
        ActionConfig::Keystroke { .. }
        | ActionConfig::Text { .. }
        | ActionConfig::Launch { .. }
        | ActionConfig::Shell { .. }
        | ActionConfig::Delay { .. }
        | ActionConfig::MouseClick { .. }
        | ActionConfig::VolumeControl { .. }
        | ActionConfig::ModeChange { .. }
        | ActionConfig::SendMidi { .. }
        | ActionConfig::MidiForward { .. }
        | ActionConfig::HidForward { .. }
        | ActionConfig::OscSend { .. }
        | ActionConfig::OscForward { .. }
        | ActionConfig::Plugin { .. }
        | ActionConfig::Tap { .. } => {}
    }
}

fn walk_condition(cond: &Condition, out: &mut BTreeSet<ExpectedPcTuple>) {
    match cond {
        Condition::ActivePcIs {
            channel, device, ..
        } => {
            out.insert(ExpectedPcTuple {
                device: device.clone(),
                channel: *channel,
            });
        }
        Condition::And { conditions } | Condition::Or { conditions } => {
            for c in conditions {
                walk_condition(c, out);
            }
        }
        Condition::Not { condition } => walk_condition(condition, out),
        // Leaf condition variants — no nested conditions, no PC reference.
        // Listed explicitly so a future variant (especially one with nested
        // `Condition` children) forces a compile-time decision here.
        Condition::Always
        | Condition::Never
        | Condition::TimeRange { .. }
        | Condition::DayOfWeek { .. }
        | Condition::AppRunning { .. }
        | Condition::AppFrontmost { .. }
        | Condition::ModeIs { .. }
        | Condition::CcValueInRange { .. }
        | Condition::NoteHeld { .. }
        | Condition::CcIsOn { .. }
        | Condition::CcIsOff { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{Mapping, Mode, Trigger};
    use indexmap::IndexMap;

    fn empty_config() -> Config {
        Config {
            mcp: Default::default(),
            per_app_modes: None,
            config_meta: Default::default(),
            security: Default::default(),
            endpoints: vec![],
            modes: vec![],
            global_mappings: vec![],
            logging: None,
            advanced_settings: Default::default(),
            last_selected_mode: None,
            default_mode: None,
            led: None,
            event_console: None,
            routes: vec![],
        }
    }

    fn trigger_note() -> Trigger {
        Trigger::Note {
            note: 36,
            velocity_min: None,
            channel: None,
            device: None,
        }
    }

    fn noop_action() -> ActionConfig {
        ActionConfig::Shell {
            sandbox: None,
            command: "true".to_string(),
            args: None,
            timeout_ms: None,
        }
    }

    fn pc_context_switch(device: &str, channel: u8) -> ActionConfig {
        let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
        mappings.insert(0, Box::new(noop_action()));
        mappings.insert(1, Box::new(noop_action()));
        ActionConfig::PcContextSwitch {
            channel,
            device: device.to_string(),
            mappings,
            default: None,
        }
    }

    fn conditional_active_pc(device: &str, channel: u8, pc: u8) -> ActionConfig {
        ActionConfig::Conditional {
            condition: Condition::ActivePcIs {
                pc,
                channel,
                device: device.to_string(),
            },
            then_action: Box::new(noop_action()),
            else_action: None,
        }
    }

    fn mapping_with(action: ActionConfig) -> Mapping {
        Mapping {
            trigger: trigger_note(),
            action,
            description: None,
            let_through: false,
        }
    }

    fn mode_with(mappings: Vec<Mapping>) -> Mode {
        Mode {
            name: "Test".into(),
            color: None,
            mappings,
        }
    }

    #[test]
    fn empty_config_yields_empty_set() {
        assert!(expected_pc_tuples(&empty_config()).is_empty());
    }

    #[test]
    fn pc_context_switch_contributes_tuple() {
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![mapping_with(pc_context_switch(
            "fcb1010", 0,
        ))])];
        let got = expected_pc_tuples(&cfg);
        assert_eq!(got.len(), 1);
        let t = got.iter().next().unwrap();
        assert_eq!(t.device, "fcb1010");
        assert_eq!(t.channel, 0);
    }

    #[test]
    fn active_pc_is_contributes_tuple() {
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![mapping_with(conditional_active_pc(
            "keyboard", 3, 42,
        ))])];
        let got = expected_pc_tuples(&cfg);
        assert_eq!(got.len(), 1);
        let t = got.iter().next().unwrap();
        assert_eq!(t.device, "keyboard");
        assert_eq!(t.channel, 3);
    }

    /// Different PCs on the same (device, channel) collapse to one tuple —
    /// the PC value doesn't disambiguate the state *source*.
    #[test]
    fn multiple_active_pcs_same_tuple_deduplicate() {
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![
            mapping_with(conditional_active_pc("fcb1010", 0, 12)),
            mapping_with(conditional_active_pc("fcb1010", 0, 13)),
            mapping_with(conditional_active_pc("fcb1010", 0, 14)),
        ])];
        assert_eq!(expected_pc_tuples(&cfg).len(), 1);
    }

    #[test]
    fn distinct_channels_and_devices_yield_distinct_tuples() {
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![
            mapping_with(conditional_active_pc("fcb1010", 0, 12)),
            mapping_with(conditional_active_pc("fcb1010", 1, 12)), // different channel
            mapping_with(conditional_active_pc("mc6", 0, 12)),     // different device
        ])];
        assert_eq!(expected_pc_tuples(&cfg).len(), 3);
    }

    #[test]
    fn active_pc_inside_and_condition_is_found() {
        let cond = Condition::And {
            conditions: vec![
                Condition::ModeIs {
                    mode: "Live".into(),
                },
                Condition::ActivePcIs {
                    pc: 7,
                    channel: 2,
                    device: "pedal".into(),
                },
            ],
        };
        let action = ActionConfig::Conditional {
            condition: cond,
            then_action: Box::new(noop_action()),
            else_action: None,
        };
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![mapping_with(action)])];
        let got = expected_pc_tuples(&cfg);
        assert_eq!(got.len(), 1);
        assert_eq!(got.iter().next().unwrap().channel, 2);
    }

    #[test]
    fn active_pc_inside_not_is_found() {
        let cond = Condition::Not {
            condition: Box::new(Condition::ActivePcIs {
                pc: 9,
                channel: 0,
                device: "x".into(),
            }),
        };
        let action = ActionConfig::Conditional {
            condition: cond,
            then_action: Box::new(noop_action()),
            else_action: None,
        };
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![mapping_with(action)])];
        assert_eq!(expected_pc_tuples(&cfg).len(), 1);
    }

    #[test]
    fn conditional_walks_both_branches() {
        let then_branch = pc_context_switch("then_dev", 0);
        let else_branch = pc_context_switch("else_dev", 0);
        let action = ActionConfig::Conditional {
            condition: Condition::Always,
            then_action: Box::new(then_branch),
            else_action: Some(Box::new(else_branch)),
        };
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![mapping_with(action)])];
        let got = expected_pc_tuples(&cfg);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn sequence_walks_nested_actions() {
        let action = ActionConfig::Sequence {
            actions: vec![noop_action(), pc_context_switch("a", 0), noop_action()],
        };
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![mapping_with(action)])];
        assert_eq!(expected_pc_tuples(&cfg).len(), 1);
    }

    #[test]
    fn repeat_walks_its_action() {
        let action = ActionConfig::Repeat {
            action: Box::new(pc_context_switch("b", 0)),
            count: 3,
            delay_ms: None,
        };
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![mapping_with(action)])];
        assert_eq!(expected_pc_tuples(&cfg).len(), 1);
    }

    #[test]
    fn global_mappings_are_walked() {
        let mut cfg = empty_config();
        cfg.global_mappings = vec![mapping_with(pc_context_switch("global_dev", 5))];
        let got = expected_pc_tuples(&cfg);
        assert_eq!(got.len(), 1);
        assert_eq!(got.iter().next().unwrap().device, "global_dev");
    }

    /// `CcContextSwitch`'s own (device, channel) is CC state, not PC, so
    /// the switch itself contributes no PC tuple. A future
    /// "expected_cc_tuples" helper can cover CC inventory in parallel.
    #[test]
    fn cc_context_switch_itself_contributes_no_pc_tuple() {
        use crate::config::types::CcRange;
        let action = ActionConfig::CcContextSwitch {
            cc: 7,
            channel: 0,
            device: "not_pc".into(),
            ranges: vec![CcRange {
                min: 0,
                max: 63,
                action: Box::new(noop_action()),
            }],
            default: None,
        };
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![mapping_with(action)])];
        assert!(expected_pc_tuples(&cfg).is_empty());
    }

    /// CcContextSwitch branches can wrap PC-dependent actions. We must
    /// recurse through its ranges + default, collecting PC tuples from
    /// the inner actions — without contributing the switch's own
    /// (device, channel) as a PC tuple.
    #[test]
    fn pc_dependency_nested_inside_cc_context_switch_is_found() {
        use crate::config::types::CcRange;
        let action = ActionConfig::CcContextSwitch {
            cc: 7,
            channel: 0,
            device: "cc_src".into(), // not a PC dependency
            ranges: vec![
                CcRange {
                    min: 0,
                    max: 63,
                    action: Box::new(pc_context_switch("nested_in_low", 2)),
                },
                CcRange {
                    min: 64,
                    max: 127,
                    action: Box::new(conditional_active_pc("nested_in_high", 3, 5)),
                },
            ],
            default: Some(Box::new(pc_context_switch("nested_in_default", 4))),
        };
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![mapping_with(action)])];
        let got = expected_pc_tuples(&cfg);

        // Three PC tuples from the nested actions; no `cc_src` tuple.
        assert_eq!(got.len(), 3, "tuples: {got:?}");
        let devices: Vec<&str> = got.iter().map(|t| t.device.as_str()).collect();
        assert!(devices.contains(&"nested_in_low"));
        assert!(devices.contains(&"nested_in_high"));
        assert!(devices.contains(&"nested_in_default"));
        assert!(!devices.contains(&"cc_src"));
    }

    #[test]
    fn tuples_are_sorted_by_device_then_channel() {
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![
            mapping_with(conditional_active_pc("zeta", 3, 12)),
            mapping_with(conditional_active_pc("alpha", 5, 12)),
            mapping_with(conditional_active_pc("alpha", 1, 12)),
        ])];
        let got: Vec<_> = expected_pc_tuples(&cfg).into_iter().collect();
        assert_eq!(got[0].device, "alpha");
        assert_eq!(got[0].channel, 1);
        assert_eq!(got[1].device, "alpha");
        assert_eq!(got[1].channel, 5);
        assert_eq!(got[2].device, "zeta");
    }

    // ────────────────────────────────────────────────────────────
    // ADR-025 Phase 3.F runtime check
    // ────────────────────────────────────────────────────────────

    use crate::control_state::PhysicalControlStateStore;
    use crate::event_processor::MidiEvent;
    use std::time::Instant;

    fn cfg_expecting_fcb1010_ch0() -> Config {
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![mapping_with(pc_context_switch(
            "fcb1010", 0,
        ))])];
        cfg
    }

    #[test]
    fn unobserved_empty_when_config_has_no_pc_deps() {
        let cfg = empty_config();
        let store = PhysicalControlStateStore::default();
        assert!(unobserved_pc_tuples(&cfg, &store).is_empty());
    }

    #[test]
    fn unobserved_returns_all_when_store_empty() {
        let cfg = cfg_expecting_fcb1010_ch0();
        let store = PhysicalControlStateStore::default();
        let got = unobserved_pc_tuples(&cfg, &store);
        assert_eq!(got.len(), 1);
        let t = got.iter().next().unwrap();
        assert_eq!(t.device, "fcb1010");
        assert_eq!(t.channel, 0);
    }

    #[test]
    fn unobserved_empty_when_all_expected_tuples_observed() {
        let cfg = cfg_expecting_fcb1010_ch0();
        let store = PhysicalControlStateStore::default();
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 12,
                channel: 0,
                time: Instant::now(),
            },
        );
        assert!(unobserved_pc_tuples(&cfg, &store).is_empty());
    }

    /// Any PC value counts as "observed" — the store just needs a key
    /// for `(device, channel)`, not a particular PC number. A user who
    /// configured branches for PC 12/13/14 and has only stomped PC 0
    /// still has the tuple observed; their branch coverage is a separate
    /// concern.
    #[test]
    fn unobserved_accepts_any_pc_value_as_observation() {
        let cfg = cfg_expecting_fcb1010_ch0();
        let store = PhysicalControlStateStore::default();
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 99,
                channel: 0,
                time: Instant::now(),
            },
        );
        assert!(unobserved_pc_tuples(&cfg, &store).is_empty());
    }

    /// Channel mismatch: config expects ch0 but the store only has ch1 —
    /// tuple still counts as unobserved (channel is part of the key).
    #[test]
    fn unobserved_treats_channel_mismatch_as_unobserved() {
        let cfg = cfg_expecting_fcb1010_ch0();
        let store = PhysicalControlStateStore::default();
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 12,
                channel: 1, // wrong channel
                time: Instant::now(),
            },
        );
        assert_eq!(unobserved_pc_tuples(&cfg, &store).len(), 1);
    }

    /// Device-name mismatch: store key uses a different alias/port string
    /// than the config references. Remains unobserved.
    #[test]
    fn unobserved_treats_device_mismatch_as_unobserved() {
        let cfg = cfg_expecting_fcb1010_ch0();
        let store = PhysicalControlStateStore::default();
        store.observe_event(
            "different_device",
            &MidiEvent::ProgramChange {
                program: 12,
                channel: 0,
                time: Instant::now(),
            },
        );
        assert_eq!(unobserved_pc_tuples(&cfg, &store).len(), 1);
    }

    /// Multi-tuple config with partial observation: only the missing
    /// tuples surface.
    #[test]
    fn unobserved_is_the_set_difference_of_expected_minus_observed() {
        let mut cfg = empty_config();
        cfg.modes = vec![mode_with(vec![
            mapping_with(conditional_active_pc("fcb1010", 0, 12)),
            mapping_with(conditional_active_pc("keyboard", 2, 5)),
            mapping_with(conditional_active_pc("pedal", 3, 1)),
        ])];
        let store = PhysicalControlStateStore::default();
        // Observe only the middle one.
        store.observe_event(
            "keyboard",
            &MidiEvent::ProgramChange {
                program: 7,
                channel: 2,
                time: Instant::now(),
            },
        );
        let got = unobserved_pc_tuples(&cfg, &store);
        assert_eq!(got.len(), 2);
        let devices: Vec<&str> = got.iter().map(|t| t.device.as_str()).collect();
        assert!(devices.contains(&"fcb1010"));
        assert!(devices.contains(&"pedal"));
        assert!(!devices.contains(&"keyboard"));
    }
}
