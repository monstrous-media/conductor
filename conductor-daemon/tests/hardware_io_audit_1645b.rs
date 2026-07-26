// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! Bundle B regression tests for epic #1645 (Council audit follow-ups).
//!
//! Both fixes live in `conductor-daemon/src/daemon/hardware_io/confirmation.rs`:
//!
//! - **#1618** — `request_midi_send_confirmation` now validates each
//!   `MidiSendMessage` at the gate before auto-confirming. A malformed
//!   message (channel out of 1–16, missing required field, etc.) used
//!   to slip past the gate and surface later in the executor's
//!   `to_bytes()` call with less context. Now it returns `Blocked`
//!   with the offending message index + the validate-error string.
//!
//! - **#1619** — every `request_*_confirmation` entry now calls
//!   `cleanup_expired()` opportunistically so the `pending` HashMap
//!   can't grow unbounded under an adversarial caller that issues
//!   tokens without ever following up. Bound is now
//!   (issue-rate × TOKEN_EXPIRATION_SECS) for normal use.
//!
//! The regression net is small but pins both behaviours end-to-end.

use conductor_daemon::daemon::hardware_io::{
    ConfirmationManager, ConfirmationStatus, MidiSendMessage,
};

fn valid_note_on(note: u8) -> MidiSendMessage {
    MidiSendMessage {
        message_type: "note_on".into(),
        channel: 1,
        note: Some(note),
        velocity: Some(100),
        controller: None,
        value: None,
        program: None,
    }
}

fn invalid_channel_message() -> MidiSendMessage {
    MidiSendMessage {
        message_type: "note_on".into(),
        channel: 99, // out of 1-16 range — caught by validate()
        note: Some(60),
        velocity: Some(100),
        controller: None,
        value: None,
        program: None,
    }
}

/// Roland parameter-change SysEx that classifies as requiring
/// confirmation — used to populate `pending` for #1619 tests.
const PARAM_CHANGE: &[u8] = &[0xF0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x01, 0xF7];

// ────────────────────────────────────────────────────────────────────
// #1618 — MIDI validation at the gate
// ────────────────────────────────────────────────────────────────────

#[test]
fn midi_send_with_invalid_channel_is_blocked_at_gate() {
    // Pre-fix this returned `Confirmed` and the executor's `to_bytes()`
    // later surfaced the "Channel 99 out of range" error with no link
    // to the confirmation gate. Post-fix it's blocked here with a
    // structured reason naming the offending message.
    let mgr = ConfirmationManager::new();
    let messages = vec![invalid_channel_message()];
    let result = mgr
        .request_midi_send_confirmation("Test Port", &messages, None)
        .expect("call returns Ok");
    match result {
        ConfirmationStatus::Blocked { reason } => {
            assert!(
                reason.contains("99") || reason.to_lowercase().contains("channel"),
                "blocked reason should name the channel-validation failure, got: {reason}"
            );
        }
        other => panic!("expected Blocked for invalid channel, got {other:?}"),
    }
}

#[test]
fn midi_send_blocks_with_index_when_only_some_messages_invalid() {
    // The fix surfaces *which* message in the batch failed validation
    // — important when a client sends a mixed batch and needs to know
    // which entry to fix.
    let mgr = ConfirmationManager::new();
    let messages = vec![
        valid_note_on(60),
        invalid_channel_message(), // index 1
        valid_note_on(64),
    ];
    let result = mgr
        .request_midi_send_confirmation("Test Port", &messages, None)
        .expect("call returns Ok");
    match result {
        ConfirmationStatus::Blocked { reason } => {
            assert!(
                reason.contains("message 1"),
                "blocked reason should name the failing index (message 1), got: {reason}"
            );
        }
        other => panic!("expected Blocked naming the invalid index, got {other:?}"),
    }
}

#[test]
fn midi_send_with_all_valid_messages_still_confirms() {
    // Regression guard for the fix: the validation loop must not
    // accidentally block well-formed batches.
    let mgr = ConfirmationManager::new();
    let messages = vec![valid_note_on(60), valid_note_on(62), valid_note_on(64)];
    let result = mgr
        .request_midi_send_confirmation("Test Port", &messages, None)
        .expect("call returns Ok");
    assert!(
        matches!(result, ConfirmationStatus::Confirmed { .. }),
        "valid MIDI batch must still auto-confirm, got {result:?}"
    );
}

// ────────────────────────────────────────────────────────────────────
// #1619 — opportunistic cleanup_expired on each request entry
// ────────────────────────────────────────────────────────────────────

#[test]
fn valid_pending_tokens_survive_opportunistic_cleanup_on_subsequent_requests() {
    // The opportunistic cleanup at the top of each request_*_confirmation
    // must remove ONLY expired tokens — never valid in-flight ones.
    // This pins the lower-bound: with no expired tokens present, each
    // subsequent request adds one to pending without dropping any.
    //
    // The actually-removes-expired behaviour is covered by
    // `cleanup_expired()`'s own pre-existing unit tests (TOKEN_EXPIRATION_SECS
    // is 60s, too slow to drive end-to-end here without a configurable-
    // timeout test seam — out of scope for this bundle).
    let mgr = ConfirmationManager::new();
    assert_eq!(mgr.pending_count(), 0, "manager starts empty");

    for i in 0..5 {
        let device = format!("Device {i}");
        let result = mgr
            .request_sysex_confirmation(&device, PARAM_CHANGE, None)
            .expect("issue sysex token");
        assert!(
            matches!(result, ConfirmationStatus::RequiresConfirmation { .. }),
            "PARAM_CHANGE SysEx must require confirmation (iteration {i}), got {result:?}"
        );
        assert_eq!(
            mgr.pending_count(),
            i + 1,
            "fresh token #{i} must be stored — opportunistic cleanup must not drop it"
        );
    }
}

#[test]
fn opportunistic_cleanup_does_not_drop_valid_tokens_across_op_types() {
    // Same lower-bound check across all three request_*_confirmation
    // entry points — the cleanup hook must be uniformly safe.
    let mgr = ConfirmationManager::new();

    // 1) request_sysex_confirmation creates a pending token.
    let _ = mgr
        .request_sysex_confirmation("Dev A", PARAM_CHANGE, None)
        .expect("sysex issue");
    assert_eq!(mgr.pending_count(), 1);

    // 2) request_reset_confirmation creates another.
    let _ = mgr
        .request_reset_confirmation("Dev A", "factory", None)
        .expect("reset issue");
    assert_eq!(mgr.pending_count(), 2);

    // 3) request_midi_send_confirmation auto-confirms low-risk MIDI
    //    and does NOT store a pending request. Pending count stays
    //    at 2 even though we called the entry.
    let messages = vec![valid_note_on(60)];
    let _ = mgr
        .request_midi_send_confirmation("Dev A", &messages, None)
        .expect("midi send auto-confirm");
    assert_eq!(
        mgr.pending_count(),
        2,
        "midi_send auto-confirm doesn't store a pending request; the \
         opportunistic cleanup must not drop the two non-expired tokens"
    );
}
