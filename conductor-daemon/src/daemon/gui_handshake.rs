// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-027 §D19 — GUI launch handshake (#1215).
//!
//! Phase 1A's peer pinning defeats most GUI impersonation, but a
//! same-user attacker who places a binary at a path the user happens
//! to trust could still pass. D19 closes that gap by binding GUI
//! authority to the daemon-spawned process tree via a launch-time
//! nonce:
//!
//! 1. Daemon spawns the GUI as a child process and writes a fresh
//!    256-bit nonce to its **stdin** (NOT env — env is readable from
//!    `/proc/<pid>/environ` on Linux).
//! 2. Daemon registers the `(child_pid, nonce)` pair as pending.
//! 3. GUI reads the nonce from stdin at startup, sends it on its
//!    FIRST IPC request via `IpcCommand::Handshake { nonce }`.
//! 4. Daemon verifies the nonce matches AND the connecting peer PID
//!    matches the spawn PID. On success: upgrade tier to `GuiTrusted`.
//!    On failure or absence: stay at the default (probably `Untrusted`).
//!
//! An independently launched fake GUI never sees the nonce, so it
//! cannot replay even with the right `argv[0]` and matching exe path.
//!
//! This module ships the **nonce registry only** — the
//! storage + verification surface. Daemon-side GUI spawn (writing
//! the nonce to stdin) and the IPC `Handshake` dispatch (routing
//! verify results into the gate) are separate follow-up PRs.

use std::collections::HashMap;
use std::sync::Mutex;

/// Length of the launch nonce in bytes. 32 bytes = 256 bits of OS-RNG
/// entropy — comfortably out of brute-force range for the nonce
/// lifetime (single IPC connect after launch).
pub const HANDSHAKE_NONCE_BYTES: usize = 32;

/// A handshake nonce. Wrapped in a newtype so it can't accidentally
/// be compared against arbitrary `[u8; 32]` keys, and so the
/// constant-time `PartialEq` impl below can't be bypassed.
///
/// **Equality is constant-time** via `subtle::ConstantTimeEq`
/// (Council review on PR #1222). A naive derived `PartialEq` on
/// `[u8; 32]` short-circuits on the first byte mismatch, which
/// leaks the matching prefix length to a local attacker who can
/// time the daemon's IPC responses. For a 256-bit nonce on a
/// localhost socket the practical attack is borderline, but
/// comparing secret values in constant time is the standard
/// security hygiene and costs essentially nothing here.
#[derive(Clone)]
pub struct HandshakeNonce([u8; HANDSHAKE_NONCE_BYTES]);

impl PartialEq for HandshakeNonce {
    fn eq(&self, other: &Self) -> bool {
        use subtle::ConstantTimeEq;
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for HandshakeNonce {}

impl HandshakeNonce {
    /// Generate a fresh nonce from the OS RNG. Fails only if the OS
    /// RNG is unavailable (extraordinarily rare; treated as a hard
    /// error rather than a silent fallback to weak randomness).
    pub fn from_os_rng() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; HANDSHAKE_NONCE_BYTES];
        // getrandom 0.3+ renamed the free fill function from `getrandom` to `fill`.
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// Construct from raw bytes — used by IPC deserialisation when a
    /// GUI client sends its claimed nonce on the wire.
    pub fn from_bytes(bytes: [u8; HANDSHAKE_NONCE_BYTES]) -> Self {
        Self(bytes)
    }

    /// Encode as URL-safe base64 for stdin transport. Stable
    /// representation so the GUI on the other end of the pipe can
    /// parse it reliably; base64 keeps the wire human-debuggable
    /// without exposing the raw bytes in tracing output.
    pub fn to_base64(&self) -> String {
        use base64::{Engine as _, engine::general_purpose};
        general_purpose::URL_SAFE_NO_PAD.encode(self.0)
    }

    /// Decode from base64. Returns `None` for any non-conforming
    /// input — wrong length, invalid characters. Constant-time
    /// rejection here doesn't matter (untrusted data); what matters
    /// is that we don't panic.
    pub fn from_base64(s: &str) -> Option<Self> {
        use base64::{Engine as _, engine::general_purpose};
        let bytes = general_purpose::URL_SAFE_NO_PAD.decode(s).ok()?;
        let arr: [u8; HANDSHAKE_NONCE_BYTES] = bytes.try_into().ok()?;
        Some(Self(arr))
    }
}

impl std::fmt::Debug for HandshakeNonce {
    /// Never print the bytes. A debug-format leak of a still-pending
    /// nonce would let any reader of the log impersonate the GUI on
    /// its first IPC connect. Print a length-only redacted form.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HandshakeNonce(<{} redacted bytes>)",
            HANDSHAKE_NONCE_BYTES
        )
    }
}

/// Result of a handshake verification.
#[derive(Debug, PartialEq, Eq)]
pub enum HandshakeVerdict {
    /// Nonce matched the pending registration for this PID.
    /// Connection should be upgraded to `GuiTrusted`. The pending
    /// nonce is consumed (single-use) — a second handshake attempt
    /// from the same PID will fail.
    Match,
    /// PID has no pending registration. Common case for non-GUI
    /// peers; the caller should leave the connection at its default
    /// tier rather than treating this as a failure to flag.
    NoPending,
    /// PID has a pending registration but the nonce didn't match.
    /// The pending nonce is INVALIDATED (one strike) so a subsequent
    /// guess can't burn the registry. Distinguished from `NoPending`
    /// so the gate / audit log can flag it as a deliberate
    /// impersonation attempt.
    Mismatch,
}

/// In-memory registry of pending GUI handshakes. Thread-safe; one
/// instance is held by `DaemonService` for the daemon's lifetime
/// and shared with the IPC server via `Arc`.
#[derive(Default)]
pub struct GuiHandshakeRegistry {
    pending: Mutex<HashMap<u32, HandshakeNonce>>,
}

impl GuiHandshakeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pending handshake for `child_pid` with `nonce`.
    /// Overwrites any prior pending entry for the same PID — the
    /// daemon's GUI launcher is the sole writer in production, so a
    /// PID collision means the previous launch was abandoned.
    ///
    /// On a poisoned `Mutex`, logs a warning and returns without
    /// touching state. The verify path is separately
    /// poison-protected and returns `NoPending` on the same
    /// condition, so the registry overall fails closed (no
    /// handshake match → no elevation) — but the warning surfaces
    /// the prior panic so operators can investigate
    /// (Council review on PR #1222).
    pub fn register_pending(&self, child_pid: u32, nonce: HandshakeNonce) {
        match self.pending.lock() {
            Ok(mut pending) => {
                pending.insert(child_pid, nonce);
            }
            Err(_) => {
                tracing::warn!(
                    "gui_handshake: register_pending(pid={}) skipped — Mutex poisoned. \
                     A prior panic left the registry in an unknown state; subsequent \
                     handshakes from this PID will fail closed (NoPending).",
                    child_pid
                );
            }
        }
    }

    /// Verify a handshake attempt. On `Match`, the pending entry is
    /// consumed (single-use semantics — a replay from the same PID
    /// would return `NoPending`). On `Mismatch`, the entry is also
    /// removed so a brute-force walker can't repeatedly probe.
    pub fn verify(&self, peer_pid: u32, attempt: &HandshakeNonce) -> HandshakeVerdict {
        let mut pending = match self.pending.lock() {
            Ok(p) => p,
            Err(_) => return HandshakeVerdict::NoPending,
        };
        match pending.get(&peer_pid) {
            None => HandshakeVerdict::NoPending,
            Some(expected) if expected == attempt => {
                pending.remove(&peer_pid);
                HandshakeVerdict::Match
            }
            Some(_) => {
                // One strike: invalidate the pending entry so a
                // brute-force walker can't keep guessing. The real
                // GUI never sees a mismatch because it has the
                // exact bytes piped to stdin.
                pending.remove(&peer_pid);
                HandshakeVerdict::Mismatch
            }
        }
    }

    /// Drop a pending handshake by PID — used when the daemon's GUI
    /// spawn fails after `register_pending` but before the child
    /// actually connects. Idempotent.
    ///
    /// On poisoned `Mutex`, logs a warning. Mirrors
    /// `register_pending`'s posture (Council review on PR #1222).
    pub fn forget(&self, child_pid: u32) {
        match self.pending.lock() {
            Ok(mut pending) => {
                pending.remove(&child_pid);
            }
            Err(_) => {
                tracing::warn!(
                    "gui_handshake: forget(pid={}) skipped — Mutex poisoned",
                    child_pid
                );
            }
        }
    }

    #[cfg(test)]
    fn pending_count_for_test(&self) -> usize {
        self.pending.lock().map(|p| p.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_nonce(byte: u8) -> HandshakeNonce {
        HandshakeNonce::from_bytes([byte; HANDSHAKE_NONCE_BYTES])
    }

    #[test]
    fn nonce_roundtrips_through_base64() {
        let nonce = HandshakeNonce::from_bytes([0xAB; HANDSHAKE_NONCE_BYTES]);
        let s = nonce.to_base64();
        let back = HandshakeNonce::from_base64(&s).expect("decode");
        assert_eq!(nonce, back);
    }

    #[test]
    fn base64_rejects_malformed_input() {
        // Too short.
        assert!(HandshakeNonce::from_base64("abc").is_none());
        // Wrong-length decoded bytes.
        assert!(HandshakeNonce::from_base64("AAAA").is_none());
        // Invalid chars (URL_SAFE_NO_PAD rejects standard padding).
        assert!(HandshakeNonce::from_base64("not base64!").is_none());
    }

    #[test]
    fn debug_never_leaks_bytes() {
        let nonce = HandshakeNonce::from_bytes([0xDE; HANDSHAKE_NONCE_BYTES]);
        let s = format!("{:?}", nonce);
        assert!(
            !s.contains("de") && !s.contains("DE") && !s.contains("0xDE"),
            "Debug leaked nonce bytes: {s}"
        );
        assert!(s.contains("redacted"));
    }

    #[test]
    fn os_rng_produces_distinct_nonces() {
        // Two consecutive nonces must differ — sanity check that
        // we're not accidentally reusing a zero buffer.
        let a = HandshakeNonce::from_os_rng().expect("rng a");
        let b = HandshakeNonce::from_os_rng().expect("rng b");
        assert_ne!(a, b, "OS RNG must produce distinct nonces");
    }

    #[test]
    fn verify_unknown_pid_is_no_pending() {
        let reg = GuiHandshakeRegistry::new();
        assert_eq!(
            reg.verify(12345, &fixed_nonce(0x01)),
            HandshakeVerdict::NoPending
        );
    }

    #[test]
    fn verify_matching_nonce_consumes_entry() {
        let reg = GuiHandshakeRegistry::new();
        reg.register_pending(12345, fixed_nonce(0x42));

        assert_eq!(reg.pending_count_for_test(), 1);
        assert_eq!(
            reg.verify(12345, &fixed_nonce(0x42)),
            HandshakeVerdict::Match
        );
        // Single-use: a second handshake from the same PID finds nothing.
        assert_eq!(reg.pending_count_for_test(), 0);
        assert_eq!(
            reg.verify(12345, &fixed_nonce(0x42)),
            HandshakeVerdict::NoPending
        );
    }

    #[test]
    fn verify_mismatched_nonce_invalidates_entry() {
        let reg = GuiHandshakeRegistry::new();
        reg.register_pending(12345, fixed_nonce(0x42));

        // Wrong nonce: Mismatch AND entry removed (one strike).
        assert_eq!(
            reg.verify(12345, &fixed_nonce(0xFF)),
            HandshakeVerdict::Mismatch
        );
        assert_eq!(reg.pending_count_for_test(), 0);
        // Even the correct nonce doesn't help now — the registry
        // has been emptied.
        assert_eq!(
            reg.verify(12345, &fixed_nonce(0x42)),
            HandshakeVerdict::NoPending
        );
    }

    #[test]
    fn register_overwrites_prior_pending_for_same_pid() {
        let reg = GuiHandshakeRegistry::new();
        reg.register_pending(12345, fixed_nonce(0x42));
        // A second register (e.g. the previous launch was abandoned
        // and the daemon spawned a fresh GUI) replaces the entry.
        reg.register_pending(12345, fixed_nonce(0x99));

        assert_eq!(
            reg.verify(12345, &fixed_nonce(0x42)),
            HandshakeVerdict::Mismatch
        );
    }

    #[test]
    fn forget_removes_pending_without_signal() {
        let reg = GuiHandshakeRegistry::new();
        reg.register_pending(12345, fixed_nonce(0x42));
        reg.forget(12345);

        // A subsequent verify sees no pending and doesn't penalise
        // the would-be peer (no Mismatch flag).
        assert_eq!(
            reg.verify(12345, &fixed_nonce(0x42)),
            HandshakeVerdict::NoPending
        );
    }

    #[test]
    fn distinct_pids_are_isolated() {
        let reg = GuiHandshakeRegistry::new();
        reg.register_pending(1, fixed_nonce(0xAA));
        reg.register_pending(2, fixed_nonce(0xBB));

        // PID 1's nonce doesn't verify against PID 2's slot.
        assert_eq!(
            reg.verify(1, &fixed_nonce(0xBB)),
            HandshakeVerdict::Mismatch
        );
        // PID 2's entry is intact (the Mismatch above only affects PID 1).
        assert_eq!(reg.verify(2, &fixed_nonce(0xBB)), HandshakeVerdict::Match);
    }
}
