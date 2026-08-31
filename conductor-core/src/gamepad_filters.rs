// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Gamepad input-stream quality filters (#599).
//!
//! Containment for macOS gilrs backend quirks observed on the Xbox Wireless
//! Controller over Bluetooth (see PR #2337 verification evidence):
//!
//! - [`AxisZeroFilter`] — duplicate HID elements sharing an axis usage emit a
//!   spurious `0.0` twin alongside every real reading.
//! - [`TriggerNoiseGate`] — idle analog triggers chatter (~0.05 every 1-3s),
//!   each reading quantising to 0 below the deadzone.
//!
//! Both filters make an immediate, synchronous keep/drop decision per
//! reading — nothing on the non-zero signal path is ever buffered or
//! deferred, so they add **zero latency** to live input. Per-event cost is a
//! fixed-array index plus an `Instant` compare (single-digit nanoseconds).
//! The real fix is upstream (gilrs/objc2-io-kit duplicate-element handling);
//! these normalize the stream at the device boundary until then.

use std::time::{Duration, Instant};

/// Suppression window for [`AxisZeroFilter`].
///
/// Observed spurious twins arrive within the same millisecond as their real
/// reading; genuine recenter zeros arrive ≥15ms after the last non-zero
/// (Bluetooth HID report cadence). 4ms sits between with margin on both
/// sides. A held zero is released as a synthetic recenter after this window
/// (see [`AxisZeroFilter::due_recenters`]), so even hardware that recenters
/// faster than the window only ever sees the recenter **delayed** by up to
/// 4ms — never lost.
pub const AXIS_ZERO_SUPPRESS_WINDOW: Duration = Duration::from_millis(4);

/// Fixed slot order for the axes the filters track.
///
/// Index = slot used by [`AxisZeroFilter`]; unknown axes are never tracked
/// (their readings always pass).
const SLOT_AXES: [gilrs::Axis; 6] = [
    gilrs::Axis::LeftStickX,
    gilrs::Axis::LeftStickY,
    gilrs::Axis::RightStickX,
    gilrs::Axis::RightStickY,
    gilrs::Axis::LeftZ,
    gilrs::Axis::RightZ,
];

/// Inline hold/release filter for spurious zero axis readings (#599).
///
/// The macOS gilrs backend (objc2-io-kit) exposes duplicate HID elements
/// sharing an axis usage on some controllers (observed: Xbox Wireless
/// Controller over Bluetooth): each real `AxisChanged(axis, v)` reading is
/// accompanied — within the same millisecond — by a spurious
/// `AxisChanged(axis, 0.0)` from the dead twin element. Forwarding both makes
/// the stream bounce value→0→value: value bars flicker to center and encoder
/// direction detection flaps CW/CCW on every update.
///
/// Semantics per reading (all decisions immediate — no buffering of non-zero
/// signal, zero added latency):
/// - **non-zero**: always forwarded; cancels any held zero on that axis
///   (that zero was the twin).
/// - **`0.0` within [`AXIS_ZERO_SUPPRESS_WINDOW`] of the last non-zero on the
///   same axis**: *held*, not forwarded — indistinguishable from a twin at
///   this point. If no non-zero follows before the window expires, the poll
///   loop's [`due_recenters`](Self::due_recenters) call releases it as a
///   genuine recenter (worst-case recenter delay: the window, 4ms — inside
///   the 8.3ms gamepad polling budget). This covers high-rate hardware whose
///   genuine flick-to-center lands inside the window (cloud-review finding,
///   PR #2337).
/// - **`0.0` outside the window** (or with no prior non-zero): forwarded
///   immediately — the normal recenter path on observed BT cadence (≥15ms).
///
/// Time-windowed rather than per-poll-batch on purpose: the daemon polls at
/// 1ms, so a real reading and its same-millisecond twin can straddle two
/// drains — a batch-scoped filter misses that split.
#[derive(Debug, Default)]
pub struct AxisZeroFilter {
    /// Most recent non-zero reading time per tracked axis.
    last_nonzero: [Option<Instant>; 6],
    /// A `0.0` reading held inside the suppression window, awaiting either
    /// cancellation (a newer non-zero → it was the twin) or release via
    /// [`due_recenters`](Self::due_recenters) (genuine recenter).
    pending_zero: [bool; 6],
}

impl AxisZeroFilter {
    /// Create an empty filter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fixed slot per tracked axis — `None` for axes we don't map (always kept).
    fn axis_slot(axis: gilrs::Axis) -> Option<usize> {
        SLOT_AXES.iter().position(|&a| a == axis)
    }

    /// Whether a reading for `axis` with this `value` observed at `now`
    /// should be forwarded immediately. A `false` for a zero reading means
    /// *held*, not necessarily dropped — see [`due_recenters`](Self::due_recenters).
    pub fn should_keep(&mut self, axis: gilrs::Axis, value: f32, now: Instant) -> bool {
        let Some(slot) = Self::axis_slot(axis) else {
            return true;
        };
        if value != 0.0 {
            self.last_nonzero[slot] = Some(now);
            // Any held zero was the duplicate-element twin of an earlier
            // reading — the axis is demonstrably still deflected.
            self.pending_zero[slot] = false;
            return true;
        }
        match self.last_nonzero[slot] {
            Some(t) if now.duration_since(t) < AXIS_ZERO_SUPPRESS_WINDOW => {
                self.pending_zero[slot] = true;
                false
            }
            _ => true,
        }
    }

    /// Release held zeros whose suppression window has expired with no newer
    /// non-zero reading — they were genuine recenters, not twins. Returns the
    /// axes to emit a synthetic `0.0` reading for. Call once per poll tick.
    pub fn due_recenters(&mut self, now: Instant) -> Vec<gilrs::Axis> {
        let mut due = Vec::new();
        for (slot, &axis) in SLOT_AXES.iter().enumerate() {
            if self.pending_zero[slot]
                && let Some(t) = self.last_nonzero[slot]
                && now.duration_since(t) >= AXIS_ZERO_SUPPRESS_WINDOW
            {
                self.pending_zero[slot] = false;
                due.push(axis);
            }
        }
        due
    }
}

/// Transition-aware gate for analog trigger rest noise (#599).
///
/// Idle triggers chatter (observed: ~0.05 every 1-3s on an Xbox Wireless
/// Controller), and every `ButtonChanged` reading below the deadzone
/// quantises to 0 — forwarding them all floods the event stream with
/// no-information zero events. This gate suppresses repeated quantised zeros
/// per trigger encoder while always passing:
/// - every reading whose quantised value is non-zero (active pull — the raw
///   analog keeps flowing for value bars), and
/// - the first zero after a non-zero (the release transition, so consumers
///   see the trigger return to rest).
#[derive(Debug, Default)]
pub struct TriggerNoiseGate {
    /// Last forwarded quantised value per trigger encoder [132, 133].
    last: [u8; 2],
}

impl TriggerNoiseGate {
    /// Create a gate with both triggers at rest.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a reading for `encoder` with this `quantised` MIDI value
    /// should be forwarded. Non-trigger encoders always pass.
    pub fn should_emit(&mut self, encoder: u8, quantised: u8) -> bool {
        let idx = match encoder {
            crate::gamepad_events::encoder_ids::LEFT_TRIGGER => 0,
            crate::gamepad_events::encoder_ids::RIGHT_TRIGGER => 1,
            _ => return true,
        };
        let changed = self.last[idx] != quantised;
        self.last[idx] = quantised;
        quantised != 0 || changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_axis_zero_filter_holds_spurious_twin_zero() {
        use gilrs::Axis::*;

        let mut f = AxisZeroFilter::new();
        let t0 = Instant::now();

        // Observed macOS pattern: real reading + same-millisecond zero twin
        assert!(f.should_keep(RightStickY, 0.0578, t0));
        assert!(
            !f.should_keep(RightStickY, 0.0, t0),
            "same-instant twin held"
        );

        // A newer non-zero cancels the held twin — it must NOT later flush
        assert!(f.should_keep(RightStickY, 0.061, t0 + Duration::from_millis(2)));
        assert!(
            f.due_recenters(t0 + Duration::from_millis(20)).is_empty(),
            "twin cancelled by newer non-zero must never flush... \
             (last_nonzero advanced, pending cleared)"
        );
    }

    #[test]
    fn test_axis_zero_filter_releases_genuine_fast_recenter() {
        use gilrs::Axis::*;

        // Cloud-review scenario (PR #2337): high-rate hardware recenters
        // within the window — the zero is held, then released as a synthetic
        // recenter when the window expires with no newer non-zero.
        let mut f = AxisZeroFilter::new();
        let t0 = Instant::now();

        assert!(f.should_keep(LeftStickY, 0.9, t0));
        assert!(
            !f.should_keep(LeftStickY, 0.0, t0 + Duration::from_millis(2)),
            "fast genuine recenter is held at first (indistinguishable from twin)"
        );
        // Window not yet expired — nothing due
        assert!(f.due_recenters(t0 + Duration::from_millis(3)).is_empty());
        // Window expired with no newer non-zero — recenter released
        assert_eq!(
            f.due_recenters(t0 + Duration::from_millis(4)),
            vec![LeftStickY],
            "held zero must be released as a recenter after the window"
        );
        // Released exactly once
        assert!(f.due_recenters(t0 + Duration::from_millis(10)).is_empty());
    }

    #[test]
    fn test_axis_zero_filter_keeps_slow_recenter_immediately() {
        use gilrs::Axis::*;

        let mut f = AxisZeroFilter::new();
        let t0 = Instant::now();

        // Normal BT cadence: final zero arrives ≥15ms after the last non-zero
        assert!(f.should_keep(LeftStickY, -0.24, t0));
        assert!(
            f.should_keep(LeftStickY, 0.0, t0 + Duration::from_millis(15)),
            "recenter outside the window passes immediately"
        );

        // A zero with no prior non-zero reading on that axis is kept
        assert!(f.should_keep(LeftStickX, 0.0, t0));

        // Zero on a DIFFERENT axis than the recent non-zero is kept
        assert!(f.should_keep(RightStickX, 0.9, t0));
        assert!(f.should_keep(RightStickY, 0.0, t0));
    }

    #[test]
    fn test_axis_zero_filter_tracks_axes_independently() {
        use gilrs::Axis::*;

        let mut f = AxisZeroFilter::new();
        let t0 = Instant::now();

        assert!(f.should_keep(LeftStickX, 0.5, t0));
        assert!(f.should_keep(LeftStickY, 0.5, t0));
        assert!(!f.should_keep(LeftStickX, 0.0, t0));
        assert!(!f.should_keep(LeftStickY, 0.0, t0));
        // Both axes' held zeros flush independently
        let mut due = f.due_recenters(t0 + Duration::from_millis(5));
        due.sort_by_key(|a| format!("{:?}", a));
        assert_eq!(due, vec![LeftStickX, LeftStickY]);
    }

    #[test]
    fn test_trigger_noise_gate_suppresses_rest_chatter() {
        let mut gate = TriggerNoiseGate::new();

        // Idle chatter: raw ~0.05 quantises to 0 — suppressed at rest
        assert!(
            !gate.should_emit(132, 0),
            "repeated zero at rest suppressed"
        );
        assert!(!gate.should_emit(132, 0));

        // Active pull: every non-zero reading flows (value bar precision)
        assert!(gate.should_emit(132, 40));
        assert!(gate.should_emit(132, 40), "unchanged non-zero still flows");
        assert!(gate.should_emit(132, 90));

        // Release: first zero after non-zero is the transition — passes once
        assert!(gate.should_emit(132, 0), "release transition must emit");
        assert!(
            !gate.should_emit(132, 0),
            "subsequent rest chatter suppressed"
        );
    }

    #[test]
    fn test_trigger_noise_gate_tracks_triggers_independently_and_passes_others() {
        let mut gate = TriggerNoiseGate::new();

        assert!(gate.should_emit(132, 50));
        // Right trigger still at rest — its zeros stay suppressed
        assert!(!gate.should_emit(133, 0));
        // Left trigger release transition unaffected by right-trigger state
        assert!(gate.should_emit(132, 0));

        // Non-trigger encoders always pass
        assert!(gate.should_emit(128, 0));
        assert!(gate.should_emit(7, 0));
    }
}
