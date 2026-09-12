// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Trigger, shadowing, and condition validators.

use super::*;

pub(super) fn validate_trigger(
    trigger: &Trigger,
    path: &str,
    device_aliases: &HashSet<&String>,
    ctx: &mut ValidationCtx,
) {
    // Device reference validation (from former loader.rs)
    //
    // The original wording was misleading — it said "mapping will only
    // match if this device connects", implying the issue would resolve when
    // the hardware appeared. It won't: the alias is a config-level identifier.
    // Even with the hardware connected, without a matching `[[endpoints]]`
    // entry the daemon assigns the port a generated DeviceId (port name with a
    // `#N` instance suffix when duplicates appear, via
    // `DeviceId::from_port_instance` in `input_manager`), not the trigger's
    // alias. The trigger never matches until either an `[[endpoints]]` entry
    // with this alias exists or the `device` filter is removed.
    if let Some(ref_alias) = trigger.device()
        && !device_aliases.contains(ref_alias)
    {
        ctx.warning(
            path,
            format!(
                "Trigger references device alias '{0}', but no [[endpoints]] entry \
                 defines this alias. This mapping will never match until you \
                 either (a) add an [[endpoints]] entry with alias = \"{0}\" or \
                 (b) remove the `device` filter from the trigger.",
                ref_alias
            ),
        );
    }

    // Validate MIDI channel range (0-indexed: 0-15, displayed as 1-16)
    if let Some(ch) = trigger.channel()
        && ch > 15
    {
        ctx.error(
            path,
            format!("MIDI channel out of range: {} (must be 0-15)", ch),
        );
    }

    match trigger {
        Trigger::Note { note, .. } => {
            ctx.midi_features.push("Note".to_string());
            if *note > 127 {
                ctx.error(
                    path,
                    format!("Note number out of range: {} (must be 0-127)", note),
                );
            }
        }
        Trigger::VelocityRange {
            note,
            soft_max,
            medium_max,
            ..
        } => {
            ctx.midi_features.push("VelocityRange".to_string());
            if *note > 127 {
                ctx.error(
                    path,
                    format!("Note number out of range: {} (must be 0-127)", note),
                );
            }
            // Velocity zone overlap warning (from former validator.rs)
            if let (Some(soft), Some(med)) = (soft_max, medium_max)
                && soft >= med
            {
                ctx.warning(
                    path,
                    format!(
                        "soft_max ({}) >= medium_max ({}) — velocity zones overlap",
                        soft, med
                    ),
                );
            }
        }
        Trigger::LongPress { note, .. } => {
            ctx.midi_features.push("LongPress".to_string());
            if *note > 127 {
                ctx.error(
                    path,
                    format!("Note number out of range: {} (must be 0-127)", note),
                );
            }
        }
        Trigger::DoubleTap { note, .. } => {
            ctx.midi_features.push("DoubleTap".to_string());
            if *note > 127 {
                ctx.error(
                    path,
                    format!("Note number out of range: {} (must be 0-127)", note),
                );
            }
        }
        Trigger::NoteChord { notes, .. } => {
            ctx.midi_features.push("NoteChord".to_string());
            for (i, note) in notes.iter().enumerate() {
                if *note > 127 {
                    ctx.error(
                        format!("{}.notes[{}]", path, i),
                        format!("Note number out of range: {} (must be 0-127)", note),
                    );
                }
            }
            if notes.is_empty() {
                ctx.error(path, "NoteChord must have at least one note");
            }
            // Chord size warning (from former validator.rs)
            if notes.len() == 1 {
                ctx.warning(
                    path,
                    "NoteChord with fewer than 2 notes — use Note trigger instead",
                );
            }
        }
        Trigger::EncoderTurn { cc, direction, .. } => {
            ctx.midi_features.push("EncoderTurn".to_string());
            if *cc > 127 {
                ctx.error(
                    path,
                    format!("CC number out of range: {} (must be 0-127)", cc),
                );
            }
            if let Some(dir) = direction
                && dir != "Clockwise"
                && dir != "CounterClockwise"
            {
                ctx.error(
                    path,
                    format!(
                        "Invalid direction: '{}' (must be 'Clockwise' or 'CounterClockwise')",
                        dir
                    ),
                );
            }
        }
        Trigger::CC { cc, .. } => {
            ctx.midi_features.push("CC".to_string());
            if *cc > 127 {
                ctx.error(
                    path,
                    format!("CC number out of range: {} (must be 0-127)", cc),
                );
            }
        }
        Trigger::ProgramChange { pc, .. } => {
            ctx.midi_features.push("ProgramChange".to_string());
            if let Some(p) = pc
                && *p > 127
            {
                ctx.error(
                    path,
                    format!("Program Change number out of range: {} (must be 0-127)", p),
                );
            }
        }
        Trigger::Aftertouch { .. } => {
            ctx.midi_features.push("Aftertouch".to_string());
        }
        Trigger::PolyAftertouch { note, .. } => {
            ctx.midi_features.push("PolyAftertouch".to_string());
            if *note > 127 {
                ctx.error(
                    path,
                    format!("Note number out of range: {} (must be 0-127)", note),
                );
            }
        }
        Trigger::PitchBend { .. } => {
            ctx.midi_features.push("PitchBend".to_string());
        }
        Trigger::GamepadButton { button, .. } => {
            ctx.hid_features.push("GamepadButton".to_string());
            if *button < 128 {
                // Layer 1 treated this as error; Layer 2 as warning.
                // Unified: Error (prevents MIDI conflicts)
                ctx.error(
                    path,
                    format!(
                        "Gamepad button ID out of range: {} (must be 128-255 to avoid MIDI conflicts)",
                        button
                    ),
                );
            }
        }
        Trigger::GamepadButtonChord { buttons, .. } => {
            ctx.hid_features.push("GamepadButtonChord".to_string());
            for (i, button) in buttons.iter().enumerate() {
                if *button < 128 {
                    ctx.error(
                        format!("{}.buttons[{}]", path, i),
                        format!(
                            "Gamepad button ID out of range: {} (must be 128-255 to avoid MIDI conflicts)",
                            button
                        ),
                    );
                }
            }
            if buttons.is_empty() {
                ctx.error(path, "GamepadButtonChord must have at least one button");
            }
        }
        Trigger::GamepadAnalogStick {
            axis, direction, ..
        } => {
            ctx.hid_features.push("GamepadAnalogStick".to_string());
            // 128-131 = analog sticks; ADR-047 §D3b adds the d-pad-as-axis
            // encoder ids 147/148 (kept in sync with the matcher in mapping.rs).
            if !((128..=131).contains(axis) || matches!(*axis, 147 | 148)) {
                ctx.error(
                    path,
                    format!(
                        "Gamepad analog stick axis out of range: {} (must be 128-131, or 147/148 for d-pad-as-axis)",
                        axis
                    ),
                );
            }
            if let Some(dir) = direction
                && dir != "Clockwise"
                && dir != "CounterClockwise"
            {
                ctx.error(
                    path,
                    format!(
                        "Invalid direction: '{}' (must be 'Clockwise' or 'CounterClockwise')",
                        dir
                    ),
                );
            }
        }
        Trigger::GamepadTrigger { trigger, .. } => {
            ctx.hid_features.push("GamepadTrigger".to_string());
            if *trigger != 132 && *trigger != 133 {
                ctx.error(
                    path,
                    format!(
                        "Gamepad trigger ID out of range: {} (must be 132 or 133)",
                        trigger
                    ),
                );
            }
        }
        // OSC triggers (ADR-039-A)
        Trigger::OscMessage { address, .. } => {
            if address.is_empty() || !address.starts_with('/') {
                ctx.error(
                    path,
                    format!(
                        "OscMessage address '{}' is invalid (OSC addresses must start with '/')",
                        address
                    ),
                );
            }
        }
        Trigger::OscAddressPattern { pattern, .. } => {
            if let Err(e) = crate::osc_pattern::OscPattern::compile(pattern) {
                ctx.error(
                    path,
                    format!("OscAddressPattern '{}' is invalid: {}", pattern, e),
                );
            }
        }
        Trigger::OscArgRange {
            arg_index,
            min,
            max,
            ..
        } => {
            if !min.is_finite() || !max.is_finite() {
                ctx.error(
                    path,
                    "OscArgRange min/max must be finite numbers".to_string(),
                );
            } else if min > max {
                ctx.error(path, format!("OscArgRange min {} exceeds max {}", min, max));
            }
            // Cap the index defensively: the OSC parser bounds args per
            // message, and a huge index is certainly a config mistake.
            if *arg_index > 63 {
                ctx.error(
                    path,
                    format!("OscArgRange arg_index {} is out of range (0-63)", arg_index),
                );
            }
        }
    }
}

/// Within each mode, the rule engine matches first-match-wins. If
/// two mappings have overlapping triggers and the broader one appears
/// first, the narrower one will never fire — a class of bug that is
/// silent today (the user sees no error, just an action that doesn't
/// happen). Walk each mode's mappings in order and emit a warning for
/// every pair (i, j) where i < j and `mappings[i].trigger.shadows(
/// &mappings[j].trigger)`.
///
/// Scope is intentionally narrow in v1: only the four trigger types
/// most often involved in shadow bugs (Note, CC, Aftertouch,
/// PolyAftertouch) are analysed by `Trigger::shadows`. Cross-type pairs
/// and uncovered variants are not flagged. Future follow-ups will
/// extend coverage; the validator wiring stays the same.
pub(super) fn warn_shadowed_mappings(
    mappings: &[crate::config::Mapping],
    scope_label: &str,
    base_path: &str,
    ctx: &mut ValidationCtx,
) {
    for (i, earlier) in mappings.iter().enumerate() {
        for (j, later) in mappings.iter().enumerate().skip(i + 1) {
            if !earlier.trigger.shadows(&later.trigger) {
                continue;
            }
            let later_path = format!("{}.mappings[{}]", base_path, j);
            let earlier_desc = earlier.description.as_deref().unwrap_or("(no description)");
            let later_desc = later.description.as_deref().unwrap_or("(no description)");
            ctx.warning(
                &later_path,
                format!(
                    "{0}: mapping #{1} ('{2}') is shadowed by mapping #{3} ('{4}'), \
                     which has the same or broader trigger and appears earlier. \
                     Mapping #{1} will never fire because the rule engine matches \
                     first-match-wins. Either reorder the mappings (move the more-\
                     specific one earlier) or narrow the broader trigger.",
                    scope_label, j, later_desc, i, earlier_desc
                ),
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Condition validation (ADR-025 Phase 2)
// ────────────────────────────────────────────────────────────────

/// Walk a `Condition` tree, emitting errors for bounds violations and
/// structurally-impossible expressions (e.g. `CcValueInRange { min: 80,
/// max: 20 }` which can never be satisfied at runtime).
///
/// Surfacing at load time prevents the silent-always-false failure mode:
/// a broken condition would otherwise just send every trigger down the
/// `else_action` branch with no user-visible feedback.
pub(super) fn validate_condition(
    condition: &crate::actions::Condition,
    path: &str,
    ctx: &mut ValidationCtx,
) {
    use crate::actions::Condition;
    match condition {
        Condition::ActivePcIs {
            pc,
            channel,
            device,
        } => {
            if device.is_empty() {
                ctx.error(path, "ActivePcIs: device is required");
            } else if !ctx.device_known(device) {
                ctx.error(path, format!("ActivePcIs: unknown device '{}'", device));
            }
            if *pc > 127 {
                ctx.error(path, format!("ActivePcIs: pc out of range {} (0-127)", pc));
            }
            if *channel > 15 {
                ctx.error(
                    path,
                    format!("ActivePcIs: channel out of range {} (0-15)", channel),
                );
            }
        }
        Condition::CcValueInRange {
            cc,
            channel,
            min,
            max,
            device,
        } => {
            if device.is_empty() {
                ctx.error(path, "CcValueInRange: device is required");
            } else if !ctx.device_known(device) {
                ctx.error(path, format!("CcValueInRange: unknown device '{}'", device));
            }
            if *cc > 127 {
                ctx.error(
                    path,
                    format!("CcValueInRange: cc out of range {} (0-127)", cc),
                );
            }
            if *channel > 15 {
                ctx.error(
                    path,
                    format!("CcValueInRange: channel out of range {} (0-15)", channel),
                );
            }
            if *min > 127 {
                ctx.error(
                    path,
                    format!("CcValueInRange: min out of range {} (0-127)", min),
                );
            }
            if *max > 127 {
                ctx.error(
                    path,
                    format!("CcValueInRange: max out of range {} (0-127)", max),
                );
            }
            if *min > *max {
                // ADR-025 P2: a CcValueInRange with
                // min > max is unsatisfiable. Catching at load time
                // avoids the silent always-false branch at runtime.
                ctx.error(
                    path,
                    format!(
                        "CcValueInRange: min ({}) > max ({}) is unsatisfiable; swap the bounds or widen the range",
                        min, max
                    ),
                );
            }
        }
        Condition::NoteHeld {
            note,
            channel,
            device,
            ..
        } => {
            if device.is_empty() {
                ctx.error(path, "NoteHeld: device is required");
            } else if !ctx.device_known(device) {
                ctx.error(path, format!("NoteHeld: unknown device '{}'", device));
            }
            if *note > 127 {
                ctx.error(
                    path,
                    format!("NoteHeld: note out of range {} (0-127)", note),
                );
            }
            if *channel > 15 {
                ctx.error(
                    path,
                    format!("NoteHeld: channel out of range {} (0-15)", channel),
                );
            }
        }
        // ADR-025 Phase 2.C sugar: fixed bounds (64..=127 / 0..=63)
        // make min>max structurally impossible, so only cc/channel/
        // device need validation. Same shape as the CcValueInRange
        // arm minus the min/max checks.
        Condition::CcIsOn {
            cc,
            channel,
            device,
        }
        | Condition::CcIsOff {
            cc,
            channel,
            device,
        } => {
            let kind = if matches!(condition, Condition::CcIsOn { .. }) {
                "CcIsOn"
            } else {
                "CcIsOff"
            };
            if device.is_empty() {
                ctx.error(path, format!("{}: device is required", kind));
            } else if !ctx.device_known(device) {
                ctx.error(path, format!("{}: unknown device '{}'", kind, device));
            }
            if *cc > 127 {
                ctx.error(path, format!("{}: cc out of range {} (0-127)", kind, cc));
            }
            if *channel > 15 {
                ctx.error(
                    path,
                    format!("{}: channel out of range {} (0-15)", kind, channel),
                );
            }
        }
        Condition::And { conditions } | Condition::Or { conditions } => {
            for (i, c) in conditions.iter().enumerate() {
                validate_condition(c, &format!("{}[{}]", path, i), ctx);
            }
        }
        Condition::Not { condition } => {
            validate_condition(condition, &format!("{}.not", path), ctx);
        }
        // Pre-ADR-025 variants: no additional checks needed beyond what
        // serde already enforces. Listed explicitly so adding a new
        // variant in the future is a compile error here.
        Condition::Always
        | Condition::Never
        | Condition::TimeRange { .. }
        | Condition::DayOfWeek { .. }
        | Condition::AppRunning { .. }
        | Condition::AppFrontmost { .. }
        | Condition::ModeIs { .. } => {}
    }
}

// ────────────────────────────────────────────────────────────────
// Action validation
// ────────────────────────────────────────────────────────────────
