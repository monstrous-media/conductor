// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// ── Velocity zone overlap warning (from former validator.rs) ──

#[test]
fn test_velocity_zone_overlap_warning() {
    let config = config_with_mapping(
        Trigger::VelocityRange {
            note: 60,
            soft_max: Some(100),
            medium_max: Some(80),
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(report.is_valid()); // warnings don't make it invalid
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("velocity zones overlap"))
    );
}

#[test]
fn test_note_chord_size_warning() {
    let config = config_with_mapping(
        Trigger::NoteChord {
            notes: vec![60],
            timeout_ms: None,
            channel: None,
            device: None,
        },
        ActionConfig::Keystroke {
            keys: "a".to_string(),
            modifiers: vec![],
        },
    );
    let report = validate_config(&config);
    assert!(report.is_valid()); // single-note chord is valid but warned
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("fewer than 2 notes"))
    );
}

// ── LED config validation tests ─────────────

#[test]
fn test_led_config_valid() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        enabled: true,
        brightness: 100,
        scheme: "reactive".to_string(),
        idle_timeout_secs: 0,
        mode_colors: std::collections::BTreeMap::new(),
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
    });
    let report = validate_config(&config);
    assert!(report.is_valid());
}

#[test]
fn test_led_brightness_too_high() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        enabled: true,
        brightness: 200,
        scheme: "reactive".to_string(),
        idle_timeout_secs: 0,
        mode_colors: std::collections::BTreeMap::new(),
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("brightness"))
    );
}

#[test]
fn test_led_unknown_scheme() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        enabled: true,
        brightness: 100,
        scheme: "disco".to_string(),
        idle_timeout_secs: 0,
        mode_colors: std::collections::BTreeMap::new(),
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("Unknown LED scheme"))
    );
}

#[test]
fn test_led_mode_color_invalid_ref() {
    let mut config = default_config();
    let mut mode_colors = std::collections::BTreeMap::new();
    mode_colors.insert(
        "NonExistentMode".to_string(),
        crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
    );
    config.led = Some(crate::config::types::LedConfig {
        enabled: true,
        brightness: 100,
        scheme: "reactive".to_string(),
        idle_timeout_secs: 0,
        mode_colors,
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("non-existent mode"))
    );
}

#[test]
fn test_led_none_backward_compat() {
    let mut config = default_config();
    config.led = None;
    let report = validate_config(&config);
    assert!(report.is_valid());
}

#[test]
fn test_plugin_action_valid() {
    let config = config_with_action(ActionConfig::Plugin {
        plugin: "my-plugin".to_string(),
        params: serde_json::json!({"key": "value"}),
    });
    let report = validate_config(&config);
    assert!(report.is_valid(), "Valid plugin should pass: {:?}", report);
}

#[test]
fn test_plugin_action_empty_name() {
    let config = config_with_action(ActionConfig::Plugin {
        plugin: "".to_string(),
        params: serde_json::json!({}),
    });
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "Empty plugin name should fail validation"
    );
}

#[test]
fn test_plugin_action_invalid_chars() {
    let config = config_with_action(ActionConfig::Plugin {
        plugin: "my plugin!".to_string(),
        params: serde_json::json!({}),
    });
    let report = validate_config(&config);
    assert!(
        !report.is_valid(),
        "Plugin name with spaces/special chars should fail"
    );
}

#[test]
fn test_plugin_action_null_params_warns() {
    let config = config_with_action(ActionConfig::Plugin {
        plugin: "my-plugin".to_string(),
        params: serde_json::Value::Null,
    });
    let report = validate_config(&config);
    // Should be valid but have a warning
    assert!(report.is_valid());
    assert!(
        !report.warnings.is_empty(),
        "Null params should produce a warning"
    );
}

#[test]
fn test_plugin_action_dot_namespaced() {
    let config = config_with_action(ActionConfig::Plugin {
        plugin: "com.example.my-plugin".to_string(),
        params: serde_json::json!({"key": "value"}),
    });
    let report = validate_config(&config);
    assert!(
        report.is_valid(),
        "Dot-namespaced plugin names should be valid: {:?}",
        report
    );
}

// ── MIDI LED Config Validation Tests ────────

#[test]
fn test_midi_led_config_valid() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        midi: Some(crate::config::types::MidiLedConfig::default()),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid(), "Valid MIDI LED config: {:?}", report);
}

#[test]
fn test_midi_led_channel_zero() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        midi: Some(crate::config::types::MidiLedConfig {
            channel: 0,
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(report.errors.iter().any(|e| e.message.contains("channel")));
}

#[test]
fn test_midi_led_channel_too_high() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        midi: Some(crate::config::types::MidiLedConfig {
            channel: 17,
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(report.errors.iter().any(|e| e.message.contains("channel")));
}

#[test]
fn test_midi_led_no_config_backward_compat() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid());
}

// ========== HID LED Validation Tests ==========

#[test]
fn test_hid_led_valid_profile() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            hid_profile: Some("mikro-mk3".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_hid_led_unknown_profile() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            hid_profile: Some("unknown-device".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(report.errors.iter().any(|e| e.message.contains("Unknown")));
}

#[test]
fn test_hid_led_no_vendor_no_profile_fails() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            product_id: Some(0x1234),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("vendor_id"))
    );
}

#[test]
fn test_hid_led_explicit_valid() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            vendor_id: Some(0x17CC),
            product_id: Some(0x1700),
            buffer_size: Some(80),
            pad_led_offset: Some(39),
            pad_count: Some(16),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_hid_led_buffer_overflow_fails() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            buffer_size: Some(10),
            pad_led_offset: Some(5),
            pad_count: Some(8),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_hid_led_pad_layout_mismatch() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            hid_profile: Some("mikro-mk3".to_string()),
            pad_count: Some(16),
            pad_layout: Some(vec![0, 1, 2]), // 3 != 16
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("pad_layout"))
    );
}

#[test]
fn test_hid_led_duplicate_pad_layout() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: Some(crate::config::types::HidLedConfig {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            buffer_size: Some(20),
            pad_led_offset: Some(0),
            pad_count: Some(4),
            pad_layout: Some(vec![0, 1, 1, 3]),
            ..Default::default()
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.to_lowercase().contains("duplicate"))
    );
}

#[test]
fn test_hid_led_no_config_backward_compat() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        hid: None,
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid());
}

// ========== Velocity Color Map Validation ==========

#[test]
fn test_velocity_map_valid_default() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap::default()),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_velocity_map_overlapping_ranges() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap {
            ranges: vec![
                crate::config::types::VelocityRange {
                    min: 0,
                    max: 80,
                    color: crate::config::types::RgbColor { r: 0, g: 255, b: 0 },
                },
                crate::config::types::VelocityRange {
                    min: 60, // overlaps with 0-80
                    max: 127,
                    color: crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
                },
            ],
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.message.contains("Overlapping"))
    );
}

#[test]
fn test_velocity_map_inverted_range() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap {
            ranges: vec![crate::config::types::VelocityRange {
                min: 80,
                max: 40, // inverted
                color: crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
            }],
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_velocity_map_gap_warns() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap {
            ranges: vec![
                crate::config::types::VelocityRange {
                    min: 0,
                    max: 30,
                    color: crate::config::types::RgbColor { r: 0, g: 255, b: 0 },
                },
                crate::config::types::VelocityRange {
                    min: 50, // gap: 31-49 unmapped
                    max: 127,
                    color: crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
                },
            ],
        }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid()); // gaps are warnings, not errors
    assert!(report.warnings.iter().any(|w| w.message.contains("Gap")));
}

#[test]
fn test_velocity_map_empty_ranges_fails() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: Some(crate::config::types::VelocityColorMap { ranges: vec![] }),
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(!report.is_valid());
}

#[test]
fn test_velocity_map_no_config_backward_compat() {
    let mut config = default_config();
    config.led = Some(crate::config::types::LedConfig {
        velocity_colors: None,
        ..Default::default()
    });
    let report = validate_config(&config);
    assert!(report.is_valid());
}

// ── Mutation-gap tests: LED / velocity boundary pinning ──
//
// Each test kills specific cargo-mutants survivors: the existing
// LED-region tests assert only `!is_valid()` + message substrings,
// which lets boundary flips (`>`↔`>=`), early-`return` deletions,
// sort removal, and gap arithmetic mutate undetected. These assert
// exact paths and exact counts instead.

#[test]
fn test_hid_led_zero_vendor_id_rejected_at_exact_path() {
    // vendor_id: Some(0) must be caught by the explicit `== 0` check —
    // NOT by the resolve_profile Err path (which fires only when the id
    // is absent entirely).
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0),
        product_id: Some(0x1700),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.vendor_id", 1, 1);

    // Boundary: 1 is the smallest valid id.
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(1),
        product_id: Some(0x1700),
        ..Default::default()
    }));
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_hid_led_zero_product_id_rejected_at_exact_path() {
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(0),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.product_id", 1, 1);

    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(1),
        ..Default::default()
    }));
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_hid_led_zero_pad_count_rejected_at_exact_path() {
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(0x1700),
        pad_count: Some(0),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.pad_count", 1, 1);

    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(0x1700),
        pad_count: Some(1),
        pad_layout: Some(vec![0]),
        ..Default::default()
    }));
    assert!(report.is_valid(), "errors: {:?}", report.errors);
}

#[test]
fn test_hid_led_palette_64_boundary() {
    // 64 entries fit the 6-bit color index exactly; 65 do not. Pins the
    // `> 64` comparison and the interpolated len() in the message.
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(0x1700),
        color_palette: Some(vec![rgb(); 64]),
        ..Default::default()
    }));
    assert!(
        report.is_valid(),
        "64 entries must pass: {:?}",
        report.errors
    );

    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0x17CC),
        product_id: Some(0x1700),
        color_palette: Some(vec![rgb(); 65]),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.color_palette", 1, 1);
    assert!(
        report.errors[0].message.contains("65 entries"),
        "message must name the offending size: {:?}",
        report.errors[0].message
    );
}

#[test]
fn test_hid_led_unknown_profile_stops_further_validation() {
    // The unknown-profile error must be the ONLY finding — the early
    // `return` prevents resolve_profile from stacking a second error.
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        hid_profile: Some("no-such-device".to_string()),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.hid_profile", 1, 1);
}

#[test]
fn test_hid_led_checks_fire_independently() {
    // Each post-resolve check is an independent `if` — all four must
    // report on one pass.
    let report = validate_config(&hid_led_config(crate::config::types::HidLedConfig {
        vendor_id: Some(0),
        product_id: Some(0),
        pad_count: Some(0),
        color_palette: Some(vec![rgb(); 65]),
        ..Default::default()
    }));
    assert_errors_at(&report, "led.hid.vendor_id", 1, 4);
    assert_errors_at(&report, "led.hid.product_id", 1, 4);
    assert_errors_at(&report, "led.hid.pad_count", 1, 4);
    assert_errors_at(&report, "led.hid.color_palette", 1, 4);
}

#[test]
fn test_velocity_min_127_boundary() {
    // u8 means only 128..=255 can trip the `> 127` check.
    let report = validate_config(&velocity_config(vec![vrange(128, 130)]));
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.path == "led.velocity_colors.ranges[0]"
                && e.message.contains("min must be 0-127")),
        "min 128 must error at ranges[0]: {:?}",
        report.errors
    );

    let report = validate_config(&velocity_config(vec![vrange(127, 127)]));
    assert!(report.is_valid(), "min 127 is legal: {:?}", report.errors);
}

#[test]
fn test_velocity_max_127_boundary_and_index_interpolation() {
    // Second range bad → the error path must say ranges[1], pinning the
    // index interpolation.
    let report = validate_config(&velocity_config(vec![vrange(0, 100), vrange(101, 128)]));
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.path == "led.velocity_colors.ranges[1]"
                && e.message.contains("max must be 0-127")),
        "max 128 in second range must error at ranges[1]: {:?}",
        report.errors
    );

    let report = validate_config(&velocity_config(vec![vrange(0, 100), vrange(101, 127)]));
    assert!(report.is_valid(), "max 127 is legal: {:?}", report.errors);
}

#[test]
fn test_velocity_min_equals_max_is_legal() {
    // Pins `min > max` (not >=): a single-velocity range is valid.
    let report = validate_config(&velocity_config(vec![
        vrange(0, 3),
        vrange(4, 4),
        vrange(5, 127),
    ]));
    assert!(
        report.is_valid(),
        "min == max is legal: {:?}",
        report.errors
    );
    assert!(
        report.warnings.is_empty(),
        "no gaps here: {:?}",
        report.warnings
    );

    let report = validate_config(&velocity_config(vec![vrange(5, 4)]));
    assert_errors_at(&report, "led.velocity_colors.ranges[0]", 1, 1);
}

#[test]
fn test_velocity_touching_ranges_overlap_exactly_once() {
    // prev.max >= next.min: touching at 40 IS an overlap; adjacent
    // 39/40 is NOT (and produces no gap warning either). Pins `>=`
    // in both directions and the else-chain.
    let report = validate_config(&velocity_config(vec![vrange(0, 40), vrange(40, 127)]));
    assert_errors_at(&report, "led.velocity_colors.ranges", 1, 1);
    assert_eq!(
        report.errors[0].message, "Overlapping ranges: [0-40] and [40-127]",
        "exact overlap message pins the interpolated bounds"
    );
    assert!(
        report.warnings.is_empty(),
        "overlap must not also warn: {:?}",
        report.warnings
    );

    let report = validate_config(&velocity_config(vec![vrange(0, 39), vrange(40, 127)]));
    assert!(
        report.is_valid(),
        "adjacent ranges are clean: {:?}",
        report.errors
    );
    assert!(
        report.warnings.is_empty(),
        "adjacent ranges leave no gap: {:?}",
        report.warnings
    );
}

#[test]
fn test_velocity_overlap_detected_after_sorting() {
    // Input deliberately in REVERSE min-order: the overlap is only
    // visible after sort_by_key, and the message order proves the sort
    // ran (prev = the lower range).
    let report = validate_config(&velocity_config(vec![vrange(60, 127), vrange(0, 80)]));
    assert_errors_at(&report, "led.velocity_colors.ranges", 1, 1);
    assert_eq!(
        report.errors[0].message, "Overlapping ranges: [0-80] and [60-127]",
        "sorted order must put the lower range first"
    );
}

#[test]
fn test_velocity_gap_message_pins_arithmetic() {
    // Single-value gap: 31..31. The exact message pins prev.max+1 and
    // next.min-1; the empty-errors + is_valid asserts pin gap-as-warning.
    let report = validate_config(&velocity_config(vec![vrange(0, 30), vrange(32, 127)]));
    assert!(report.is_valid());
    assert!(
        report.errors.is_empty(),
        "gaps are warnings: {:?}",
        report.errors
    );
    assert_eq!(
        report.warnings.len(),
        1,
        "exactly one gap: {:?}",
        report.warnings
    );
    assert!(
        report.warnings[0]
            .message
            .contains("Gap in velocity coverage: 31-31 is unmapped"),
        "gap bounds must be exact: {:?}",
        report.warnings[0].message
    );

    // Off-by-one the other way: 31 meets 0-30 exactly — no gap.
    let report = validate_config(&velocity_config(vec![vrange(0, 30), vrange(31, 127)]));
    assert!(
        report.warnings.is_empty(),
        "no gap at exact adjacency: {:?}",
        report.warnings
    );
}

#[test]
fn test_velocity_two_gaps_two_warnings() {
    // Three ranges, two gaps — pins the windows(2) pairing.
    let report = validate_config(&velocity_config(vec![
        vrange(0, 10),
        vrange(20, 30),
        vrange(40, 50),
    ]));
    assert!(report.is_valid());
    assert_eq!(
        report.warnings.len(),
        2,
        "one warning per gap: {:?}",
        report.warnings
    );
}

#[test]
fn test_velocity_empty_ranges_single_error() {
    // Exactly one error (the early return prevents anything further).
    let report = validate_config(&velocity_config(vec![]));
    assert_errors_at(&report, "led.velocity_colors.ranges", 1, 1);
}

// ── Mutation-gap tests: MIDI LED boundaries (shard survivors) ──

#[test]
fn test_midi_led_channel_16_boundary() {
    // Channel 16 is the last legal value; 17 trips `> 16`.
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        channel: 16,
        ..Default::default()
    }));
    assert!(report.is_valid(), "channel 16 legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        channel: 17,
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.channel", 1, 1);
    assert!(report.errors[0].message.contains("got 17"));
}

#[test]
fn test_midi_led_note_velocities_127_boundary() {
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        note_on_velocity: 127,
        note_off_velocity: 127,
        ..Default::default()
    }));
    assert!(report.is_valid(), "127 legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        note_on_velocity: 128,
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.note_on_velocity", 1, 1);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        note_off_velocity: 128,
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.note_off_velocity", 1, 1);
}

#[test]
fn test_midi_led_color_velocity_127_boundary_at_named_path() {
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        colors: crate::config::types::MidiLedColors {
            amber: 127,
            ..Default::default()
        },
        ..Default::default()
    }));
    assert!(report.is_valid(), "127 legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        colors: crate::config::types::MidiLedColors {
            amber: 128,
            ..Default::default()
        },
        ..Default::default()
    }));
    // Exact per-color path pins the loop's name interpolation.
    assert_errors_at(&report, "led.midi.colors.amber", 1, 1);
    assert!(report.errors[0].message.contains("got 128"));
}

#[test]
fn test_midi_led_custom_mapping_pad_127_boundary() {
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(127, note_on(1, 1), note_on(1, 0))],
        ..Default::default()
    }));
    assert!(report.is_valid(), "pad 127 legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(128, note_on(1, 1), note_on(1, 0))],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[0]", 1, 1);
    assert!(report.errors[0].message.contains("got 128"));
}

#[test]
fn test_midi_led_duplicate_pad_detection_polarity() {
    // Distinct pads → clean (kills `delete !`, which would flag every
    // insert as a duplicate); same pad twice → exactly one error, at
    // the SECOND occurrence's index.
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![
            custom_mapping(1, note_on(1, 1), note_on(1, 0)),
            custom_mapping(2, note_on(2, 1), note_on(2, 0)),
        ],
        ..Default::default()
    }));
    assert!(
        report.is_valid(),
        "distinct pads legal: {:?}",
        report.errors
    );

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![
            custom_mapping(7, note_on(1, 1), note_on(1, 0)),
            custom_mapping(7, note_on(2, 1), note_on(2, 0)),
        ],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[1]", 1, 1);
    assert!(
        report.errors[0]
            .message
            .contains("Duplicate custom mapping for pad 7")
    );
}

#[test]
fn test_midi_led_message_note_and_velocity_boundaries() {
    // NoteOn arm: note and velocity each pinned at 127/128, with the
    // led_on / led_off sub-path exact. Any error here also kills the
    // replace-whole-fn-with-() mutant on validate_midi_led_message.
    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, note_on(127, 127), note_on(127, 127))],
        ..Default::default()
    }));
    assert!(report.is_valid(), "127s legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, note_on(128, 0), note_on(0, 0))],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[0].led_on", 1, 1);
    assert!(
        report.errors[0]
            .message
            .contains("Note must be 0-127, got 128")
    );

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, note_on(0, 0), note_on(0, 128))],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[0].led_off", 1, 1);
    assert!(
        report.errors[0]
            .message
            .contains("Velocity must be 0-127, got 128")
    );
}

#[test]
fn test_midi_led_message_cc_boundaries() {
    use crate::config::types::MidiLedMessage;
    let cc = |cc: u8, value: u8| MidiLedMessage::Cc { cc, value };

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, cc(127, 127), cc(0, 0))],
        ..Default::default()
    }));
    assert!(report.is_valid(), "127s legal: {:?}", report.errors);

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, cc(128, 0), cc(0, 0))],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[0].led_on", 1, 1);
    assert!(
        report.errors[0]
            .message
            .contains("CC must be 0-127, got 128")
    );

    let report = validate_config(&midi_led_config(crate::config::types::MidiLedConfig {
        custom_mappings: vec![custom_mapping(0, cc(0, 0), cc(0, 128))],
        ..Default::default()
    }));
    assert_errors_at(&report, "led.midi.custom_mappings[0].led_off", 1, 1);
    assert!(
        report.errors[0]
            .message
            .contains("Value must be 0-127, got 128")
    );
}
