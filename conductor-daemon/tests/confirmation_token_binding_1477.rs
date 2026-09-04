// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Regression tests for confirmation tokens that must be bound to
//! the operation they approve.
//!
//! ## The bug
//!
//! Before the fix, `request_*_confirmation(..., Some(token))` delegated
//! straight to `confirm_operation(token_id)` without comparing the
//! incoming `(operation_type, device, payload)` against the stored
//! pending request. A token issued for a SysEx parameter change on
//! Device A could therefore be replayed to confirm a factory reset on
//! Device B — the executor proceeded with the new operation while the
//! user had only ever consented to the original one.
//!
//! ## What these tests pin
//!
//! 1. Cross-operation replay is rejected (sysex token ↛ reset,
//!    reset token ↛ sysex, reset token ↛ midi_send).
//! 2. Cross-device replay within the same op type is rejected
//!    (sysex on Device A token ↛ sysex on Device B).
//! 3. Cross-payload replay within the same op type + device is
//!    rejected (sysex with data X ↛ sysex with data Y).
//! 4. The legitimate single-device single-payload flow still confirms
//!    successfully (regression guard for the fix itself).

use conductor_daemon::daemon::hardware_io::{ConfirmationManager, ConfirmationStatus};

/// Helper: issue a token via the SysEx confirmation path.
///
/// Returns the token id. The body uses parameter-change SysEx
/// (manufacturer 0x41 = Roland, then arbitrary parameter bytes),
/// chosen because `SysExValidator` classifies it as `ParameterChange`
/// — which `requires_confirmation()` returns true for, so a token is
/// actually issued (vs. low-risk auto-approve which returns
/// `Confirmed` directly without storing a pending request).
fn issue_sysex_token(mgr: &ConfirmationManager, device: &str, data: &[u8]) -> String {
    let status = mgr
        .request_sysex_confirmation(device, data, None)
        .expect("issue sysex token");
    match status {
        ConfirmationStatus::RequiresConfirmation { token, .. } => token.id,
        other => panic!("expected RequiresConfirmation for SysEx parameter change, got {other:?}"),
    }
}

/// Helper: issue a token via the reset confirmation path.
fn issue_reset_token(mgr: &ConfirmationManager, device: &str, reset_type: &str) -> String {
    let status = mgr
        .request_reset_confirmation(device, reset_type, None)
        .expect("issue reset token");
    match status {
        ConfirmationStatus::RequiresConfirmation { token, .. } => token.id,
        other => panic!("expected RequiresConfirmation for reset, got {other:?}"),
    }
}

/// A Roland parameter-change SysEx that `SysExValidator` classifies as
/// requiring confirmation — used as a stable fixture across tests.
const PARAM_CHANGE_A: &[u8] = &[0xF0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x01, 0xF7];
const PARAM_CHANGE_B: &[u8] = &[0xF0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x02, 0xF7];

// ────────────────────────────────────────────────────────────────────
// 1. Cross-operation replay rejection
// ────────────────────────────────────────────────────────────────────

#[test]
fn sysex_token_cannot_confirm_reset() {
    let mgr = ConfirmationManager::new();
    let token = issue_sysex_token(&mgr, "Maschine Mikro", PARAM_CHANGE_A);
    assert_eq!(mgr.pending_count(), 1, "token must be stored after issue");

    // Replay the SysEx token against a reset operation on the same
    // device. Without binding, this would Confirm the reset (using
    // the stored SysEx request's identity).
    let result = mgr
        .request_reset_confirmation("Maschine Mikro", "factory", Some(&token))
        .expect("call returns Ok");

    assert!(
        matches!(result, ConfirmationStatus::InvalidToken { .. }),
        "sysex token must not confirm a reset operation, got {result:?}"
    );
    assert_eq!(
        mgr.pending_count(),
        0,
        "mismatched token must be consumed (not left in pending for brute-force retry)"
    );
}

#[test]
fn reset_token_cannot_confirm_sysex() {
    let mgr = ConfirmationManager::new();
    let token = issue_reset_token(&mgr, "Launchpad X", "factory");
    assert_eq!(mgr.pending_count(), 1);

    let result = mgr
        .request_sysex_confirmation("Launchpad X", PARAM_CHANGE_A, Some(&token))
        .expect("call returns Ok");

    assert!(
        matches!(result, ConfirmationStatus::InvalidToken { .. }),
        "reset token must not confirm a sysex operation, got {result:?}"
    );
    assert_eq!(mgr.pending_count(), 0, "mismatched token must be consumed");
}

#[test]
fn reset_token_cannot_confirm_midi_send() {
    use conductor_daemon::daemon::hardware_io::MidiSendMessage;

    let mgr = ConfirmationManager::new();
    let token = issue_reset_token(&mgr, "Launchpad X", "factory");
    assert_eq!(mgr.pending_count(), 1);

    let messages = vec![MidiSendMessage {
        message_type: "note_on".into(),
        channel: 1,
        note: Some(60),
        velocity: Some(100),
        controller: None,
        value: None,
        program: None,
    }];

    let result = mgr
        .request_midi_send_confirmation("Launchpad X", &messages, Some(&token))
        .expect("call returns Ok");

    assert!(
        matches!(result, ConfirmationStatus::InvalidToken { .. }),
        "reset token must not confirm a midi_send operation, got {result:?}"
    );
    assert_eq!(mgr.pending_count(), 0, "mismatched token must be consumed");
}

// ────────────────────────────────────────────────────────────────────
// 2. Cross-device replay rejection (same op type)
// ────────────────────────────────────────────────────────────────────

#[test]
fn sysex_token_for_one_device_cannot_confirm_another() {
    let mgr = ConfirmationManager::new();
    let token = issue_sysex_token(&mgr, "Device A", PARAM_CHANGE_A);
    assert_eq!(mgr.pending_count(), 1);

    let result = mgr
        .request_sysex_confirmation("Device B", PARAM_CHANGE_A, Some(&token))
        .expect("call returns Ok");

    assert!(
        matches!(result, ConfirmationStatus::InvalidToken { .. }),
        "sysex token bound to Device A must not confirm Device B, got {result:?}"
    );
    assert_eq!(mgr.pending_count(), 0, "mismatched token must be consumed");
}

#[test]
fn reset_token_for_one_device_cannot_confirm_another() {
    let mgr = ConfirmationManager::new();
    let token = issue_reset_token(&mgr, "Device A", "factory");
    assert_eq!(mgr.pending_count(), 1);

    let result = mgr
        .request_reset_confirmation("Device B", "factory", Some(&token))
        .expect("call returns Ok");

    assert!(
        matches!(result, ConfirmationStatus::InvalidToken { .. }),
        "reset token bound to Device A must not confirm Device B, got {result:?}"
    );
    assert_eq!(mgr.pending_count(), 0, "mismatched token must be consumed");
}

// ────────────────────────────────────────────────────────────────────
// 3. Cross-payload replay rejection (same op type + device)
// ────────────────────────────────────────────────────────────────────

#[test]
fn sysex_token_for_one_payload_cannot_confirm_another() {
    let mgr = ConfirmationManager::new();
    let token = issue_sysex_token(&mgr, "Device A", PARAM_CHANGE_A);
    assert_eq!(mgr.pending_count(), 1);

    // Same device, same op type, but different SysEx body. This is
    // the heart of the confused-deputy attack: the user reviewed
    // PARAM_CHANGE_A but the executor would proceed with PARAM_CHANGE_B.
    let result = mgr
        .request_sysex_confirmation("Device A", PARAM_CHANGE_B, Some(&token))
        .expect("call returns Ok");

    assert!(
        matches!(result, ConfirmationStatus::InvalidToken { .. }),
        "sysex token bound to PARAM_CHANGE_A must not confirm PARAM_CHANGE_B, got {result:?}"
    );
    assert_eq!(mgr.pending_count(), 0, "mismatched token must be consumed");
}

#[test]
fn reset_token_for_one_reset_type_cannot_confirm_another() {
    let mgr = ConfirmationManager::new();
    let token = issue_reset_token(&mgr, "Device A", "soft");
    assert_eq!(mgr.pending_count(), 1);

    // Same device, but escalating from `soft` (medium risk, reversible)
    // to `factory` (high risk, irreversible). The user reviewed only
    // the soft reset.
    let result = mgr
        .request_reset_confirmation("Device A", "factory", Some(&token))
        .expect("call returns Ok");

    assert!(
        matches!(result, ConfirmationStatus::InvalidToken { .. }),
        "soft-reset token must not confirm factory-reset, got {result:?}"
    );
    assert_eq!(mgr.pending_count(), 0, "mismatched token must be consumed");
}

// ────────────────────────────────────────────────────────────────────
// 4. Regression guards for the fix itself
// ────────────────────────────────────────────────────────────────────

#[test]
fn matching_sysex_descriptor_confirms_successfully() {
    let mgr = ConfirmationManager::new();
    let token = issue_sysex_token(&mgr, "Device A", PARAM_CHANGE_A);
    assert_eq!(mgr.pending_count(), 1);

    let result = mgr
        .request_sysex_confirmation("Device A", PARAM_CHANGE_A, Some(&token))
        .expect("call returns Ok");

    assert!(
        matches!(result, ConfirmationStatus::Confirmed { .. }),
        "exact-match replay (same device, same payload, same op type) \
         must still confirm — this guards against the binding fix \
         over-rejecting the legitimate flow. Got {result:?}"
    );
    assert_eq!(
        mgr.pending_count(),
        0,
        "successful confirm must consume the token"
    );
}

#[test]
fn matching_reset_descriptor_confirms_successfully() {
    let mgr = ConfirmationManager::new();
    let token = issue_reset_token(&mgr, "Device A", "factory");
    assert_eq!(mgr.pending_count(), 1);

    let result = mgr
        .request_reset_confirmation("Device A", "factory", Some(&token))
        .expect("call returns Ok");

    assert!(
        matches!(result, ConfirmationStatus::Confirmed { .. }),
        "exact-match reset replay must still confirm, got {result:?}"
    );
    assert_eq!(
        mgr.pending_count(),
        0,
        "successful confirm must consume the token"
    );
}

// ────────────────────────────────────────────────────────────────────
// 5. Brute-force-retry invariant
// ────────────────────────────────────────────────────────────────────
//
// Surfaced by Copilot review: the previous test set proved
// that a mismatched-descriptor confirm returns `InvalidToken`, but
// nothing pinned the *consumption* semantic that prevents an attacker
// from retrying the same token until they happen to guess (or grind
// out) a matching descriptor. The fix consumes the token even on
// mismatch — these tests demonstrate that property end-to-end.

#[test]
fn mismatched_descriptor_blocks_subsequent_legitimate_confirm_with_same_token() {
    let mgr = ConfirmationManager::new();
    let token = issue_sysex_token(&mgr, "Device A", PARAM_CHANGE_A);
    assert_eq!(mgr.pending_count(), 1);

    // Attempt 1: wrong descriptor — rejected and token consumed.
    let attack = mgr
        .request_reset_confirmation("Device A", "factory", Some(&token))
        .expect("call returns Ok");
    assert!(matches!(attack, ConfirmationStatus::InvalidToken { .. }));
    assert_eq!(mgr.pending_count(), 0);

    // Attempt 2: the *correct* descriptor with the same token id.
    // Even though the descriptor matches what the token was issued
    // for, the token was consumed on the mismatched first call and
    // is therefore no longer redeemable. This blocks the brute-force
    // retry shape where an attacker tries multiple descriptors against
    // the same token until one happens to land.
    let retry = mgr
        .request_sysex_confirmation("Device A", PARAM_CHANGE_A, Some(&token))
        .expect("call returns Ok");
    assert!(
        matches!(retry, ConfirmationStatus::InvalidToken { .. }),
        "second call must fail with InvalidToken (token already consumed), got {retry:?}"
    );
}

#[test]
fn cross_op_brute_force_burns_token_per_attempt() {
    // The single-token version of the previous test, demonstrating
    // that each mismatched attempt costs one token — an attacker can't
    // amortise a token across the operation-type / device / payload
    // search space.
    let mgr = ConfirmationManager::new();
    let token_a = issue_sysex_token(&mgr, "Device A", PARAM_CHANGE_A);
    let token_b = issue_sysex_token(&mgr, "Device A", PARAM_CHANGE_B);
    assert_eq!(
        mgr.pending_count(),
        2,
        "two distinct issues must store two pending requests"
    );

    // Try token_a against the wrong reset op — consumed.
    let _ = mgr.request_reset_confirmation("Device A", "factory", Some(&token_a));
    assert_eq!(mgr.pending_count(), 1, "exactly one token consumed");

    // Try token_b against the wrong reset op — also consumed.
    let _ = mgr.request_reset_confirmation("Device A", "factory", Some(&token_b));
    assert_eq!(mgr.pending_count(), 0, "the second token consumed too");
}
