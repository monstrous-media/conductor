// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! #1433: the interactive `midi_simulator` must REJECT malformed numeric input
//! for `long`, `double`, `chord`, and `encoder` rather than silently defaulting
//! (long/double/encoder) or dropping invalid tokens (chord) — otherwise it
//! emits events for different controls than the operator typed, which is
//! especially misleading for a diagnostic simulator.
//!
//! These are black-box tests: they run the built binary, feed commands on stdin,
//! and inspect stdout. The simulator is purely in-memory (`MidiSimulator`), so
//! no MIDI hardware is required and the tests are CI-safe.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn midi_simulator_path() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_midi_simulator") {
        return PathBuf::from(p);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("workspace root")
        .join("target")
        .join("debug")
        .join("midi_simulator")
}

/// Run the simulator, feeding `commands` on stdin, and return combined stdout.
fn run_with_input(commands: &str) -> String {
    let mut child = Command::new(midi_simulator_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn midi_simulator");

    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(commands.as_bytes())
        .expect("write stdin");

    let out = child.wait_with_output().expect("wait midi_simulator");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// After a rejected command, the `events` command must report an empty queue —
/// i.e. nothing was emitted. `<cmd>` is followed by `events` then `quit`.
fn rejects_and_emits_nothing(cmd: &str) -> String {
    run_with_input(&format!("{cmd}\nevents\nquit\n"))
}

#[test]
fn long_rejects_invalid_note_and_emits_nothing() {
    let out = rejects_and_emits_nothing("long abc");
    assert!(out.contains("Invalid note number"), "should reject: {out}");
    assert!(
        out.contains("No events in queue"),
        "no event should be emitted for invalid input: {out}"
    );
    assert!(
        !out.contains("Long press complete"),
        "must not perform the gesture: {out}"
    );
}

#[test]
fn double_rejects_invalid_note_and_emits_nothing() {
    let out = rejects_and_emits_nothing("double abc");
    assert!(out.contains("Invalid note number"), "should reject: {out}");
    assert!(out.contains("No events in queue"), "no event: {out}");
}

#[test]
fn encoder_rejects_invalid_cc_and_emits_nothing() {
    let out = rejects_and_emits_nothing("encoder nope cw");
    assert!(out.contains("Invalid CC number"), "should reject: {out}");
    assert!(out.contains("No events in queue"), "no event: {out}");
}

#[test]
fn chord_rejects_any_invalid_note_and_emits_nothing() {
    let out = rejects_and_emits_nothing("chord 60 nope 64");
    assert!(
        out.contains("Invalid note in chord"),
        "should reject the whole chord: {out}"
    );
    assert!(out.contains("No events in queue"), "no event: {out}");
}

#[test]
fn valid_long_press_still_emits_events() {
    // Positive control: a valid command DOES queue events, so the assertions
    // above aren't vacuously empty for some unrelated reason.
    let out = run_with_input("long 60 100\nevents\nquit\n");
    assert!(
        out.contains("Long press complete"),
        "valid input runs: {out}"
    );
    assert!(
        !out.contains("No events in queue"),
        "a valid gesture should queue events: {out}"
    );
}

// #2130: numeric-but-out-of-range MIDI data bytes (128–255) used to be accepted
// and silently masked to 7 bits by the simulator (`note 200` → note 72). They
// must now be rejected with the 0–127 range message and emit nothing — the same
// black-box contract as the #1433 non-numeric rejections above.

#[test]
fn note_rejects_out_of_range_value_and_emits_nothing() {
    // 200 would have been masked to 200 & 0x7F = 72.
    let out = rejects_and_emits_nothing("note 200 100");
    assert!(
        out.contains("out of the MIDI data-byte range 0–127"),
        "should reject out-of-range note with the range message: {out}"
    );
    assert!(out.contains("No events in queue"), "no event: {out}");
}

#[test]
fn long_rejects_out_of_range_note_and_emits_nothing() {
    let out = rejects_and_emits_nothing("long 200");
    assert!(
        out.contains("Invalid note number") && out.contains("0–127"),
        "should reject out-of-range note: {out}"
    );
    assert!(out.contains("No events in queue"), "no event: {out}");
    assert!(
        !out.contains("Long press complete"),
        "must not perform the gesture: {out}"
    );
}

#[test]
fn cc_rejects_out_of_range_value_and_emits_nothing() {
    let out = rejects_and_emits_nothing("cc 1 200");
    assert!(
        out.contains("Invalid CC value") && out.contains("0–127"),
        "should reject out-of-range CC value: {out}"
    );
    assert!(out.contains("No events in queue"), "no event: {out}");
}
