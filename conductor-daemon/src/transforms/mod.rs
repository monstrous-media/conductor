// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Cross-protocol signal transforms (ADR-031 § 7 / Phase 5).
//!
//! Each submodule implements one variant of `SignalTransform` from
//! `conductor_core::config::types`. Today only the `Midi` variant has
//! a working runtime — see `conductor_core::transform::MidiTransform`
//! (lives in core because MIDI→MIDI is the foundational case).
//!
//! Cross-protocol variants (`MidiToOsc`, `OscToMidi`, `OscToArtNet`,
//! `MidiToArtNet`, `HidToArtNet`, `HidToMidi`, `HidToOsc`) live here in the
//! daemon crate because:
//! - They need protocol-specific encoder dependencies (`rosc` for OSC,
//!   raw byte construction for Art-Net) that `conductor-core` keeps
//!   out of its strict dependency surface.
//! - The runtime senders live in the daemon's `connector_registry` /
//!   output dispatch, so co-locating the transforms in the same crate
//!   avoids cross-crate test fixture duplication.
//!
//! `RouteEngine::compile()` admits each transform once its runtime ships and
//! EXCLUDES the rest with `ExclusionReason::CrossProtocolTransformUnsupported`.
//! All eight `SignalTransform` variants are now admitted: `Midi`, `MidiToOsc`,
//! `OscToMidi` (ADR-039-A Slice 1, #1361), `OscToArtNet` (ADR-039-A Slice 1b,
//! #2324), `MidiToArtNet`, `HidToArtNet`, `HidToMidi`, `HidToOsc`. The
//! structured HID/OSC transforms read the original event threaded via
//! `RouteEvalContext` (ADR-039-B §6.2.1).

pub mod hid_to_artnet;
pub mod hid_to_midi;
pub mod hid_to_osc;
pub mod hid_trigger;
pub mod midi_to_artnet;
pub mod midi_to_osc;
pub mod osc_to_artnet;
pub mod osc_to_midi;
