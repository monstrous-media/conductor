// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ── Shadowed-mapping detection ─────────────────────────────
//
// The rule engine matches first-match-wins. If two mappings in the
// same mode have overlapping triggers and the broader one appears
// first, the narrower one never fires. These tests pin the shadow
// detection on the four trigger types covered in v1: Note, CC,
// Aftertouch, PolyAftertouch. Cross-type pairs and uncovered
// variants must not produce false positives.

#[test]
fn test_shadow_exact_duplicate_note_triggers_warns() {
    // The issue's primary example: two mappings with identical Note
    // triggers — the second never fires.
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, None),
        note_trigger(60, None, None, None),
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert_eq!(
        warnings.len(),
        1,
        "exact duplicate must produce exactly one shadow warning, got: {warnings:?}"
    );
    assert!(
        warnings[0].contains("shadowed by mapping #0"),
        "warning must point at the earlier shadowing mapping; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_velocity_min_subset_warns() {
    // Note{velocity_min:None} accepts any velocity; Note{velocity_min:80}
    // only accepts ≥80 — strict subset, so the second is shadowed.
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, None),
        note_trigger(60, Some(80), None, None),
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert_eq!(
        warnings.len(),
        1,
        "velocity-min subset must shadow; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_device_filter_subset_warns() {
    // Note without device filter (matches any device) covers Note that
    // requires device "mpk".
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, None),
        note_trigger(60, None, None, Some("mpk")),
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert_eq!(
        warnings.len(),
        1,
        "no-device-filter must shadow specific-device-filter; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_disjoint_devices_no_warn() {
    // Two specific-device-filter triggers on different devices are
    // disjoint — neither shadows the other.
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, Some("mpk")),
        note_trigger(60, None, None, Some("mikro")),
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert!(
        warnings.is_empty(),
        "disjoint device filters must NOT shadow; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_different_notes_no_warn() {
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, None),
        note_trigger(61, None, None, None),
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert!(
        warnings.is_empty(),
        "different note numbers must NOT shadow; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_cross_type_no_warn() {
    // A Note trigger and a CC trigger never overlap — different
    // ProcessedEvent types.
    let cfg = config_with_mappings(vec![
        note_trigger(60, None, None, None),
        Trigger::CC {
            cc: 7,
            value_min: None,
            channel: None,
            device: None,
        },
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert!(
        warnings.is_empty(),
        "cross-type triggers must NOT shadow; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_cc_value_min_subset_warns() {
    let cfg = config_with_mappings(vec![
        Trigger::CC {
            cc: 7,
            value_min: None,
            channel: None,
            device: None,
        },
        Trigger::CC {
            cc: 7,
            value_min: Some(64),
            channel: None,
            device: None,
        },
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert_eq!(
        warnings.len(),
        1,
        "CC value_min subset must shadow; got: {warnings:?}"
    );
}

#[test]
fn test_shadow_unanalyzed_variant_no_warn() {
    // LongPress is intentionally not analysed in v1 — the
    // conservative default returns false. Two identical LongPress
    // triggers must NOT raise a shadow warning until subset rules
    // for that variant are added.
    let cfg = config_with_mappings(vec![
        Trigger::LongPress {
            note: 60,
            duration_ms: Some(2000),
            channel: None,
            device: None,
        },
        Trigger::LongPress {
            note: 60,
            duration_ms: Some(2000),
            channel: None,
            device: None,
        },
    ]);
    let report = validate_config(&cfg);
    let warnings = shadow_warnings(&report);
    assert!(
        warnings.is_empty(),
        "unanalyzed variant must NOT shadow in v1; got: {warnings:?}"
    );
}

// ── MidiForward raw-port-name target lint ─────────────────
//
// A `MidiForward.target` that matches a `[[bindings]]` alias gets
// hot-plug-aware output routing (the rescan loop refreshes the
// device output map for aliased outputs). A raw port-name target
// bypasses that map entirely — it still forwards, but receives no
// hot-plug liveness, status pill, mute affordance, or future
// ADR-031 connector treatment. The validator emits a non-blocking
// warning so the operator can choose to define a binding.

// ── ADR-047 §D3a: frozen legacy gamepad sentinel (id 255) ──

#[test]
fn test_gamepad_analog_stick_accepts_dpad_axis_ids_d3b() {
    // ADR-047 §D3b: 147/148 (d-pad-as-axis) must validate, kept in sync with
    // the matcher in mapping.rs. Out-of-range still errors.
    for axis in [128u8, 131, 147, 148] {
        let cfg = config_with_mappings(vec![Trigger::GamepadAnalogStick {
            axis,
            direction: None,
            device: None,
        }]);
        let report = validate_config(&cfg);
        assert!(
            report.is_valid(),
            "axis {axis} should validate; errors: {:?}",
            report.errors
        );
    }
    let bad = config_with_mappings(vec![Trigger::GamepadAnalogStick {
        axis: 200,
        direction: None,
        device: None,
    }]);
    assert!(
        !validate_config(&bad).is_valid(),
        "axis 200 is out of range and must error"
    );
}

#[test]
fn test_gamepad_button_255_warns() {
    let cfg = config_with_mappings(vec![Trigger::GamepadButton {
        button: 255,
        velocity_min: None,
        device: None,
    }]);
    let report = validate_config(&cfg);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("255") && w.message.contains("ADR-047")),
        "button 255 must warn; got: {:?}",
        report.warnings
    );
}

#[test]
fn test_gamepad_chord_containing_255_warns() {
    let cfg = config_with_mappings(vec![Trigger::GamepadButtonChord {
        buttons: vec![128, 255],
        timeout_ms: None,
        device: None,
    }]);
    let report = validate_config(&cfg);
    assert!(
        report.warnings.iter().any(|w| w.message.contains("255")),
        "chord containing 255 must warn; got: {:?}",
        report.warnings
    );
}

#[test]
fn test_valid_gamepad_button_does_not_warn_255() {
    let cfg = config_with_mappings(vec![Trigger::GamepadButton {
        button: 128, // South — valid
        velocity_min: None,
        device: None,
    }]);
    let report = validate_config(&cfg);
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.message.contains("legacy") && w.message.contains("255")),
        "valid button must not warn about 255; got: {:?}",
        report.warnings
    );
}

#[test]
fn test_disable_legacy_sentinel_drops_only_255_binds() {
    let mut cfg = config_with_mappings(vec![
        Trigger::GamepadButton {
            button: 128,
            velocity_min: None,
            device: None,
        },
        Trigger::GamepadButton {
            button: 255,
            velocity_min: None,
            device: None,
        },
        Trigger::GamepadButtonChord {
            buttons: vec![129, 255],
            timeout_ms: None,
            device: None,
        },
    ]);
    let removed = disable_legacy_gamepad_sentinel_binds(&mut cfg);
    assert_eq!(
        removed, 2,
        "the 255 button and the 255-chord must be dropped"
    );
    let remaining: Vec<_> = cfg.modes[0].mappings.iter().map(|m| &m.trigger).collect();
    assert_eq!(remaining.len(), 1);
    assert!(
        matches!(remaining[0], Trigger::GamepadButton { button: 128, .. }),
        "only the valid button-128 mapping survives"
    );
}

#[test]
fn test_midi_forward_raw_port_name_warns() {
    // Target doesn't match any [[bindings]] alias → raw port name.
    let config = config_with_action(midi_forward("Komplete Audio 6 MK2"));
    let report = validate_config(&config);
    // Non-blocking: the config is still valid.
    assert!(
        report.is_valid(),
        "raw-port-name target must NOT be a hard error"
    );
    let warning = report
        .warnings
        .iter()
        .find(|w| w.message.contains("MidiForward target"))
        .expect("expected a MidiForward raw-port-name warning");
    assert!(
        warning.message.contains("Komplete Audio 6 MK2"),
        "warning must name the target; got: {}",
        warning.message
    );
    assert!(
        warning.message.contains("[[endpoints]]"),
        "warning must point at the [[endpoints]] remediation; got: {}",
        warning.message
    );
}

#[test]
fn test_midi_forward_aliased_target_no_warn() {
    // Target matches a defined [[bindings]] alias → no warning.
    let mut config = config_with_action(midi_forward("studio_out"));
    config.endpoints = vec![ep_input(
        "studio_out",
        vec![DeviceMatcher::NameContains {
            value: "Komplete".to_string(),
        }],
    )];
    let report = validate_config(&config);
    assert!(report.is_valid());
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.message.contains("MidiForward target")),
        "aliased target must NOT produce the raw-port-name warning"
    );
}

#[test]
fn test_midi_forward_empty_target_still_errors_not_warns() {
    // Empty target is a hard error (pre-existing behaviour); the
    // raw-port-name warning must not replace or suppress it.
    let config = config_with_action(midi_forward(""));
    let report = validate_config(&config);
    assert!(!report.is_valid(), "empty target must remain a hard error");
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("MidiForward requires target")),
        "empty target must keep the 'requires target port name' error"
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.message.contains("does not match any [[endpoints]]")),
        "empty target must NOT also emit the raw-port-name warning"
    );
}
