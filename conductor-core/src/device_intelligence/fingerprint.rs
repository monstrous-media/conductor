// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Event fingerprinting engine (ADR-022 Phase 5D, #755)
//!
//! Classifies device type from event statistics: note distribution,
//! CC usage, velocity patterns, timing characteristics.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Device type classification based on event fingerprinting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceCategory {
    /// Pad controller (notes clustered, velocity-sensitive)
    PadController,
    /// Keyboard (wide note range, velocity-sensitive)
    Keyboard,
    /// Fader/knob controller (mostly CC events)
    FaderController,
    /// Encoder controller (relative CC values)
    EncoderController,
    /// Game controller (HID events)
    GameController,
    /// Mixed/unknown
    Unknown,
}

/// Event statistics for fingerprinting.
#[derive(Debug, Clone, Default)]
pub struct EventStats {
    pub note_count: usize,
    pub cc_count: usize,
    pub gamepad_count: usize,
    pub unique_notes: HashMap<u8, usize>,
    pub unique_ccs: HashMap<u8, usize>,
    /// Distinct CC *values* seen (value → hit count), across all CC events
    /// (#1451). Needed to tell a relative encoder (values clustered on the
    /// increment/decrement codes, e.g. 1/127 or 63/65) from an absolute fader
    /// (values spread across the 0–127 range) — `unique_ccs` hit-density alone
    /// can't.
    pub cc_values: HashMap<u8, usize>,
    pub velocity_sum: u32,
    pub velocity_count: usize,
    pub min_note: Option<u8>,
    pub max_note: Option<u8>,
    /// Timestamp of last recorded event (milliseconds since epoch)
    pub last_event_ms: Option<u64>,
}

impl EventStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a note event.
    pub fn record_note(&mut self, note: u8, velocity: u8) {
        self.note_count += 1;
        *self.unique_notes.entry(note).or_insert(0) += 1;
        self.velocity_sum += velocity as u32;
        self.velocity_count += 1;
        self.min_note = Some(self.min_note.map_or(note, |m| m.min(note)));
        self.max_note = Some(self.max_note.map_or(note, |m| m.max(note)));
    }

    /// Record a CC event with its value (#1451).
    ///
    /// The value feeds relative-encoder detection in [`classify`]: a relative
    /// encoder emits a small set of increment/decrement codes, while an
    /// absolute fader sweeps across the range.
    pub fn record_cc(&mut self, cc: u8, value: u8) {
        self.cc_count += 1;
        *self.unique_ccs.entry(cc).or_insert(0) += 1;
        *self.cc_values.entry(value).or_insert(0) += 1;
    }

    /// Record a gamepad event.
    pub fn record_gamepad(&mut self) {
        self.gamepad_count += 1;
    }

    /// Update the last event timestamp (milliseconds since epoch).
    pub fn touch(&mut self, now_ms: u64) {
        self.last_event_ms = Some(now_ms);
    }

    /// Classify the device type based on accumulated statistics.
    pub fn classify(&self) -> DeviceCategory {
        let total = self.note_count + self.cc_count + self.gamepad_count;
        if total == 0 {
            return DeviceCategory::Unknown;
        }

        // Gamepad dominance
        if self.gamepad_count as f64 / total as f64 > 0.5 {
            return DeviceCategory::GameController;
        }

        // CC dominance
        if self.cc_count as f64 / total as f64 > 0.7 {
            // A small, densely-hit CC set *could* be a handful of encoders —
            // but only if the VALUES look relative (#1451). A single absolute
            // fader moved >10 times has the same hit-density signature, so
            // requiring relative-value evidence stops it being misread as an
            // encoder. Relative encoders emit a small set of increment/
            // decrement codes (1/127 two's-complement, 63/65 offset); absolute
            // faders sweep across 0–127.
            let max_cc_hits = self.unique_ccs.values().max().copied().unwrap_or(0);
            let dense = max_cc_hits > 10 && self.unique_ccs.len() <= 4;
            let relative_values = self.has_relative_cc_values();
            if dense && relative_values {
                return DeviceCategory::EncoderController;
            }
            return DeviceCategory::FaderController;
        }

        // Note dominance — only commit to a note-device category when notes are
        // the MAJORITY of traffic (#1452). Previously any `note_count > 0` was
        // enough, so a mixed stream (e.g. 40% notes / 60% CC, with CC below its
        // 0.7 bar) was forced into Pad/Keyboard. Requiring note majority keeps
        // genuinely mixed/ambiguous devices in `Unknown` — the enum's documented
        // mixed category — avoiding overconfident binding suggestions, while a
        // pad with a few aux CC knobs (note-majority) still classifies by range.
        if self.note_count as f64 / total as f64 > 0.5 {
            let note_range = match (self.min_note, self.max_note) {
                (Some(lo), Some(hi)) => hi - lo,
                _ => 0,
            };
            if note_range > 36 {
                return DeviceCategory::Keyboard;
            }
            return DeviceCategory::PadController;
        }

        DeviceCategory::Unknown
    }

    /// True when the recorded CC values look like relative-encoder traffic
    /// (#1451): at least two distinct values, ALL confined to the canonical
    /// increment/decrement bands. An absolute fader's spread of values fails
    /// this, so it is no longer misclassified as an encoder after enough
    /// repeated movement. The two-distinct-value floor also stops a fader held
    /// near centre (a single value in the 64 band) reading as relative.
    fn has_relative_cc_values(&self) -> bool {
        self.cc_values.len() >= 2
            && self
                .cc_values
                .keys()
                .all(|&v| Self::is_relative_cc_value(v))
    }

    /// A CC value that fits a relative-encoder increment/decrement code: near 0
    /// or 127 (two's-complement sign-magnitude: +1=1, −1=127) or near 64
    /// (offset binary: +1=65, −1=63). Absolute-fader sweep values land outside
    /// these narrow bands.
    fn is_relative_cc_value(v: u8) -> bool {
        v <= 2 || (62..=66).contains(&v) || v >= 125
    }

    /// Confidence score (0.0 to 1.0) — higher with more events.
    pub fn confidence(&self) -> f64 {
        let total = (self.note_count + self.cc_count + self.gamepad_count) as f64;
        // Saturates at ~50 events
        (total / 50.0).min(1.0)
    }
}

/// Binding suggestion from fingerprinting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingSuggestion {
    pub category: DeviceCategory,
    pub confidence: f64,
    pub suggested_alias: String,
    pub suggested_protocol: String,
    pub reasoning: String,
}

/// Generate a binding suggestion from event statistics.
pub fn suggest_binding(stats: &EventStats, port_name: &str) -> BindingSuggestion {
    let category = stats.classify();
    let confidence = stats.confidence();

    let (alias, protocol, reasoning) = match &category {
        DeviceCategory::PadController => (
            "pads".to_string(),
            "midi".to_string(),
            format!(
                "Detected pad controller: {} unique notes in narrow range",
                stats.unique_notes.len()
            ),
        ),
        DeviceCategory::Keyboard => (
            "keys".to_string(),
            "midi".to_string(),
            format!(
                "Detected keyboard: note range {}-{}",
                stats.min_note.unwrap_or(0),
                stats.max_note.unwrap_or(127)
            ),
        ),
        DeviceCategory::FaderController => (
            "faders".to_string(),
            "midi".to_string(),
            format!(
                "Detected fader controller: {} unique CCs",
                stats.unique_ccs.len()
            ),
        ),
        DeviceCategory::EncoderController => (
            "encoders".to_string(),
            "midi".to_string(),
            format!(
                "Detected encoder controller: {} CCs with high event density",
                stats.unique_ccs.len()
            ),
        ),
        DeviceCategory::GameController => (
            "gamepad".to_string(),
            "hid".to_string(),
            "Detected game controller (HID events)".to_string(),
        ),
        DeviceCategory::Unknown => {
            let alias = port_name
                .split_whitespace()
                .next()
                .unwrap_or("device")
                .to_lowercase();
            (
                alias,
                "midi".to_string(),
                "Insufficient data to classify".to_string(),
            )
        }
    };

    BindingSuggestion {
        category,
        confidence,
        suggested_alias: alias,
        suggested_protocol: protocol,
        reasoning,
    }
}

#[cfg(test)]
#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_pad_controller() {
        let mut stats = EventStats::new();
        for note in 36..52 {
            for _ in 0..3 {
                stats.record_note(note, 100);
            }
        }
        assert_eq!(stats.classify(), DeviceCategory::PadController);
    }

    #[test]
    fn test_classify_keyboard() {
        let mut stats = EventStats::new();
        for note in 21..109 {
            stats.record_note(note, 80);
        }
        assert_eq!(stats.classify(), DeviceCategory::Keyboard);
    }

    #[test]
    fn test_classify_fader() {
        let mut stats = EventStats::new();
        for cc in 0..8 {
            for i in 0..5 {
                // Absolute-fader sweep values (spread across the range).
                stats.record_cc(cc, (i * 25) as u8);
            }
        }
        assert_eq!(stats.classify(), DeviceCategory::FaderController);
    }

    /// #1451 regression. A single absolute fader moved >10 times has the same
    /// hit-density signature an encoder used to be detected by, but its values
    /// sweep the range. Without relative-value evidence it must NOT classify as
    /// an encoder.
    #[test]
    fn test_repeated_absolute_fader_is_not_an_encoder() {
        let mut stats = EventStats::new();
        // CC 7 moved 12 times across the full range (absolute sweep).
        for v in [0u8, 10, 25, 40, 55, 70, 80, 95, 110, 120, 127, 64] {
            stats.record_cc(7, v);
        }
        assert_ne!(
            stats.classify(),
            DeviceCategory::EncoderController,
            "a repeatedly-moved absolute fader must not be read as an encoder (#1451)"
        );
        assert_eq!(stats.classify(), DeviceCategory::FaderController);
    }

    /// Complement to #1451: a genuine relative encoder — values confined to the
    /// increment/decrement codes (here 1 / 127) on a densely-hit CC — still
    /// classifies as an encoder.
    #[test]
    fn test_relative_encoder_is_detected() {
        let mut stats = EventStats::new();
        // CC 16 turned back and forth: +1 (1) and −1 (127), 6 each = 12 hits.
        for _ in 0..6 {
            stats.record_cc(16, 1);
            stats.record_cc(16, 127);
        }
        assert_eq!(
            stats.classify(),
            DeviceCategory::EncoderController,
            "relative-code values (1/127) on a densely-hit CC indicate an encoder"
        );
    }

    /// Offset-binary relative encoding (63 / 65 around centre) is also detected.
    #[test]
    fn test_relative_encoder_offset_binary_is_detected() {
        let mut stats = EventStats::new();
        for _ in 0..6 {
            stats.record_cc(20, 65);
            stats.record_cc(20, 63);
        }
        assert_eq!(stats.classify(), DeviceCategory::EncoderController);
    }

    #[test]
    fn test_classify_gamepad() {
        let mut stats = EventStats::new();
        for _ in 0..20 {
            stats.record_gamepad();
        }
        stats.record_note(60, 100); // minority
        assert_eq!(stats.classify(), DeviceCategory::GameController);
    }

    #[test]
    fn test_classify_unknown_empty() {
        let stats = EventStats::new();
        assert_eq!(stats.classify(), DeviceCategory::Unknown);
    }

    /// #1452: a stream with substantial but NON-dominant note traffic — 40 note
    /// events and 60 CC events spread across enough CC numbers that
    /// `cc_count / total` is not > 0.7 — is a mixed/ambiguous device. It must
    /// classify as `Unknown` (the enum's documented mixed category) rather than
    /// being forced into `PadController` just because `note_count > 0`.
    #[test]
    fn test_classify_mixed_non_dominant_notes_is_unknown() {
        let mut stats = EventStats::new();
        // 40 narrow-range note events — present, but a minority of traffic.
        for _ in 0..40 {
            stats.record_note(38, 100);
        }
        // 60 CC events across 12 CCs with absolute-sweep values: cc/total = 0.6
        // (not > 0.7, so the CC-dominance branch is skipped) and the values are
        // not relative-encoder codes.
        for cc in 0..12u8 {
            for v in 0..5u8 {
                stats.record_cc(cc, v * 25);
            }
        }
        // note/total = 0.40, cc/total = 0.60 — nothing dominates.
        assert_eq!(
            stats.classify(),
            DeviceCategory::Unknown,
            "non-dominant notes (40%) mixed with CC (60%) must stay Unknown, not PadController (#1452)"
        );
    }

    /// #1452 complement: when notes ARE the majority (70% notes / 30% CC) the
    /// device still classifies by note range — a pad controller with a few aux
    /// CC knobs stays `PadController`. Guards against over-correcting the
    /// dominance threshold into rejecting legitimate note devices.
    #[test]
    fn test_classify_dominant_notes_with_minority_cc_is_pad() {
        let mut stats = EventStats::new();
        for _ in 0..70 {
            stats.record_note(40, 100);
        }
        for cc in 0..6u8 {
            for v in 0..5u8 {
                stats.record_cc(cc, v * 25);
            }
        }
        // note/total = 0.70 (> 0.5) — notes dominate.
        assert_eq!(stats.classify(), DeviceCategory::PadController);
    }

    #[test]
    fn test_confidence_scales_with_events() {
        let mut stats = EventStats::new();
        assert_eq!(stats.confidence(), 0.0);
        for _ in 0..25 {
            stats.record_note(60, 100);
        }
        assert!(stats.confidence() > 0.4);
        for _ in 0..25 {
            stats.record_note(64, 100);
        }
        assert!((stats.confidence() - 1.0).abs() < 0.01);
    }

    // ── D7 Accuracy Benchmark (ADR-022 P13: >80%) ──

    /// One labeled device profile for the accuracy benchmark:
    /// `(device_name, event_generator_fn, expected_category)`.
    type LabeledDeviceProfile = (&'static str, Box<dyn Fn(&mut EventStats)>, DeviceCategory);

    /// Labeled device profiles for accuracy testing.
    fn labeled_device_profiles() -> Vec<LabeledDeviceProfile> {
        vec![
            // Pad controllers
            (
                "Maschine Mikro MK3",
                Box::new(|s: &mut EventStats| {
                    for note in 36..52 {
                        for _ in 0..5 {
                            s.record_note(note, 80 + (note % 40));
                        }
                    }
                }),
                DeviceCategory::PadController,
            ),
            (
                "Launchpad Mini MK3",
                Box::new(|s: &mut EventStats| {
                    // Launchpad pads are clustered in a narrow range (not full 0-63)
                    for note in 36..68 {
                        for _ in 0..3 {
                            s.record_note(note, 127);
                        }
                    }
                }),
                DeviceCategory::PadController,
            ),
            (
                "Akai MPD218",
                Box::new(|s: &mut EventStats| {
                    for note in 36..52 {
                        for _ in 0..4 {
                            s.record_note(note, 60 + (note % 50));
                        }
                    }
                }),
                DeviceCategory::PadController,
            ),
            // Keyboards
            (
                "Arturia KeyStep 37",
                Box::new(|s: &mut EventStats| {
                    for note in 36..74 {
                        s.record_note(note, 64);
                    }
                }),
                DeviceCategory::Keyboard,
            ),
            (
                "Roland A-88",
                Box::new(|s: &mut EventStats| {
                    for note in 21..109 {
                        s.record_note(note, 80);
                    }
                }),
                DeviceCategory::Keyboard,
            ),
            (
                "Komplete Kontrol S61",
                Box::new(|s: &mut EventStats| {
                    for note in 24..85 {
                        for _ in 0..2 {
                            s.record_note(note, 70);
                        }
                    }
                }),
                DeviceCategory::Keyboard,
            ),
            // Fader controllers
            (
                "KORG nanoKONTROL2",
                Box::new(|s: &mut EventStats| {
                    // Absolute faders/knobs sweeping across the range.
                    for cc in 0..8 {
                        for i in 0..10 {
                            s.record_cc(cc, (i * 12) as u8);
                        }
                    }
                    for cc in 16..24 {
                        for i in 0..8 {
                            s.record_cc(cc, (i * 16) as u8);
                        }
                    }
                }),
                DeviceCategory::FaderController,
            ),
            (
                "Behringer X-Touch Mini",
                Box::new(|s: &mut EventStats| {
                    for cc in 1..9 {
                        for i in 0..12 {
                            s.record_cc(cc, (i * 10) as u8);
                        }
                    }
                    for cc in 11..19 {
                        for i in 0..6 {
                            s.record_cc(cc, (i * 20) as u8);
                        }
                    }
                }),
                DeviceCategory::FaderController,
            ),
            // Game controllers
            (
                "Xbox Controller",
                Box::new(|s: &mut EventStats| {
                    for _ in 0..30 {
                        s.record_gamepad();
                    }
                    s.record_note(60, 100); // occasional minority
                }),
                DeviceCategory::GameController,
            ),
            (
                "PS5 DualSense",
                Box::new(|s: &mut EventStats| {
                    for _ in 0..25 {
                        s.record_gamepad();
                    }
                }),
                DeviceCategory::GameController,
            ),
        ]
    }

    #[test]
    fn test_fingerprint_accuracy_above_80_percent() {
        let profiles = labeled_device_profiles();
        let total = profiles.len();
        let mut correct = 0;

        for (name, generator, expected) in &profiles {
            let mut stats = EventStats::new();
            generator(&mut stats);
            let actual = stats.classify();
            if actual == *expected {
                correct += 1;
            } else {
                eprintln!(
                    "MISCLASSIFIED: {} — expected {:?}, got {:?}",
                    name, expected, actual
                );
            }
        }

        let accuracy = correct as f64 / total as f64;
        assert!(
            accuracy >= 0.8,
            "Fingerprint accuracy {:.0}% ({}/{}) is below 80% threshold",
            accuracy * 100.0,
            correct,
            total
        );
        // Print accuracy for visibility
        eprintln!(
            "Fingerprint accuracy: {:.0}% ({}/{})",
            accuracy * 100.0,
            correct,
            total
        );
    }

    #[test]
    fn test_suggest_binding_pad() {
        let mut stats = EventStats::new();
        for note in 36..52 {
            stats.record_note(note, 100);
        }
        let suggestion = suggest_binding(&stats, "Maschine Mikro");
        assert_eq!(suggestion.category, DeviceCategory::PadController);
        assert_eq!(suggestion.suggested_alias, "pads");
        assert_eq!(suggestion.suggested_protocol, "midi");
    }
}
