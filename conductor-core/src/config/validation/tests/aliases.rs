// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ─── ADR-025 Phase 2.G: context-switch validator ──────────
//
// Two new classes of check on top of the field-level bounds that
// already shipped in 2.D:
//
//   1. CcContextSwitch range overlap detection — first-match-wins
//      at runtime, so overlapping ranges silently mask later
//      branches. Detect pairwise, order-independent.
//   2. Device alias resolution — unknown device in a condition or
//      context-switch is an ERROR (stronger than trigger's WARNING,
//      because state conditions can't observe a device that isn't
//      bound to the store).

// ── Device alias resolution: context-switch actions ───────────────

#[test]
fn validator_rejects_unknown_device_in_pc_context_switch() {
    use indexmap::IndexMap;
    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    mappings.insert(
        0,
        Box::new(ActionConfig::Shell {
            sandbox: None,
            command: "echo a".into(),
            args: None,
            timeout_ms: None,
        }),
    );
    let cfg = make_config_with_action_and_devices(
        ActionConfig::PcContextSwitch {
            channel: 0,
            device: "unknown_alias".into(),
            mappings,
            default: None,
        },
        vec![], // no devices declared
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| {
            e.message.contains("PcContextSwitch")
                && e.message.contains("unknown device")
                && e.message.contains("unknown_alias")
        }),
        "expected unknown-device error, got errors: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_unknown_device_in_cc_context_switch() {
    use crate::config::types::CcRange;
    let cfg = make_config_with_action_and_devices(
        ActionConfig::CcContextSwitch {
            cc: 1,
            channel: 0,
            device: "ghost".into(),
            ranges: vec![CcRange {
                min: 0,
                max: 63,
                action: Box::new(ActionConfig::Shell {
                    sandbox: None,
                    command: "echo a".into(),
                    args: None,
                    timeout_ms: None,
                }),
            }],
            default: None,
        },
        vec![],
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| {
            e.message.contains("CcContextSwitch")
                && e.message.contains("unknown device")
                && e.message.contains("ghost")
        }),
        "expected unknown-device error, got errors: {:?}",
        report.errors
    );
}

#[test]
fn validator_accepts_known_device_in_context_switch_and_condition() {
    use crate::actions::Condition;
    use crate::config::types::CcRange;
    let cfg = make_config_with_action_and_devices(
        ActionConfig::Conditional {
            condition: Condition::CcIsOn {
                cc: 64,
                channel: 0,
                device: "fcb1010".into(),
            },
            then_action: Box::new(ActionConfig::CcContextSwitch {
                cc: 7,
                channel: 0,
                device: "fcb1010".into(),
                ranges: vec![CcRange {
                    min: 0,
                    max: 127,
                    action: Box::new(ActionConfig::Shell {
                        sandbox: None,
                        command: "echo a".into(),
                        args: None,
                        timeout_ms: None,
                    }),
                }],
                default: None,
            }),
            else_action: None,
        },
        vec![device_identity("fcb1010")],
    );
    let report = validate_config(&cfg);
    assert!(
        !report
            .errors
            .iter()
            .any(|e| e.message.contains("unknown device")),
        "expected no unknown-device error, got: {:?}",
        report.errors
    );
}

// ── Device alias resolution: state conditions ─────────────────────

#[test]
fn validator_rejects_unknown_device_in_active_pc_is() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition_and_devices(
        Condition::ActivePcIs {
            pc: 0,
            channel: 0,
            device: "phantom".into(),
        },
        vec![],
    );
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("ActivePcIs")
                && e.message.contains("unknown device")
                && e.message.contains("phantom")),
        "expected ActivePcIs unknown-device error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_unknown_device_in_cc_value_in_range() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition_and_devices(
        Condition::CcValueInRange {
            cc: 1,
            channel: 0,
            min: 0,
            max: 63,
            device: "phantom".into(),
        },
        vec![],
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| {
            e.message.contains("CcValueInRange")
                && e.message.contains("unknown device")
                && e.message.contains("phantom")
        }),
        "expected CcValueInRange unknown-device error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_unknown_device_in_note_held() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition_and_devices(
        Condition::NoteHeld {
            note: 60,
            channel: 0,
            device: "phantom".into(),
            ttl_override_ms: None,
        },
        vec![],
    );
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("NoteHeld")
            && e.message.contains("unknown device")
            && e.message.contains("phantom")),
        "expected NoteHeld unknown-device error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_unknown_device_in_cc_is_on_off() {
    use crate::actions::Condition;
    for cond in [
        Condition::CcIsOn {
            cc: 64,
            channel: 0,
            device: "phantom".into(),
        },
        Condition::CcIsOff {
            cc: 64,
            channel: 0,
            device: "phantom".into(),
        },
    ] {
        let name = match cond {
            Condition::CcIsOn { .. } => "CcIsOn",
            Condition::CcIsOff { .. } => "CcIsOff",
            _ => unreachable!(),
        };
        let cfg = make_config_with_condition_and_devices(cond, vec![]);
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| {
                e.message.contains(name)
                    && e.message.contains("unknown device")
                    && e.message.contains("phantom")
            }),
            "expected {} unknown-device error, got: {:?}",
            name,
            report.errors
        );
    }
}

// ── CcContextSwitch range overlap detection ───────────────────────

#[test]
fn validator_rejects_overlapping_cc_ranges_contained() {
    // [0, 100] fully contains [20, 40] — overlap error.
    let cfg = cc_switch_with_ranges(vec![(0, 100), (20, 40)]);
    let report = validate_config(&cfg);
    let overlap = report
        .errors
        .iter()
        .find(|e| {
            e.message.contains("CcContextSwitch")
                && e.message.contains("overlap")
                && e.message.contains("[0]")
                && e.message.contains("[1]")
        })
        .unwrap_or_else(|| {
            panic!(
                "expected overlap error naming both range indices, got: {:?}",
                report.errors
            )
        });
    // Anchored to the later (masked) range so UIs can point
    // directly at the branch that will never fire.
    assert!(
        overlap.path.ends_with(".ranges[1]"),
        "overlap error should anchor to the masked range path, got path: {:?}",
        overlap.path
    );
}

#[test]
fn validator_rejects_overlapping_cc_ranges_partial() {
    // [0, 50] partially overlaps [40, 80] at 40-50.
    let cfg = cc_switch_with_ranges(vec![(0, 50), (40, 80)]);
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("overlap")),
        "expected partial overlap error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_overlapping_cc_ranges_unsorted_pair() {
    // Non-adjacent overlap in an unsorted list:
    // [0,20], [60,127], [10,15] — (10,15) overlaps (0,20) even
    // though they aren't adjacent in the Vec. The spec's naive
    // "prev_max only" check would miss this; we detect pairwise.
    let cfg = cc_switch_with_ranges(vec![(0, 20), (60, 127), (10, 15)]);
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("overlap")),
        "expected non-adjacent overlap error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_overlapping_cc_ranges_identical() {
    let cfg = cc_switch_with_ranges(vec![(0, 50), (0, 50)]);
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("overlap")),
        "expected identical-range overlap error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_accepts_adjacent_non_overlapping_cc_ranges() {
    // [0,49] then [50,99] then [100,127] — touching at min-1/max
    // boundary but non-overlapping. Valid config.
    let cfg = cc_switch_with_ranges(vec![(0, 49), (50, 99), (100, 127)]);
    let report = validate_config(&cfg);
    assert!(
        !report.errors.iter().any(|e| e.message.contains("overlap")),
        "expected no overlap error for adjacent ranges, got: {:?}",
        report.errors
    );
}

// ═══════════════════════════════════════════════════════════════════
// ADR-027 D3 §3.2 — `allow_interpreters` policy
//
// The validator runs `resolve_effective_binary` against every Shell
// action; when the resolved binary is a known interpreter family
// and `advanced_settings.allow_interpreters` is `Deny` or `Warn`,
// emits a config-load diagnostic. `Allow` is the opt-in escape
// hatch for power users who deliberately rely on shell scripting.
// ═══════════════════════════════════════════════════════════════════

#[test]
fn allow_interpreters_default_is_warn() {
    // Default for new configs and `..default_config()` builders —
    // backwards-compat without users opting in, but the warning
    // surfaces the new gate so they're aware of the policy.
    let settings = crate::config::types::AdvancedSettings::default();
    assert_eq!(settings.allow_interpreters, InterpreterPolicy::Warn);
}

#[test]
fn allow_interpreters_warn_emits_warning_for_sh() {
    // Default policy (Warn): `/bin/sh -c …` should produce a
    // validation WARNING, not an error. Config still loads.
    let config = config_with_action_and_policy(
        ActionConfig::Shell {
            sandbox: None,
            command: "/bin/sh".to_string(),
            args: Some(vec!["-c".to_string(), "echo hi".to_string()]),
            timeout_ms: None,
        },
        InterpreterPolicy::Warn,
    );
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "Warn policy keeps the config valid — got errors: {:?}",
        report.errors
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("interpreter")
                && (w.message.contains("sh") || w.message.contains("Sh"))),
        "expected an interpreter-family warning — got warnings: {:?}",
        report
            .warnings
            .iter()
            .map(|w| &w.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn allow_interpreters_deny_emits_error_for_sh() {
    let config = config_with_action_and_policy(
        ActionConfig::Shell {
            sandbox: None,
            command: "/bin/sh".to_string(),
            args: Some(vec!["-c".to_string(), "echo hi".to_string()]),
            timeout_ms: None,
        },
        InterpreterPolicy::Deny,
    );
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "Deny policy must reject interpreter invocations"
    );
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("interpreter")),
        "expected an interpreter-family error — got: {:?}",
        report.errors
    );
}

#[test]
fn allow_interpreters_allow_emits_no_finding_for_sh() {
    // Explicit opt-in: users who know they want shell semantics.
    // No warning, no error.
    let config = config_with_action_and_policy(
        ActionConfig::Shell {
            sandbox: None,
            command: "/bin/sh".to_string(),
            args: Some(vec!["-c".to_string(), "echo hi".to_string()]),
            timeout_ms: None,
        },
        InterpreterPolicy::Allow,
    );
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "Allow policy keeps the config valid — got errors: {:?}",
        report.errors
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.message.contains("interpreter")),
        "Allow policy must NOT emit an interpreter warning — got: {:?}",
        report
            .warnings
            .iter()
            .map(|w| &w.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn allow_interpreters_warn_fires_on_env_wrapper() {
    // Wrapper unwinding: `env python -c …` resolves to python →
    // warn. Closes the canonical D3 bypass class.
    let config = config_with_action_and_policy(
        ActionConfig::Shell {
            sandbox: None,
            command: "/usr/bin/env".to_string(),
            args: Some(vec![
                "python".to_string(),
                "-c".to_string(),
                "print(1)".to_string(),
            ]),
            timeout_ms: None,
        },
        InterpreterPolicy::Warn,
    );
    let report = validate_config(&config);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("interpreter")
                && w.message.to_lowercase().contains("python")),
        "expected a Python interpreter warning — got: {:?}",
        report
            .warnings
            .iter()
            .map(|w| &w.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn allow_interpreters_does_not_fire_on_non_interpreter_binary() {
    // `/bin/ls` is not an interpreter — no finding regardless of
    // policy. Verifies the resolver classified correctly + the
    // policy is gated on `family.is_some()`.
    for policy in [
        InterpreterPolicy::Allow,
        InterpreterPolicy::Warn,
        InterpreterPolicy::Deny,
    ] {
        let config = config_with_action_and_policy(
            ActionConfig::Shell {
                sandbox: None,
                command: "/bin/ls -la".to_string(),
                args: None,
                timeout_ms: None,
            },
            policy,
        );
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "non-interpreter binary should never trip the policy ({:?}) — got: {:?}",
            policy,
            report.errors
        );
        assert!(
            !report
                .warnings
                .iter()
                .any(|w| w.message.contains("interpreter")),
            "non-interpreter binary should never trip the policy ({:?}) — got warnings: {:?}",
            policy,
            report
                .warnings
                .iter()
                .map(|w| &w.message)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn allow_interpreters_warn_message_includes_resolved_binary_path() {
    // Diagnostic UX: the warning should name the resolved binary
    // so users can find it (Phase 3 GUI editor will key its chip
    // off this same data).
    let config = config_with_action_and_policy(
        ActionConfig::Shell {
            sandbox: None,
            command: "/usr/bin/env".to_string(),
            args: Some(vec![
                "python".to_string(),
                "-c".to_string(),
                "1".to_string(),
            ]),
            timeout_ms: None,
        },
        InterpreterPolicy::Warn,
    );
    let report = validate_config(&config);
    let interpreter_warning = report
        .warnings
        .iter()
        .find(|w| w.message.contains("interpreter"))
        .expect("expected an interpreter warning");
    assert!(
        interpreter_warning.message.contains("python"),
        "warning should name `python` — got: {:?}",
        interpreter_warning.message
    );
}
