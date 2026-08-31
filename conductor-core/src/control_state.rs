// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Physical control state store (ADR-025).
//!
//! Tracks the last-known physical state of hardware MIDI controls per
//! `(device, channel, kind, number)`. Observation happens on raw ingress,
//! before any `MidiTransform` — so the store reflects what the user
//! *actually did to the hardware*, not the transformed logical stream.
//!
//! State is:
//! - **Device-keyed.** Not scoped by profile or mode. Hardware reality is
//!   global; software scope (e.g. "last PC *while in Edit mode*") is a
//!   separate concern deferred to a future ADR.
//! - **In-memory only.** Cleared on daemon restart — users re-stomp.
//! - **Lazy-evicted for `NoteHeld` TTL.** The default TTL covers runaway
//!   notes (stuck keys, disconnected devices) without racing the user.
//!   PC/CC/PB/aftertouch persist indefinitely as "last-known values".
//!
//! See `docs/adrs/ADR-025-control-state-routing.md` for design rationale,
//! and `docs/pc-context-routing/implementation-spec.md` for the full
//! file-by-file plan.

use dashmap::DashMap;
use std::time::{Duration, Instant};

use crate::event_processor::MidiEvent;
use crate::events::InputEvent;

/// Concrete device identifier, resolved from a `DeviceRef` at config load
/// time via the ADR-022 binding resolver.
pub type DeviceId = String;

/// Surface type used in config/conditions before binding resolution.
///
/// Resolved against the current `BindingRegistry` at config compile time;
/// unknown refs in condition-bearing actions are load-time errors (per
/// ADR-025 D5 v3).
pub type DeviceRef = String;

/// Keys into the physical state store.
///
/// Four lexical axes: device, channel, kind, and number. `PitchBend` and
/// `ChannelAftertouch` are per-channel and omit the number axis.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub enum StateKey {
    ProgramChange {
        device: DeviceId,
        channel: u8,
    },
    ControlChange {
        device: DeviceId,
        channel: u8,
        cc: u8,
    },
    NoteHeld {
        device: DeviceId,
        channel: u8,
        note: u8,
    },
    PolyAftertouch {
        device: DeviceId,
        channel: u8,
        note: u8,
    },
    ChannelAftertouch {
        device: DeviceId,
        channel: u8,
    },
    PitchBend {
        device: DeviceId,
        channel: u8,
    },
}

impl StateKey {
    /// Return the device this key belongs to (zero-copy borrow).
    pub fn device(&self) -> &str {
        match self {
            StateKey::ProgramChange { device, .. }
            | StateKey::ControlChange { device, .. }
            | StateKey::NoteHeld { device, .. }
            | StateKey::PolyAftertouch { device, .. }
            | StateKey::ChannelAftertouch { device, .. }
            | StateKey::PitchBend { device, .. } => device,
        }
    }

    /// Return the channel this key is scoped to.
    pub fn channel(&self) -> u8 {
        match self {
            StateKey::ProgramChange { channel, .. }
            | StateKey::ControlChange { channel, .. }
            | StateKey::NoteHeld { channel, .. }
            | StateKey::PolyAftertouch { channel, .. }
            | StateKey::ChannelAftertouch { channel, .. }
            | StateKey::PitchBend { channel, .. } => *channel,
        }
    }
}

/// Live value at a given [`StateKey`].
///
/// `value` covers the full MIDI range: 0-127 for PC/CC/note velocity and
/// aftertouch, 0-16383 for pitch bend.
#[derive(Clone, Copy, Debug)]
pub struct ControlValue {
    pub value: u16,
    pub updated_at: Instant,
}

impl ControlValue {
    pub fn new(value: u16) -> Self {
        Self {
            value,
            updated_at: Instant::now(),
        }
    }
}

/// Scope of a state reset operation.
///
/// Mirrors MIDI's channel-mode messages (CC 120/121/123) plus a
/// device-wide `DropDevice` variant used on hot-unplug and for manual
/// reset via the `conductor_reset_control_state` MCP tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetScope {
    /// CC 120 All Sound Off — clears `NoteHeld` and `PolyAftertouch`.
    AllSoundOff,
    /// CC 121 Reset All Controllers — clears `ControlChange`, resets
    /// `PitchBend` to centre (8192), clears `ChannelAftertouch`.
    ResetAllControllers,
    /// CC 123 All Notes Off — clears `NoteHeld` only.
    AllNotesOff,
    /// Device disconnect / manual full reset — clears everything for
    /// this device on this channel (caller picks channel or uses
    /// `drop_device` for all channels).
    DropDevice,
}

/// Physical control state store.
///
/// One instance per daemon. Shared via `Arc<_>` to the ingress dispatcher
/// (write path) and the condition evaluator (read path).
pub struct PhysicalControlStateStore {
    entries: DashMap<StateKey, ControlValue>,
    default_ttl: Duration,
}

impl PhysicalControlStateStore {
    /// Construct an empty store. `default_ttl_ms` applies to `NoteHeld`
    /// entries only — PC/CC/PB/aftertouch persist as "last-known values".
    pub fn new(default_ttl_ms: u64) -> Self {
        Self {
            entries: DashMap::new(),
            default_ttl: Duration::from_millis(default_ttl_ms),
        }
    }

    /// Ingress observation. Called once per raw event, *before* any
    /// `MidiTransform`.
    ///
    /// Writes all relevant state transitions:
    /// - `NoteOn { velocity: 0 }` is treated as `NoteOff` (clears `NoteHeld`).
    /// - `NoteOn { velocity > 0 }` inserts/refreshes `NoteHeld`.
    /// - `NoteOff` clears `NoteHeld`.
    /// - `ControlChange` for CC 120/121/123 triggers the corresponding
    ///   [`ResetScope`]; other CCs update `ControlChange`.
    /// - `ProgramChange`, `PitchBend`, `Aftertouch`, `PolyPressure`
    ///   update their respective keys.
    pub fn observe_event(&self, device_id: &str, event: &MidiEvent) {
        match event {
            MidiEvent::NoteOn {
                note,
                velocity,
                channel,
                ..
            } => {
                if *velocity == 0 {
                    // Running-status Note Off convention.
                    self.remove_key(&StateKey::NoteHeld {
                        device: device_id.to_string(),
                        channel: *channel,
                        note: *note,
                    });
                } else {
                    self.put(
                        StateKey::NoteHeld {
                            device: device_id.to_string(),
                            channel: *channel,
                            note: *note,
                        },
                        u16::from(*velocity),
                    );
                }
            }
            MidiEvent::NoteOff { note, channel, .. } => {
                self.remove_key(&StateKey::NoteHeld {
                    device: device_id.to_string(),
                    channel: *channel,
                    note: *note,
                });
            }
            MidiEvent::ControlChange {
                cc, value, channel, ..
            } => match *cc {
                120 => self.clear_channel(device_id, *channel, ResetScope::AllSoundOff),
                121 => self.clear_channel(device_id, *channel, ResetScope::ResetAllControllers),
                123 => self.clear_channel(device_id, *channel, ResetScope::AllNotesOff),
                _ => self.put(
                    StateKey::ControlChange {
                        device: device_id.to_string(),
                        channel: *channel,
                        cc: *cc,
                    },
                    u16::from(*value),
                ),
            },
            MidiEvent::ProgramChange {
                program, channel, ..
            } => {
                self.put(
                    StateKey::ProgramChange {
                        device: device_id.to_string(),
                        channel: *channel,
                    },
                    u16::from(*program),
                );
            }
            MidiEvent::PitchBend { value, channel, .. } => {
                self.put(
                    StateKey::PitchBend {
                        device: device_id.to_string(),
                        channel: *channel,
                    },
                    *value,
                );
            }
            MidiEvent::Aftertouch {
                pressure, channel, ..
            } => {
                self.put(
                    StateKey::ChannelAftertouch {
                        device: device_id.to_string(),
                        channel: *channel,
                    },
                    u16::from(*pressure),
                );
            }
            MidiEvent::PolyPressure {
                note,
                pressure,
                channel,
                ..
            } => {
                self.put(
                    StateKey::PolyAftertouch {
                        device: device_id.to_string(),
                        channel: *channel,
                        note: *note,
                    },
                    u16::from(*pressure),
                );
            }
        }
    }

    /// Ingress observation driven by the protocol-agnostic [`InputEvent`]
    /// form — which is what the daemon's `process_input_event` /
    /// `process_device_event` paths actually receive. Events whose channel
    /// is `None` (gamepad / non-MIDI sources) are ignored: the store is
    /// MIDI-only. Raw-byte InputEvents are not observed — they represent
    /// transparently-passed SysEx/etc. with no state semantics.
    pub fn observe_input_event(&self, device_id: &str, event: &InputEvent) {
        match event {
            InputEvent::PadPressed {
                pad,
                velocity,
                channel: Some(channel),
                ..
            } => {
                if *velocity == 0 {
                    self.remove_key(&StateKey::NoteHeld {
                        device: device_id.to_string(),
                        channel: *channel,
                        note: *pad,
                    });
                } else {
                    self.put(
                        StateKey::NoteHeld {
                            device: device_id.to_string(),
                            channel: *channel,
                            note: *pad,
                        },
                        u16::from(*velocity),
                    );
                }
            }
            InputEvent::PadReleased {
                pad,
                channel: Some(channel),
                ..
            } => {
                self.remove_key(&StateKey::NoteHeld {
                    device: device_id.to_string(),
                    channel: *channel,
                    note: *pad,
                });
            }
            InputEvent::ControlChange {
                control,
                value,
                channel: Some(channel),
                ..
            } => match *control {
                120 => self.clear_channel(device_id, *channel, ResetScope::AllSoundOff),
                121 => self.clear_channel(device_id, *channel, ResetScope::ResetAllControllers),
                123 => self.clear_channel(device_id, *channel, ResetScope::AllNotesOff),
                _ => self.put(
                    StateKey::ControlChange {
                        device: device_id.to_string(),
                        channel: *channel,
                        cc: *control,
                    },
                    u16::from(*value),
                ),
            },
            InputEvent::ProgramChange {
                program,
                channel: Some(channel),
                ..
            } => {
                self.put(
                    StateKey::ProgramChange {
                        device: device_id.to_string(),
                        channel: *channel,
                    },
                    u16::from(*program),
                );
            }
            InputEvent::PitchBend {
                value,
                channel: Some(channel),
                ..
            } => {
                self.put(
                    StateKey::PitchBend {
                        device: device_id.to_string(),
                        channel: *channel,
                    },
                    *value,
                );
            }
            InputEvent::Aftertouch {
                pressure,
                channel: Some(channel),
                ..
            } => {
                self.put(
                    StateKey::ChannelAftertouch {
                        device: device_id.to_string(),
                        channel: *channel,
                    },
                    u16::from(*pressure),
                );
            }
            InputEvent::PolyPressure {
                pad,
                pressure,
                channel: Some(channel),
                ..
            } => {
                self.put(
                    StateKey::PolyAftertouch {
                        device: device_id.to_string(),
                        channel: *channel,
                        note: *pad,
                    },
                    u16::from(*pressure),
                );
            }
            // Non-MIDI (channel = None) or non-observable variants.
            _ => {}
        }
    }

    /// Read a value. Returns `None` if absent or expired.
    ///
    /// For `NoteHeld` keys, applies lazy TTL eviction: if the entry is
    /// older than `default_ttl`, remove it (with a re-check under the
    /// write lock to avoid deadlocks and clobbering concurrent refreshes)
    /// and return `None`.
    pub fn get(&self, key: &StateKey) -> Option<ControlValue> {
        // First, read-only snapshot (drops guard immediately).
        let snapshot = *self.entries.get(key)?.value();

        if matches!(key, StateKey::NoteHeld { .. })
            && snapshot.updated_at.elapsed() > self.default_ttl
        {
            // Conditional removal: only drop if the entry is *still*
            // expired — if someone refreshed it between our read and the
            // write-side check, leave it.
            self.entries
                .remove_if(key, |_, v| v.updated_at.elapsed() > self.default_ttl);
            // Either we removed it, or someone concurrently refreshed:
            // re-read under the read lock and return that (or None).
            return self.entries.get(key).map(|v| *v.value());
        }

        Some(snapshot)
    }

    /// ADR-025 Phase 3.F helper (#886): check whether a `ProgramChange`
    /// entry exists for `(device, channel)`. Centralises the
    /// presence-check logic so callers don't rebuild the `StateKey`
    /// shape at every site.
    ///
    /// **Allocation note:** this method still allocates a `String`
    /// internally to construct the `StateKey` for DashMap's `&K` lookup
    /// contract — `contains_key` requires a `&StateKey`, not a borrowed
    /// decomposition of the key's fields. The only thing centralising
    /// the call here wins is consistency (one place to maintain the
    /// key shape) and a single site to optimise in future (e.g.
    /// migrating to a map shape that supports `Borrow`-based lookup or
    /// hashing the `(&str, u8)` tuple directly). Actual allocation
    /// elimination would require a deeper refactor of the store's
    /// internals, out of scope for this helper.
    ///
    /// No TTL check needed — `ProgramChange` entries have no TTL
    /// (`NoteHeld` is the only TTL-sensitive kind).
    pub fn has_program_change(&self, device: &str, channel: u8) -> bool {
        self.entries.contains_key(&StateKey::ProgramChange {
            device: device.to_string(),
            channel,
        })
    }

    /// Channel-scoped reset. Mirrors the MIDI channel-mode messages and
    /// is also reachable via `conductor_reset_control_state` (store-only,
    /// does not emit MIDI).
    pub fn clear_channel(&self, device_id: &str, channel: u8, scope: ResetScope) {
        self.entries.retain(|key, _value| {
            // Retain everything not on this device/channel.
            if key.device() != device_id || key.channel() != channel {
                return true;
            }
            match scope {
                ResetScope::AllSoundOff => !matches!(
                    key,
                    StateKey::NoteHeld { .. } | StateKey::PolyAftertouch { .. }
                ),
                ResetScope::ResetAllControllers => !matches!(
                    key,
                    StateKey::ControlChange { .. }
                        | StateKey::PitchBend { .. }
                        | StateKey::ChannelAftertouch { .. }
                ),
                ResetScope::AllNotesOff => !matches!(key, StateKey::NoteHeld { .. }),
                ResetScope::DropDevice => false,
            }
        });

        // CC 121 also centres pitch bend and zeroes channel aftertouch
        // (ADR-025 §5.3 — explicit "known last state after reset" rather
        // than "unknown", so conditions can match `ChannelAftertouch = 0`).
        if matches!(scope, ResetScope::ResetAllControllers) {
            self.put(
                StateKey::PitchBend {
                    device: device_id.to_string(),
                    channel,
                },
                8192, // MIDI centre
            );
            self.put(
                StateKey::ChannelAftertouch {
                    device: device_id.to_string(),
                    channel,
                },
                0,
            );
        }
    }

    /// Drop every entry belonging to `device_id`. Called on hot-unplug
    /// and by the reset MCP tool.
    pub fn drop_device(&self, device_id: &str) {
        self.entries.retain(|key, _| key.device() != device_id);
    }

    /// Diagnostics snapshot. If `filter` is `Some`, returns only entries
    /// for that device.
    ///
    /// Expired `NoteHeld` entries are filtered out so callers (MCP, CLI,
    /// GUI) see the same logical view as `get()` — otherwise an LLM
    /// reading `conductor_get_control_state` would see stuck-note
    /// artefacts that a condition evaluator treats as gone. The expired
    /// entries are *not* removed from the backing map here (snapshot is
    /// read-only); physical removal happens via `get()`'s lazy eviction
    /// or the periodic `sweep_expired()` task.
    pub fn snapshot(&self, filter: Option<&str>) -> Vec<(StateKey, ControlValue)> {
        let ttl = self.default_ttl;
        self.entries
            .iter()
            .filter(|entry| match filter {
                Some(dev) => entry.key().device() == dev,
                None => true,
            })
            .filter(|entry| {
                !(matches!(entry.key(), StateKey::NoteHeld { .. })
                    && entry.value().updated_at.elapsed() > ttl)
            })
            .map(|entry| (entry.key().clone(), *entry.value()))
            .collect()
    }

    /// Periodic eviction sweep. Called by a background task; safe to
    /// call from any thread.
    pub fn sweep_expired(&self) {
        let ttl = self.default_ttl;
        self.entries.retain(|key, value| {
            !(matches!(key, StateKey::NoteHeld { .. }) && value.updated_at.elapsed() > ttl)
        });
    }

    fn put(&self, key: StateKey, value: u16) {
        self.entries.insert(key, ControlValue::new(value));
    }

    fn remove_key(&self, key: &StateKey) {
        self.entries.remove(key);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

impl Default for PhysicalControlStateStore {
    /// 30-second default TTL — chosen to cover runaway notes (stuck
    /// keys, disconnected devices) without fighting the user on normal
    /// sustained notes.
    fn default() -> Self {
        Self::new(30_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn pc(program: u8, channel: u8) -> MidiEvent {
        MidiEvent::ProgramChange {
            program,
            channel,
            time: Instant::now(),
        }
    }

    fn cc(cc: u8, value: u8, channel: u8) -> MidiEvent {
        MidiEvent::ControlChange {
            cc,
            value,
            channel,
            time: Instant::now(),
        }
    }

    fn note_on(note: u8, velocity: u8, channel: u8) -> MidiEvent {
        MidiEvent::NoteOn {
            note,
            velocity,
            channel,
            time: Instant::now(),
        }
    }

    fn note_off(note: u8, channel: u8) -> MidiEvent {
        MidiEvent::NoteOff {
            note,
            channel,
            time: Instant::now(),
        }
    }

    fn pb(value: u16, channel: u8) -> MidiEvent {
        MidiEvent::PitchBend {
            value,
            channel,
            time: Instant::now(),
        }
    }

    fn chan_at(pressure: u8, channel: u8) -> MidiEvent {
        MidiEvent::Aftertouch {
            pressure,
            channel,
            time: Instant::now(),
        }
    }

    fn poly_at(note: u8, pressure: u8, channel: u8) -> MidiEvent {
        MidiEvent::PolyPressure {
            note,
            pressure,
            channel,
            time: Instant::now(),
        }
    }

    #[test]
    fn observe_event_stores_pc() {
        let store = PhysicalControlStateStore::default();
        store.observe_event("fcb1010", &pc(12, 0));

        let value = store
            .get(&StateKey::ProgramChange {
                device: "fcb1010".into(),
                channel: 0,
            })
            .expect("pc should be stored");
        assert_eq!(value.value, 12);
    }

    #[test]
    fn observe_event_per_channel_isolation() {
        let store = PhysicalControlStateStore::default();
        store.observe_event("fcb1010", &pc(12, 1));

        assert!(
            store
                .get(&StateKey::ProgramChange {
                    device: "fcb1010".into(),
                    channel: 5
                })
                .is_none()
        );
        assert_eq!(
            store
                .get(&StateKey::ProgramChange {
                    device: "fcb1010".into(),
                    channel: 1
                })
                .unwrap()
                .value,
            12
        );
    }

    #[test]
    fn observe_event_per_device_isolation() {
        let store = PhysicalControlStateStore::default();
        store.observe_event("fcb1010", &pc(12, 0));
        store.observe_event("mc6", &pc(42, 0));

        assert_eq!(
            store
                .get(&StateKey::ProgramChange {
                    device: "fcb1010".into(),
                    channel: 0
                })
                .unwrap()
                .value,
            12
        );
        assert_eq!(
            store
                .get(&StateKey::ProgramChange {
                    device: "mc6".into(),
                    channel: 0
                })
                .unwrap()
                .value,
            42
        );
    }

    #[test]
    fn note_on_vel_zero_treated_as_note_off() {
        let store = PhysicalControlStateStore::default();
        store.observe_event("k1", &note_on(60, 100, 0));
        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_some()
        );

        store.observe_event("k1", &note_on(60, 0, 0)); // Running-status off
        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
    }

    #[test]
    fn note_held_cleared_on_note_off() {
        let store = PhysicalControlStateStore::default();
        store.observe_event("k1", &note_on(60, 100, 0));
        store.observe_event("k1", &note_off(60, 0));

        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
    }

    #[test]
    fn note_held_expires_after_ttl() {
        let store = PhysicalControlStateStore::new(10); // 10ms TTL
        store.observe_event("k1", &note_on(60, 100, 0));
        thread::sleep(Duration::from_millis(25));

        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
    }

    #[test]
    fn note_held_expired_read_returns_none_and_removes_entry() {
        let store = PhysicalControlStateStore::new(10);
        store.observe_event("k1", &note_on(60, 100, 0));
        assert_eq!(store.len(), 1);

        thread::sleep(Duration::from_millis(25));
        // First read expires-and-removes
        let _ = store.get(&StateKey::NoteHeld {
            device: "k1".into(),
            channel: 0,
            note: 60,
        });
        assert_eq!(store.len(), 0, "expired entry should be removed on read");
    }

    #[test]
    fn cc120_clears_note_held_and_poly_aftertouch() {
        let store = PhysicalControlStateStore::default();
        store.observe_event("k1", &note_on(60, 100, 0));
        store.observe_event("k1", &poly_at(60, 90, 0));
        store.observe_event("k1", &cc(7, 100, 0)); // non-reset CC, should survive

        store.observe_event("k1", &cc(120, 0, 0)); // All Sound Off

        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
        assert!(
            store
                .get(&StateKey::PolyAftertouch {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
        assert!(
            store
                .get(&StateKey::ControlChange {
                    device: "k1".into(),
                    channel: 0,
                    cc: 7
                })
                .is_some()
        );
    }

    #[test]
    fn cc121_clears_cc_and_resets_pitchbend_centre() {
        // Per ADR-025: CC121 clears CC, resets PitchBend to centre (8192),
        // and resets ChannelAftertouch to 0 (explicit zero, not absent —
        // the semantic is "known last state after reset", not "unknown").
        let store = PhysicalControlStateStore::default();
        store.observe_event("k1", &cc(7, 100, 0));
        store.observe_event("k1", &pb(4096, 0));
        store.observe_event("k1", &chan_at(80, 0));
        store.observe_event("k1", &note_on(60, 100, 0)); // survives

        store.observe_event("k1", &cc(121, 0, 0)); // Reset All Controllers

        assert!(
            store
                .get(&StateKey::ControlChange {
                    device: "k1".into(),
                    channel: 0,
                    cc: 7
                })
                .is_none()
        );
        let pb_after = store
            .get(&StateKey::PitchBend {
                device: "k1".into(),
                channel: 0,
            })
            .expect("pitch bend should be reset to centre");
        assert_eq!(pb_after.value, 8192);
        let at_after = store
            .get(&StateKey::ChannelAftertouch {
                device: "k1".into(),
                channel: 0,
            })
            .expect("channel aftertouch should be zeroed, not absent (ADR-025)");
        assert_eq!(at_after.value, 0);
        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_some()
        );
    }

    #[test]
    fn cc123_clears_note_held_only() {
        let store = PhysicalControlStateStore::default();
        store.observe_event("k1", &note_on(60, 100, 0));
        store.observe_event("k1", &cc(7, 100, 0));
        store.observe_event("k1", &pb(4096, 0));

        store.observe_event("k1", &cc(123, 0, 0)); // All Notes Off

        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
        assert!(
            store
                .get(&StateKey::ControlChange {
                    device: "k1".into(),
                    channel: 0,
                    cc: 7
                })
                .is_some()
        );
        assert!(
            store
                .get(&StateKey::PitchBend {
                    device: "k1".into(),
                    channel: 0
                })
                .is_some()
        );
    }

    #[test]
    fn drop_device_clears_all_device_state_preserves_others() {
        let store = PhysicalControlStateStore::default();
        store.observe_event("fcb1010", &pc(12, 0));
        store.observe_event("fcb1010", &cc(7, 100, 0));
        store.observe_event("mc6", &pc(42, 0));

        store.drop_device("fcb1010");

        assert!(
            store
                .get(&StateKey::ProgramChange {
                    device: "fcb1010".into(),
                    channel: 0
                })
                .is_none()
        );
        assert!(
            store
                .get(&StateKey::ControlChange {
                    device: "fcb1010".into(),
                    channel: 0,
                    cc: 7
                })
                .is_none()
        );
        assert_eq!(
            store
                .get(&StateKey::ProgramChange {
                    device: "mc6".into(),
                    channel: 0
                })
                .unwrap()
                .value,
            42
        );
    }

    #[test]
    fn pc_and_cc_persist_without_ttl() {
        let store = PhysicalControlStateStore::new(10); // 10ms NoteHeld TTL
        store.observe_event("k1", &pc(12, 0));
        store.observe_event("k1", &cc(7, 100, 0));
        thread::sleep(Duration::from_millis(25));

        assert_eq!(
            store
                .get(&StateKey::ProgramChange {
                    device: "k1".into(),
                    channel: 0
                })
                .unwrap()
                .value,
            12
        );
        assert_eq!(
            store
                .get(&StateKey::ControlChange {
                    device: "k1".into(),
                    channel: 0,
                    cc: 7
                })
                .unwrap()
                .value,
            100
        );
    }

    #[test]
    fn snapshot_filters_by_device() {
        let store = PhysicalControlStateStore::default();
        store.observe_event("fcb1010", &pc(12, 0));
        store.observe_event("mc6", &pc(42, 0));

        let all = store.snapshot(None);
        assert_eq!(all.len(), 2);

        let only_fcb = store.snapshot(Some("fcb1010"));
        assert_eq!(only_fcb.len(), 1);
        assert_eq!(only_fcb[0].0.device(), "fcb1010");
    }

    #[test]
    fn snapshot_hides_expired_note_held_to_match_get() {
        // MCP consumers of the snapshot read it as "hardware reality
        // right now." If get() would treat a NoteHeld as expired,
        // snapshot must not surface it either — otherwise an LLM
        // asking \"is this note held?\" via snapshot would get a
        // different answer than a condition evaluator reading via
        // get(). Non-TTL entries (PC/CC/PB/AT) always stick around.
        let store = PhysicalControlStateStore::new(10); // 10ms TTL
        store.observe_event("k1", &note_on(60, 100, 0));
        store.observe_event("k1", &pc(12, 0));
        thread::sleep(Duration::from_millis(25));

        let entries = store.snapshot(None);
        let kinds: Vec<_> = entries.iter().map(|(k, _)| k).collect();
        assert!(
            !kinds.iter().any(|k| matches!(k, StateKey::NoteHeld { .. })),
            "expired NoteHeld should not appear in snapshot, got: {:?}",
            kinds
        );
        assert!(
            kinds
                .iter()
                .any(|k| matches!(k, StateKey::ProgramChange { .. })),
            "non-TTL PC must still be in snapshot, got: {:?}",
            kinds
        );
    }

    #[test]
    fn sweep_expired_removes_only_stale_note_held() {
        let store = PhysicalControlStateStore::new(10);
        store.observe_event("k1", &note_on(60, 100, 0));
        store.observe_event("k1", &pc(12, 0));

        thread::sleep(Duration::from_millis(25));
        store.sweep_expired();

        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
        assert!(
            store
                .get(&StateKey::ProgramChange {
                    device: "k1".into(),
                    channel: 0
                })
                .is_some()
        );
    }

    // ────────────────────────────────────────────────────────────
    // observe_input_event parity
    // ────────────────────────────────────────────────────────────

    fn ie_pc(program: u8, channel: u8) -> InputEvent {
        InputEvent::ProgramChange {
            program,
            channel: Some(channel),
            time: Instant::now(),
        }
    }

    fn ie_cc(control: u8, value: u8, channel: u8) -> InputEvent {
        InputEvent::ControlChange {
            control,
            value,
            channel: Some(channel),
            time: Instant::now(),
        }
    }

    fn ie_note_on(pad: u8, velocity: u8, channel: u8) -> InputEvent {
        InputEvent::PadPressed {
            pad,
            velocity,
            channel: Some(channel),
            time: Instant::now(),
        }
    }

    fn ie_note_off(pad: u8, channel: u8) -> InputEvent {
        InputEvent::PadReleased {
            pad,
            channel: Some(channel),
            time: Instant::now(),
        }
    }

    fn ie_pc_no_channel() -> InputEvent {
        InputEvent::ProgramChange {
            program: 12,
            channel: None,
            time: Instant::now(),
        }
    }

    #[test]
    fn observe_input_event_stores_pc() {
        let store = PhysicalControlStateStore::default();
        store.observe_input_event("fcb1010", &ie_pc(12, 0));
        assert_eq!(
            store
                .get(&StateKey::ProgramChange {
                    device: "fcb1010".into(),
                    channel: 0
                })
                .unwrap()
                .value,
            12,
        );
    }

    #[test]
    fn observe_input_event_ignores_none_channel() {
        // Non-MIDI source (gamepad) should be dropped silently.
        let store = PhysicalControlStateStore::default();
        store.observe_input_event("fcb1010", &ie_pc_no_channel());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn observe_input_event_cc120_clears_notes() {
        let store = PhysicalControlStateStore::default();
        store.observe_input_event("k1", &ie_note_on(60, 100, 0));
        store.observe_input_event("k1", &ie_cc(7, 100, 0));
        store.observe_input_event("k1", &ie_cc(120, 0, 0));

        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
        assert!(
            store
                .get(&StateKey::ControlChange {
                    device: "k1".into(),
                    channel: 0,
                    cc: 7
                })
                .is_some()
        );
    }

    #[test]
    fn observe_input_event_note_off_clears_held() {
        let store = PhysicalControlStateStore::default();
        store.observe_input_event("k1", &ie_note_on(60, 100, 0));
        store.observe_input_event("k1", &ie_note_off(60, 0));
        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
    }

    #[test]
    fn observe_input_event_note_on_vel_zero_clears_held() {
        let store = PhysicalControlStateStore::default();
        store.observe_input_event("k1", &ie_note_on(60, 100, 0));
        store.observe_input_event("k1", &ie_note_on(60, 0, 0));
        assert!(
            store
                .get(&StateKey::NoteHeld {
                    device: "k1".into(),
                    channel: 0,
                    note: 60
                })
                .is_none()
        );
    }

    #[test]
    fn concurrent_read_with_eviction_no_deadlock() {
        use std::sync::Arc;
        let store = Arc::new(PhysicalControlStateStore::new(5));

        let mut handles = vec![];
        for t in 0..8 {
            let s = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                for i in 0..10_000 {
                    let note = ((t * 10_000 + i) % 128) as u8;
                    if i % 2 == 0 {
                        s.observe_event("k1", &note_on(note, 100, 0));
                    } else {
                        let _ = s.get(&StateKey::NoteHeld {
                            device: "k1".into(),
                            channel: 0,
                            note,
                        });
                    }
                }
            }));
        }
        for h in handles {
            h.join().expect("no thread should panic or deadlock");
        }
    }
}
