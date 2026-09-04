// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Tests for EventProcessor chord detection with configurable timeout

use conductor_core::event_processor::{EventProcessor, ProcessedEvent};
use conductor_core::events::MidiEvent;
use std::time::{Duration, Instant};

#[test]
fn test_chord_three_notes_default_timeout() {
    // Default 50ms timeout — 3 notes pressed within 50ms should all be captured
    let mut processor = EventProcessor::new();
    let base = Instant::now();

    // Press 3 notes within 20ms of each other (well within 50ms window)
    let events1 = processor.process(MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 0,
        time: base,
    });
    assert!(
        events1
            .iter()
            .all(|e| !matches!(e, ProcessedEvent::ChordDetected { .. }))
    );

    let events2 = processor.process(MidiEvent::NoteOn {
        note: 40,
        velocity: 90,
        channel: 0,
        time: base + Duration::from_millis(10),
    });
    // Should have a 2-note chord with exactly [36, 40] (velocities [100, 90])
    let chord2 = events2.iter().find_map(|e| match e {
        ProcessedEvent::ChordDetected {
            notes, velocities, ..
        } => Some((notes.clone(), velocities.clone())),
        _ => None,
    });
    let (notes2, velocities2) = chord2.expect("2-note chord should be detected");
    assert_eq!(
        notes2,
        vec![36, 40],
        "intermediate chord must be exactly [36, 40]"
    );
    assert_eq!(velocities2, vec![100, 90]);

    let events3 = processor.process(MidiEvent::NoteOn {
        note: 44,
        velocity: 80,
        channel: 0,
        time: base + Duration::from_millis(20),
    });
    // Should have a 3-note chord with exactly [36, 40, 44] (velocities [100, 90, 80])
    let chord3 = events3.iter().find_map(|e| match e {
        ProcessedEvent::ChordDetected {
            notes, velocities, ..
        } => Some((notes.clone(), velocities.clone())),
        _ => None,
    });
    let (notes3, velocities3) = chord3.expect("3-note chord should be detected");
    assert_eq!(
        notes3,
        vec![36, 40, 44],
        "all three notes must be captured in order"
    );
    assert_eq!(velocities3, vec![100, 90, 80]);
}

#[test]
fn test_with_chord_timeout_extended_window() {
    // Extended 150ms timeout — 3 notes spread over 100ms should all be captured
    let mut processor = EventProcessor::with_chord_timeout(150);
    let base = Instant::now();

    processor.process(MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 0,
        time: base,
    });
    processor.process(MidiEvent::NoteOn {
        note: 40,
        velocity: 90,
        channel: 0,
        time: base + Duration::from_millis(60),
    });
    let events3 = processor.process(MidiEvent::NoteOn {
        note: 44,
        velocity: 80,
        channel: 0,
        time: base + Duration::from_millis(100),
    });

    let chord = events3.iter().find_map(|e| match e {
        ProcessedEvent::ChordDetected {
            notes, velocities, ..
        } => Some((notes.clone(), velocities.clone())),
        _ => None,
    });
    let (notes, velocities) = chord.expect("3-note chord should be detected with 150ms window");
    assert_eq!(
        notes,
        vec![36, 40, 44],
        "all three notes captured within 150ms window"
    );
    assert_eq!(velocities, vec![100, 90, 80]);
}

#[test]
fn test_default_timeout_drops_slow_third_note() {
    // Default 50ms timeout — note 1 at 0ms, note 2 at 30ms, note 3 at 70ms
    // Note 1 should be dropped from chord buffer (70ms > 50ms from note 1)
    let mut processor = EventProcessor::new();
    let base = Instant::now();

    processor.process(MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 0,
        time: base,
    });
    processor.process(MidiEvent::NoteOn {
        note: 40,
        velocity: 90,
        channel: 0,
        time: base + Duration::from_millis(30),
    });
    let events3 = processor.process(MidiEvent::NoteOn {
        note: 44,
        velocity: 80,
        channel: 0,
        time: base + Duration::from_millis(70),
    });

    let chord = events3.iter().find_map(|e| match e {
        ProcessedEvent::ChordDetected {
            notes, velocities, ..
        } => Some((notes.clone(), velocities.clone())),
        _ => None,
    });
    // Note 36 (at 0ms) is >50ms before note 44 (at 70ms), so it is pruned;
    // exactly [40, 44] must remain — asserting the contents, not just the
    // count, so a regression that kept the wrong note (or stale 36) is caught.
    let (notes, velocities) = chord.expect("a chord should still be detected");
    assert_eq!(
        notes,
        vec![40, 44],
        "the slow first note (36) must be dropped, leaving exactly [40, 44]"
    );
    assert_eq!(velocities, vec![90, 80]);
}

#[test]
fn test_two_note_chord_still_works() {
    // Regression: 2-note chord should still be detected
    let mut processor = EventProcessor::new();
    let base = Instant::now();

    processor.process(MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 0,
        time: base,
    });
    let events2 = processor.process(MidiEvent::NoteOn {
        note: 40,
        velocity: 90,
        channel: 0,
        time: base + Duration::from_millis(10),
    });

    let chord = events2.iter().find_map(|e| match e {
        ProcessedEvent::ChordDetected {
            notes, velocities, ..
        } => Some((notes.clone(), velocities.clone())),
        _ => None,
    });
    assert!(chord.is_some());
    let (notes, velocities) = chord.unwrap();
    assert_eq!(notes.len(), 2);
    assert_eq!(notes, vec![36, 40]);
    assert_eq!(velocities, vec![100, 90]);
}

#[test]
fn test_set_chord_timeout_dynamic() {
    // Start with default 50ms, then switch to 150ms dynamically
    let mut processor = EventProcessor::new();
    assert_eq!(processor.chord_timeout(), Duration::from_millis(50));

    // Dynamically extend timeout (simulates MIDI Learn start)
    processor.set_chord_timeout(Duration::from_millis(150));
    assert_eq!(processor.chord_timeout(), Duration::from_millis(150));

    let base = Instant::now();

    // 3 notes spread over 100ms — should all be captured with 150ms timeout
    processor.process(MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 0,
        time: base,
    });
    processor.process(MidiEvent::NoteOn {
        note: 40,
        velocity: 90,
        channel: 0,
        time: base + Duration::from_millis(60),
    });
    let events3 = processor.process(MidiEvent::NoteOn {
        note: 44,
        velocity: 80,
        channel: 0,
        time: base + Duration::from_millis(100),
    });

    let chord = events3.iter().find_map(|e| match e {
        ProcessedEvent::ChordDetected {
            notes, velocities, ..
        } => Some((notes.clone(), velocities.clone())),
        _ => None,
    });
    let (notes, velocities) =
        chord.expect("3-note chord should be detected after set_chord_timeout(150ms)");
    assert_eq!(
        notes,
        vec![36, 40, 44],
        "all three notes captured after dynamic 150ms"
    );
    assert_eq!(velocities, vec![100, 90, 80]);

    // Reset back to default (simulates MIDI Learn stop)
    processor.set_chord_timeout(Duration::from_millis(50));
    assert_eq!(processor.chord_timeout(), Duration::from_millis(50));

    // The getter check above only proves the stored value changed.
    // Verify the reset actually took BEHAVIORAL effect — i.e. pruning now
    // uses the 50ms window again, not the previous 150ms. Process a fresh
    // 3-note sequence spaced 0/30/70ms (start well after the earlier notes so
    // they are long out of any window). The first note (at +0ms) is 70ms
    // before the third (>50ms), so under the restored 50ms window it must be
    // pruned, leaving exactly [40, 44]. If `set_chord_timeout` had updated the
    // stored value but left pruning on the old 150ms window, all three would
    // survive ([36, 40, 44]) and this assertion would fail — which is the
    // exact regression the test now guards.
    let base2 = base + Duration::from_millis(500);
    processor.process(MidiEvent::NoteOn {
        note: 36,
        velocity: 100,
        channel: 0,
        time: base2,
    });
    processor.process(MidiEvent::NoteOn {
        note: 40,
        velocity: 90,
        channel: 0,
        time: base2 + Duration::from_millis(30),
    });
    let events_after_reset = processor.process(MidiEvent::NoteOn {
        note: 44,
        velocity: 80,
        channel: 0,
        time: base2 + Duration::from_millis(70),
    });

    let chord_after_reset = events_after_reset.iter().find_map(|e| match e {
        ProcessedEvent::ChordDetected {
            notes, velocities, ..
        } => Some((notes.clone(), velocities.clone())),
        _ => None,
    });
    let (notes_after, velocities_after) =
        chord_after_reset.expect("a chord should be detected after the reset");
    assert_eq!(
        notes_after,
        vec![40, 44],
        "after reset to 50ms, the slow first note (36) must be pruned again — \
         proving pruning uses the restored 50ms window, not the prior 150ms"
    );
    assert_eq!(velocities_after, vec![90, 80]);
}
