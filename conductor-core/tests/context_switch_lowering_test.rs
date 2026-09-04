// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-025 Phase 2.E — parse-time lowering of context-switch sugar.
//!
//! `lower_action(ActionConfig) -> Action` expands the Phase 2.D sugar
//! (`PcContextSwitch` / `CcContextSwitch`) into nested `Conditional`
//! chains keyed by `ActivePcIs` / `CcValueInRange`. For ≤64 branches
//! (MAX_LINEAR_BRANCHES) this is a direct translation — each branch
//! becomes one layer of a Conditional chain where THEN fires the
//! branch action and ELSE falls through to the next branch, with the
//! optional default as the innermost else.
//!
//! The >64 branch case lowers to [`Action::ContextSwitchTable`]
//! (Phase 2.F), with a HashMap-keyed branch table for PC and a
//! range-scan table for CC. Pinned by the
//! `lower_*_oversize_emits_context_switch_table` tests at the
//! bottom of this file.
//!
//! Invariants this test file pins:
//!   (i)   Authoring order == evaluation priority. First branch wraps
//!         everything else.
//!   (ii)  The default branch — when present — is the innermost else.
//!         When omitted, a no-op (`Sequence([])`) is the innermost else.
//!   (iii) Nested sugar (e.g. PcContextSwitch inside a Sequence) is
//!         also lowered recursively.
//!   (iv)  End-to-end: the ADR-025 FCB1010-shape TOML config lowers to
//!         an Action that would route correctly when executed.

use conductor_core::Condition;
use conductor_core::actions::Action;
use conductor_core::config::compile::lower_action;
use conductor_core::config::types::{ActionConfig, CcRange};
use indexmap::IndexMap;

// ────────────────────────────────────────────────────────────────
// Test helpers
// ────────────────────────────────────────────────────────────────

fn shell(command: &str) -> ActionConfig {
    ActionConfig::Shell {
        sandbox: None,
        command: command.into(),
        args: None,
        timeout_ms: None,
    }
}

fn boxed_shell(command: &str) -> Box<ActionConfig> {
    Box::new(shell(command))
}

// Walk a Conditional-chain and collect the branches as
// (condition, then-command-string) pairs plus the innermost else
// "tail" Action, so the test assertions focus on structure rather than
// replicating the entire walk every time.
fn walk_chain(action: Action) -> (Vec<(Condition, String)>, Action) {
    let mut branches = Vec::new();
    let mut cursor = action;
    loop {
        match cursor {
            Action::Conditional {
                condition,
                then_action,
                else_action,
            } => {
                let then_cmd = match *then_action {
                    Action::Shell {
                        command: cmd,
                        args: _,
                        ..
                    } => cmd,
                    other => format!("<non-shell: {:?}>", other),
                };
                branches.push((condition, then_cmd));
                cursor = else_action.map(|a| *a).unwrap_or(Action::Sequence(vec![]));
            }
            tail => return (branches, tail),
        }
    }
}

// ────────────────────────────────────────────────────────────────
// PcContextSwitch → nested Conditional
// ────────────────────────────────────────────────────────────────

#[test]
fn lower_pc_context_switch_empty_mappings_returns_default_or_noop() {
    // No branches + no default → innermost else is the no-op tail.
    let cfg = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings: IndexMap::new(),
        default: None,
    };
    let action = lower_action(cfg);
    match action {
        Action::Sequence(v) if v.is_empty() => {}
        other => panic!("expected no-op Sequence([]), got {:?}", other),
    }
}

#[test]
fn lower_pc_context_switch_default_only_returns_default_action() {
    // No branches, default present → lowered form is just the default.
    let cfg = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings: IndexMap::new(),
        default: Some(boxed_shell("echo fallback")),
    };
    match lower_action(cfg) {
        Action::Shell {
            command: cmd,
            args: _,
            ..
        } => assert_eq!(cmd, "echo fallback"),
        other => panic!("expected Shell(echo fallback), got {:?}", other),
    }
}

#[test]
fn lower_pc_context_switch_single_branch() {
    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    mappings.insert(12, boxed_shell("echo pc12"));

    let cfg = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings,
        default: Some(boxed_shell("echo fallback")),
    };
    let (branches, tail) = walk_chain(lower_action(cfg));

    assert_eq!(branches.len(), 1);
    match &branches[0].0 {
        Condition::ActivePcIs {
            pc,
            channel,
            device,
        } => {
            assert_eq!(*pc, 12);
            assert_eq!(*channel, 0);
            assert_eq!(device, "fcb1010");
        }
        other => panic!("expected ActivePcIs, got {:?}", other),
    }
    assert_eq!(branches[0].1, "echo pc12");
    match tail {
        Action::Shell {
            command: cmd,
            args: _,
            ..
        } => assert_eq!(cmd, "echo fallback"),
        other => panic!("expected fallback shell, got {:?}", other),
    }
}

#[test]
fn lower_pc_context_switch_preserves_authoring_order() {
    // Invariant (i): first authored branch becomes the OUTERMOST
    // Conditional. Evaluation short-circuits on first match so
    // authoring order == priority order.
    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    mappings.insert(42, boxed_shell("echo first"));
    mappings.insert(12, boxed_shell("echo second"));
    mappings.insert(99, boxed_shell("echo third"));

    let cfg = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings,
        default: Some(boxed_shell("echo fallback")),
    };
    let (branches, _tail) = walk_chain(lower_action(cfg));

    let ordered_pcs: Vec<u8> = branches
        .iter()
        .map(|(cond, _)| match cond {
            Condition::ActivePcIs { pc, .. } => *pc,
            other => panic!("unexpected condition {:?}", other),
        })
        .collect();
    assert_eq!(
        ordered_pcs,
        vec![42, 12, 99],
        "first authored branch must be outermost in the lowered chain"
    );
}

#[test]
fn lower_pc_context_switch_default_is_innermost_else() {
    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    mappings.insert(1, boxed_shell("echo one"));
    mappings.insert(2, boxed_shell("echo two"));

    let cfg = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings,
        default: Some(boxed_shell("echo default")),
    };
    let (_branches, tail) = walk_chain(lower_action(cfg));
    match tail {
        Action::Shell {
            command: cmd,
            args: _,
            ..
        } => assert_eq!(cmd, "echo default"),
        other => panic!("tail should be the default action, got {:?}", other),
    }
}

// ────────────────────────────────────────────────────────────────
// CcContextSwitch → nested Conditional
// ────────────────────────────────────────────────────────────────

#[test]
fn lower_cc_context_switch_preserves_ranges_and_order() {
    let cfg = ActionConfig::CcContextSwitch {
        cc: 1,
        channel: 0,
        device: "keyboard".into(),
        ranges: vec![
            CcRange {
                min: 0,
                max: 31,
                action: boxed_shell("echo low"),
            },
            CcRange {
                min: 32,
                max: 95,
                action: boxed_shell("echo mid"),
            },
            CcRange {
                min: 96,
                max: 127,
                action: boxed_shell("echo high"),
            },
        ],
        default: Some(boxed_shell("echo fallback")),
    };
    let (branches, tail) = walk_chain(lower_action(cfg));

    assert_eq!(branches.len(), 3);
    let seen: Vec<(u8, u8, String)> = branches
        .iter()
        .map(|(cond, cmd)| match cond {
            Condition::CcValueInRange {
                cc,
                channel,
                min,
                max,
                device,
            } => {
                assert_eq!(*cc, 1);
                assert_eq!(*channel, 0);
                assert_eq!(device, "keyboard");
                (*min, *max, cmd.clone())
            }
            other => panic!("expected CcValueInRange, got {:?}", other),
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (0, 31, "echo low".into()),
            (32, 95, "echo mid".into()),
            (96, 127, "echo high".into()),
        ],
        "ranges must be emitted in authoring order"
    );
    match tail {
        Action::Shell {
            command: cmd,
            args: _,
            ..
        } => assert_eq!(cmd, "echo fallback"),
        other => panic!("expected fallback tail, got {:?}", other),
    }
}

#[test]
fn lower_cc_context_switch_no_default_gives_noop_tail() {
    let cfg = ActionConfig::CcContextSwitch {
        cc: 1,
        channel: 0,
        device: "keyboard".into(),
        ranges: vec![CcRange {
            min: 64,
            max: 127,
            action: boxed_shell("echo loud"),
        }],
        default: None,
    };
    let (_branches, tail) = walk_chain(lower_action(cfg));
    match tail {
        Action::Sequence(v) if v.is_empty() => {}
        other => panic!("expected no-op Sequence([]), got {:?}", other),
    }
}

// ────────────────────────────────────────────────────────────────
// Recursive lowering — sugar nested in other containers
// ────────────────────────────────────────────────────────────────

#[test]
fn lower_recurses_into_sequence() {
    // PcContextSwitch inside a Sequence must ALSO get lowered —
    // otherwise a mixed config would leave sugar in the runtime AST.
    let mut inner: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    inner.insert(12, boxed_shell("echo pc"));

    let cfg = ActionConfig::Sequence {
        actions: vec![
            shell("echo before"),
            ActionConfig::PcContextSwitch {
                channel: 0,
                device: "fcb1010".into(),
                mappings: inner,
                default: None,
            },
            shell("echo after"),
        ],
    };

    match lower_action(cfg) {
        Action::Sequence(actions) => {
            assert_eq!(actions.len(), 3);
            assert!(
                matches!(&actions[0], Action::Shell { command: c, args: _, .. } if c == "echo before")
            );
            // Middle must be a lowered Conditional chain, NOT leftover sugar.
            assert!(
                matches!(
                    &actions[1],
                    Action::Conditional {
                        condition: Condition::ActivePcIs { .. },
                        ..
                    }
                ),
                "middle element should be a lowered Conditional, got {:?}",
                actions[1]
            );
            assert!(
                matches!(&actions[2], Action::Shell { command: c, args: _, .. } if c == "echo after")
            );
        }
        other => panic!("expected Sequence, got {:?}", other),
    }
}

#[test]
fn lower_recurses_into_conditional_branches() {
    // A Conditional whose then/else contain sugar.
    let mut branches: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    branches.insert(1, boxed_shell("echo one"));

    let cfg = ActionConfig::Conditional {
        condition: Condition::Always,
        then_action: Box::new(ActionConfig::PcContextSwitch {
            channel: 0,
            device: "fcb1010".into(),
            mappings: branches,
            default: None,
        }),
        else_action: Some(boxed_shell("echo else")),
    };

    match lower_action(cfg) {
        Action::Conditional {
            then_action,
            else_action,
            ..
        } => {
            assert!(matches!(
                *then_action,
                Action::Conditional {
                    condition: Condition::ActivePcIs { .. },
                    ..
                }
            ));
            assert!(
                matches!(&*else_action.unwrap(), Action::Shell { command: c, args: _, .. } if c == "echo else")
            );
        }
        other => panic!("expected Conditional, got {:?}", other),
    }
}

#[test]
fn lower_recurses_into_repeat_inner() {
    let mut branches: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    branches.insert(5, boxed_shell("echo five"));

    let cfg = ActionConfig::Repeat {
        action: Box::new(ActionConfig::PcContextSwitch {
            channel: 0,
            device: "fcb1010".into(),
            mappings: branches,
            default: None,
        }),
        count: 3,
        delay_ms: Some(10),
    };

    match lower_action(cfg) {
        Action::Repeat { action, count, .. } => {
            assert_eq!(count, 3);
            assert!(matches!(
                *action,
                Action::Conditional {
                    condition: Condition::ActivePcIs { .. },
                    ..
                }
            ));
        }
        other => panic!("expected Repeat, got {:?}", other),
    }
}

// Field extractors that PANIC on the wrong variant (no silent fallthrough),
// returning the inner pieces so these tests assert exact FIELD VALUES of
// the lowered nodes — not just the enum discriminant. A regression that lowers
// a branch/default to a Conditional with the WRONG condition fields or inner
// action is then caught, not just a regression that leaves raw sugar.
fn expect_active_pc(action: &Action) -> (u8, u8, &str, &Action) {
    match action {
        Action::Conditional {
            condition:
                Condition::ActivePcIs {
                    pc,
                    channel,
                    device,
                },
            then_action,
            ..
        } => (*pc, *channel, device.as_str(), then_action),
        other => panic!("expected ActivePcIs Conditional, got {other:?}"),
    }
}

fn expect_cc_range(action: &Action) -> (u8, u8, u8, u8, &str, &Action) {
    match action {
        Action::Conditional {
            condition:
                Condition::CcValueInRange {
                    cc,
                    channel,
                    min,
                    max,
                    device,
                },
            then_action,
            ..
        } => (*cc, *channel, *min, *max, device.as_str(), then_action),
        other => panic!("expected CcValueInRange Conditional, got {other:?}"),
    }
}

fn expect_shell(action: &Action) -> &str {
    match action {
        Action::Shell { command, .. } => command.as_str(),
        other => panic!("expected Shell, got {other:?}"),
    }
}

#[test]
fn lower_recurses_into_pc_context_switch_branch() {
    // Every other switch test uses Shell leaves for branch actions, so
    // recursion INTO a context-switch branch was unpinned. A branch action that
    // is itself sugar (here a CcContextSwitch) must be recursively lowered to
    // the CORRECT CcValueInRange Conditional — not left as sugar, and not a
    // differently-keyed/valued Conditional.
    let inner_cc = ActionConfig::CcContextSwitch {
        cc: 7,
        channel: 0,
        device: "modwheel".into(),
        ranges: vec![CcRange {
            min: 0,
            max: 63,
            action: boxed_shell("echo lo"),
        }],
        default: Some(boxed_shell("echo cc-default")),
    };
    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    mappings.insert(12, Box::new(inner_cc));

    let cfg = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings,
        default: Some(boxed_shell("echo pc-default")),
    };

    let lowered = lower_action(cfg);
    // Outer: ActivePcIs(pc=12) branch wrapping the lowered inner CC switch.
    let (pc, channel, device, branch_action) = expect_active_pc(&lowered);
    assert_eq!(
        (pc, channel, device),
        (12, 0, "fcb1010"),
        "outer PC branch fields"
    );

    // The branch action must be the LOWERED CcContextSwitch: a CcValueInRange
    // Conditional carrying the inner switch's exact fields and its range action.
    let (cc, ch, min, max, dev, range_then) = expect_cc_range(branch_action);
    assert_eq!(
        (cc, ch, min, max, dev),
        (7, 0, 0, 63, "modwheel"),
        "PC branch action must be lowered to the inner CcValueInRange (exact fields)"
    );
    assert_eq!(expect_shell(range_then), "echo lo", "inner range action");
}

#[test]
fn lower_recurses_into_cc_context_switch_range() {
    // A CC RANGE action that is itself sugar (here a PcContextSwitch)
    // must be recursively lowered to the correct ActivePcIs Conditional.
    let mut inner_pc_map: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    inner_pc_map.insert(3, boxed_shell("echo pc3"));
    let inner_pc = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings: inner_pc_map,
        default: None,
    };

    let cfg = ActionConfig::CcContextSwitch {
        cc: 7,
        channel: 0,
        device: "modwheel".into(),
        ranges: vec![CcRange {
            min: 64,
            max: 127,
            action: Box::new(inner_pc),
        }],
        default: Some(boxed_shell("echo cc-default")),
    };

    let lowered = lower_action(cfg);
    let (cc, channel, min, max, device, range_action) = expect_cc_range(&lowered);
    assert_eq!(
        (cc, channel, min, max, device),
        (7, 0, 64, 127, "modwheel"),
        "outer CC range fields"
    );

    // The range action must be the LOWERED PcContextSwitch.
    let (pc, ch, dev, pc_then) = expect_active_pc(range_action);
    assert_eq!(
        (pc, ch, dev),
        (3, 0, "fcb1010"),
        "CC range action must be lowered to the inner ActivePcIs (exact fields)"
    );
    assert_eq!(expect_shell(pc_then), "echo pc3", "inner PC branch action");
}

#[test]
fn lower_recurses_into_context_switch_default() {
    // A switch DEFAULT that is itself sugar must be lowered (the default
    // becomes the innermost else of the chain). Here a CcContextSwitch whose
    // default is a PcContextSwitch.
    let mut inner_pc_map: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    inner_pc_map.insert(9, boxed_shell("echo pc9"));
    let inner_pc = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings: inner_pc_map,
        default: None,
    };

    let cfg = ActionConfig::CcContextSwitch {
        cc: 7,
        channel: 0,
        device: "modwheel".into(),
        ranges: vec![CcRange {
            min: 0,
            max: 63,
            action: boxed_shell("echo lo"),
        }],
        default: Some(Box::new(inner_pc)),
    };

    // One range + default → Conditional(CcValueInRange, then=Shell, else=<default>).
    let lowered = lower_action(cfg);
    let default_action = match lowered {
        Action::Conditional { else_action, .. } => {
            *else_action.expect("default present → innermost else")
        }
        other => panic!("expected outer CcValueInRange Conditional, got {other:?}"),
    };

    // The default must be the LOWERED PcContextSwitch (exact fields + action).
    let (pc, ch, dev, pc_then) = expect_active_pc(&default_action);
    assert_eq!(
        (pc, ch, dev),
        (9, 0, "fcb1010"),
        "switch default must be lowered to the inner ActivePcIs (exact fields)"
    );
    assert_eq!(
        expect_shell(pc_then),
        "echo pc9",
        "inner default branch action"
    );
}

#[test]
fn lower_passes_through_leaf_actions() {
    // Non-sugar variants must round-trip via lower_action identically
    // to calling .into() directly. Keeps the translation table as a
    // single source of truth — no drift between Action::from and
    // lower_action for ordinary variants.
    let leaves = vec![
        ActionConfig::Shell {
            sandbox: None,
            command: "echo x".into(),
            args: None,
            timeout_ms: None,
        },
        ActionConfig::Text { text: "hi".into() },
        ActionConfig::Launch {
            app: "Calculator".into(),
        },
        ActionConfig::Delay { ms: 100 },
    ];
    for cfg in leaves {
        let via_lower = lower_action(cfg.clone());
        let via_from: Action = Action::from(cfg);
        // Both paths use the same Action variant — we check a shallow
        // invariant (variant type) rather than PartialEq because
        // `Action` doesn't derive PartialEq for all its variants.
        assert_eq!(
            std::mem::discriminant(&via_lower),
            std::mem::discriminant(&via_from),
            "lower_action should match From<ActionConfig> for leaf variants"
        );
    }
}

// ────────────────────────────────────────────────────────────────
// End-to-end: the FCB1010 TOML → lowered Action
// ────────────────────────────────────────────────────────────────

#[test]
fn fcb1010_config_lowers_to_working_action_tree() {
    // The canonical shape from the ADR-025 spec: stomp a preset (PC
    // on the Komplete-bound DIN channel), then the expression pedal
    // CC (mapped elsewhere) routes through a PcContextSwitch that
    // selects different SendMidi targets. This end-to-end test proves
    // that the TOML the user authors today produces a fully-
    // executable nested Conditional chain after lowering.
    use conductor_core::config::types::Config;

    let toml_input = r#"
[[modes]]
name = "Default"

[[modes.mappings]]

[modes.mappings.trigger]
type = "CC"
cc = 7
channel = 0
device = "fcb1010"

[modes.mappings.action]
type = "PcContextSwitch"
channel = 0
device = "fcb1010"

[modes.mappings.action.mappings.12]
type = "Shell"
command = "echo lead"

[modes.mappings.action.mappings.13]
type = "Shell"
command = "echo rhythm"

[modes.mappings.action.mappings.14]
type = "Shell"
command = "echo clean"

[modes.mappings.action.default]
type = "Shell"
command = "echo default"
"#;

    let cfg: Config = toml::from_str(toml_input).expect("valid config");
    let action_config = cfg.modes[0].mappings[0].action.clone();
    let lowered = lower_action(action_config);

    let (branches, tail) = walk_chain(lowered);
    assert_eq!(branches.len(), 3, "three PC branches expected");
    let pcs: Vec<u8> = branches
        .iter()
        .map(|(cond, _)| match cond {
            Condition::ActivePcIs { pc, .. } => *pc,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(pcs, vec![12, 13, 14]);
    match tail {
        Action::Shell {
            command: cmd,
            args: _,
            ..
        } => assert_eq!(cmd, "echo default"),
        other => panic!("expected default tail, got {:?}", other),
    }
}

// ────────────────────────────────────────────────────────────────
// >64 branch escape hatch — ContextSwitchTable (Phase 2.F)
// ────────────────────────────────────────────────────────────────
//
// Beyond MAX_LINEAR_BRANCHES, a nested `Conditional` chain becomes a
// deep AST with O(n) worst-case evaluation. `Action::ContextSwitchTable`
// is the escape hatch: O(1) hash lookup for PC, linear scan (small N
// so no binary search yet) for CC ranges. The lowering preserves
// branch actions and default.

#[test]
fn lower_pc_context_switch_oversize_emits_context_switch_table() {
    use conductor_core::actions::{ContextBranchTable, ContextKind};

    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    for pc in 0..=99 {
        mappings.insert(pc, boxed_shell(&format!("echo pc{}", pc)));
    }
    assert!(
        mappings.len() > conductor_core::config::compile::MAX_LINEAR_BRANCHES,
        "test must exceed threshold"
    );
    let cfg = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings,
        default: Some(boxed_shell("echo fallback")),
    };
    match lower_action(cfg) {
        Action::ContextSwitchTable {
            kind,
            channel,
            device,
            branches,
            default,
            source,
        } => {
            assert_eq!(kind, ContextKind::Pc);
            assert_eq!(channel, 0);
            assert_eq!(device, "fcb1010");
            match &branches {
                ContextBranchTable::Pc(map) => {
                    assert_eq!(map.len(), 100);
                    // Spot-check a few entries
                    match &**map.get(&12).expect("PC 12 branch") {
                        Action::Shell {
                            command: cmd,
                            args: _,
                            ..
                        } => assert_eq!(cmd, "echo pc12"),
                        other => panic!("expected Shell, got {:?}", other),
                    }
                    match &**map.get(&99).expect("PC 99 branch") {
                        Action::Shell {
                            command: cmd,
                            args: _,
                            ..
                        } => assert_eq!(cmd, "echo pc99"),
                        other => panic!("expected Shell, got {:?}", other),
                    }
                }
                other => panic!("expected Pc branch table, got {:?}", other),
            }
            let default_action = default.expect("default should round-trip");
            match &*default_action {
                Action::Shell {
                    command: cmd,
                    args: _,
                    ..
                } => assert_eq!(cmd, "echo fallback"),
                other => panic!("expected default Shell, got {:?}", other),
            }
            assert!(
                source.origin.contains("PcContextSwitch"),
                "LoweringSource should identify origin as PcContextSwitch, got: {}",
                source.origin
            );
        }
        other => panic!(
            "oversize PcContextSwitch must lower to ContextSwitchTable; got {:?}",
            other
        ),
    }
}

#[test]
fn lower_pc_context_switch_oversize_with_no_default_has_none_default() {
    use conductor_core::actions::ContextKind;

    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    for pc in 0..=70 {
        mappings.insert(pc, boxed_shell("echo x"));
    }
    let cfg = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings,
        default: None,
    };
    match lower_action(cfg) {
        Action::ContextSwitchTable { kind, default, .. } => {
            assert_eq!(kind, ContextKind::Pc);
            assert!(default.is_none(), "no default → None, not a no-op action");
        }
        other => panic!("expected ContextSwitchTable, got {:?}", other),
    }
}

#[test]
fn lower_cc_context_switch_oversize_emits_context_switch_table() {
    use conductor_core::actions::{ContextBranchTable, ContextKind};

    let mut ranges = Vec::new();
    for i in 0..=99u8 {
        ranges.push(CcRange {
            min: i,
            max: i,
            action: boxed_shell(&format!("echo v{}", i)),
        });
    }
    assert!(ranges.len() > conductor_core::config::compile::MAX_LINEAR_BRANCHES);
    let cfg = ActionConfig::CcContextSwitch {
        cc: 1,
        channel: 0,
        device: "keyboard".into(),
        ranges,
        default: Some(boxed_shell("echo fallback")),
    };
    match lower_action(cfg) {
        Action::ContextSwitchTable {
            kind,
            channel,
            device,
            branches,
            default,
            source,
        } => {
            assert_eq!(kind, ContextKind::Cc);
            assert_eq!(channel, 0);
            assert_eq!(device, "keyboard");
            match &branches {
                ContextBranchTable::Cc {
                    cc: watched_cc,
                    ranges: ranges_vec,
                } => {
                    assert_eq!(*watched_cc, 1, "watched CC number preserved");
                    assert_eq!(ranges_vec.len(), 100);
                    // The spec says "(min, max, action); sorted,
                    // non-overlapping". Authoring order is ascending
                    // here; the lowering preserves that.
                    assert_eq!(ranges_vec[0].0, 0, "first range min");
                    assert_eq!(ranges_vec[0].1, 0, "first range max");
                    assert_eq!(ranges_vec[99].0, 99);
                    assert_eq!(ranges_vec[99].1, 99);
                }
                other => panic!("expected Cc branch table, got {:?}", other),
            }
            assert!(default.is_some(), "default should round-trip");
            assert!(
                source.origin.contains("CcContextSwitch"),
                "LoweringSource origin should identify CcContextSwitch, got: {}",
                source.origin
            );
        }
        other => panic!(
            "oversize CcContextSwitch must lower to ContextSwitchTable; got {:?}",
            other
        ),
    }
}
