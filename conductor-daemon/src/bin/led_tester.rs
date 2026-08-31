// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// This diagnostic tool only uses external crates (hidapi, midir)
// and standard library - no conductor_core imports needed

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use midir::{Ignore, MidiInput};
use std::error::Error;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const MIKRO_MK3_VENDOR_ID: u16 = 0x17cc;
const MIKRO_MK3_PRODUCT_ID: u16 = 0x1700;
const LED_REPORT_ID: u8 = 0x80;

fn led_address_strategies(note: u8) -> Vec<(&'static str, usize)> {
    let mut strategies = vec![("Direct: note * 3", (note as usize) * 3)];

    if let Some(offset_note) = note.checked_sub(12) {
        let offset = (offset_note as usize) * 3;
        strategies.push(("Offset from 12: (note - 12) * 3", offset));
        strategies.push(("Offset from 12 + 1", offset + 1));
    }

    strategies.push(("Note value as offset", note as usize));
    strategies.push(("Note + 16", (note as usize) + 16));
    strategies
}

/// Outcome of waiting for a pad press during a diagnostic round (#1414).
#[derive(Debug, PartialEq, Eq)]
enum WaitOutcome {
    /// A pad was captured (its MIDI note number).
    Pad(u8),
    /// No pad arrived before the timeout elapsed.
    Timeout,
}

/// Poll `poll_note` until it yields a note or `timeout` elapses, sleeping
/// `step` between polls, then return the outcome.
///
/// This replaces the original inline wait loop, which `continue`d on
/// timeout while `elapsed` stayed past the threshold — printing "Timeout"
/// forever with no sleep or exit (#1414). Returning [`WaitOutcome::Timeout`]
/// lets the caller fall back to a real prompt instead of spinning. The note
/// source and clock step are parameters so the outcome logic is unit-testable
/// without MIDI hardware.
fn wait_for_pad<F>(mut poll_note: F, timeout: Duration, step: Duration) -> WaitOutcome
where
    F: FnMut() -> Option<u8>,
{
    let start = std::time::Instant::now();
    loop {
        if let Some(note) = poll_note() {
            return WaitOutcome::Pad(note);
        }
        if start.elapsed() >= timeout {
            return WaitOutcome::Timeout;
        }
        thread::sleep(step);
    }
}

/// MIDI client/connection name this tool registers. #2136: derived from the
/// Cargo bin target via `CARGO_BIN_NAME` (which is `led_tester`, underscored)
/// rather than a hardcoded hyphenated `"led-tester"`, so the advertised name
/// can never drift from the actual executable name.
const MIDI_CLIENT_NAME: &str = env!("CARGO_BIN_NAME");

fn main() -> Result<(), Box<dyn Error>> {
    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║  Mikro MK3 LED Address Finder                             ║");
    println!("║  Tests different LED addresses to find the right mapping  ║");
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!();

    // Open HID device
    let api = hidapi::HidApi::new()?;
    let mut hid_device = None;

    for device in api.device_list() {
        if device.vendor_id() == MIKRO_MK3_VENDOR_ID
            && device.product_id() == MIKRO_MK3_PRODUCT_ID
            && device.interface_number() == 0
        {
            hid_device = Some(device.open_device(&api)?);
            break;
        }
    }
    let hid_device = Arc::new(Mutex::new(hid_device.ok_or("Mikro MK3 HID not found")?));
    println!("✓ Connected to Mikro MK3 HID");

    // Capture MIDI notes
    let captured_note = Arc::new(Mutex::new(None::<u8>));
    let mut midi_in = MidiInput::new("LED Tester")?;
    midi_in.ignore(Ignore::None);

    let ports = midi_in.ports();
    let mikro_port = ports
        .iter()
        .find(|p| {
            midi_in
                .port_name(p)
                .unwrap_or_default()
                .contains("Mikro MK3")
        })
        .ok_or("Mikro MK3 MIDI not found")?;

    let captured_note_clone = Arc::clone(&captured_note);
    let _midi_conn = midi_in.connect(
        mikro_port,
        MIDI_CLIENT_NAME,
        move |_stamp, message, _| {
            if message.len() >= 3 && (message[0] & 0xF0) == 0x90 && message[2] > 0 {
                *captured_note_clone.lock().unwrap() = Some(message[1]);
            }
        },
        (),
    )?;

    println!("✓ Connected to Mikro MK3 MIDI");
    println!();

    println!("Instructions:");
    println!("  1. Press a pad on Pad Page A");
    println!("  2. I'll try different LED addresses");
    println!("  3. Tell me when you see RED light up");
    println!();

    loop {
        println!("─────────────────────────────────────────────");
        print!("Press any pad (60s timeout): ");
        io::stdout().flush()?;

        // Wait for a pad press, or time out cleanly (#1414). The previous
        // inline loop spun printing "Timeout" forever and advertised a 'q'
        // quit it never read; this returns to a real prompt on timeout.
        *captured_note.lock().unwrap() = None;
        let note = match wait_for_pad(
            || *captured_note.lock().unwrap(),
            Duration::from_secs(60),
            Duration::from_millis(10),
        ) {
            WaitOutcome::Pad(n) => n,
            WaitOutcome::Timeout => {
                println!("No pad detected within 60s.");
                print!("Try again? (y/n): ");
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                if input.trim().to_lowercase() == "y" {
                    continue; // back to the outer prompt
                }
                break; // exit the diagnostic loop
            }
        };

        println!("Captured MIDI note {}", note);
        println!();
        println!("Testing LED positions for note {}...", note);

        // Try different strategies:
        // Strategy 1: Direct note index (note * 3 for RGB)
        // Strategy 2: Note offset from 12 (* 3 for RGB)
        // Strategy 3: Specific buffer positions

        let test_strategies = led_address_strategies(note);

        for (desc, offset) in test_strategies {
            if offset >= 62 {
                continue; // Skip if would overflow RGB triplet
            }

            // Clear all LEDs first
            let mut buffer = vec![LED_REPORT_ID];
            buffer.resize(80, 0);

            // Set this offset to bright RED
            buffer[offset] = 255; // R
            buffer[offset + 1] = 0; // G
            buffer[offset + 2] = 0; // B

            // Pad to 65 bytes and write
            buffer.resize(65, 0);
            if let Ok(dev) = hid_device.lock() {
                let _ = dev.write(&buffer);
            }

            println!("  Testing {} (offset {})...", desc, offset);
            thread::sleep(Duration::from_millis(800));

            // Check if user saw it
            print!("      Did you see RED light? (y/n): ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            if input.trim().to_lowercase() == "y" {
                println!();
                println!("✓✓✓ FOUND IT! ✓✓✓");
                println!("MIDI Note {} → {} (offset {})", note, desc, offset);
                println!();
                break;
            }

            // Turn off
            let mut buffer = vec![LED_REPORT_ID];
            buffer.resize(65, 0);
            if let Ok(dev) = hid_device.lock() {
                let _ = dev.write(&buffer);
            }
        }

        print!("Test another pad? (y/n): ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if input.trim().to_lowercase() != "y" {
            break;
        }
    }

    // Clear all LEDs
    let mut buffer = vec![LED_REPORT_ID];
    buffer.resize(65, 0);
    if let Ok(dev) = hid_device.lock() {
        let _ = dev.write(&buffer);
    }

    println!();
    println!("Done!");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// #2136: the advertised MIDI client name must equal the underscored Cargo
    /// bin target (`led_tester`), not a hyphenated alias. Using
    /// `env!("CARGO_BIN_NAME")` ties them together; this guards against a future
    /// re-hardcoding (e.g. back to `"led-tester"`) or a bin rename to a
    /// hyphenated form.
    #[test]
    fn midi_client_name_matches_underscored_bin_target() {
        assert_eq!(MIDI_CLIENT_NAME, "led_tester");
        assert!(
            !MIDI_CLIENT_NAME.contains('-'),
            "MIDI client name must not be hyphenated: {MIDI_CLIENT_NAME}"
        );
    }

    #[test]
    fn wait_for_pad_returns_captured_note() {
        // Note arrives on the 3rd poll.
        let calls = AtomicU32::new(0);
        let outcome = wait_for_pad(
            || {
                if calls.fetch_add(1, Ordering::SeqCst) >= 2 {
                    Some(42)
                } else {
                    None
                }
            },
            Duration::from_secs(5),
            Duration::from_millis(1),
        );
        assert_eq!(outcome, WaitOutcome::Pad(42));
    }

    #[test]
    fn wait_for_pad_times_out_promptly_without_spinning() {
        // #1414: a note that never arrives must yield Timeout quickly, not
        // loop forever. The old inline loop `continue`d past the threshold.
        let calls = AtomicU32::new(0);
        let start = std::time::Instant::now();
        let outcome = wait_for_pad(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                None
            },
            Duration::from_millis(40),
            Duration::from_millis(5),
        );
        assert_eq!(outcome, WaitOutcome::Timeout);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "must return promptly after the timeout, not spin"
        );
        assert!(
            calls.load(Ordering::SeqCst) < 1000,
            "poll count must be bounded by timeout/step, not an unbounded spin: {}",
            calls.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn led_address_strategies_skip_offset_from_12_below_12() {
        for note in [0, 11] {
            let strategies = led_address_strategies(note);
            assert!(
                strategies
                    .iter()
                    .all(|(desc, _)| !desc.starts_with("Offset from 12")),
                "note {note} should skip offset-from-12 strategies: {strategies:?}"
            );
        }
    }

    #[test]
    fn led_address_strategies_include_offset_from_12_at_and_above_12() {
        assert_eq!(
            led_address_strategies(12),
            vec![
                ("Direct: note * 3", 36),
                ("Offset from 12: (note - 12) * 3", 0),
                ("Offset from 12 + 1", 1),
                ("Note value as offset", 12),
                ("Note + 16", 28),
            ]
        );

        assert_eq!(
            led_address_strategies(127),
            vec![
                ("Direct: note * 3", 381),
                ("Offset from 12: (note - 12) * 3", 345),
                ("Offset from 12 + 1", 346),
                ("Note value as offset", 127),
                ("Note + 16", 143),
            ]
        );
    }
}
