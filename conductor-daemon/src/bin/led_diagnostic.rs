// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// This diagnostic tool only uses external crates (hidapi, midir)
// and standard library - no conductor_core imports needed

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use hidapi::{HidApi, HidDevice};
use midir::{Ignore, MidiInput};
use std::collections::HashMap;
use std::error::Error;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Native Instruments Vendor ID
const NI_VENDOR_ID: u16 = 0x17CC;
const MIKRO_MK3_PRODUCT_ID: u16 = 0x1700;
const LED_REPORT_ID: u8 = 0x80;

/// Fixed length of the LED HID report this diagnostic writes.
///
/// This tool indexes addresses **directly** into the report buffer (the
/// report-ID byte sits at index 0, LED bytes follow), and the highest address
/// it probes is `0x4B` (75) — whose RGB triplet lands at bytes 75/76/77. The
/// report must therefore stay at least 78 bytes long; 80 gives headroom and
/// matches the production driver's LED-buffer size.
///
/// Note: this is NOT identical to the production on-wire length.
/// `conductor_core::mikro_leds` writes `vec![LED_REPORT_ID]` + an 80-byte LED
/// buffer = 81 bytes, and it offsets LED data past the ID rather than counting
/// the ID inside the address space. The only invariant that matters here is
/// that this diagnostic's report MUST NOT be truncated below the addresses it
/// probes.
const LED_REPORT_LEN: usize = 80;

/// Build the LED HID report that lights the LED at `addr` bright red.
///
/// The previous inline code wrote the RGB triplet into an 80-byte buffer
/// and then `buffer.resize(65, 0)` *before* the HID write — truncating the
/// report back to 65 bytes. Every high address (`addr + 2 >= 65`, i.e. the
/// `0x42`/`0x45`/`0x48`/`0x4B` pad tests) had its just-written LED bytes
/// chopped off, so those LEDs never lit and the operator wrongly concluded the
/// address was wrong. The report is now built at its full [`LED_REPORT_LEN`] so
/// high addresses reach the device intact.
fn build_led_report(addr: u8) -> Vec<u8> {
    let i = addr as usize;
    // Enforce the report-length invariant explicitly: if a future address (or a
    // reuse of this helper) put the RGB triplet past the end of the report, the
    // bare `buffer[i] = …` below would panic with an opaque index-out-of-bounds.
    // A named assertion makes the contract — and any violation — actionable.
    assert!(
        i + 2 < LED_REPORT_LEN,
        "LED address 0x{addr:02X} ({i}) + RGB triplet exceeds the {LED_REPORT_LEN}-byte report",
    );
    let mut buffer = vec![LED_REPORT_ID];
    buffer.resize(LED_REPORT_LEN, 0);
    buffer[i] = 255; // R
    buffer[i + 1] = 0; // G
    buffer[i + 2] = 0; // B
    buffer
}

/// Build the all-off LED HID report (clears every LED).
///
/// Kept at the same [`LED_REPORT_LEN`] as [`build_led_report`] so the device
/// always sees a consistent report length.
fn build_clear_report() -> Vec<u8> {
    let mut buffer = vec![LED_REPORT_ID];
    buffer.resize(LED_REPORT_LEN, 0);
    buffer
}

/// Format the discovered `MIDI note → LED address` mappings for the summary,
/// one line per note, sorted by note number for deterministic output.
///
/// The discovered LED address used to be thrown away — `results` stored
/// a bare `bool` and the summary printed `LED address (found)` with no value,
/// so the whole point of the diagnostic (learning *which* address drives each
/// pad) was lost. `results` now carries the matched address and this renders it
/// as `0x{addr:02X}`.
fn format_discovered_mapping(results: &HashMap<u8, u8>) -> Vec<String> {
    let mut entries: Vec<(&u8, &u8)> = results.iter().collect();
    entries.sort_by_key(|(note, _)| **note);
    entries
        .iter()
        .map(|(note, addr)| format!("MIDI Note {note} → LED address 0x{addr:02X}"))
        .collect()
}

struct CaptureData {
    midi_note: Option<u8>,
    midi_velocity: Option<u8>,
    hid_buffer: Vec<u8>,
}

/// Outcome of waiting for a pad press.
#[derive(Debug, PartialEq, Eq)]
enum WaitOutcome {
    /// A pad was captured: `(note, velocity)`.
    Pad(u8, u8),
    /// No pad arrived before the timeout elapsed.
    Timeout,
}

/// Poll `poll` until it yields a `(note, velocity)` capture or `timeout`
/// elapses, sleeping `step` between polls, then return the outcome.
///
/// The per-test prompt advertised a `q` quit, but the previous wait was
/// an UNBOUNDED MIDI polling loop that never read stdin — so quitting was
/// impossible if no pad was pressed or the device went silent. Returning
/// [`WaitOutcome::Timeout`] lets the caller fall back to a real stdin prompt
/// (where `q`/`n` quits). The note source and clock step are parameters so the
/// outcome logic is unit-testable without MIDI hardware (mirrors led_tester's
/// `wait_for_pad`).
fn wait_for_pad<F>(mut poll: F, timeout: Duration, step: Duration) -> WaitOutcome
where
    F: FnMut() -> Option<(u8, u8)>,
{
    let start = std::time::Instant::now();
    loop {
        if let Some((note, velocity)) = poll() {
            return WaitOutcome::Pad(note, velocity);
        }
        if start.elapsed() >= timeout {
            return WaitOutcome::Timeout;
        }
        std::thread::sleep(step);
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("╔════════════════════════════════════════╗");
    println!("║  Mikro MK3 LED Diagnostic Tool        ║");
    println!("║  Press pads to capture MIDI + HID data ║");
    println!("╚════════════════════════════════════════╝\n");

    // Open HID device for LED control
    let api = HidApi::new()?;
    let mut hid_device: Option<HidDevice> = None;

    for device_info in api.device_list() {
        if device_info.vendor_id() == NI_VENDOR_ID
            && device_info.product_id() == MIKRO_MK3_PRODUCT_ID
            && device_info.interface_number() == 0
        {
            println!("✓ Found Mikro MK3 HID interface");
            hid_device = Some(device_info.open_device(&api)?);
            break;
        }
    }

    let hid_device = hid_device.ok_or("Mikro MK3 HID not found")?;

    // Open MIDI input
    let mut midi_in = MidiInput::new("LED Diagnostic MIDI")?;
    midi_in.ignore(Ignore::None);

    let midi_ports = midi_in.ports();
    let mikro_port = midi_ports
        .iter()
        .find(|p| {
            midi_in
                .port_name(p)
                .unwrap_or_default()
                .contains("Mikro MK3")
        })
        .ok_or("Mikro MK3 MIDI port not found")?;

    println!("✓ Found Mikro MK3 MIDI input\n");

    // Shared capture data
    let capture = Arc::new(Mutex::new(CaptureData {
        midi_note: None,
        midi_velocity: None,
        hid_buffer: Vec::new(),
    }));

    let capture_clone = capture.clone();

    // Connect MIDI input
    let _midi_conn = midi_in.connect(
        mikro_port,
        "diagnostic",
        move |_timestamp, message, _| {
            if message.len() >= 3 && (message[0] & 0xF0) == 0x90 && message[2] > 0 {
                let mut cap = capture_clone.lock().unwrap();
                cap.midi_note = Some(message[1]);
                cap.midi_velocity = Some(message[2]);
                println!("\n📥 MIDI: Note {} velocity {}", message[1], message[2]);
            }
        },
        (),
    )?;

    println!("Ready! Press pads one at a time.\n");
    println!("Instructions:");
    println!("1. Press and release a pad");
    println!("2. Wait to see if LED lights up");
    println!("3. Type 'y' if LED lit up, 'n' if not");
    println!("4. Type 'q' to quit\n");

    // note → discovered LED address (was `bool`, which discarded the
    // address the diagnostic exists to find).
    let mut results: HashMap<u8, u8> = HashMap::new();
    let mut test_count = 0;

    loop {
        println!("\n─────────────────────────────────────");
        println!("Test #{}: Press any pad (60s timeout)", test_count + 1);

        // Clear previous capture
        {
            let mut cap = capture.lock().unwrap();
            cap.midi_note = None;
            cap.midi_velocity = None;
            cap.hid_buffer.clear();
        }

        // Wait for a pad press, or time out cleanly so the advertised quit is
        // actually reachable. The previous loop polled MIDI forever and
        // never read stdin, so the operator could never quit if no pad was
        // pressed or the device went silent.
        let (note, velocity) = match wait_for_pad(
            || {
                let cap = capture.lock().unwrap();
                match (cap.midi_note, cap.midi_velocity) {
                    (Some(n), Some(v)) => Some((n, v)),
                    _ => None,
                }
            },
            Duration::from_secs(60),
            Duration::from_millis(50),
        ) {
            WaitOutcome::Pad(n, v) => (n, v),
            WaitOutcome::Timeout => {
                print!(
                    "\nNo pad detected within 60s. Retry? (q/n to quit, anything else retries): "
                );
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let answer = input.trim();
                // Quit ONLY on an explicit q/n, so an empty line or typo can't
                // accidentally end the session — it just retries.
                if answer.eq_ignore_ascii_case("q") || answer.eq_ignore_ascii_case("n") {
                    break; // quit → fall through to the summary
                }
                continue; // retry this test
            }
        };

        println!("\n✓ Captured: Note {} vel {}", note, velocity);

        // Now test LED with different addresses
        println!("\nTesting LED addresses...");

        let test_addresses = [
            (0x1E, "LED_PAD13 (0x1E) - should be pad index 0"),
            (0x21, "LED_PAD14 (0x21) - should be pad index 1"),
            (0x24, "LED_PAD15 (0x24) - should be pad index 2"),
            (0x27, "LED_PAD16 (0x27) - should be pad index 3"),
            (0x2A, "LED_PAD09 (0x2A) - should be pad index 4"),
            (0x2D, "LED_PAD10 (0x2D) - should be pad index 5"),
            (0x30, "LED_PAD11 (0x30) - should be pad index 6"),
            (0x33, "LED_PAD12 (0x33) - should be pad index 7"),
            (0x36, "LED_PAD05 (0x36) - should be pad index 8"),
            (0x39, "LED_PAD06 (0x39) - should be pad index 9"),
            (0x3C, "LED_PAD07 (0x3C) - should be pad index 10"),
            (0x3F, "LED_PAD08 (0x3F) - should be pad index 11"),
            (0x42, "LED_PAD01 (0x42) - should be pad index 12"),
            (0x45, "LED_PAD02 (0x45) - should be pad index 13"),
            (0x48, "LED_PAD03 (0x48) - should be pad index 14"),
            (0x4B, "LED_PAD04 (0x4B) - should be pad index 15"),
        ];

        for (addr, desc) in &test_addresses {
            // Light the LED at this address bright red. The report is built at
            // its full length so high addresses are not truncated.
            let buffer = build_led_report(*addr);

            hid_device.write(&buffer)?;
            std::thread::sleep(Duration::from_millis(300));

            print!("\n  Testing {} - Did you see RED light? (y/n): ", desc);
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            if input.trim().eq_ignore_ascii_case("y") {
                println!(
                    "  ✓✓✓ FOUND IT! Note {} maps to address 0x{:02X} ({})",
                    note, addr, desc
                );
                results.insert(note, *addr);

                // Turn off
                hid_device.write(&build_clear_report())?;

                break;
            } else {
                // Turn off and try next
                hid_device.write(&build_clear_report())?;
            }
        }

        test_count += 1;

        println!("\n\nResults so far:");
        println!("─────────────────────────────────────");
        for line in format_discovered_mapping(&results) {
            println!("  {line}");
        }

        print!("\nContinue testing? (y/n): ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            break;
        }
    }

    println!("\n\n╔════════════════════════════════════════╗");
    println!("║  Diagnostic Complete!                  ║");
    println!("╚════════════════════════════════════════╝");
    println!("\nTested {} pads", test_count);
    println!("\nMapping discovered:");
    for line in format_discovered_mapping(&results) {
        println!("  {line}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A capture is returned as `Pad`.
    #[test]
    fn wait_for_pad_returns_capture() {
        let calls = Cell::new(0u32);
        let outcome = wait_for_pad(
            || {
                let n = calls.get();
                calls.set(n + 1);
                // None for the first two polls, then a capture.
                if n >= 2 { Some((60, 100)) } else { None }
            },
            Duration::from_secs(5),
            Duration::from_millis(1),
        );
        assert_eq!(outcome, WaitOutcome::Pad(60, 100));
    }

    /// With no capture, the wait returns `Timeout` PROMPTLY instead of
    /// polling MIDI forever — which is what makes the quit prompt reachable.
    #[test]
    fn wait_for_pad_times_out_promptly_without_spinning() {
        let polls = Cell::new(0u32);
        let start = std::time::Instant::now();
        let outcome = wait_for_pad(
            || {
                polls.set(polls.get() + 1);
                None
            },
            Duration::from_millis(30),
            Duration::from_millis(5),
        );
        assert_eq!(outcome, WaitOutcome::Timeout);
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "must return shortly after the timeout"
        );
        // ~6 polls expected (30ms / 5ms step). A tight loop with no sleep would
        // poll thousands/millions of times in 30ms; bound well below that so a
        // regression that drops the `sleep(step)` is caught, not just slowness.
        assert!(
            polls.get() < 100,
            "wait must SLEEP between polls, not spin: {} polls in 30ms",
            polls.get()
        );
    }

    /// The highest address this tool probes is `0x4B` (75). Its RGB
    /// triplet occupies bytes 75/76/77, so the report MUST be at least 78 bytes
    /// long. The previous code truncated the report to 65 bytes *after* writing,
    /// which dropped every byte from 65 onward — those LEDs never lit. The
    /// builder must keep all three colour bytes for a high address.
    #[test]
    fn build_led_report_keeps_high_address_bytes() {
        let addr = 0x4Bu8; // LED_PAD04 — the highest address the tool tests.
        let report = build_led_report(addr);

        // Assert the EXACT report length, not merely "long enough": a future
        // shrink that still happened to leave bytes 75–77 reachable would slip
        // past a `>=` check but is exactly the truncation contract we guard.
        assert_eq!(
            report.len(),
            LED_REPORT_LEN,
            "report must be the full length so high addresses are not truncated \
             (a resize-to-65 truncation regression); got len {}",
            report.len()
        );
        assert_eq!(
            report[addr as usize], 255,
            "R byte must survive to the wire"
        );
        assert_eq!(report[addr as usize + 1], 0, "G byte must survive");
        assert_eq!(report[addr as usize + 2], 0, "B byte must survive");
        // The probed addresses all start at 0x1E, so a pad address never
        // overwrites the report-ID byte at index 0.
        assert_eq!(report[0], LED_REPORT_ID);
    }

    /// The discovered mapping must report the ACTUAL LED address, not a
    /// generic "(found)" placeholder. Before the fix, `results` was
    /// `HashMap<u8, bool>` and the address was discarded; the summary could only
    /// say an address was found, never which one. The map now carries the
    /// address and the formatter renders it as `0x{addr:02X}`, sorted by note.
    #[test]
    fn discovered_mapping_reports_the_actual_led_address() {
        let mut results: HashMap<u8, u8> = HashMap::new();
        results.insert(60, 0x42); // Note 60 → LED_PAD01
        results.insert(36, 0x1E); // Note 36 → LED_PAD13

        let lines = format_discovered_mapping(&results);

        assert_eq!(lines.len(), 2);
        // Sorted by note number: 36 before 60.
        assert_eq!(lines[0], "MIDI Note 36 → LED address 0x1E");
        assert_eq!(lines[1], "MIDI Note 60 → LED address 0x42");
        // The concrete address is present and the discarded-address placeholder
        // is gone.
        assert!(lines.iter().all(|l| l.contains("0x")));
        assert!(!lines.iter().any(|l| l.contains("(found)")));
    }

    /// An empty result set yields no mapping lines (no panic, no placeholder).
    #[test]
    fn discovered_mapping_empty_is_empty() {
        let results: HashMap<u8, u8> = HashMap::new();
        assert!(format_discovered_mapping(&results).is_empty());
    }

    /// The report-length invariant is enforced: an address whose RGB triplet
    /// would not fit in the report panics with the named assertion rather than a
    /// bare index-out-of-bounds.
    #[test]
    #[should_panic(expected = "exceeds the")]
    fn build_led_report_rejects_address_past_report_end() {
        // 0x4E (78): 78 + 2 == 80 == LED_REPORT_LEN, so the B byte would land
        // one past the end.
        let _ = build_led_report(0x4E);
    }

    /// A low address (`0x1E`, the first pad tested) still lights correctly — the
    /// fix must not regress the addresses that already worked.
    #[test]
    fn build_led_report_keeps_low_address_bytes() {
        let report = build_led_report(0x1E);
        assert_eq!(report.len(), LED_REPORT_LEN);
        assert_eq!(report[0x1E], 255);
        assert_eq!(report[0x1F], 0);
        assert_eq!(report[0x20], 0);
    }

    /// The clear report is the same length as a lit report and is all-off
    /// (apart from the report-ID byte), so the device sees a consistent report
    /// length.
    #[test]
    fn build_clear_report_is_full_length_and_off() {
        let report = build_clear_report();
        assert_eq!(report.len(), LED_REPORT_LEN);
        assert_eq!(report[0], LED_REPORT_ID);
        assert!(
            report[1..].iter().all(|&b| b == 0),
            "every LED byte must be off in the clear report"
        );
    }
}
