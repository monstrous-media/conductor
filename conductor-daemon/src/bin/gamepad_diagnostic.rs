// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
#![allow(clippy::print_stdout, clippy::print_stderr)]

//! gamepad_diagnostic — dump raw gilrs events for troubleshooting (#599).
//!
//! Prints every event gilrs delivers (button presses, button analog changes,
//! axis changes) exactly as the backend reports them, alongside the Conductor
//! encoder/button ID each would map to. Use this to diagnose why a control
//! doesn't reach the daemon: if a movement prints nothing here, the gap is in
//! the gilrs backend (e.g. macOS Bluetooth — see #2229), not in Conductor.
//!
//! Usage:
//!   cargo run --bin gamepad_diagnostic            # dump events until Ctrl-C
//!
//! macOS: the binary needs an Input Monitoring grant (System Settings →
//! Privacy & Security → Input Monitoring). Re-add it after every rebuild —
//! the ad-hoc cdhash changes per build (ADR-029 §D5).

use conductor_core::gamepad_events::{axis_to_encoder_id, button_to_id, encoder_ids};
use std::time::Instant;

fn main() {
    // ADR-047 §D1: build gilrs through the daemon's shared constructor so the
    // diagnostic applies the SAME user `~/.conductor/gamecontrollerdb.txt`
    // mapping layer the daemon uses — otherwise verifying a user override here
    // would be misleading (Copilot review, PR #2440).
    let mut gilrs = match conductor_daemon::gamepad_device::build_gilrs() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Failed to initialise gilrs: {}", e);
            std::process::exit(1);
        }
    };

    // Give the async IOKit callback time to register Bluetooth-LE controllers
    // (~50ms after Gilrs::new(); see #2229).
    std::thread::sleep(std::time::Duration::from_millis(200));
    while gilrs.next_event().is_some() {} // drain connect events for the listing

    println!("Connected gamepads:");
    let mut any = false;
    for (id, gamepad) in gilrs.gamepads() {
        any = true;
        // ADR-047 §D2: print the plain 32-char hex GUID — the exact form a
        // `ControllerGuid` matcher accepts. (The hyphenated `uuid::Uuid` form is
        // shown alongside for reference but is NOT what the config wants.)
        let guid_hex: String = gamepad.uuid().iter().map(|b| format!("{b:02x}")).collect();
        println!(
            "  [{}] {} (guid {} | uuid {})",
            id,
            gamepad.name(),
            guid_hex,
            uuid::Uuid::from_bytes(gamepad.uuid())
        );
    }
    if !any {
        println!("  (none — check Input Monitoring permissions on macOS)");
    }
    println!("\nDumping raw events (Ctrl-C to stop)...\n");

    let start = Instant::now();
    loop {
        while let Some(event) = gilrs.next_event() {
            let t = start.elapsed().as_millis();
            match event.event {
                gilrs::EventType::ButtonPressed(b, code) => {
                    println!(
                        "[{:>7}ms] ButtonPressed   {:?} (code {}) -> pad {:?}",
                        t,
                        b,
                        code,
                        button_to_id(b)
                    );
                }
                gilrs::EventType::ButtonReleased(b, code) => {
                    println!(
                        "[{:>7}ms] ButtonReleased  {:?} (code {}) -> pad {:?}",
                        t,
                        b,
                        code,
                        button_to_id(b)
                    );
                }
                gilrs::EventType::ButtonChanged(b, v, code) => {
                    // On macOS (and some gilrs backends) analog trigger travel arrives as
                    // ButtonChanged(LeftTrigger2|RightTrigger2, value) rather than
                    // AxisChanged(LeftZ|RightZ). Those map to encoder 132/133 in the event
                    // stream — show the encoder id so users know what to put in config.
                    let encoder_id: Option<u8> = match b {
                        gilrs::Button::LeftTrigger2 => Some(encoder_ids::LEFT_TRIGGER),
                        gilrs::Button::RightTrigger2 => Some(encoder_ids::RIGHT_TRIGGER),
                        _ => None,
                    };
                    println!(
                        "[{:>7}ms] ButtonChanged   {:?} = {:+.4} (code {}) -> encoder {:?}",
                        t, b, v, code, encoder_id
                    );
                }
                gilrs::EventType::ButtonRepeated(b, code) => {
                    println!("[{:>7}ms] ButtonRepeated  {:?} (code {})", t, b, code);
                }
                gilrs::EventType::AxisChanged(a, v, code) => {
                    println!(
                        "[{:>7}ms] AxisChanged     {:?} = {:+.4} (code {}) -> encoder {:?}",
                        t,
                        a,
                        v,
                        code,
                        axis_to_encoder_id(a)
                    );
                }
                other => {
                    println!("[{:>7}ms] {:?}", t, other);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}
