// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Parse-time lowering (ADR-025 Phase 2.E).
//!
//! Translates `ActionConfig` trees into the runtime `Action` AST,
//! expanding the context-switch sugar variants (`PcContextSwitch`,
//! `CcContextSwitch`) into nested `Conditional` chains before they
//! reach the executor. The runtime evaluator sees one shape —
//! `Conditional` — for all state-based routing, regardless of how
//! the config author wrote it.
//!
//! # Why lowering (vs a new runtime variant)
//!
//! Keeping a single `Action::Conditional` evaluator has compounding
//! value:
//!
//! 1. **No divergence.** A change to `CcValueInRange` semantics (e.g.
//!    a future inclusive-vs-exclusive bounds tweak) propagates to
//!    every context-switch config automatically because the sugar
//!    lowers to the canonical form.
//! 2. **Composition for free.** Context-switch sugar nested inside
//!    composite actions — `Sequence { actions: [CcContextSwitch(...)] }`,
//!    `Conditional { then_action: PcContextSwitch(...) }`, or a
//!    `Repeat { action: CcContextSwitch(...) }` — Just Works™ because
//!    the composite variants recurse into [`lower_action`] before
//!    execution. No new runtime plumbing needed.
//! 3. **Matches the implementation spec** (§2.3) without introducing
//!    runtime variants that need their own dispatcher.
//!
//! # Ordering contract
//!
//! Authoring order in TOML == evaluation priority. The first branch
//! authored becomes the **outermost** `Conditional`, so its THEN
//! fires first and its ELSE falls through to the next branch. We
//! walk `mappings.into_iter().rev()` to build the chain from the
//! inside out: start with the default (or no-op) as the innermost
//! else, then wrap each earlier branch around it.
//!
//! # Escape hatch for large tables
//!
//! When a context-switch has more than [`MAX_LINEAR_BRANCHES`] branches,
//! a deeply-nested `Conditional` chain becomes a suboptimal shape —
//! O(n) worst-case evaluation and a deep AST. For that case the
//! lowering emits [`Action::ContextSwitchTable`] instead, which the
//! dispatcher can evaluate with O(1) hash lookup (PC) or a linear
//! scan over inclusive ranges (CC). Same `default` and `device` /
//! `channel` semantics; just a different runtime shape.
//!
//! The common case (≤64 branches) keeps the simpler nested
//! `Conditional` form — it composes trivially with `Not` / `And` /
//! `Or`, tracks cleanly through the Events panel routing trace, and
//! hits the same evaluator as every other state-based routing.

use crate::actions::{Action, Condition, ContextBranchTable, ContextKind, LoweringSource};
use crate::config::types::{ActionConfig, CcRange};
use indexmap::IndexMap;
use std::collections::HashMap;

/// Threshold above which a context-switch AST is emitted as
/// [`Action::ContextSwitchTable`] instead of a nested
/// [`Action::Conditional`] chain.
///
/// The choice of 64 balances two concerns: most real configs have
/// far fewer branches (FCB1010 has 10 presets × 5 banks = 50 at most),
/// so the common case keeps the simpler nested-Conditional shape;
/// rare high-branch configs (MIDI program banks, large zone tables)
/// get the O(1) lookup path. MIDI PC is 7-bit so 128 is the absolute
/// upper bound — half that is a reasonable pivot.
pub const MAX_LINEAR_BRANCHES: usize = 64;

/// Lower an [`ActionConfig`] tree to a runtime [`Action`].
///
/// Context-switch sugar (`PcContextSwitch`, `CcContextSwitch`) is
/// expanded into nested `Conditional` chains. All other variants
/// pass through [`Action::from`] unchanged, except for composite
/// variants (`Sequence`, `Conditional`, `Repeat`) where nested
/// children are recursively lowered so sugar buried deep in the
/// tree still gets rewritten.
///
/// See the module docstring for the ordering contract and oversize-
/// branch handling.
pub fn lower_action(cfg: ActionConfig) -> Action {
    match cfg {
        ActionConfig::PcContextSwitch {
            channel,
            device,
            mappings,
            default,
        } => {
            if mappings.len() > MAX_LINEAR_BRANCHES {
                lower_pc_to_table(channel, device, mappings, default)
            } else {
                lower_pc_to_nested_conditional(channel, device, mappings, default)
            }
        }
        ActionConfig::CcContextSwitch {
            cc,
            channel,
            device,
            ranges,
            default,
        } => {
            if ranges.len() > MAX_LINEAR_BRANCHES {
                lower_cc_to_table(cc, channel, device, ranges, default)
            } else {
                lower_cc_to_nested_conditional(cc, channel, device, ranges, default)
            }
        }

        // Composite variants: recurse so nested sugar gets lowered too.
        ActionConfig::Sequence { actions } => {
            Action::Sequence(actions.into_iter().map(lower_action).collect())
        }
        ActionConfig::Conditional {
            condition,
            then_action,
            else_action,
        } => Action::Conditional {
            condition,
            then_action: Box::new(lower_action(*then_action)),
            else_action: else_action.map(|a| Box::new(lower_action(*a))),
        },
        ActionConfig::Repeat {
            action,
            count,
            delay_ms,
        } => Action::Repeat {
            action: Box::new(lower_action(*action)),
            count,
            delay_ms,
        },

        // Leaf variants: delegate to the existing From<ActionConfig>
        // impl. Keeps the translation table as a single source of
        // truth for non-sugar actions.
        other => Action::from(other),
    }
}

/// Walk branches in reverse so the first-authored branch becomes the
/// outermost `Conditional` — i.e. authoring order == evaluation
/// priority.
fn lower_pc_to_nested_conditional(
    channel: u8,
    device: String,
    mappings: IndexMap<u8, Box<ActionConfig>>,
    default: Option<Box<ActionConfig>>,
) -> Action {
    // Innermost else: the default action (or a no-op Sequence if
    // omitted). Conditions that match nothing and have no default
    // silently do nothing, matching the ADR-025 D7 "absence ≠ match"
    // rule at the action layer.
    let mut acc: Action = default
        .map(|a| lower_action(*a))
        .unwrap_or(Action::Sequence(vec![]));

    for (pc, action) in mappings.into_iter().rev() {
        acc = Action::Conditional {
            condition: Condition::ActivePcIs {
                pc,
                channel,
                device: device.clone(),
            },
            then_action: Box::new(lower_action(*action)),
            else_action: Some(Box::new(acc)),
        };
    }
    acc
}

/// Same shape as [`lower_pc_to_nested_conditional`] but keyed on
/// `CcValueInRange` instead of `ActivePcIs`.
fn lower_cc_to_nested_conditional(
    cc: u8,
    channel: u8,
    device: String,
    ranges: Vec<CcRange>,
    default: Option<Box<ActionConfig>>,
) -> Action {
    let mut acc: Action = default
        .map(|a| lower_action(*a))
        .unwrap_or(Action::Sequence(vec![]));

    for range in ranges.into_iter().rev() {
        acc = Action::Conditional {
            condition: Condition::CcValueInRange {
                cc,
                channel,
                min: range.min,
                max: range.max,
                device: device.clone(),
            },
            then_action: Box::new(lower_action(*range.action)),
            else_action: Some(Box::new(acc)),
        };
    }
    acc
}

/// Lower an oversize PcContextSwitch to [`Action::ContextSwitchTable`]
/// with a `HashMap<u8, _>` branch table. O(1) lookup at dispatch time
/// regardless of branch count.
///
/// Inner actions are recursively lowered so nested sugar (e.g. a
/// Conditional with a CcIsOn condition inside one of the branches)
/// still gets expanded.
fn lower_pc_to_table(
    channel: u8,
    device: String,
    mappings: IndexMap<u8, Box<ActionConfig>>,
    default: Option<Box<ActionConfig>>,
) -> Action {
    let mut table: HashMap<u8, Box<Action>> = HashMap::with_capacity(mappings.len());
    for (pc, action) in mappings {
        table.insert(pc, Box::new(lower_action(*action)));
    }
    Action::ContextSwitchTable {
        kind: ContextKind::Pc,
        channel,
        device,
        branches: ContextBranchTable::Pc(table),
        default: default.map(|a| Box::new(lower_action(*a))),
        source: LoweringSource {
            origin: "PcContextSwitch".to_string(),
            branch_index: None,
        },
    }
}

/// Lower an oversize CcContextSwitch to [`Action::ContextSwitchTable`]
/// with an in-order `Vec<(min, max, action)>` range table. Dispatch
/// is first-match-wins in authoring order. The validator (task #26)
/// enforces non-overlap so the first match is also the only match.
fn lower_cc_to_table(
    cc: u8,
    channel: u8,
    device: String,
    ranges: Vec<CcRange>,
    default: Option<Box<ActionConfig>>,
) -> Action {
    let ranges_lowered: Vec<(u8, u8, Box<Action>)> = ranges
        .into_iter()
        .map(|r| (r.min, r.max, Box::new(lower_action(*r.action))))
        .collect();
    Action::ContextSwitchTable {
        kind: ContextKind::Cc,
        channel,
        device,
        branches: ContextBranchTable::Cc {
            cc,
            ranges: ranges_lowered,
        },
        default: default.map(|a| Box::new(lower_action(*a))),
        source: LoweringSource {
            origin: "CcContextSwitch".to_string(),
            branch_index: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Behavioural tests live in `conductor-core/tests/context_switch_
    // lowering_test.rs` — they exercise lower_action through the public
    // API and pin the structural invariants (ordering, default-as-tail,
    // recursive lowering). This inline module keeps a few quick-running
    // smoke tests for the helper functions themselves.

    #[test]
    fn max_linear_branches_is_power_of_two_for_hot_path_friendliness() {
        // Purely a lint: future reviewers may want to bump the
        // threshold. Powers of two are nicer for modular arithmetic
        // if the ContextSwitchTable uses a bitmask shard. Kept as an
        // assertion so the intent is visible and any future change
        // is deliberate.
        assert!(MAX_LINEAR_BRANCHES.is_power_of_two());
    }

    #[test]
    fn lower_shell_passes_through() {
        let action = lower_action(ActionConfig::Shell {
            sandbox: None,
            command: "echo test".into(),
            args: None,
            timeout_ms: None,
        });
        match action {
            Action::Shell { command, args, .. } => {
                assert_eq!(command, "echo test");
                assert_eq!(args, None);
            }
            other => panic!("expected Shell, got {:?}", other),
        }
    }
}
