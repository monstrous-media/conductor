// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ─── ADR-025 Phase 2: validate_condition ─────────

#[test]
fn validator_rejects_cc_value_in_range_with_min_greater_than_max() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcValueInRange {
        cc: 1,
        channel: 0,
        min: 80,
        max: 20,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("min") && e.message.contains("max")),
        "expected min>max error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_value_in_range_with_out_of_range_bounds() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcValueInRange {
        cc: 1,
        channel: 0,
        min: 0,
        max: 200,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(report.errors.iter().any(|e| e.message.contains("max")));
}

#[test]
fn validator_rejects_active_pc_is_missing_device() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::ActivePcIs {
        pc: 12,
        channel: 0,
        device: "".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report.errors.iter().any(|e| e.message.contains("device")),
        "expected device-required error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_note_held_out_of_range() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::NoteHeld {
        note: 200,
        channel: 0,
        device: "keyboard".into(),
        ttl_override_ms: None,
    });
    let report = validate_config(&cfg);
    assert!(report.errors.iter().any(|e| e.message.contains("note")));
}

#[test]
fn validator_recurses_into_and_or_not() {
    use crate::actions::Condition;
    // Nested broken condition inside And should still surface.
    let cfg = make_config_with_condition(Condition::And {
        conditions: vec![
            Condition::Always,
            Condition::CcValueInRange {
                cc: 1,
                channel: 0,
                min: 80,
                max: 20,
                device: "keyboard".into(),
            },
        ],
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("min") && e.message.contains("max")),
        "And should recurse into children; errors: {:?}",
        report.errors
    );
}

#[test]
fn validator_accepts_well_formed_state_conditions() {
    use crate::actions::Condition;
    // "keyboard" is the alias seeded by `make_config_with_condition`.
    let cfg = make_config_with_condition(Condition::ActivePcIs {
        pc: 12,
        channel: 0,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "well-formed condition must pass, got errors: {:?}",
        report.errors
    );
}

// ─── ADR-025 Phase 2.C: CcIsOn / CcIsOff sugar ──────────────────

#[test]
fn validator_accepts_well_formed_cc_is_on() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOn {
        cc: 64,
        channel: 0,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report.errors.is_empty(),
        "CcIsOn must pass, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_accepts_well_formed_cc_is_off() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOff {
        cc: 64,
        channel: 0,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(report.errors.is_empty());
}

#[test]
fn validator_rejects_cc_is_on_missing_device() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOn {
        cc: 64,
        channel: 0,
        device: "".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOn") && e.message.contains("device")),
        "expected CcIsOn device error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_is_off_out_of_range() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOff {
        cc: 200, // out of range
        channel: 0,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOff") && e.message.contains("cc")),
        "expected CcIsOff cc bounds error, got: {:?}",
        report.errors
    );
}

// ─── Symmetric coverage ──────────────────────────
//
// The shared validator arm uses a `matches!(condition, CcIsOn {..})`
// check to pick which of the two error-message kinds to emit. These
// tests pin both branches of that selection so a copy-paste mistake
// — e.g. swapping `CcIsOn`/`CcIsOff` in the matches arm or in the
// emitted message prefix — surfaces immediately instead of leaking
// through unnoticed.

#[test]
fn validator_rejects_cc_is_off_missing_device() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOff {
        cc: 64,
        channel: 0,
        device: "".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOff") && e.message.contains("device")),
        "expected CcIsOff device error (not CcIsOn), got: {:?}",
        report.errors
    );
    // Explicitly guard against the wrong kind being emitted.
    assert!(
        !report.errors.iter().any(|e| e.message.contains("CcIsOn")),
        "CcIsOff error should not mention CcIsOn; got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_is_on_out_of_range() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOn {
        cc: 200, // out of range
        channel: 0,
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOn") && e.message.contains("cc")),
        "expected CcIsOn cc bounds error, got: {:?}",
        report.errors
    );
    assert!(
        !report.errors.iter().any(|e| e.message.contains("CcIsOff")),
        "CcIsOn error should not mention CcIsOff; got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_is_on_bad_channel() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOn {
        cc: 64,
        channel: 42, // out of range
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOn") && e.message.contains("channel")),
        "expected CcIsOn channel error, got: {:?}",
        report.errors
    );
}

#[test]
fn validator_rejects_cc_is_off_bad_channel() {
    use crate::actions::Condition;
    let cfg = make_config_with_condition(Condition::CcIsOff {
        cc: 64,
        channel: 42, // out of range
        device: "keyboard".into(),
    });
    let report = validate_config(&cfg);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("CcIsOff") && e.message.contains("channel")),
        "expected CcIsOff channel error, got: {:?}",
        report.errors
    );
}
