// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Interactive MIDI Device Simulator CLI
//!
//! This tool provides an interactive command-line interface for simulating
//! MIDI device events without requiring physical hardware.
//!
//! This diagnostic tool uses the test simulator module from conductor-daemon/tests/
//! No conductor_core imports needed - this is a standalone testing utility.

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// Import the simulator helper from THIS crate's tests module
// (conductor-daemon/tests/midi_simulator.rs).
//
// #2137/#2139: `#[path]` is resolved relative to this file's directory
// (conductor-daemon/src/bin/). Three `../` reached the REPO-ROOT crate's
// `tests/midi_simulator.rs` — a different, divergent copy outside this crate —
// instead of the intended daemon helper. Two `../` correctly targets
// conductor-daemon/tests/midi_simulator.rs.
#[path = "../../tests/midi_simulator.rs"]
mod midi_simulator;

use midi_simulator::{EncoderDirection, Gesture, MidiSimulator};

/// Parse a MIDI **data byte** from CLI input, rejecting anything outside the
/// 7-bit range 0–127.
///
/// #2130: the simulator parsed note / velocity / CC number / CC value /
/// pressure as a plain `u8`, so values 128–255 were accepted and then silently
/// masked to 7 bits by the underlying simulator (`& 0x7F`) — e.g. `note 200`
/// became note 72. A diagnostic tool must reject impossible MIDI data, not
/// quietly reinterpret it. Parsing as a wide signed integer means ANY numeric
/// input outside 0–127 — including negatives and values larger than `u16`
/// (e.g. `70000`) — gets the explicit out-of-range message; only genuinely
/// non-numeric input falls to "not a valid number".
fn parse_midi_data_byte(s: &str) -> Result<u8, String> {
    match s.parse::<i64>() {
        Ok(v) if (0..=127).contains(&v) => Ok(v as u8),
        Ok(v) => Err(format!("{v} is out of the MIDI data-byte range 0–127")),
        Err(_) => Err(format!("'{s}' is not a valid number")),
    }
}

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║          Conductor - Interactive MIDI Simulator             ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    let simulator = Arc::new(Mutex::new(MidiSimulator::new(0)));
    simulator.lock().unwrap().set_debug(true);

    print_help();

    loop {
        print!("\n> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        let parts: Vec<&str> = input.split_whitespace().collect();
        let command = parts[0].to_lowercase();

        match command.as_str() {
            "help" | "h" | "?" => print_help(),
            "quit" | "exit" | "q" => {
                println!("Goodbye!");
                break;
            }
            "clear" | "c" => {
                simulator.lock().unwrap().clear_events();
                println!("✓ Event queue cleared");
            }
            "events" | "e" => {
                // #1434: `events` is a read-only display (the help advertises
                // `clear` as the destructive command). Use the non-clearing
                // accessor so inspecting the queue doesn't consume it.
                let events = simulator.lock().unwrap().peek_events();
                if events.is_empty() {
                    println!("No events in queue");
                } else {
                    println!("Captured events:");
                    for (i, event) in events.iter().enumerate() {
                        println!("  {}: {:02X?}", i + 1, event);
                    }
                }
            }

            // Note commands
            "note" | "n" => {
                if parts.len() < 3 {
                    println!("Usage: note <number> <velocity>");
                    continue;
                }
                let note = match parse_midi_data_byte(parts[1]) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("✗ Invalid note number: {e}");
                        continue;
                    }
                };
                let velocity = match parse_midi_data_byte(parts[2]) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("✗ Invalid velocity: {e}");
                        continue;
                    }
                };
                let sim = simulator.lock().unwrap();
                sim.note_on(note, velocity);
                thread::sleep(Duration::from_millis(100));
                sim.note_off(note);
                println!("✓ Sent note {} with velocity {}", note, velocity);
            }

            // Velocity test
            "velocity" | "v" => {
                if parts.len() < 2 {
                    println!("Usage: velocity <note>");
                    continue;
                }
                let note = match parse_midi_data_byte(parts[1]) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("✗ Invalid note number: {e}");
                        continue;
                    }
                };
                println!("Simulating velocity levels (soft, medium, hard)...");
                let sim = simulator.lock().unwrap();

                // Soft
                sim.note_on(note, 30);
                thread::sleep(Duration::from_millis(100));
                sim.note_off(note);
                thread::sleep(Duration::from_millis(200));

                // Medium
                sim.note_on(note, 70);
                thread::sleep(Duration::from_millis(100));
                sim.note_off(note);
                thread::sleep(Duration::from_millis(200));

                // Hard
                sim.note_on(note, 110);
                thread::sleep(Duration::from_millis(100));
                sim.note_off(note);

                println!("✓ Velocity test complete");
            }

            // Long press
            "long" | "l" => {
                if parts.len() < 2 {
                    println!("Usage: long <note> [duration_ms]");
                    continue;
                }
                // #1433: reject invalid input instead of silently defaulting —
                // a diagnostic simulator must emit exactly what was requested.
                // #2130: also reject out-of-range MIDI data bytes (128–255).
                let note = match parse_midi_data_byte(parts[1]) {
                    Ok(n) => n,
                    Err(e) => {
                        println!("✗ Invalid note number: {e}");
                        continue;
                    }
                };
                let duration = match parts.get(2) {
                    None => 2500,
                    Some(s) => match s.parse::<u64>() {
                        Ok(d) => d,
                        Err(_) => {
                            println!("✗ Invalid duration: '{}'", s);
                            continue;
                        }
                    },
                };

                println!("Simulating long press for {}ms...", duration);
                simulator
                    .lock()
                    .unwrap()
                    .perform_gesture(Gesture::LongPress {
                        note,
                        velocity: 80,
                        hold_ms: duration,
                    });
                println!("✓ Long press complete");
            }

            // Double tap
            "double" | "d" => {
                if parts.len() < 2 {
                    println!("Usage: double <note> [gap_ms]");
                    continue;
                }
                // #1433: reject invalid input instead of silently defaulting.
                // #2130: also reject out-of-range MIDI data bytes (128–255).
                let note = match parse_midi_data_byte(parts[1]) {
                    Ok(n) => n,
                    Err(e) => {
                        println!("✗ Invalid note number: {e}");
                        continue;
                    }
                };
                let gap = match parts.get(2) {
                    None => 200,
                    Some(s) => match s.parse::<u64>() {
                        Ok(g) => g,
                        Err(_) => {
                            println!("✗ Invalid gap: '{}'", s);
                            continue;
                        }
                    },
                };

                println!("Simulating double-tap with {}ms gap...", gap);
                simulator
                    .lock()
                    .unwrap()
                    .perform_gesture(Gesture::DoubleTap {
                        note,
                        velocity: 80,
                        tap_duration_ms: 50,
                        gap_ms: gap,
                    });
                println!("✓ Double-tap complete");
            }

            // Chord
            "chord" => {
                if parts.len() < 2 {
                    println!("Usage: chord <note1> <note2> [note3] [note4]...");
                    continue;
                }

                // #1433: fail the whole chord if ANY note token is invalid,
                // rather than silently dropping it and emitting a different
                // chord than the user typed. #2130: reject out-of-range
                // (128–255) data bytes too — `collect::<Result<…>>` short-
                // circuits on the first bad token.
                let notes: Vec<u8> = match parts[1..]
                    .iter()
                    .map(|s| parse_midi_data_byte(s))
                    .collect::<Result<Vec<u8>, _>>()
                {
                    Ok(v) => v,
                    Err(e) => {
                        println!("✗ Invalid note in chord: {e}");
                        continue;
                    }
                };

                if notes.len() < 2 {
                    println!("✗ Need at least 2 notes for a chord");
                    continue;
                }

                println!("Simulating chord: {:?}", notes);
                simulator.lock().unwrap().perform_gesture(Gesture::Chord {
                    notes,
                    velocity: 80,
                    stagger_ms: 10,
                    hold_ms: 500,
                });
                println!("✓ Chord complete");
            }

            // Encoder
            "encoder" | "enc" => {
                if parts.len() < 3 {
                    println!("Usage: encoder <cc> <cw|ccw> [steps]");
                    continue;
                }

                // #1433: reject an invalid CC instead of defaulting to CC1.
                // #2130: also reject out-of-range CC numbers (128–255).
                let cc = match parse_midi_data_byte(parts[1]) {
                    Ok(c) => c,
                    Err(e) => {
                        println!("✗ Invalid CC number: {e}");
                        continue;
                    }
                };
                let direction = match parts[2].to_lowercase().as_str() {
                    "cw" | "clockwise" => EncoderDirection::Clockwise,
                    "ccw" | "counterclockwise" => EncoderDirection::CounterClockwise,
                    _ => {
                        println!("✗ Direction must be 'cw' or 'ccw'");
                        continue;
                    }
                };
                let steps = match parts.get(3) {
                    None => 5,
                    Some(s) => match s.parse::<u8>() {
                        Ok(v) => v,
                        Err(_) => {
                            println!("✗ Invalid steps: '{}'", s);
                            continue;
                        }
                    },
                };

                println!(
                    "Simulating encoder CC{} {:?} {} steps...",
                    cc, direction, steps
                );
                simulator
                    .lock()
                    .unwrap()
                    .perform_gesture(Gesture::EncoderTurn {
                        cc,
                        direction,
                        steps,
                        step_delay_ms: 50,
                    });
                println!("✓ Encoder simulation complete");
            }

            // Aftertouch
            "aftertouch" | "at" => {
                if parts.len() < 2 {
                    println!("Usage: aftertouch <pressure>");
                    continue;
                }
                let pressure = match parse_midi_data_byte(parts[1]) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("✗ Invalid pressure: {e}");
                        continue;
                    }
                };
                simulator.lock().unwrap().aftertouch(pressure);
                println!("✓ Sent aftertouch pressure {}", pressure);
            }

            // Pitch bend
            "pitch" | "pb" => {
                if parts.len() < 2 {
                    println!("Usage: pitch <value>");
                    println!("  value: 0-16383 (center=8192)");
                    continue;
                }
                if let Ok(value) = parts[1].parse::<u16>() {
                    if value > 16383 {
                        println!("✗ Pitch bend value must be 0-16383");
                        continue;
                    }
                    simulator.lock().unwrap().pitch_bend(value);
                    println!("✓ Sent pitch bend {}", value);
                } else {
                    println!("✗ Invalid pitch bend value");
                }
            }

            // Control Change
            "cc" => {
                if parts.len() < 3 {
                    println!("Usage: cc <number> <value>");
                    continue;
                }
                let cc = match parse_midi_data_byte(parts[1]) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("✗ Invalid CC number: {e}");
                        continue;
                    }
                };
                let value = match parse_midi_data_byte(parts[2]) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("✗ Invalid CC value: {e}");
                        continue;
                    }
                };
                simulator.lock().unwrap().control_change(cc, value);
                println!("✓ Sent CC{} = {}", cc, value);
            }

            // Demo scenarios
            "demo" => {
                println!("\nRunning demonstration scenarios...\n");
                run_demo(&simulator);
                println!("\n✓ Demo complete");
            }

            "scenario" | "s" => {
                if parts.len() < 2 {
                    println!("Available scenarios:");
                    println!("  1. velocity    - Test all velocity levels");
                    println!("  2. timing      - Test short/medium/long press");
                    println!("  3. doubletap   - Test double-tap detection");
                    println!("  4. chord       - Test chord detection");
                    println!("  5. encoder     - Test encoder rotation");
                    println!("  6. complex     - Complex mixed scenario");
                    continue;
                }

                match parts[1] {
                    "velocity" | "1" => run_velocity_scenario(&simulator),
                    "timing" | "2" => run_timing_scenario(&simulator),
                    "doubletap" | "3" => run_doubletap_scenario(&simulator),
                    "chord" | "4" => run_chord_scenario(&simulator),
                    "encoder" | "5" => run_encoder_scenario(&simulator),
                    "complex" | "6" => run_complex_scenario(&simulator),
                    _ => println!("✗ Unknown scenario"),
                }
            }

            _ => {
                println!(
                    "✗ Unknown command '{}'. Type 'help' for available commands.",
                    command
                );
            }
        }
    }
}

fn print_help() {
    println!("╭─────────────────────────────────────────────────────────────╮");
    println!("│ COMMANDS                                                    │");
    println!("├─────────────────────────────────────────────────────────────┤");
    println!("│ Basic:                                                      │");
    println!("│   help, h, ?              Show this help message            │");
    println!("│   quit, exit, q           Exit the simulator                │");
    println!("│   clear, c                Clear event queue                 │");
    println!("│   events, e               Show captured events              │");
    println!("├─────────────────────────────────────────────────────────────┤");
    println!("│ MIDI Events:                                                │");
    println!("│   note <num> <vel>        Send note on/off                  │");
    println!("│   velocity <note>         Test velocity levels              │");
    println!("│   long <note> [ms]        Simulate long press               │");
    println!("│   double <note> [gap_ms]  Simulate double-tap               │");
    println!("│   chord <n1> <n2> ...     Simulate chord                    │");
    println!("│   encoder <cc> <cw|ccw>   Simulate encoder rotation         │");
    println!("│   aftertouch <pressure>   Send aftertouch                   │");
    println!("│   pitch <value>           Send pitch bend (0-16383)         │");
    println!("│   cc <num> <val>          Send control change               │");
    println!("├─────────────────────────────────────────────────────────────┤");
    println!("│ Scenarios:                                                  │");
    println!("│   demo                    Run full demonstration            │");
    println!("│   scenario [name]         Run specific test scenario        │");
    println!("╰─────────────────────────────────────────────────────────────╯");
}

fn run_demo(simulator: &Arc<Mutex<MidiSimulator>>) {
    println!("1. Testing velocity levels...");
    let sim = simulator.lock().unwrap();
    sim.perform_gesture(Gesture::VelocityRamp {
        note: 60,
        min_velocity: 20,
        max_velocity: 120,
        steps: 3,
    });
    drop(sim);
    thread::sleep(Duration::from_millis(500));

    println!("2. Testing long press...");
    simulator
        .lock()
        .unwrap()
        .perform_gesture(Gesture::LongPress {
            note: 61,
            velocity: 80,
            hold_ms: 2500,
        });

    println!("3. Testing double-tap...");
    simulator
        .lock()
        .unwrap()
        .perform_gesture(Gesture::DoubleTap {
            note: 62,
            velocity: 80,
            tap_duration_ms: 50,
            gap_ms: 200,
        });

    println!("4. Testing chord...");
    simulator.lock().unwrap().perform_gesture(Gesture::Chord {
        notes: vec![60, 64, 67],
        velocity: 80,
        stagger_ms: 10,
        hold_ms: 500,
    });

    println!("5. Testing encoder...");
    simulator
        .lock()
        .unwrap()
        .perform_gesture(Gesture::EncoderTurn {
            cc: 1,
            direction: EncoderDirection::Clockwise,
            steps: 10,
            step_delay_ms: 30,
        });
}

fn run_velocity_scenario(simulator: &Arc<Mutex<MidiSimulator>>) {
    println!("Testing velocity levels: Soft (30), Medium (70), Hard (110)");
    let sim = simulator.lock().unwrap();

    sim.note_on(60, 30);
    thread::sleep(Duration::from_millis(100));
    sim.note_off(60);
    thread::sleep(Duration::from_millis(200));

    sim.note_on(60, 70);
    thread::sleep(Duration::from_millis(100));
    sim.note_off(60);
    thread::sleep(Duration::from_millis(200));

    sim.note_on(60, 110);
    thread::sleep(Duration::from_millis(100));
    sim.note_off(60);

    println!("✓ Velocity scenario complete");
}

fn run_timing_scenario(simulator: &Arc<Mutex<MidiSimulator>>) {
    println!("Testing press durations: Short (100ms), Medium (500ms), Long (2500ms)");

    // Short press
    simulator
        .lock()
        .unwrap()
        .perform_gesture(Gesture::SimpleTap {
            note: 60,
            velocity: 80,
            duration_ms: 100,
        });
    thread::sleep(Duration::from_millis(300));

    // Medium press
    simulator
        .lock()
        .unwrap()
        .perform_gesture(Gesture::SimpleTap {
            note: 60,
            velocity: 80,
            duration_ms: 500,
        });
    thread::sleep(Duration::from_millis(300));

    // Long press
    simulator
        .lock()
        .unwrap()
        .perform_gesture(Gesture::LongPress {
            note: 60,
            velocity: 80,
            hold_ms: 2500,
        });

    println!("✓ Timing scenario complete");
}

fn run_doubletap_scenario(simulator: &Arc<Mutex<MidiSimulator>>) {
    println!("Testing double-tap with 200ms gap");
    simulator
        .lock()
        .unwrap()
        .perform_gesture(Gesture::DoubleTap {
            note: 60,
            velocity: 80,
            tap_duration_ms: 50,
            gap_ms: 200,
        });
    println!("✓ Double-tap scenario complete");
}

fn run_chord_scenario(simulator: &Arc<Mutex<MidiSimulator>>) {
    println!("Testing chord detection: C major (60, 64, 67)");
    simulator.lock().unwrap().perform_gesture(Gesture::Chord {
        notes: vec![60, 64, 67],
        velocity: 80,
        stagger_ms: 10,
        hold_ms: 500,
    });
    println!("✓ Chord scenario complete");
}

fn run_encoder_scenario(simulator: &Arc<Mutex<MidiSimulator>>) {
    println!("Testing encoder: 5 steps CW, then 5 steps CCW");

    simulator
        .lock()
        .unwrap()
        .perform_gesture(Gesture::EncoderTurn {
            cc: 1,
            direction: EncoderDirection::Clockwise,
            steps: 5,
            step_delay_ms: 50,
        });

    thread::sleep(Duration::from_millis(500));

    simulator
        .lock()
        .unwrap()
        .perform_gesture(Gesture::EncoderTurn {
            cc: 1,
            direction: EncoderDirection::CounterClockwise,
            steps: 5,
            step_delay_ms: 50,
        });

    println!("✓ Encoder scenario complete");
}

fn run_complex_scenario(simulator: &Arc<Mutex<MidiSimulator>>) {
    println!("Running complex scenario: mixed events...");

    let sim = simulator.lock().unwrap();

    // Note + encoder
    sim.note_on(60, 80);
    thread::sleep(Duration::from_millis(100));
    sim.control_change(1, 70);
    thread::sleep(Duration::from_millis(100));
    sim.note_off(60);

    thread::sleep(Duration::from_millis(300));

    // Chord + aftertouch
    sim.note_on(60, 80);
    sim.note_on(64, 80);
    sim.note_on(67, 80);
    thread::sleep(Duration::from_millis(200));
    sim.aftertouch(100);
    thread::sleep(Duration::from_millis(300));
    sim.note_off(60);
    sim.note_off(64);
    sim.note_off(67);

    thread::sleep(Duration::from_millis(300));

    // Pitch bend + notes
    sim.pitch_bend(8192); // Center
    thread::sleep(Duration::from_millis(100));
    sim.note_on(72, 100);
    thread::sleep(Duration::from_millis(100));
    sim.pitch_bend(12000); // Bend up
    thread::sleep(Duration::from_millis(100));
    sim.pitch_bend(8192); // Back to center
    thread::sleep(Duration::from_millis(100));
    sim.note_off(72);

    println!("✓ Complex scenario complete");
}

#[cfg(test)]
mod tests {
    use super::parse_midi_data_byte;

    /// #2137/#2139: the bin must include **this crate's** simulator helper
    /// (`conductor-daemon/tests/midi_simulator.rs`), not the repo-root crate's
    /// divergent copy. `HELPER_CRATE` exists only in the daemon copy, so if the
    /// `#[path]` regresses to three `../` (the root crate) this fails to
    /// compile.
    #[test]
    fn includes_the_daemon_test_helper() {
        assert_eq!(super::midi_simulator::HELPER_CRATE, "conductor-daemon");
    }

    /// #2130: the boundary value 127 is the maximum legal MIDI data byte.
    #[test]
    fn accepts_valid_data_bytes_including_boundaries() {
        assert_eq!(parse_midi_data_byte("0"), Ok(0));
        assert_eq!(parse_midi_data_byte("64"), Ok(64));
        assert_eq!(parse_midi_data_byte("127"), Ok(127));
    }

    /// #2130: 128–255 are valid `u8` values but NOT valid MIDI data bytes — the
    /// old `.parse::<u8>()` accepted them and the simulator masked them to 7
    /// bits (`note 200` → note 72). They must now be rejected, not reinterpreted.
    #[test]
    fn rejects_data_bytes_above_127() {
        // 128 is the first impossible value; the old code silently masked it to 0.
        assert!(parse_midi_data_byte("128").is_err());
        // 200 is the example from the finding (would have become 200 & 0x7F = 72).
        let err = parse_midi_data_byte("200").expect_err("200 must be rejected");
        assert!(
            err.contains("0–127"),
            "rejection should name the valid range; got: {err}"
        );
        assert!(parse_midi_data_byte("255").is_err());
    }

    /// Numeric-but-out-of-range inputs (including values larger than `u16` and
    /// negatives) get the explicit range message, NOT a misleading "not a
    /// number" — only genuinely non-numeric input is "not a valid number".
    #[test]
    fn out_of_range_numbers_report_the_range_not_a_parse_error() {
        let big = parse_midi_data_byte("70000").expect_err("70000 must be rejected");
        assert!(big.contains("0–127"), "got: {big}");
        let neg = parse_midi_data_byte("-1").expect_err("-1 must be rejected");
        assert!(neg.contains("0–127"), "got: {neg}");
        // Genuinely non-numeric input is the only "not a valid number" case.
        let nan = parse_midi_data_byte("abc").expect_err("abc must be rejected");
        assert!(nan.contains("not a valid number"), "got: {nan}");
    }
}
