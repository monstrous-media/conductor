// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! #2131 (clawpatch #2103): `MidiEvent::from_midi_msg` must reject a MIDI
//! channel-voice frame that carries MORE bytes than the message uses, instead
//! of silently dropping the trailing byte(s).
//!
//! `MidiMsg::from_midi` returns `(message, bytes_consumed)`. The parser used to
//! ignore `bytes_consumed`, so a two-byte ProgramChange (`0xC0`) or
//! ChannelPressure (`0xD0`) — or any message — handed an extra trailing data
//! byte parsed the leading bytes and quietly discarded the rest, accepting a
//! malformed frame. It now rejects any frame whose length exceeds what the
//! message consumed.

use conductor_core::event_processor::MidiEvent;

// ---- Well-formed frames still parse (no regression) -----------------------

#[test]
fn exact_length_program_change_is_accepted() {
    // ProgramChange is a TWO-byte message: status + program.
    MidiEvent::from_midi_msg(&[0xC0, 0x2A]).expect("a 2-byte ProgramChange is valid");
}

#[test]
fn exact_length_channel_pressure_is_accepted() {
    // ChannelPressure/Aftertouch is a TWO-byte message: status + pressure.
    MidiEvent::from_midi_msg(&[0xD0, 0x40]).expect("a 2-byte ChannelPressure is valid");
}

#[test]
fn exact_length_note_on_is_accepted() {
    // NoteOn is a THREE-byte message: status + note + velocity.
    MidiEvent::from_midi_msg(&[0x90, 60, 100]).expect("a 3-byte NoteOn is valid");
}

// ---- Over-long frames are rejected (the #2131 fix) ------------------------

#[test]
fn program_change_with_trailing_byte_is_rejected() {
    // [0xC0, 0x2A] is a complete ProgramChange; 0x63 is an extra byte that used
    // to be silently dropped.
    let err = MidiEvent::from_midi_msg(&[0xC0, 0x2A, 0x63])
        .expect_err("a 3-byte ProgramChange has a trailing byte and must be rejected");
    assert!(
        err.contains("trailing") || err.contains("Malformed"),
        "error should flag the dropped trailing byte; got: {err}"
    );
}

#[test]
fn channel_pressure_with_trailing_byte_is_rejected() {
    let err = MidiEvent::from_midi_msg(&[0xD0, 0x64, 0x37])
        .expect_err("a 3-byte ChannelPressure has a trailing byte and must be rejected");
    assert!(
        err.contains("trailing") || err.contains("Malformed"),
        "got: {err}"
    );
}

#[test]
fn three_byte_message_with_trailing_byte_is_rejected() {
    // The same guard applies to 3-byte messages handed a 4th byte.
    let err = MidiEvent::from_midi_msg(&[0x90, 60, 100, 0x55])
        .expect_err("a 4-byte NoteOn has a trailing byte and must be rejected");
    assert!(
        err.contains("trailing") || err.contains("Malformed"),
        "got: {err}"
    );
}
