// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! MIDI recursion guard (ADR-015 D8) + cascade suppression
//!
//! Prevents SendMidi/MidiForward output from being re-ingested as input,
//! which would cause infinite recursion (feedback loop). Two layers:
//!
//! 1. **Per-message echo guard** (ADR-015 D8): a 64-entry ring buffer of
//!    FNV-1a fingerprints with 100ms TTL. When a MIDI message is sent,
//!    its fingerprint is recorded; incoming events that fingerprint-match
//!    a recent send are suppressed. Catches exact-bytes echo only.
//!
//! 2. **Per-port blanket suppression** (on by default): when
//!    `AdvancedSettings::allow_cascade = false` (the default), every
//!    `SendMidi`/`MidiForward` completion opens a TTL window on the
//!    *output port*; any subsequent MIDI input on that port within the
//!    window is dropped regardless of byte content. Catches the
//!    cross-note cascade case where mapping A sends note 63 and mapping
//!    B is triggered by note 63 looping back. Set `allow_cascade = true`
//!    to opt out — only the per-message echo guard runs in that mode.
//!
//! The two layers are independent: the echo guard always runs; the
//! blanket layer only fires when (1) `allow_cascade = false` and (2) a
//! port window is currently active.
//!
//! ## Thread safety
//!
//! `MidiRecursionGuard` is not `Sync` and provides no internal
//! synchronization — concurrent callers wrap it in a `std::sync::Mutex`
//! at the use site (see `EngineManager::recursion_guard`). All public
//! methods take `&mut self` (the blanket-suppression layer prunes
//! expired entries on read), so external synchronization is required.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Number of entries in the ring buffer
const RING_SIZE: usize = 64;

/// Time-to-live for fingerprint entries
const TTL: std::time::Duration = std::time::Duration::from_millis(100);

/// Upper clamp for `set_blanket_suppression` TTL.
///
/// Cascade suppression exists to break tight feedback loops measured in
/// milliseconds; >1s of blanket muting on a port is almost certainly a
/// misconfiguration, and unbounded `u64` ttls would let an absurd config
/// value reach `Instant::checked_add` (which returns `None` near
/// `Instant::MAX` and would otherwise panic on `+ Duration`). Clamp to a
/// generous-but-finite ceiling.
const BLANKET_TTL_MAX_MS: u64 = 60_000;

/// FNV-1a offset basis for 64-bit
const FNV_OFFSET: u64 = 0xcbf29ce484222325;
/// FNV-1a prime for 64-bit
const FNV_PRIME: u64 = 0x100000001b3;

/// MIDI recursion guard: ring buffer for exact-bytes echo detection plus
/// per-port blanket suppression for cross-note cascade.
pub struct MidiRecursionGuard {
    /// Ring buffer of (fingerprint, source-device fingerprint, timestamp)
    /// triples (ADR-015 D8 — exact echo). `None` slots are unused (initial
    /// state of the buffer); a `Some` slot is considered an echo only when the
    /// fingerprint matches, the timestamp is within `TTL`, AND the event did
    /// NOT arrive from the same device that originated the recorded send (see
    /// [`Self::is_echo`]). Using `Option` instead of an "already-expired"
    /// sentinel sidesteps the `Instant - Duration` underflow possible at
    /// system-boot time.
    ///
    /// The middle element is the FNV-1a fingerprint of the SOURCE device id the
    /// send was attributed to (`None` when the send had no attributable
    /// source). It exists to stop the guard suppressing a controller's own
    /// rapid-repeated notes: when a route forwards `mpk → iac-out`, mpk's bytes
    /// are recorded, and without source-scoping mpk's *next* identical message
    /// (a re-strike, double-tap, or the matching note-off) would be flagged as
    /// an "echo" and dropped — intermittently leaving a forwarded note-on
    /// unpaired → stuck note. A genuine loopback echo arrives on a DIFFERENT
    /// device (the input bound to the output's port), so excluding the source
    /// device still catches it.
    ///
    /// CAVEAT: a SAME-device loopback — a self-route whose forwarded bytes come
    /// back on the very device that originated them — is intentionally NOT caught
    /// by this byte guard (it is indistinguishable from the source replaying its
    /// own notes). Such topologies rely on per-port blanket suppression
    /// ([`Self::set_blanket_suppression`]) instead.
    entries: [Option<(u64, Option<u64>, Instant)>; RING_SIZE],
    /// Next write position
    write_pos: usize,
    /// Per-port blanket-suppression "until" timestamps.
    /// Inserted by `set_blanket_suppression`, checked by
    /// `is_blanket_suppressed`. Map size is bounded by the number of
    /// distinct active output ports, which is typically <10 in real
    /// configurations; entries are pruned lazily on read.
    blanket_until: HashMap<String, Instant>,
}

impl MidiRecursionGuard {
    /// Create a new recursion guard with empty buffer.
    ///
    /// All ring slots start as `None`, so `is_echo` returns false for
    /// every input until something is recorded. No `Instant` arithmetic
    /// runs in `new()`, so daemons launched within `TTL` of system boot
    /// can't trigger an `Instant - Duration` underflow.
    pub fn new() -> Self {
        Self {
            entries: [None; RING_SIZE],
            write_pos: 0,
            blanket_until: HashMap::new(),
        }
    }

    /// Record a sent MIDI message's fingerprint, attributed to the SOURCE
    /// device that originated it (the device whose event triggered the
    /// `SendMidi`/`MidiForward`/route). `source` is `None` for sends with no
    /// attributable origin (e.g. a plugin-initiated send); such records are
    /// suppressible from any device (the pre-source-scoping behaviour).
    pub fn record(&mut self, raw_midi: &[u8], source: Option<&str>) {
        let fp = fnv1a(raw_midi);
        let source_fp = source.map(|s| fnv1a(s.as_bytes()));
        self.entries[self.write_pos] = Some((fp, source_fp, Instant::now()));
        self.write_pos = (self.write_pos + 1) % RING_SIZE;
    }

    /// Check if a received MIDI message is an echo of a recently sent message.
    ///
    /// `from_device` is the device the incoming event arrived on. A recorded
    /// send is treated as an echo of this event only when the fingerprint
    /// matches, it's within `TTL`, AND it did NOT originate from `from_device`
    /// — i.e. the bytes came back on a DIFFERENT device (a genuine loopback),
    /// not the same controller replaying its own notes. Records with no
    /// attributed source (`None`) match any device, preserving the original
    /// content-only behaviour for unattributed sends.
    ///
    /// Note: by construction this does NOT catch a same-device (self-route)
    /// loopback — see the `entries` field doc; those rely on blanket suppression.
    pub fn is_echo(&self, raw_midi: &[u8], from_device: Option<&str>) -> bool {
        let fp = fnv1a(raw_midi);
        let from_fp = from_device.map(|s| fnv1a(s.as_bytes()));
        let now = Instant::now();

        for entry in &self.entries {
            if let Some((entry_fp, source_fp, entry_time)) = entry
                && *entry_fp == fp
                && now.duration_since(*entry_time) < TTL
            {
                // Skip records that originated from this very device — that's
                // the source replaying its own input, not a loopback echo.
                let source_replay = matches!((source_fp, from_fp), (Some(s), Some(f)) if *s == f);
                if !source_replay {
                    return true;
                }
            }
        }
        false
    }

    /// Set or extend the blanket-suppression window for a port.
    /// Called from action completion when `SendMidi`/`MidiForward` writes
    /// to `port`. While the window is active, every MIDI event on that
    /// port is suppressed by [`Self::is_blanket_suppressed`] regardless
    /// of byte content. Idempotent: a later call extends the window
    /// (replaces, not adds — the most recent send wins).
    ///
    /// `ttl_ms` is clamped to [`BLANKET_TTL_MAX_MS`] before use, both to
    /// reject absurd config values (60s of port muting is already far
    /// past any cascade timescale) and to keep `Instant::checked_add`
    /// well clear of `Instant::MAX`. If `checked_add` still returns
    /// `None`, the call is a no-op (the existing window, if any, stays
    /// in place).
    pub fn set_blanket_suppression(&mut self, port: &str, ttl_ms: u64) {
        let ttl_ms = ttl_ms.min(BLANKET_TTL_MAX_MS);
        let Some(until) = Instant::now().checked_add(Duration::from_millis(ttl_ms)) else {
            return;
        };
        self.blanket_until.insert(port.to_string(), until);
    }

    /// Check whether `port` is inside an active blanket-suppression window.
    /// Returns `false` for ports that never had a window set
    /// or whose window has expired. Lazily prunes the expired entry from
    /// the map on a `false` result so a port that goes silent doesn't
    /// permanently consume memory.
    pub fn is_blanket_suppressed(&mut self, port: &str) -> bool {
        let now = Instant::now();
        match self.blanket_until.get(port) {
            Some(&until) if now < until => true,
            Some(_) => {
                // Expired — drop the entry so the map stays bounded by
                // *active* ports rather than all-time-touched ports.
                self.blanket_until.remove(port);
                false
            }
            None => false,
        }
    }
}

impl Default for MidiRecursionGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// FNV-1a hash of a byte slice
fn fnv1a(data: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_duplicate_within_ttl_detected() {
        let mut guard = MidiRecursionGuard::new();
        let msg = &[0x90, 60, 100]; // Note On
        guard.record(msg, None);
        assert!(guard.is_echo(msg, None));
    }

    #[test]
    fn test_expired_not_detected() {
        let mut guard = MidiRecursionGuard::new();
        let msg = &[0x90, 60, 100];

        // Manually inject an entry whose timestamp is past the TTL.
        let fp = fnv1a(msg);
        guard.entries[0] = Some((
            fp,
            None,
            Instant::now() - std::time::Duration::from_millis(150),
        ));
        guard.write_pos = 1;

        assert!(!guard.is_echo(msg, None));
    }

    #[test]
    fn test_different_message_passes() {
        let mut guard = MidiRecursionGuard::new();
        let msg1 = &[0x90, 60, 100]; // Note On C4
        let msg2 = &[0x90, 62, 100]; // Note On D4
        guard.record(msg1, None);
        assert!(!guard.is_echo(msg2, None));
    }

    #[test]
    fn test_ring_buffer_eviction() {
        let mut guard = MidiRecursionGuard::new();

        // Record 65 unique messages (overflows 64-entry buffer)
        for i in 0..65u8 {
            guard.record(&[0x90, i, 100], None);
        }

        // First message (i=0) should have been evicted
        assert!(!guard.is_echo(&[0x90, 0, 100], None));

        // Last message (i=64) should still be present
        assert!(guard.is_echo(&[0x90, 64, 100], None));
    }

    #[test]
    fn test_fnv1a_different_inputs() {
        let h1 = fnv1a(&[0x90, 60, 100]);
        let h2 = fnv1a(&[0x90, 62, 100]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_empty_guard_no_echo() {
        let guard = MidiRecursionGuard::new();
        assert!(!guard.is_echo(&[0x90, 60, 100], None));
    }

    // ── source-aware echo guard (stuck-note fix) ──────────────────

    #[test]
    fn source_replaying_its_own_notes_is_not_an_echo() {
        // The bug: a route `mpk → iac-out` records mpk's forwarded bytes; mpk's
        // OWN next identical message must NOT be flagged as an echo, else a
        // note-off can be dropped → stuck note.
        let mut guard = MidiRecursionGuard::new();
        let note_off = &[0x80, 45, 0];
        guard.record(note_off, Some("mpk"));
        assert!(
            !guard.is_echo(note_off, Some("mpk")),
            "the source device replaying its own bytes is not a loopback echo"
        );
    }

    #[test]
    fn loopback_on_a_different_device_is_still_an_echo() {
        // A genuine echo arrives on a DIFFERENT device (the input bound to the
        // output's port) — that must still be suppressed.
        let mut guard = MidiRecursionGuard::new();
        let note = &[0x90, 45, 100];
        guard.record(note, Some("mpk")); // forwarded mpk → iac-out
        assert!(
            guard.is_echo(note, Some("iac")),
            "the same bytes arriving on a different device is a loopback echo"
        );
    }

    #[test]
    fn unattributed_send_is_suppressible_from_any_device() {
        // A send with no source (None) keeps the original content-only
        // behaviour — suppressible regardless of the incoming device.
        let mut guard = MidiRecursionGuard::new();
        let note = &[0x90, 60, 100];
        guard.record(note, None);
        assert!(guard.is_echo(note, Some("anything")));
        assert!(guard.is_echo(note, None));
    }

    #[test]
    fn attributed_send_with_unknown_incoming_device_is_suppressed() {
        // If the incoming side has no device id, we can't prove it's the source
        // replaying, so we suppress (safe default — don't let an unattributable
        // echo cascade).
        let mut guard = MidiRecursionGuard::new();
        let note = &[0x90, 60, 100];
        guard.record(note, Some("mpk"));
        assert!(guard.is_echo(note, None));
    }

    // ── Per-port blanket suppression ──────────────────

    #[test]
    fn test_blanket_suppression_active_within_ttl() {
        // Set window for "port-A"; an immediate check is suppressed.
        // This is the cross-note cascade case the new layer exists for.
        let mut guard = MidiRecursionGuard::new();
        guard.set_blanket_suppression("port-A", 100);
        assert!(guard.is_blanket_suppressed("port-A"));
    }

    #[test]
    fn test_blanket_suppression_other_ports_unaffected() {
        // Setting suppression on port-A must NOT suppress port-B —
        // each port has its own independent window.
        let mut guard = MidiRecursionGuard::new();
        guard.set_blanket_suppression("port-A", 100);
        assert!(!guard.is_blanket_suppressed("port-B"));
    }

    #[test]
    fn test_blanket_suppression_unknown_port_not_suppressed() {
        let mut guard = MidiRecursionGuard::new();
        // Never called set_blanket_suppression — every port is open.
        assert!(!guard.is_blanket_suppressed("port-A"));
    }

    #[test]
    fn test_blanket_suppression_expires_and_prunes() {
        // Manually inject an already-expired window and confirm both
        // (a) the check returns false and
        // (b) the entry is pruned so the map stays bounded by active ports.
        let mut guard = MidiRecursionGuard::new();
        guard.blanket_until.insert(
            "port-A".to_string(),
            Instant::now() - Duration::from_millis(1),
        );
        assert!(!guard.is_blanket_suppressed("port-A"));
        assert!(
            !guard.blanket_until.contains_key("port-A"),
            "expired entry should be pruned on read"
        );
    }

    #[test]
    fn test_blanket_suppression_extend_replaces_window() {
        // A second `set_blanket_suppression` for the same port replaces
        // (not adds to) the existing window — the most recent send wins.
        let mut guard = MidiRecursionGuard::new();
        guard.set_blanket_suppression("port-A", 100);
        guard.set_blanket_suppression("port-A", 1000);
        assert!(guard.is_blanket_suppressed("port-A"));
        // Map should still have exactly one entry for "port-A".
        assert_eq!(guard.blanket_until.len(), 1);
    }

    #[test]
    fn test_blanket_suppression_clamps_absurd_ttl() {
        // u64::MAX would overflow `Instant + Duration` in older code paths.
        // The clamp pins it at BLANKET_TTL_MAX_MS so the call is safe and
        // still produces an active window.
        let mut guard = MidiRecursionGuard::new();
        guard.set_blanket_suppression("port-A", u64::MAX);
        assert!(guard.is_blanket_suppressed("port-A"));
        let until = guard.blanket_until.get("port-A").copied().unwrap();
        // Window must be ≤ BLANKET_TTL_MAX_MS from "now" (allow generous
        // scheduler slack so the test isn't flaky on slow CI).
        let max_window = Duration::from_millis(BLANKET_TTL_MAX_MS) + Duration::from_millis(50);
        assert!(until <= Instant::now() + max_window);
    }

    #[test]
    fn test_new_buffer_is_empty_no_false_positives() {
        // Regression: previously `new()` initialised slots with fingerprint 0
        // and an "expired" timestamp computed via `Instant::now() - TTL`,
        // which panics on system-boot underflow. With `Option`-init the
        // buffer is unambiguously empty: every probe (including a
        // hypothetical fp=0 input) returns false.
        let guard = MidiRecursionGuard::new();
        assert!(!guard.is_echo(&[], None)); // empty bytes hash to FNV_OFFSET, not 0
        assert!(!guard.is_echo(&[0x00], None)); // any single byte
        assert!(guard.entries.iter().all(|e| e.is_none()));
    }

    #[test]
    fn test_blanket_suppression_independent_of_echo_path() {
        // The blanket layer is orthogonal to the per-message echo guard.
        // A message that's NOT an echo (never recorded) is still
        // blanket-suppressed if its port's window is open.
        let mut guard = MidiRecursionGuard::new();
        let novel_msg = &[0x90, 60, 100];
        assert!(!guard.is_echo(novel_msg, None)); // never recorded
        guard.set_blanket_suppression("port-A", 100);
        assert!(guard.is_blanket_suppressed("port-A"));
    }
}
