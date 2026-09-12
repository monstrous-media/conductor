// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! LED, MIDI LED, HID LED, and velocity-color-map validators.

use super::*;

pub(super) fn validate_led_config(config: &Config, ctx: &mut ValidationCtx) {
    if let Some(ref led) = config.led {
        if led.brightness > 127 {
            ctx.error(
                "led.brightness",
                format!("LED brightness must be 0-127, got {}", led.brightness),
            );
        }

        let valid_schemes = crate::feedback::LightingScheme::list_all();
        if led.scheme.is_empty() {
            ctx.warning(
                "led.scheme",
                "LED scheme is empty; defaults to 'reactive'".to_string(),
            );
        } else if !valid_schemes.contains(&led.scheme.as_str()) {
            ctx.error(
                "led.scheme",
                format!(
                    "Unknown LED scheme '{}'. Valid schemes: {}",
                    led.scheme,
                    valid_schemes.join(", ")
                ),
            );
        }

        let mode_names: std::collections::HashSet<&str> =
            config.modes.iter().map(|m| m.name.as_str()).collect();
        for mode_name in led.mode_colors.keys() {
            if !mode_names.contains(mode_name.as_str()) {
                let suggestion = mode_names
                    .iter()
                    .find(|name| name.eq_ignore_ascii_case(mode_name))
                    .map(|name| format!(" (did you mean '{}'?)", name));
                ctx.error(
                    "led.mode_colors",
                    format!(
                        "LED mode_colors references non-existent mode '{}'{}",
                        mode_name,
                        suggestion.unwrap_or_default()
                    ),
                );
            }
        }
    }
}

pub(super) fn validate_midi_led_config(config: &Config, ctx: &mut ValidationCtx) {
    let midi_cfg = match config.led.as_ref().and_then(|l| l.midi.as_ref()) {
        Some(c) => c,
        None => return,
    };

    if midi_cfg.channel == 0 || midi_cfg.channel > 16 {
        ctx.error(
            "led.midi.channel",
            format!("MIDI LED channel must be 1-16, got {}", midi_cfg.channel),
        );
    }

    if midi_cfg.note_on_velocity > 127 {
        ctx.error(
            "led.midi.note_on_velocity",
            format!(
                "note_on_velocity must be 0-127, got {}",
                midi_cfg.note_on_velocity
            ),
        );
    }

    if midi_cfg.note_off_velocity > 127 {
        ctx.error(
            "led.midi.note_off_velocity",
            format!(
                "note_off_velocity must be 0-127, got {}",
                midi_cfg.note_off_velocity
            ),
        );
    }

    // Validate color velocities are in MIDI range
    let colors = &midi_cfg.colors;
    for (name, val) in [
        ("red", colors.red),
        ("green", colors.green),
        ("blue", colors.blue),
        ("yellow", colors.yellow),
        ("amber", colors.amber),
        ("off", colors.off),
    ] {
        if val > 127 {
            ctx.error(
                format!("led.midi.colors.{}", name),
                format!("Color velocity must be 0-127, got {}", val),
            );
        }
    }

    // Validate custom mappings
    for (i, mapping) in midi_cfg.custom_mappings.iter().enumerate() {
        let path = format!("led.midi.custom_mappings[{}]", i);
        if mapping.pad > 127 {
            ctx.error(
                &path,
                format!("Pad number must be 0-127, got {}", mapping.pad),
            );
        }
        validate_midi_led_message(&mapping.led_on, &format!("{}.led_on", path), ctx);
        validate_midi_led_message(&mapping.led_off, &format!("{}.led_off", path), ctx);
    }

    // Check for duplicate pad numbers in custom_mappings
    let mut seen_pads = std::collections::HashSet::new();
    for (i, mapping) in midi_cfg.custom_mappings.iter().enumerate() {
        if !seen_pads.insert(mapping.pad) {
            ctx.error(
                format!("led.midi.custom_mappings[{}]", i),
                format!("Duplicate custom mapping for pad {}", mapping.pad),
            );
        }
    }
}

pub(super) fn validate_midi_led_message(
    msg: &crate::config::types::MidiLedMessage,
    path: &str,
    ctx: &mut ValidationCtx,
) {
    use crate::config::types::MidiLedMessage;
    match msg {
        MidiLedMessage::NoteOn { note, velocity } | MidiLedMessage::NoteOff { note, velocity } => {
            if *note > 127 {
                ctx.error(path, format!("Note must be 0-127, got {}", note));
            }
            if *velocity > 127 {
                ctx.error(path, format!("Velocity must be 0-127, got {}", velocity));
            }
        }
        MidiLedMessage::Cc { cc, value } => {
            if *cc > 127 {
                ctx.error(path, format!("CC must be 0-127, got {}", cc));
            }
            if *value > 127 {
                ctx.error(path, format!("Value must be 0-127, got {}", value));
            }
        }
    }
}

pub(super) fn validate_hid_led_config(config: &Config, ctx: &mut ValidationCtx) {
    let hid_cfg = match config.led.as_ref().and_then(|l| l.hid.as_ref()) {
        Some(c) => c,
        None => return,
    };

    // Check profile name is valid first
    if let Some(ref profile_name) = hid_cfg.hid_profile
        && crate::config::types::HidLedConfig::from_profile(profile_name).is_none()
    {
        ctx.error(
            "led.hid.hid_profile",
            format!(
                "Unknown HID device profile '{}'. Available profiles: mikro-mk3",
                profile_name
            ),
        );
        return; // Can't validate further
    }

    // Validate against the *resolved* config (profile merged in)
    match hid_cfg.resolve_profile() {
        Ok(resolved) => {
            if resolved.vendor_id == 0 {
                ctx.error("led.hid.vendor_id", "vendor_id must be non-zero");
            }
            if resolved.product_id == 0 {
                ctx.error("led.hid.product_id", "product_id must be non-zero");
            }
            if resolved.pad_count == 0 {
                ctx.error("led.hid.pad_count", "pad_count must be > 0");
            }
            // encode_pad_indexed packs color_index into 6 bits, so max 64 palette entries
            if resolved.color_palette.len() > 64 {
                ctx.error(
                    "led.hid.color_palette",
                    format!(
                        "color_palette has {} entries but HID LED encoding format supports at \
                         most 64 (6-bit color index)",
                        resolved.color_palette.len()
                    ),
                );
            }
        }
        Err(e) => {
            ctx.error("led.hid", e);
        }
    }
}

pub(super) fn validate_velocity_color_map(config: &Config, ctx: &mut ValidationCtx) {
    let vcm = match config.led.as_ref().and_then(|l| l.velocity_colors.as_ref()) {
        Some(c) => c,
        None => return,
    };

    if vcm.ranges.is_empty() {
        ctx.error(
            "led.velocity_colors.ranges",
            "Velocity color map must have at least one range",
        );
        return;
    }

    for (i, range) in vcm.ranges.iter().enumerate() {
        let path = format!("led.velocity_colors.ranges[{}]", i);
        if range.min > 127 {
            ctx.error(&path, format!("min must be 0-127, got {}", range.min));
        }
        if range.max > 127 {
            ctx.error(&path, format!("max must be 0-127, got {}", range.max));
        }
        if range.min > range.max {
            ctx.error(
                &path,
                format!("min ({}) > max ({}) — invalid range", range.min, range.max),
            );
        }
    }

    // Overlap and gap detection on sorted ranges
    let mut sorted: Vec<_> = vcm.ranges.iter().collect();
    sorted.sort_by_key(|r| r.min);

    for window in sorted.windows(2) {
        let prev = window[0];
        let next = window[1];

        if prev.max >= next.min {
            ctx.error(
                "led.velocity_colors.ranges",
                format!(
                    "Overlapping ranges: [{}-{}] and [{}-{}]",
                    prev.min, prev.max, next.min, next.max
                ),
            );
        } else if (prev.max as u16) + 1 < next.min as u16 {
            ctx.warning(
                "led.velocity_colors.ranges",
                format!(
                    "Gap in velocity coverage: {}-{} is unmapped",
                    (prev.max as u16) + 1,
                    (next.min as u16) - 1
                ),
            );
        }
    }
}
