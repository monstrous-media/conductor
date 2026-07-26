// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! #1434: the interactive `midi_simulator`'s `events` command is documented as a
//! read-only display (with a separate `clear` for draining), but it used to call
//! the draining `get_events()` — so the FIRST `events` consumed the queue and a
//! second `events` showed nothing. This black-box test verifies `events` is now
//! non-destructive until `clear`. The simulator is in-memory, so no MIDI
//! hardware is required (CI-safe).

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

#[test]
fn events_is_non_destructive_until_clear() {
    // Queue an event, view it TWICE, then `clear`, then view again.
    let out = run_with_input("note 60 80\nevents\nevents\nclear\nevents\nquit\n");

    // Both `events` invocations must show the captured queue — viewing it once
    // no longer consumes it (the pre-fix bug: the 2nd view showed nothing).
    let views = out.matches("Captured events:").count();
    assert_eq!(
        views, 2,
        "both `events` views must display the queue (non-destructive); got {views}:\n{out}"
    );

    // `clear` is the only destructive command; after it the queue is empty.
    assert!(
        out.contains("Event queue cleared"),
        "clear should run: {out}"
    );
    assert!(
        out.contains("No events in queue"),
        "after `clear`, `events` must report an empty queue: {out}"
    );
}
