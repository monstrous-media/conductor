// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use colored::Colorize;
use midi_msg::{ChannelVoiceMsg, MidiMsg};
use midir::MidiInput;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Held notes are keyed by `(channel, note)` — NOT note alone. Keying by
/// note collapsed the same note held on different channels: a later press
/// overwrote the earlier press time, and a release on either channel removed the
/// single entry, so multi-channel / layered controllers reported wrong held
/// state and durations. The 1-based display channel is used as the key's channel
/// component (consistent with the rendered output).
type HeldNotes = HashMap<(u8, u8), Instant>;

/// Record a note press for `(ch, note)` at `at`.
fn track_note_on(held: &mut HeldNotes, ch: u8, note: u8, at: Instant) {
    held.insert((ch, note), at);
}

/// Remove the press for `(ch, note)`, returning its own press time if it was
/// being tracked. Releases on one channel never disturb the same note held on
/// another channel.
fn track_note_off(held: &mut HeldNotes, ch: u8, note: u8) -> Option<Instant> {
    held.remove(&(ch, note))
}

// Convert MIDI note number (0-127) to musical note name (e.g., "C4", "A#3")
fn note_to_name(note: u8) -> String {
    const NOTE_NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = (note / 12) as i32 - 1; // MIDI note 60 = C4
    let note_name = NOTE_NAMES[(note % 12) as usize];
    format!("{}{}", note_name, octave)
}

/// Format the `Note OFF` line emitted for a velocity-0 NoteOn (the standard
/// NoteOff encoding). `held` is `Some(duration)` when a matching press was being
/// tracked, `None` for an unmatched release.
///
/// This ALWAYS returns a complete line. The previous inline code only
/// printed inside `if let Some(..)`, so an unmatched velocity-0 NoteOn (the
/// diagnostic started after the note was down, missed the NoteOn, or got a
/// stray release) left the already-printed timestamp/count prefix dangling with
/// no event text or newline — corrupting the terminal output. This mirrors the
/// explicit `NoteOff` branch, which already handled the unmatched case.
fn format_note_off_velocity_zero(
    note_name: &str,
    note: u8,
    ch: u8,
    held: Option<std::time::Duration>,
) -> String {
    match held {
        Some(duration) => format!(
            "{} {:>3} ({:3}) vel=  0 ch={:2} (held {:.3}s)",
            "Note OFF".yellow().bold(),
            note_name.cyan(),
            note,
            ch,
            duration.as_secs_f32()
        ),
        None => format!(
            "{} {:>3} ({:3}) vel=  0 ch={:2}",
            "Note OFF".yellow().bold(),
            note_name.cyan(),
            note,
            ch
        ),
    }
}

/// Resolve the optional port-index CLI argument.
///
/// The previous code did `args().nth(1).and_then(|s| s.parse().ok())`,
/// so `.ok()` collapsed a PRESENT-but-non-numeric argument (e.g. `abc`) into
/// `None` — indistinguishable from "no argument given" — and the tool then
/// silently auto-selected the Mikro port instead of reporting the bad input.
///
/// This distinguishes the three cases:
/// - `None` (no argument) → `Ok(None)`: caller may auto-select.
/// - `Some(numeric)`     → `Ok(Some(idx))`: use that port.
/// - `Some(non-numeric)` → `Err(msg)`: reject; do NOT fall back to auto-select.
fn parse_port_arg(arg: Option<&str>) -> Result<Option<usize>, String> {
    match arg {
        None => Ok(None),
        Some(s) => s.parse::<usize>().map(Some).map_err(|_| {
            format!("invalid port number '{s}': expected a non-negative integer (e.g. 2)")
        }),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "{}",
        "╔══════════════════════════════════════╗".cyan().bold()
    );
    println!(
        "{}",
        "║      MIDI Diagnostic Tool            ║".cyan().bold()
    );
    println!(
        "{}",
        "╚══════════════════════════════════════╝".cyan().bold()
    );
    println!();

    let midi_in = MidiInput::new("Diagnostic")?;
    let ports = midi_in.ports();

    // List all ports
    println!("{}", "Available MIDI Ports:".green().bold());
    println!("{}", "─".repeat(40).dimmed());
    for (i, port) in ports.iter().enumerate() {
        let name = midi_in.port_name(port)?;
        println!("  {} {}", format!("[{}]", i).cyan(), name);
    }
    println!();

    // Resolve the explicit port argument first: a present-but-invalid
    // argument is rejected here rather than being silently ignored and falling
    // back to Mikro auto-select.
    let arg = std::env::args().nth(1);
    let requested = match parse_port_arg(arg.as_deref()) {
        Ok(requested) => requested,
        Err(message) => {
            // Exit non-zero so scripts/operators can detect the bad
            // argument (a CLI that fails must not report success).
            eprintln!("{}", message.red());
            eprintln!("Usage: {} [port_number]", std::env::args().next().unwrap());
            std::process::exit(1);
        }
    };

    // No explicit port → auto-select the first Mikro MK3 port by name.
    let port_index = requested.or_else(|| {
        ports
            .iter()
            .position(|p| midi_in.port_name(p).unwrap_or_default().contains("Mikro"))
    });

    let port = match port_index {
        Some(idx) if idx < ports.len() => &ports[idx],
        _ => {
            // Same non-zero exit for "no usable port" — it's a failure,
            // not a successful no-op.
            eprintln!("{}", "No Mikro MK3 found and no valid port specified".red());
            eprintln!("Usage: {} [port_number]", std::env::args().next().unwrap());
            std::process::exit(1);
        }
    };

    let port_name = midi_in.port_name(port)?;
    println!("{} {}", "Connecting to:".green(), port_name.yellow());
    println!();

    let start_time = Instant::now();
    let event_count = Arc::new(Mutex::new(0u32));
    let held_notes: Arc<Mutex<HeldNotes>> = Arc::new(Mutex::new(HashMap::new()));
    let status_line_active = Arc::new(Mutex::new(false));

    let event_count_clone = Arc::clone(&event_count);
    let held_notes_clone = Arc::clone(&held_notes);
    let status_line_active_clone = Arc::clone(&status_line_active);

    let _conn = midi_in.connect(
        port,
        "diagnostic",
        move |midi_timestamp, msg, _| {
            let now = Instant::now();
            let elapsed = now.duration_since(start_time);

            // Show both MIDI timestamp (microseconds from device) and elapsed time
            let timestamp = format!(
                "{:6.3}s (MIDI: {:10}μs)",
                elapsed.as_secs_f32(),
                midi_timestamp
            );

            // Clear status line if it's active
            let mut status_active = status_line_active_clone.lock().unwrap();
            if *status_active {
                print!("\r{:80}\r", ""); // Clear the line
                *status_active = false;
            }
            drop(status_active);

            let mut count = event_count_clone.lock().unwrap();
            *count += 1;
            print!("{} #{:4} | ", timestamp.dimmed(), count);

            // Parse MIDI message using midi-msg library
            match MidiMsg::from_midi(msg) {
                Ok((
                    MidiMsg::ChannelVoice {
                        channel,
                        msg: voice_msg,
                    },
                    _,
                ))
                | Ok((
                    MidiMsg::RunningChannelVoice {
                        channel,
                        msg: voice_msg,
                    },
                    _,
                )) => {
                    let ch = channel as u8 + 1; // Display as 1-based

                    match voice_msg {
                        ChannelVoiceMsg::NoteOn { note, velocity } => {
                            let note_name = note_to_name(note);

                            if velocity > 0 {
                                // Real note on
                                track_note_on(&mut held_notes_clone.lock().unwrap(), ch, note, now);

                                let vel_bar = "█".repeat((velocity as usize * 20) / 127);
                                println!(
                                    "{} {:>3} ({:3}) vel={:3} ch={:2} {}",
                                    "Note ON ".green().bold(),
                                    note_name.cyan(),
                                    note,
                                    velocity,
                                    ch,
                                    vel_bar.green()
                                );
                            } else {
                                // Note on with velocity 0 (acts as note off).
                                // ALWAYS print a complete line — including
                                // for an unmatched release — so the already-
                                // printed prefix never dangles.
                                let held =
                                    track_note_off(&mut held_notes_clone.lock().unwrap(), ch, note)
                                        .map(|press_time| now.duration_since(press_time));
                                println!(
                                    "{}",
                                    format_note_off_velocity_zero(&note_name, note, ch, held)
                                );
                            }
                        }

                        ChannelVoiceMsg::NoteOff { note, velocity: _ } => {
                            let note_name = note_to_name(note);

                            if let Some(press_time) =
                                track_note_off(&mut held_notes_clone.lock().unwrap(), ch, note)
                            {
                                let duration = now.duration_since(press_time);
                                println!(
                                    "{} {:>3} ({:3})         ch={:2} (held {:.3}s)",
                                    "Note OFF".yellow().bold(),
                                    note_name.cyan(),
                                    note,
                                    ch,
                                    duration.as_secs_f32()
                                );
                            } else {
                                println!(
                                    "{} {:>3} ({:3})         ch={:2}",
                                    "Note OFF".yellow().bold(),
                                    note_name.cyan(),
                                    note,
                                    ch
                                );
                            }
                        }

                        ChannelVoiceMsg::ControlChange { control } => {
                            // Extract control and value from ControlChange enum
                            use midi_msg::ControlChange;
                            if let ControlChange::CC { control: cc, value } = control {
                                let val_bar = "▬".repeat((value as usize * 20) / 127);
                                println!(
                                    "{}   cc={:3} val={:3} ch={:2} {}",
                                    "CC      ".blue().bold(),
                                    cc,
                                    value,
                                    ch,
                                    val_bar.blue()
                                );
                            } else {
                                // For other ControlChange variants, just show raw bytes
                                println!("{} {:02X?}", "CC      ".blue().bold(), msg);
                            }
                        }

                        ChannelVoiceMsg::PolyPressure { note, pressure } => {
                            let note_name = note_to_name(note);
                            let pressure_bar = "▓".repeat((pressure as usize * 20) / 127);
                            println!(
                                "{} {:>3} ({:3}) pres={:3} ch={:2} {}",
                                "PolyAT  ".purple().bold(),
                                note_name.cyan(),
                                note,
                                pressure,
                                ch,
                                pressure_bar.purple()
                            );
                        }

                        ChannelVoiceMsg::ChannelPressure { pressure } => {
                            let pressure_bar = "▓".repeat((pressure as usize * 20) / 127);
                            println!(
                                "{} pres={:3}         ch={:2} {}",
                                "ChanAT  ".purple().bold(),
                                pressure,
                                ch,
                                pressure_bar.purple()
                            );
                        }

                        ChannelVoiceMsg::PitchBend { bend } => {
                            let centered = bend as i32 - 8192; // Center is 8192

                            let direction = if centered > 0 {
                                "↑"
                            } else if centered < 0 {
                                "↓"
                            } else {
                                "◯"
                            };
                            println!(
                                "{} value={:5} ({:+6}) ch={:2} {}",
                                "PitchBend".magenta().bold(),
                                bend,
                                centered,
                                ch,
                                direction
                            );
                        }

                        ChannelVoiceMsg::ProgramChange { program } => {
                            println!(
                                "{} prog={:3}         ch={:2}",
                                "ProgChg ".cyan().bold(),
                                program,
                                ch
                            );
                        }

                        _ => {
                            // Other voice messages (HighResNoteOn, HighResNoteOff, etc.)
                            println!("{} {:02X?}", "Voice   ".cyan().bold(), msg);
                        }
                    }
                }

                Ok((MidiMsg::SystemCommon { .. }, _)) | Ok((MidiMsg::SystemRealTime { .. }, _)) => {
                    println!("{} {:02X?}", "System  ".white().bold(), msg);
                }

                _ => {
                    println!("{} {:02X?}", "Unknown ".red().bold(), msg);
                }
            }
        },
        (),
    )?;

    println!();
    println!("{}", "═".repeat(50).dimmed());
    println!("{}", "Listening for MIDI events...".green());
    println!("{}", "Press Ctrl+C to exit".yellow());
    println!();
    println!("{}", "Try these tests:".cyan().bold());
    println!("  • Press pads with different velocities");
    println!("  • Hold pads down for different durations");
    println!("  • Try double-tapping pads quickly");
    println!("  • Press multiple pads simultaneously (chords)");
    println!("  • Turn encoders slowly and quickly");
    println!("  • Use the touch strip");
    println!("  • Test Shift + pad combinations");
    println!();
    println!("{}", "═".repeat(50).dimmed());
    println!();

    // Keep the program running
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Show currently held notes
        let held = held_notes.lock().unwrap();
        if !held.is_empty() {
            let held_list: Vec<String> = held
                .iter()
                .map(|((ch, note), press_time)| {
                    let duration = Instant::now().duration_since(*press_time);
                    // Include the channel so the same note held on two channels
                    // is shown as two distinct entries.
                    format!(
                        "{}@ch{}({:.1}s)",
                        note_to_name(*note),
                        ch,
                        duration.as_secs_f32()
                    )
                })
                .collect();

            print!(
                "\r{} [{}]    ",
                "Currently held:".cyan(),
                held_list.join(", ").yellow()
            );
            use std::io::{self, Write};
            io::stdout().flush().unwrap();

            // Mark that status line is active
            *status_line_active.lock().unwrap() = true;
        } else {
            // Clear the line if no notes are held
            let mut status_active = status_line_active.lock().unwrap();
            if *status_active {
                print!("\r{:80}\r", " ");
                use std::io::{self, Write};
                io::stdout().flush().unwrap();
                *status_active = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No argument → Ok(None) so the caller may auto-select Mikro.
    #[test]
    fn parse_port_arg_none_means_auto_select() {
        assert_eq!(parse_port_arg(None), Ok(None));
    }

    /// A numeric argument is accepted as the requested port index.
    #[test]
    fn parse_port_arg_accepts_a_number() {
        assert_eq!(parse_port_arg(Some("2")), Ok(Some(2)));
        assert_eq!(parse_port_arg(Some("0")), Ok(Some(0)));
    }

    /// A PRESENT-but-non-numeric argument is REJECTED, not silently
    /// turned into None (which used to fall through to Mikro auto-select).
    #[test]
    fn parse_port_arg_rejects_non_numeric() {
        let err = parse_port_arg(Some("abc")).expect_err("non-numeric must be rejected");
        assert!(
            err.contains("invalid port number") && err.contains("abc"),
            "error must name the bad argument; got: {err}"
        );
        // Negative numbers don't parse as usize either — also rejected, not
        // silently auto-selected.
        assert!(parse_port_arg(Some("-1")).is_err());
    }

    /// An unmatched velocity-0 NoteOn (no tracked press) must still emit a
    /// complete `Note OFF` line — pre-fix it printed nothing, leaving the
    /// timestamp/count prefix dangling.
    #[test]
    fn velocity_zero_note_off_unmatched_emits_complete_line() {
        colored::control::set_override(false); // deterministic: no ANSI codes
        let line = format_note_off_velocity_zero("C4", 60, 1, None);
        assert!(
            line.contains("Note OFF"),
            "must emit a Note OFF line: {line:?}"
        );
        assert!(line.contains("60"), "must name the note number: {line:?}");
        assert!(line.contains("vel=  0"), "must show velocity 0: {line:?}");
        assert!(
            !line.contains("held"),
            "an unmatched release must not claim a held duration: {line:?}"
        );
    }

    /// A matched velocity-0 NoteOff still reports the held duration.
    #[test]
    fn velocity_zero_note_off_matched_includes_held_duration() {
        colored::control::set_override(false);
        let line =
            format_note_off_velocity_zero("C4", 60, 1, Some(std::time::Duration::from_millis(500)));
        assert!(
            line.contains("Note OFF") && line.contains("held"),
            "{line:?}"
        );
        assert!(
            line.contains("0.500"),
            "should format the held seconds: {line:?}"
        );
    }

    /// The same note held on two channels must be tracked independently —
    /// releasing one channel leaves the other held with ITS OWN press time.
    #[test]
    fn held_notes_are_tracked_per_channel() {
        let mut held: HeldNotes = HashMap::new();
        let t1 = Instant::now();
        track_note_on(&mut held, 1, 60, t1); // C4 on channel 1
        let t2 = t1 + std::time::Duration::from_millis(10);
        track_note_on(&mut held, 2, 60, t2); // C4 on channel 2

        assert_eq!(held.len(), 2, "same note on two channels = two entries");

        // Release C4 on channel 1 only.
        let released = track_note_off(&mut held, 1, 60);
        assert_eq!(
            released,
            Some(t1),
            "releasing ch1 returns ch1's own press time"
        );

        // Channel 2's C4 is still held, with its own (unoverwritten) timestamp.
        assert_eq!(
            held.get(&(2, 60)),
            Some(&t2),
            "ch2 C4 must remain held with its own press time"
        );
        assert!(!held.contains_key(&(1, 60)), "ch1 C4 must be removed");
        assert_eq!(held.len(), 1);
    }
}
