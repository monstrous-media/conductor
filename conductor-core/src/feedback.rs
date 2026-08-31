// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use crate::config::types::{
    HidLedConfig, MidiLedConfig, MidiLedMessage, ResolvedHidLedConfig, VelocityColorMap,
};
use crate::midi_feedback::MidiFeedback;
use crate::mikro_leds::{MikroMK3LEDs, RGB};
use std::error::Error;
use tracing::{debug, error, warn};

/// Which concrete feedback implementation a [`PadFeedback`] device is.
///
/// Exposed so callers (and tests of [`create_feedback_device`]) can assert
/// *which* implementation was selected for a given device name, without
/// downcasting the `Box<dyn PadFeedback>` trait object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackDeviceKind {
    /// Novation Launchpad family (note-based LED control).
    Launchpad,
    /// Akai APC family (CC-based LED control).
    Apc,
    /// Native Instruments Maschine Mikro MK3 HID LEDs.
    Mikro,
    /// Generic HID LED feedback (config-driven).
    Hid,
    /// Generic MIDI feedback (the default fallback).
    Generic,
}

/// Unified trait for device feedback (LEDs, visual indicators)
pub trait PadFeedback: Send {
    fn connect(&mut self) -> Result<(), Box<dyn Error>>;
    fn set_pad_color(&mut self, pad: u8, color: RGB) -> Result<(), Box<dyn Error>>;
    fn set_pad_velocity(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>>;
    fn set_mode_colors(&mut self, mode: u8) -> Result<(), Box<dyn Error>>;
    fn show_velocity_feedback(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>>;
    fn flash_pad(&mut self, pad: u8, color: RGB, duration_ms: u64) -> Result<(), Box<dyn Error>>;
    fn ripple_effect(&mut self, start_pad: u8, color: RGB) -> Result<(), Box<dyn Error>>;
    fn clear_all(&mut self) -> Result<(), Box<dyn Error>>;
    fn show_long_press_feedback(&mut self, pad: u8, elapsed_ms: u128)
    -> Result<(), Box<dyn Error>>;
    fn run_scheme(&mut self, scheme: &LightingScheme) -> Result<(), Box<dyn Error>>;

    /// The concrete implementation kind. Defaults to [`FeedbackDeviceKind::Generic`];
    /// device-specific implementations override it so the factory's selection is
    /// observable (e.g. for `create_feedback_device` tests).
    fn device_kind(&self) -> FeedbackDeviceKind {
        FeedbackDeviceKind::Generic
    }
}

/// LED lighting schemes.
///
/// `Clone` but not `Copy` because the `Custom` variant contains heap-allocated
/// data (`Vec<ChaseStep>`).
#[derive(Debug, Clone, PartialEq)]
pub enum LightingScheme {
    Off,
    Static(u8), // Static color based on mode
    Breathing,  // Slow breathing effect
    Pulse,      // Fast pulse effect
    Rainbow,    // Rainbow cycle
    Wave,       // Wave pattern
    Sparkle,    // Random sparkles
    Reactive,   // React to MIDI events only
    VuMeter,    // VU meter style (bottom-up)
    Spiral,     // Spiral pattern
    /// Custom chase pattern (R668). `run_scheme` applies all steps as a static
    /// snapshot; animated sequencing using `delay_ms` should be driven by the
    /// caller (e.g., a timer in the `FeedbackManager` update loop).
    Custom(Vec<ChaseStep>),
}

/// A single step in a custom LED chase pattern (R668)
///
/// Note: `run_scheme(Custom)` applies all steps as a static snapshot (initial state).
/// Animated sequencing with per-step timing is the caller's responsibility
/// (e.g., the `FeedbackManager` update loop).
#[derive(Debug, Clone, PartialEq)]
pub struct ChaseStep {
    pub pad: u8,
    pub color: RGB,
    /// Delay in milliseconds before the next step. This is metadata for callers
    /// implementing animated chase sequences — `run_scheme` does **not** sleep
    /// or use this value. The caller (e.g., FeedbackManager's update loop)
    /// should read `delay_ms` to schedule timed transitions.
    pub delay_ms: u64,
}

impl LightingScheme {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "off" => Some(Self::Off),
            "static" => Some(Self::Static(0)),
            "breathing" => Some(Self::Breathing),
            "pulse" => Some(Self::Pulse),
            "rainbow" => Some(Self::Rainbow),
            "wave" => Some(Self::Wave),
            "sparkle" => Some(Self::Sparkle),
            "reactive" => Some(Self::Reactive),
            "vumeter" => Some(Self::VuMeter),
            "spiral" => Some(Self::Spiral),
            "custom" => Some(Self::Custom(Vec::new())),
            _ => None,
        }
    }

    pub fn list_all() -> Vec<&'static str> {
        vec![
            "off",
            "static",
            "breathing",
            "pulse",
            "rainbow",
            "wave",
            "sparkle",
            "reactive",
            "vumeter",
            "spiral",
            "custom",
        ]
    }
}

/// Implementation for Mikro MK3 (HID)
impl PadFeedback for MikroMK3LEDs {
    fn device_kind(&self) -> FeedbackDeviceKind {
        FeedbackDeviceKind::Mikro
    }

    fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        self.connect()
    }

    fn set_pad_color(&mut self, pad: u8, color: RGB) -> Result<(), Box<dyn Error>> {
        self.set_pad_color(pad, color)
    }

    fn set_pad_velocity(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        self.set_pad_velocity(pad, velocity)
    }

    fn set_mode_colors(&mut self, mode: u8) -> Result<(), Box<dyn Error>> {
        self.set_mode_colors(mode)
    }

    fn show_velocity_feedback(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        self.show_velocity_feedback(pad, velocity)
    }

    fn flash_pad(&mut self, pad: u8, _color: RGB, _duration_ms: u64) -> Result<(), Box<dyn Error>> {
        self.flash_pad(pad)
    }

    fn ripple_effect(&mut self, start_pad: u8, _color: RGB) -> Result<(), Box<dyn Error>> {
        self.ripple_effect(start_pad)
    }

    fn clear_all(&mut self) -> Result<(), Box<dyn Error>> {
        self.clear_all()
    }

    fn show_long_press_feedback(
        &mut self,
        pad: u8,
        _elapsed_ms: u128,
    ) -> Result<(), Box<dyn Error>> {
        self.show_long_press_feedback(pad)
    }

    fn run_scheme(&mut self, scheme: &LightingScheme) -> Result<(), Box<dyn Error>> {
        match scheme {
            LightingScheme::Off => self.clear_all(),
            LightingScheme::Static(mode) => self.set_mode_colors(*mode),
            LightingScheme::Breathing => self.breathing_effect(),
            LightingScheme::Pulse => self.pulse_effect(),
            LightingScheme::Rainbow => self.rainbow_effect(),
            LightingScheme::Wave => self.wave_effect(),
            LightingScheme::Sparkle => self.sparkle_effect(),
            LightingScheme::Reactive => Ok(()), // Reactive only responds to events
            LightingScheme::VuMeter => self.vumeter_effect(),
            LightingScheme::Spiral => self.spiral_effect(),
            LightingScheme::Custom(steps) => {
                for step in steps {
                    self.set_pad_color(step.pad, step.color)?;
                }
                Ok(())
            }
        }
    }
}

/// Send a MIDI LED message via the given MidiFeedback connection.
fn send_midi_led_message(
    midi: &mut MidiFeedback,
    msg: &MidiLedMessage,
    channel: u8,
) -> Result<(), Box<dyn Error>> {
    match msg {
        MidiLedMessage::NoteOn { note, velocity } => {
            midi.send_note_on(*note, *velocity, channel)?;
        }
        MidiLedMessage::NoteOff { note, velocity } => {
            midi.send_note_off_vel(*note, *velocity, channel)?;
        }
        MidiLedMessage::Cc { cc, value } => {
            midi.send_cc(*cc, *value, channel)?;
        }
    }
    Ok(())
}

/// Map generic pad index to MIDI note (pad + 36). Returns None if result > 127.
fn generic_pad_to_note(pad: u8) -> Option<u8> {
    let note = pad.checked_add(36)?;
    if note > 127 { None } else { Some(note) }
}

/// Implementation for standard MIDI devices
impl PadFeedback for MidiFeedback {
    fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        // MIDI feedback doesn't need explicit connection setup
        Ok(())
    }

    fn set_pad_color(&mut self, pad: u8, color: RGB) -> Result<(), Box<dyn Error>> {
        let channel = self.config.as_ref().map_or(1, |c| c.channel);
        let Some(note) = generic_pad_to_note(pad) else {
            return Ok(());
        };
        if color.r == 0 && color.g == 0 && color.b == 0 {
            let off_vel = self.config.as_ref().map_or(0, |c| c.note_off_velocity);
            self.send_note_off_vel(note, off_vel, channel)?;
        } else {
            // Use luminance as velocity for generic MIDI
            let velocity =
                ((color.r as f32 * 0.299 + color.g as f32 * 0.587 + color.b as f32 * 0.114).round()
                    as u8)
                    .min(127);
            self.send_note_on(note, velocity, channel)?;
        }
        Ok(())
    }

    fn set_pad_velocity(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        let channel = self.config.as_ref().map_or(1, |c| c.channel);
        let Some(note) = generic_pad_to_note(pad) else {
            return Ok(());
        };
        self.send_note_on(note, velocity, channel)?;
        Ok(())
    }

    fn set_mode_colors(&mut self, _mode: u8) -> Result<(), Box<dyn Error>> {
        // MIDI devices typically don't support multi-color LEDs
        Ok(())
    }

    fn show_velocity_feedback(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        let channel = self.config.as_ref().map_or(1, |c| c.channel);
        let Some(note) = generic_pad_to_note(pad) else {
            return Ok(());
        };
        self.send_note_on(note, velocity, channel)?;
        Ok(())
    }

    fn flash_pad(&mut self, pad: u8, _color: RGB, duration_ms: u64) -> Result<(), Box<dyn Error>> {
        let channel = self.config.as_ref().map_or(1, |c| c.channel);
        let on_vel = self.config.as_ref().map_or(127, |c| c.note_on_velocity);
        let Some(note) = generic_pad_to_note(pad) else {
            return Ok(());
        };
        // #1464: flash on, then off after the duration (was note-on only, so
        // the LED stayed lit and the requested duration was ignored).
        self.flash_pad(note, on_vel, channel, duration_ms)?;
        Ok(())
    }

    fn ripple_effect(&mut self, _start_pad: u8, _color: RGB) -> Result<(), Box<dyn Error>> {
        // Not supported for standard MIDI devices
        Ok(())
    }

    fn clear_all(&mut self) -> Result<(), Box<dyn Error>> {
        let channel = self.config.as_ref().map_or(1, |c| c.channel);
        // Send note off for all pads
        for pad in 0..16u8 {
            let Some(note) = generic_pad_to_note(pad) else {
                continue;
            };
            let off_vel = self.config.as_ref().map_or(0, |c| c.note_off_velocity);
            self.send_note_off_vel(note, off_vel, channel)?;
        }
        Ok(())
    }

    fn show_long_press_feedback(
        &mut self,
        pad: u8,
        _elapsed_ms: u128,
    ) -> Result<(), Box<dyn Error>> {
        let channel = self.config.as_ref().map_or(1, |c| c.channel);
        let on_vel = self.config.as_ref().map_or(127, |c| c.note_on_velocity);
        let Some(note) = generic_pad_to_note(pad) else {
            return Ok(());
        };
        self.send_note_on(note, on_vel, channel)?;
        Ok(())
    }

    fn run_scheme(&mut self, scheme: &LightingScheme) -> Result<(), Box<dyn Error>> {
        let channel = self.config.as_ref().map_or(1, |c| c.channel);
        match scheme {
            LightingScheme::Off => self.clear_all(),
            LightingScheme::Reactive => Ok(()), // Reactive only responds to events
            LightingScheme::Custom(steps) => {
                for step in steps {
                    let Some(note) = generic_pad_to_note(step.pad) else {
                        continue;
                    };
                    let velocity = ((step.color.r as f32 * 0.299
                        + step.color.g as f32 * 0.587
                        + step.color.b as f32 * 0.114)
                        .round() as u8)
                        .min(127);
                    self.send_note_on(note, velocity, channel)?;
                }
                Ok(())
            }
            _ => {
                // Other schemes not supported for basic MIDI devices
                Ok(())
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Launchpad feedback (R628) — velocity-based note LED control
// ────────────────────────────────────────────────────────────────

/// Launchpad LED feedback using note-on velocity for color selection (R628)
pub struct LaunchpadFeedback {
    midi: MidiFeedback,
    config: MidiLedConfig,
    port: Option<usize>,
}

impl LaunchpadFeedback {
    pub fn new(config: MidiLedConfig, port: Option<usize>) -> Self {
        Self {
            midi: MidiFeedback::new(),
            config,
            port,
        }
    }

    /// Map pad index (0-63) to Launchpad grid note number.
    /// Launchpad rows are spaced 16 notes apart: note = row * 16 + col (col 0-7).
    pub fn pad_to_note(pad: u8) -> Option<u8> {
        if pad >= 64 {
            return None;
        }
        let row = pad / 8;
        let col = pad % 8;
        Some(row.saturating_mul(16).saturating_add(col))
    }
}

impl PadFeedback for LaunchpadFeedback {
    fn device_kind(&self) -> FeedbackDeviceKind {
        FeedbackDeviceKind::Launchpad
    }

    fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(port) = self.port {
            self.midi.connect(port)?;
        }
        Ok(())
    }

    fn set_pad_color(&mut self, pad: u8, color: RGB) -> Result<(), Box<dyn Error>> {
        // Check custom mapping first
        if let Some(mapping) = self.config.custom_mappings.iter().find(|m| m.pad == pad) {
            let msg = if color.r == 0 && color.g == 0 && color.b == 0 {
                mapping.led_off.clone()
            } else {
                mapping.led_on.clone()
            };
            return send_midi_led_message(&mut self.midi, &msg, self.config.channel);
        }
        let Some(note) = Self::pad_to_note(pad) else {
            return Ok(());
        };
        if color.r == 0 && color.g == 0 && color.b == 0 {
            self.midi.send_note_off_vel(
                note,
                self.config.note_off_velocity,
                self.config.channel,
            )?;
        } else {
            let velocity = self
                .config
                .colors
                .rgb_to_velocity(color.r, color.g, color.b);
            self.midi
                .send_note_on(note, velocity, self.config.channel)?;
        }
        Ok(())
    }

    fn set_pad_velocity(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        // Check custom mapping first — use custom target but provided velocity
        if let Some(mapping) = self.config.custom_mappings.iter().find(|m| m.pad == pad) {
            match &mapping.led_on {
                MidiLedMessage::NoteOn { note, .. } => {
                    self.midi
                        .send_note_on(*note, velocity, self.config.channel)?;
                }
                MidiLedMessage::NoteOff { note, .. } => {
                    self.midi
                        .send_note_off_vel(*note, velocity, self.config.channel)?;
                }
                MidiLedMessage::Cc { cc, .. } => {
                    self.midi.send_cc(*cc, velocity, self.config.channel)?;
                }
            }
            return Ok(());
        }
        let Some(note) = Self::pad_to_note(pad) else {
            return Ok(());
        };
        self.midi
            .send_note_on(note, velocity, self.config.channel)?;
        Ok(())
    }

    fn set_mode_colors(&mut self, _mode: u8) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn show_velocity_feedback(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        self.set_pad_velocity(pad, velocity)
    }

    fn flash_pad(&mut self, pad: u8, _color: RGB, _duration_ms: u64) -> Result<(), Box<dyn Error>> {
        if let Some(mapping) = self.config.custom_mappings.iter().find(|m| m.pad == pad) {
            let msg = mapping.led_on.clone();
            return send_midi_led_message(&mut self.midi, &msg, self.config.channel);
        }
        let Some(note) = Self::pad_to_note(pad) else {
            return Ok(());
        };
        self.midi
            .send_note_on(note, self.config.note_on_velocity, self.config.channel)?;
        Ok(())
    }

    fn ripple_effect(&mut self, _start_pad: u8, _color: RGB) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn clear_all(&mut self) -> Result<(), Box<dyn Error>> {
        // Send custom led_off for mapped pads
        let led_offs: Vec<_> = self
            .config
            .custom_mappings
            .iter()
            .map(|m| m.led_off.clone())
            .collect();
        for msg in &led_offs {
            send_midi_led_message(&mut self.midi, msg, self.config.channel)?;
        }
        for pad in 0..64u8 {
            // Skip pads with custom mappings (already handled above)
            if self.config.custom_mappings.iter().any(|m| m.pad == pad) {
                continue;
            }
            let Some(note) = Self::pad_to_note(pad) else {
                continue;
            };
            self.midi.send_note_off_vel(
                note,
                self.config.note_off_velocity,
                self.config.channel,
            )?;
        }
        Ok(())
    }

    fn show_long_press_feedback(
        &mut self,
        pad: u8,
        _elapsed_ms: u128,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(mapping) = self.config.custom_mappings.iter().find(|m| m.pad == pad) {
            let msg = mapping.led_on.clone();
            return send_midi_led_message(&mut self.midi, &msg, self.config.channel);
        }
        let Some(note) = Self::pad_to_note(pad) else {
            return Ok(());
        };
        self.midi
            .send_note_on(note, self.config.note_on_velocity, self.config.channel)?;
        Ok(())
    }

    fn run_scheme(&mut self, scheme: &LightingScheme) -> Result<(), Box<dyn Error>> {
        match scheme {
            LightingScheme::Off => self.clear_all(),
            LightingScheme::Reactive => Ok(()),
            LightingScheme::Custom(steps) => {
                for step in steps {
                    // #1460: route each step through set_pad_color so a
                    // configured `custom_mappings` entry for this pad is
                    // honored. The previous manual `pad_to_note` conversion
                    // bypassed custom mappings, driving the built-in Launchpad
                    // note layout instead of the configured LED target.
                    self.set_pad_color(step.pad, step.color)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

// ────────────────────────────────────────────────────────────────
// APC feedback (R629) — CC-based LED control
// ────────────────────────────────────────────────────────────────

/// APC LED feedback using CC messages for color/state control (R629)
pub struct ApcFeedback {
    midi: MidiFeedback,
    config: MidiLedConfig,
    port: Option<usize>,
}

impl ApcFeedback {
    pub fn new(config: MidiLedConfig, port: Option<usize>) -> Self {
        Self {
            midi: MidiFeedback::new(),
            config,
            port,
        }
    }

    /// Map pad index (0-39) to APC CC number.
    /// APC pads typically map to CC 56-95 (5x8 grid).
    pub fn pad_to_cc(pad: u8) -> Option<u8> {
        if pad >= 40 {
            return None;
        }
        Some(56u8.saturating_add(pad))
    }
}

impl PadFeedback for ApcFeedback {
    fn device_kind(&self) -> FeedbackDeviceKind {
        FeedbackDeviceKind::Apc
    }

    fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(port) = self.port {
            self.midi.connect(port)?;
        }
        Ok(())
    }

    fn set_pad_color(&mut self, pad: u8, color: RGB) -> Result<(), Box<dyn Error>> {
        // Check custom mapping first
        if let Some(mapping) = self.config.custom_mappings.iter().find(|m| m.pad == pad) {
            let msg = if color.r == 0 && color.g == 0 && color.b == 0 {
                mapping.led_off.clone()
            } else {
                mapping.led_on.clone()
            };
            return send_midi_led_message(&mut self.midi, &msg, self.config.channel);
        }
        let Some(cc) = Self::pad_to_cc(pad) else {
            return Ok(());
        };
        if color.r == 0 && color.g == 0 && color.b == 0 {
            self.midi
                .send_cc(cc, self.config.note_off_velocity, self.config.channel)?;
        } else {
            let velocity = self
                .config
                .colors
                .rgb_to_velocity(color.r, color.g, color.b);
            self.midi.send_cc(cc, velocity, self.config.channel)?;
        }
        Ok(())
    }

    fn set_pad_velocity(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        // Check custom mapping first — use custom target but provided velocity
        if let Some(mapping) = self.config.custom_mappings.iter().find(|m| m.pad == pad) {
            match &mapping.led_on {
                MidiLedMessage::NoteOn { note, .. } => {
                    self.midi
                        .send_note_on(*note, velocity, self.config.channel)?;
                }
                MidiLedMessage::NoteOff { note, .. } => {
                    self.midi
                        .send_note_off_vel(*note, velocity, self.config.channel)?;
                }
                MidiLedMessage::Cc { cc, .. } => {
                    self.midi.send_cc(*cc, velocity, self.config.channel)?;
                }
            }
            return Ok(());
        }
        let Some(cc) = Self::pad_to_cc(pad) else {
            return Ok(());
        };
        self.midi.send_cc(cc, velocity, self.config.channel)?;
        Ok(())
    }

    fn set_mode_colors(&mut self, _mode: u8) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn show_velocity_feedback(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        self.set_pad_velocity(pad, velocity)
    }

    fn flash_pad(&mut self, pad: u8, _color: RGB, _duration_ms: u64) -> Result<(), Box<dyn Error>> {
        if let Some(mapping) = self.config.custom_mappings.iter().find(|m| m.pad == pad) {
            let msg = mapping.led_on.clone();
            return send_midi_led_message(&mut self.midi, &msg, self.config.channel);
        }
        let Some(cc) = Self::pad_to_cc(pad) else {
            return Ok(());
        };
        self.midi
            .send_cc(cc, self.config.note_on_velocity, self.config.channel)?;
        Ok(())
    }

    fn ripple_effect(&mut self, _start_pad: u8, _color: RGB) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn clear_all(&mut self) -> Result<(), Box<dyn Error>> {
        // Send custom led_off for mapped pads
        let led_offs: Vec<_> = self
            .config
            .custom_mappings
            .iter()
            .map(|m| m.led_off.clone())
            .collect();
        for msg in &led_offs {
            send_midi_led_message(&mut self.midi, msg, self.config.channel)?;
        }
        for pad in 0..40u8 {
            if self.config.custom_mappings.iter().any(|m| m.pad == pad) {
                continue;
            }
            let Some(cc) = Self::pad_to_cc(pad) else {
                continue;
            };
            self.midi
                .send_cc(cc, self.config.note_off_velocity, self.config.channel)?;
        }
        Ok(())
    }

    fn show_long_press_feedback(
        &mut self,
        pad: u8,
        _elapsed_ms: u128,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(mapping) = self.config.custom_mappings.iter().find(|m| m.pad == pad) {
            let msg = mapping.led_on.clone();
            return send_midi_led_message(&mut self.midi, &msg, self.config.channel);
        }
        let Some(cc) = Self::pad_to_cc(pad) else {
            return Ok(());
        };
        self.midi
            .send_cc(cc, self.config.note_on_velocity, self.config.channel)?;
        Ok(())
    }

    fn run_scheme(&mut self, scheme: &LightingScheme) -> Result<(), Box<dyn Error>> {
        match scheme {
            LightingScheme::Off => self.clear_all(),
            LightingScheme::Reactive => Ok(()),
            LightingScheme::Custom(steps) => {
                for step in steps {
                    // #1460: route through set_pad_color so a configured
                    // `custom_mappings` entry for this pad is honored instead
                    // of the built-in APC CC layout (the manual `pad_to_cc`
                    // conversion bypassed custom mappings).
                    self.set_pad_color(step.pad, step.color)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

// ────────────────────────────────────────────────────────────────
// HID LED Feedback — Issue #367 (config-driven)
// ────────────────────────────────────────────────────────────────

/// Generic HID LED feedback using a resolved config profile.
/// Replaces hardcoded MikroMK3LEDs for config-driven HID devices.
pub struct HidLedFeedback {
    config: ResolvedHidLedConfig,
    device: Option<hidapi::HidDevice>,
    buffer: Vec<u8>,
    velocity_colors: Option<VelocityColorMap>,
}

impl HidLedFeedback {
    /// Create from an unresolved config. Resolves profile and validates.
    pub fn new(
        config: &HidLedConfig,
        velocity_colors: Option<VelocityColorMap>,
    ) -> Result<Self, String> {
        let resolved = config.resolve_profile()?;
        let buffer = vec![0u8; resolved.buffer_size];
        Ok(Self {
            config: resolved,
            device: None,
            buffer,
            velocity_colors,
        })
    }

    /// Create from the built-in MK3 profile.
    pub fn mikro_mk3(velocity_colors: Option<VelocityColorMap>) -> Self {
        let mk3 = HidLedConfig::mikro_mk3();
        Self::new(&mk3, velocity_colors).expect("built-in MK3 profile is always valid")
    }

    /// Map logical pad index to physical LED position using pad_layout.
    fn map_pad(&self, pad_index: u8) -> u8 {
        if self.config.pad_layout.is_empty() || pad_index as usize >= self.config.pad_layout.len() {
            pad_index
        } else {
            self.config.pad_layout[pad_index as usize]
        }
    }

    /// Encode color index + brightness for indexed-LED devices (6-bit color index).
    ///
    /// Only the lower 6 bits of `color_index` are used (max 63) since the
    /// result is packed into a single `u8` as `(index << 2) | brightness_bits`.
    fn encode_pad_indexed(color_index: u8, brightness: u8) -> u8 {
        if brightness == 0 {
            return 0;
        }
        let b_bits = brightness & 0b11;
        ((color_index & 0x3F) << 2) | b_bits
    }

    /// Map velocity to color index using velocity color map or defaults.
    fn velocity_to_color_index(&self, velocity: u8) -> u8 {
        if let Some(ref vcm) = self.velocity_colors
            && let Some(rgb) = vcm.color_for_velocity(velocity)
        {
            return self.rgb_to_palette_index(rgb.r, rgb.g, rgb.b);
        }
        // Default mapping: look up standard colors from the device's actual palette
        match velocity {
            0..=39 => self.rgb_to_palette_index(0, 255, 0), // Green
            40..=79 => self.rgb_to_palette_index(255, 255, 0), // Yellow
            80..=127 => self.rgb_to_palette_index(255, 0, 0), // Red
            _ => self.rgb_to_palette_index(0, 255, 0),      // Green
        }
    }

    /// Find nearest palette color for an RGB value.
    fn rgb_to_palette_index(&self, r: u8, g: u8, b: u8) -> u8 {
        if self.config.color_palette.is_empty() {
            return 0;
        }
        debug_assert!(
            self.config.color_palette.len() <= 64,
            "palette exceeds 6-bit index range (max 64 entries)"
        );
        let mut best_idx = 0u8;
        let mut best_dist = u32::MAX;
        for (i, c) in self.config.color_palette.iter().enumerate() {
            let dr = (r as i32 - c.r as i32).unsigned_abs();
            let dg = (g as i32 - c.g as i32).unsigned_abs();
            let db = (b as i32 - c.b as i32).unsigned_abs();
            let dist = dr * dr + dg * dg + db * db;
            if dist < best_dist {
                best_dist = dist;
                best_idx = u8::try_from(i).unwrap_or(0);
            }
        }
        best_idx
    }

    /// Map velocity to brightness (MK3 style).
    fn velocity_to_brightness(velocity: u8) -> u8 {
        if velocity == 0 {
            0x00
        } else {
            let factor = 0.4 + (velocity as f32 / 127.0) * 0.6;
            if factor < 0.5 {
                0x7c // Dim
            } else if factor < 0.75 {
                0x7e // Normal
            } else {
                0x7f // Bright
            }
        }
    }

    fn write_buffer(&self) -> Result<(), Box<dyn Error>> {
        let Some(ref device) = self.device else {
            return Ok(());
        };
        let mut data = vec![self.config.led_report_id];
        data.extend_from_slice(&self.buffer);
        debug!(bytes = data.len(), "Writing HID LED buffer");
        device.write(&data)?;
        Ok(())
    }
}

impl PadFeedback for HidLedFeedback {
    fn device_kind(&self) -> FeedbackDeviceKind {
        FeedbackDeviceKind::Hid
    }

    fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        let api = hidapi::HidApi::new()?;
        for info in api.device_list() {
            if info.vendor_id() == self.config.vendor_id
                && info.product_id() == self.config.product_id
                && info.interface_number() == self.config.interface_number as i32
            {
                match info.open_device(&api) {
                    Ok(dev) => {
                        self.device = Some(dev);
                        debug!(
                            vendor = format!("0x{:04X}", self.config.vendor_id),
                            product = format!("0x{:04X}", self.config.product_id),
                            "Connected to HID LED device"
                        );
                        return Ok(());
                    }
                    Err(e) => {
                        error!("Failed to open HID device: {}", e);
                    }
                }
            }
        }
        Err(format!(
            "HID device {:04X}:{:04X} not found",
            self.config.vendor_id, self.config.product_id
        )
        .into())
    }

    fn set_pad_color(&mut self, pad: u8, color: RGB) -> Result<(), Box<dyn Error>> {
        if pad >= self.config.pad_count {
            return Ok(());
        }
        let led_pos = self.map_pad(pad) as usize;
        let offset = self.config.pad_led_offset + led_pos;
        if offset >= self.buffer.len() {
            return Ok(());
        }
        if color.r == 0 && color.g == 0 && color.b == 0 {
            self.buffer[offset] = 0;
        } else {
            let idx = self.rgb_to_palette_index(color.r, color.g, color.b);
            self.buffer[offset] = Self::encode_pad_indexed(idx, 0x7e);
        }
        self.write_buffer()
    }

    fn set_pad_velocity(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        if pad >= self.config.pad_count {
            return Ok(());
        }
        let led_pos = self.map_pad(pad) as usize;
        let offset = self.config.pad_led_offset + led_pos;
        if offset >= self.buffer.len() {
            return Ok(());
        }
        let color_idx = self.velocity_to_color_index(velocity);
        let brightness = Self::velocity_to_brightness(velocity);
        self.buffer[offset] = Self::encode_pad_indexed(color_idx, brightness);
        self.write_buffer()
    }

    fn set_mode_colors(&mut self, _mode: u8) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn show_velocity_feedback(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        self.set_pad_velocity(pad, velocity)
    }

    fn flash_pad(
        &mut self,
        _pad: u8,
        _color: RGB,
        _duration_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn ripple_effect(&mut self, _start_pad: u8, _color: RGB) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn clear_all(&mut self) -> Result<(), Box<dyn Error>> {
        for i in 0..self.config.pad_count as usize {
            let led_pos = self.map_pad(i as u8) as usize;
            let offset = self.config.pad_led_offset + led_pos;
            if offset < self.buffer.len() {
                self.buffer[offset] = 0;
            }
        }
        self.write_buffer()
    }

    fn show_long_press_feedback(
        &mut self,
        _pad: u8,
        _elapsed_ms: u128,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    fn run_scheme(&mut self, scheme: &LightingScheme) -> Result<(), Box<dyn Error>> {
        match scheme {
            LightingScheme::Off => self.clear_all(),
            LightingScheme::Reactive => Ok(()),
            LightingScheme::Custom(steps) => {
                for step in steps {
                    self.set_pad_color(step.pad, step.color)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Create a `FeedbackManager` wired to the config's `default_fade_ms`.
///
/// If `fade_ms` is `Some`, uses `FeedbackManager::with_fade_ms`; otherwise
/// falls back to `FeedbackManager::new` (1000 ms default).
pub fn create_feedback_manager(
    device: Box<dyn PadFeedback>,
    fade_ms: Option<u64>,
) -> FeedbackManager {
    match fade_ms {
        Some(ms) => FeedbackManager::with_fade_ms(device, ms),
        None => FeedbackManager::new(device),
    }
}

/// Factory function to create the appropriate feedback device.
///
/// Detects Launchpad and APC devices by name and creates device-specific
/// feedback implementations. Falls back to generic MIDI feedback.
///
/// TODO: The daemon should wrap the returned device with [`create_feedback_manager`]
/// to honour `default_fade_ms` from the user's config. Currently the daemon
/// uses this function directly without the manager layer.
pub fn create_feedback_device(
    device_name: &str,
    midi_port: Option<usize>,
    enable_hid: bool,
    midi_led_config: Option<&MidiLedConfig>,
    hid_led_config: Option<&HidLedConfig>,
    velocity_colors: Option<&VelocityColorMap>,
) -> Box<dyn PadFeedback> {
    let name_lower = device_name.to_lowercase();

    // Config-driven HID feedback takes priority (Issue #367).
    // Two failure modes:
    // - `connect()` fails → return disconnected `HidLedFeedback` (retryable;
    //   the device is expected and the caller can retry via `connect()`).
    // - `new()` fails → config/profile is invalid (not retryable); fall
    //   through to MIDI so the user still gets feedback, with an error log.
    if let Some(hid_cfg) = hid_led_config {
        match HidLedFeedback::new(hid_cfg, velocity_colors.cloned()) {
            Ok(mut fb) => {
                if let Err(e) = fb.connect() {
                    warn!(
                        "HID LED device configured but connect() failed ({}); \
                         call connect() to retry",
                        e
                    );
                }
                return Box::new(fb);
            }
            Err(e) => {
                error!("Failed to create HID LED feedback: {}", e);
            }
        }
    }

    // Legacy: Use HID for Mikro MK3 only if explicitly enabled
    if enable_hid && name_lower.contains("maschine") && name_lower.contains("mikro") {
        let mut fb = MikroMK3LEDs::new();
        if let Err(e) = fb.connect() {
            error!("Failed to connect Mikro MK3 HID feedback: {}", e);
        }
        return Box::new(fb);
    }

    // Device-specific feedback only when explicitly configured (opt-in)
    if let Some(config) = midi_led_config.cloned() {
        // Launchpad detection (R628) — note-based LED control
        if name_lower.contains("launchpad") {
            let mut fb = LaunchpadFeedback::new(config, midi_port);
            if let Err(e) = fb.connect() {
                error!("Failed to connect Launchpad feedback: {}", e);
            }
            return Box::new(fb);
        }

        // APC detection (R629) — CC-based LED control
        if name_lower.contains("apc") {
            let mut fb = ApcFeedback::new(config, midi_port);
            if let Err(e) = fb.connect() {
                error!("Failed to connect APC feedback: {}", e);
            }
            return Box::new(fb);
        }
    }

    // Generic MIDI feedback — use config for channel/velocity when available
    if let Some(port) = midi_port {
        let mut midi_fb = if let Some(config) = midi_led_config {
            MidiFeedback::with_config(config.clone())
        } else {
            MidiFeedback::new()
        };
        if let Err(e) = midi_fb.connect(port) {
            error!("Failed to connect MIDI feedback: {}", e);
        }
        Box::new(midi_fb)
    } else {
        Box::new(MidiFeedback::new())
    }
}

use std::collections::HashMap;
use std::time::Instant;

/// Manager for LED feedback with reactive state tracking
///
/// FeedbackManager wraps a PadFeedback device and provides higher-level
/// feedback management including:
/// - Reactive pad press/release with fade-out tracking
/// - Mode changes with color updates
/// - Lighting scheme switching
/// - Active pad state management
pub struct FeedbackManager {
    device: Box<dyn PadFeedback>,
    current_scheme: LightingScheme,
    reactive_state: HashMap<u8, (Instant, u8)>, // (pad -> (press_time, velocity))
    flash_state: HashMap<u8, Instant>,          // pad -> flash_end_time
    current_mode: u8,
    /// Configurable fade duration (Issue #332, R727). Default 1000ms.
    fade_duration_ms: u64,
}

impl FeedbackManager {
    /// Create a new FeedbackManager with the given device
    ///
    /// Defaults to Reactive lighting scheme, mode 0, and 1000ms fade.
    pub fn new(device: Box<dyn PadFeedback>) -> Self {
        Self {
            device,
            current_scheme: LightingScheme::Reactive,
            reactive_state: HashMap::new(),
            flash_state: HashMap::new(),
            current_mode: 0,
            fade_duration_ms: 1000,
        }
    }

    /// Create with a custom fade duration (Issue #332, R727).
    pub fn with_fade_ms(device: Box<dyn PadFeedback>, fade_ms: u64) -> Self {
        Self {
            device,
            current_scheme: LightingScheme::Reactive,
            reactive_state: HashMap::new(),
            flash_state: HashMap::new(),
            current_mode: 0,
            fade_duration_ms: fade_ms,
        }
    }

    /// Handle a pad press event
    ///
    /// In reactive mode, this records the pad press time and velocity,
    /// and triggers velocity-based LED feedback.
    pub fn on_pad_press(&mut self, pad: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        // Only track reactive fade-out state (and show velocity feedback) while
        // actually in Reactive mode (#1459). Inserting unconditionally let
        // presses made during Static/Rainbow/etc. inflate `active_pads()` and
        // linger in `reactive_state`; since switching *into* Reactive does not
        // clear state, a later `update()` would then process those stale
        // presses as completed fade-outs.
        if self.current_scheme == LightingScheme::Reactive {
            self.reactive_state.insert(pad, (Instant::now(), velocity));
            self.device.show_velocity_feedback(pad, velocity)?;
        }

        Ok(())
    }

    /// Handle a pad release event
    ///
    /// In reactive mode, the pad remains tracked for fade-out.
    /// It will be removed after the configured fade duration (default 1000ms) during update().
    pub fn on_pad_release(&mut self, _pad: u8) -> Result<(), Box<dyn Error>> {
        // In reactive mode, we keep the pad in state for fade-out
        // It will be removed during update() after the configured fade duration
        Ok(())
    }

    /// Handle a mode change event
    ///
    /// Updates the current mode and applies mode-specific colors to the device.
    pub fn on_mode_change(&mut self, mode: u8, _color: RGB) -> Result<(), Box<dyn Error>> {
        self.current_mode = mode;
        self.device.set_mode_colors(mode)?;
        Ok(())
    }

    /// Set the lighting scheme
    ///
    /// Clears reactive state when switching away from Reactive mode.
    /// Applies the new scheme to the device immediately.
    pub fn set_scheme(&mut self, scheme: LightingScheme) -> Result<(), Box<dyn Error>> {
        // Clear reactive state when switching schemes
        if scheme != LightingScheme::Reactive {
            self.reactive_state.clear();
        }

        self.current_scheme = scheme;
        self.device.run_scheme(&self.current_scheme)?;
        Ok(())
    }

    /// Flash a pad LED for a given duration (non-blocking).
    /// Turns the LED on immediately; the off will be sent during the next update()
    /// after duration_ms has elapsed.
    pub fn flash_pad(
        &mut self,
        pad: u8,
        color: RGB,
        duration_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        self.device.flash_pad(pad, color, duration_ms)?;
        let deadline =
            match Instant::now().checked_add(std::time::Duration::from_millis(duration_ms)) {
                Some(d) => d,
                None => {
                    tracing::warn!(duration_ms, "Flash duration overflow, capping at 60s");
                    Instant::now() + std::time::Duration::from_secs(60)
                }
            };
        self.flash_state.insert(pad, deadline);
        Ok(())
    }

    /// Update reactive fade-out state
    ///
    /// Should be called periodically (e.g., in your event loop).
    /// Returns a list of pads that have completed their fade-out period (configured
    /// fade duration, default 1000ms) or flash-off period.
    ///
    /// # Behavior
    /// - Reactive fade-outs only run in Reactive scheme
    /// - Flash-offs run in all schemes
    pub fn update(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut completed = Vec::new();
        let now = Instant::now();

        // Process reactive fade-outs (only in reactive mode)
        if self.current_scheme == LightingScheme::Reactive {
            let fade_duration = std::time::Duration::from_millis(self.fade_duration_ms);

            let faded: Vec<u8> = self
                .reactive_state
                .iter()
                .filter_map(|(pad, (press_time, _velocity))| {
                    if now.duration_since(*press_time) >= fade_duration {
                        Some(*pad)
                    } else {
                        None
                    }
                })
                .collect();

            for pad in &faded {
                self.reactive_state.remove(pad);
            }
            completed.extend(faded);
        }

        // Process flash-offs (runs in all schemes)
        let flash_completed: Vec<u8> = self
            .flash_state
            .iter()
            .filter_map(
                |(pad, end_time)| {
                    if now >= *end_time { Some(*pad) } else { None }
                },
            )
            .collect();

        for pad in &flash_completed {
            self.flash_state.remove(pad);
            self.device.set_pad_color(*pad, RGB { r: 0, g: 0, b: 0 })?;
        }
        completed.extend(flash_completed);

        Ok(completed)
    }

    /// Clear all LED state
    ///
    /// Clears reactive state and sends clear command to device.
    pub fn clear(&mut self) -> Result<(), Box<dyn Error>> {
        self.reactive_state.clear();
        self.flash_state.clear();
        self.device.clear_all()?;
        Ok(())
    }

    /// Get the current lighting scheme
    pub fn current_scheme(&self) -> LightingScheme {
        self.current_scheme.clone()
    }

    /// Get the current mode
    pub fn current_mode(&self) -> u8 {
        self.current_mode
    }

    /// Get the number of active pads (in reactive mode)
    pub fn active_pads(&self) -> usize {
        self.reactive_state.len()
    }

    /// Get immutable access to the underlying device
    pub fn device(&self) -> &dyn PadFeedback {
        &*self.device
    }

    /// Get mutable access to the underlying device
    pub fn device_mut(&mut self) -> &mut dyn PadFeedback {
        &mut *self.device
    }

    /// Get the configured fade duration in milliseconds.
    pub fn fade_duration_ms(&self) -> u64 {
        self.fade_duration_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{
        HidLedConfig, MidiLedConfig, MidiLedCustomMapping, MidiLedMessage, VelocityColorMap,
    };
    use crate::mikro_leds::RGB;

    /// #1460: build a Launchpad feedback whose pad 0 is custom-mapped to a
    /// non-default note, channel 1.
    fn launchpad_with_pad0_mapped_to_note(note: u8, velocity: u8) -> LaunchpadFeedback {
        let config = MidiLedConfig {
            channel: 1,
            custom_mappings: vec![MidiLedCustomMapping {
                pad: 0,
                led_on: MidiLedMessage::NoteOn { note, velocity },
                led_off: MidiLedMessage::NoteOff { note, velocity: 0 },
            }],
            ..Default::default()
        };
        LaunchpadFeedback::new(config, None)
    }

    // ── HidLedFeedback helpers ──────────────────────────────────

    fn mk3_feedback() -> HidLedFeedback {
        HidLedFeedback::mikro_mk3(None)
    }

    fn mk3_feedback_with_vcm(vcm: VelocityColorMap) -> HidLedFeedback {
        HidLedFeedback::mikro_mk3(Some(vcm))
    }

    // ── velocity_to_color_index ─────────────────────────────────

    #[test]
    fn velocity_to_color_index_defaults_green() {
        let fb = mk3_feedback();
        // 0-39 → green (7)
        assert_eq!(fb.velocity_to_color_index(0), 7);
        assert_eq!(fb.velocity_to_color_index(20), 7);
        assert_eq!(fb.velocity_to_color_index(39), 7);
    }

    #[test]
    fn velocity_to_color_index_defaults_yellow() {
        let fb = mk3_feedback();
        // 40-79 → yellow (5)
        assert_eq!(fb.velocity_to_color_index(40), 5);
        assert_eq!(fb.velocity_to_color_index(60), 5);
        assert_eq!(fb.velocity_to_color_index(79), 5);
    }

    #[test]
    fn velocity_to_color_index_defaults_red() {
        let fb = mk3_feedback();
        // 80-127 → red (1)
        assert_eq!(fb.velocity_to_color_index(80), 1);
        assert_eq!(fb.velocity_to_color_index(100), 1);
        assert_eq!(fb.velocity_to_color_index(127), 1);
    }

    #[test]
    fn velocity_to_color_index_with_velocity_color_map() {
        let mut vcm = VelocityColorMap { ranges: Vec::new() };
        // Single range mapping all velocities to pure red
        vcm.ranges.push(crate::config::types::VelocityRange {
            min: 0,
            max: 127,
            color: crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
        });
        let fb = mk3_feedback_with_vcm(vcm);
        // Should use palette lookup instead of default ranges.
        // VCM maps all velocities to pure red (255,0,0) → palette index 1.
        let idx = fb.velocity_to_color_index(64);
        assert_eq!(idx, fb.rgb_to_palette_index(255, 0, 0));
    }

    // ── rgb_to_palette_index ────────────────────────────────────

    #[test]
    fn rgb_to_palette_index_empty_palette() {
        // Build a config with an empty palette to test the fallback
        let mut cfg = HidLedConfig::mikro_mk3();
        cfg.color_palette = Some(Vec::new());
        let fb = HidLedFeedback::new(&cfg, None).unwrap();
        // Empty palette → always returns 0
        assert_eq!(fb.rgb_to_palette_index(255, 0, 0), 0);
    }

    #[test]
    fn rgb_to_palette_index_nearest_match() {
        let fb = mk3_feedback();
        // The MK3 palette has known colors; pure red should match the red entry
        let idx_red = fb.rgb_to_palette_index(255, 0, 0);
        let idx_green = fb.rgb_to_palette_index(0, 255, 0);
        // They should differ (different palette entries)
        assert_ne!(idx_red, idx_green);
    }

    #[test]
    fn rgb_to_palette_index_exact_black() {
        let fb = mk3_feedback();
        // Black (0,0,0) should match the first palette entry if palette starts with black,
        // or the nearest dark color
        let idx = fb.rgb_to_palette_index(0, 0, 0);
        assert!(idx < fb.config.color_palette.len() as u8);
    }

    #[test]
    fn rgb_to_palette_index_full_64_entry_palette() {
        // Build a config with exactly 64 palette entries (max allowed by 6-bit index)
        let palette: Vec<crate::config::types::RgbColor> = (0..64)
            .map(|i| crate::config::types::RgbColor {
                r: (i * 4) as u8,
                g: 0,
                b: 0,
            })
            .collect();
        let cfg = HidLedConfig {
            vendor_id: Some(0x1234),
            product_id: Some(0x5678),
            buffer_size: Some(80),
            pad_led_offset: Some(0),
            pad_count: Some(16),
            color_palette: Some(palette),
            ..Default::default()
        };
        let fb = HidLedFeedback::new(&cfg, None).unwrap();
        // Should find the nearest match without panicking
        let idx = fb.rgb_to_palette_index(128, 0, 0);
        assert!(idx < 64, "palette index must fit in 6 bits");
    }

    #[test]
    fn rgb_to_palette_index_result_within_palette_bounds() {
        let fb = mk3_feedback();
        // All results should be valid palette indices
        for r in [0u8, 64, 128, 192, 255] {
            for g in [0u8, 128, 255] {
                let idx = fb.rgb_to_palette_index(r, g, 0);
                assert!(
                    (idx as usize) < fb.config.color_palette.len(),
                    "index {} out of palette bounds (len {})",
                    idx,
                    fb.config.color_palette.len()
                );
            }
        }
    }

    // ── encode_pad_indexed ─────────────────────────────────────────

    #[test]
    fn encode_pad_indexed_zero_brightness_returns_zero() {
        assert_eq!(HidLedFeedback::encode_pad_indexed(5, 0), 0);
    }

    #[test]
    fn encode_pad_indexed_encodes_color_and_brightness() {
        // color_index=1, brightness=0x03 → (1 << 2) | 3 = 7
        assert_eq!(HidLedFeedback::encode_pad_indexed(1, 0x03), 7);
        // color_index=7, brightness=0x7f → (7 << 2) | (0x7f & 0b11) = 28 | 3 = 31
        assert_eq!(HidLedFeedback::encode_pad_indexed(7, 0x7f), 31);
    }

    #[test]
    fn encode_pad_indexed_brightness_masked_to_2_bits() {
        // brightness=0x7e → 0x7e & 0b11 = 0b10 = 2
        assert_eq!(
            HidLedFeedback::encode_pad_indexed(3, 0x7e),
            (3 << 2) | 2 // = 14
        );
    }

    #[test]
    fn encode_pad_indexed_color_index_masked_to_6_bits() {
        // color_index >= 64 should not panic; upper bits are masked off.
        // color_index=64 (0x40) → masked to 0 → (0 << 2) | 3 = 3
        assert_eq!(HidLedFeedback::encode_pad_indexed(64, 0x7f), 3);
        // color_index=127 (0x7F) → masked to 63 (0x3F) → (63 << 2) | 3 = 255
        assert_eq!(HidLedFeedback::encode_pad_indexed(127, 0x7f), 255);
        // color_index=255 → masked to 63 → same as above
        assert_eq!(HidLedFeedback::encode_pad_indexed(255, 0x7f), 255);
        // color_index=63 (max valid) should work normally: (63 << 2) | 1 = 253
        assert_eq!(HidLedFeedback::encode_pad_indexed(63, 0x01), 253);
    }

    // ── velocity_to_brightness ──────────────────────────────────

    #[test]
    fn velocity_to_brightness_zero_is_off() {
        assert_eq!(HidLedFeedback::velocity_to_brightness(0), 0x00);
    }

    #[test]
    fn velocity_to_brightness_low_velocity() {
        // velocity=1 → factor ≈ 0.405 → dim (0x7c)
        assert_eq!(HidLedFeedback::velocity_to_brightness(1), 0x7c);
    }

    #[test]
    fn velocity_to_brightness_mid_velocity() {
        // velocity=64 → factor ≈ 0.70 → normal (0x7e)
        assert_eq!(HidLedFeedback::velocity_to_brightness(64), 0x7e);
    }

    #[test]
    fn velocity_to_brightness_high_velocity() {
        // velocity=127 → factor=1.0 → bright (0x7f)
        assert_eq!(HidLedFeedback::velocity_to_brightness(127), 0x7f);
    }

    // ── set_pad_color / set_pad_velocity logic ──────────────────

    #[test]
    fn set_pad_color_out_of_range_is_noop() {
        let mut fb = mk3_feedback();
        // pad >= pad_count should be a no-op
        let result = fb.set_pad_color(255, RGB { r: 127, g: 0, b: 0 });
        assert!(result.is_ok());
    }

    #[test]
    fn set_pad_velocity_out_of_range_is_noop() {
        let mut fb = mk3_feedback();
        let result = fb.set_pad_velocity(255, 100);
        assert!(result.is_ok());
    }

    #[test]
    fn set_pad_color_black_clears_buffer() {
        let mut fb = mk3_feedback();
        // First set a color, then set black
        let _ = fb.set_pad_color(0, RGB { r: 255, g: 0, b: 0 });
        let _ = fb.set_pad_color(0, RGB { r: 0, g: 0, b: 0 });
        let led_pos = fb.map_pad(0) as usize;
        let offset = fb.config.pad_led_offset + led_pos;
        assert_eq!(fb.buffer[offset], 0);
    }

    #[test]
    fn set_pad_color_nonblack_writes_encoded_value() {
        let mut fb = mk3_feedback();
        let _ = fb.set_pad_color(0, RGB { r: 255, g: 0, b: 0 });
        let led_pos = fb.map_pad(0) as usize;
        let offset = fb.config.pad_led_offset + led_pos;
        // Should be non-zero (encoded color + brightness)
        assert_ne!(fb.buffer[offset], 0);
    }

    #[test]
    fn set_pad_velocity_writes_encoded_value() {
        let mut fb = mk3_feedback();
        let _ = fb.set_pad_velocity(0, 100);
        let led_pos = fb.map_pad(0) as usize;
        let offset = fb.config.pad_led_offset + led_pos;
        // Should be non-zero for velocity > 0
        assert_ne!(fb.buffer[offset], 0);
    }

    #[test]
    fn set_pad_velocity_zero_writes_zero() {
        let mut fb = mk3_feedback();
        let _ = fb.set_pad_velocity(0, 0);
        let led_pos = fb.map_pad(0) as usize;
        let offset = fb.config.pad_led_offset + led_pos;
        // velocity=0 → brightness=0 → encode returns 0
        assert_eq!(fb.buffer[offset], 0);
    }

    // ── FeedbackManager::with_fade_ms ───────────────────────────

    #[test]
    fn feedback_manager_with_fade_ms_stores_duration() {
        use crate::midi_feedback::MidiFeedback;
        let device: Box<dyn PadFeedback> = Box::new(MidiFeedback::new());
        let mgr = FeedbackManager::with_fade_ms(device, 500);
        assert_eq!(mgr.fade_duration_ms(), 500);
    }

    #[test]
    fn feedback_manager_new_defaults_to_1000ms() {
        use crate::midi_feedback::MidiFeedback;
        let device: Box<dyn PadFeedback> = Box::new(MidiFeedback::new());
        let mgr = FeedbackManager::new(device);
        assert_eq!(mgr.fade_duration_ms(), 1000);
    }

    // ── create_feedback_manager factory ──────────────────────────

    #[test]
    fn create_feedback_manager_with_some_fade() {
        use crate::midi_feedback::MidiFeedback;
        let device: Box<dyn PadFeedback> = Box::new(MidiFeedback::new());
        let mgr = create_feedback_manager(device, Some(250));
        assert_eq!(mgr.fade_duration_ms(), 250);
    }

    #[test]
    fn create_feedback_manager_with_none_defaults() {
        use crate::midi_feedback::MidiFeedback;
        let device: Box<dyn PadFeedback> = Box::new(MidiFeedback::new());
        let mgr = create_feedback_manager(device, None);
        assert_eq!(mgr.fade_duration_ms(), 1000);
    }

    // ── #1460: Custom lighting schemes honor custom_mappings ─────

    /// #1460 regression. A `LightingScheme::Custom` step for a custom-mapped
    /// pad must drive the configured LED target (the mapping's `led_on`), not
    /// the built-in Launchpad note layout (`pad_to_note`).
    #[test]
    fn launchpad_custom_scheme_uses_custom_mapping_not_default_note() {
        // pad 0's default note is pad_to_note(0) == 0; map it to note 99.
        let mut fb = launchpad_with_pad0_mapped_to_note(99, 5);
        assert_eq!(LaunchpadFeedback::pad_to_note(0), Some(0));

        fb.run_scheme(&LightingScheme::Custom(vec![ChaseStep {
            pad: 0,
            color: RGB { r: 255, g: 0, b: 0 },
            delay_ms: 0,
        }]))
        .expect("run_scheme Custom");

        let captured = fb.midi.captured();
        // The mapped target (note-on, note 99, velocity 5 on channel 1).
        assert!(
            captured.contains(&[0x90, 99, 5]),
            "Custom scheme must use the custom mapping's LED target (#1460); got {captured:?}"
        );
        // And NOT the default pad_to_note(0) == note 0.
        assert!(
            !captured.iter().any(|m| m[0] == 0x90 && m[1] == 0),
            "Custom scheme must not drive the default note for a mapped pad (#1460); got {captured:?}"
        );
    }

    /// APC equivalent — Custom scheme honors a CC custom mapping rather than
    /// the built-in `pad_to_cc` layout.
    #[test]
    fn apc_custom_scheme_uses_custom_mapping_not_default_cc() {
        // pad 0's default CC is pad_to_cc(0) == 56; map it to CC 88.
        let config = MidiLedConfig {
            channel: 1,
            custom_mappings: vec![MidiLedCustomMapping {
                pad: 0,
                led_on: MidiLedMessage::Cc { cc: 88, value: 7 },
                led_off: MidiLedMessage::Cc { cc: 88, value: 0 },
            }],
            ..Default::default()
        };
        let mut fb = ApcFeedback::new(config, None);
        assert_eq!(ApcFeedback::pad_to_cc(0), Some(56));

        fb.run_scheme(&LightingScheme::Custom(vec![ChaseStep {
            pad: 0,
            color: RGB { r: 0, g: 255, b: 0 },
            delay_ms: 0,
        }]))
        .expect("run_scheme Custom");

        let captured = fb.midi.captured();
        assert!(
            captured.contains(&[0xB0, 88, 7]),
            "APC Custom scheme must use the custom mapping's CC target (#1460); got {captured:?}"
        );
        assert!(
            !captured.iter().any(|m| m[0] == 0xB0 && m[1] == 56),
            "APC Custom scheme must not drive the default CC for a mapped pad (#1460); got {captured:?}"
        );
    }

    /// #1464 regression. `MidiFeedback::flash_pad` must turn the LED back OFF
    /// after the flash — previously it only sent note-on (ignoring the
    /// duration), so the pad stayed lit.
    #[test]
    fn midi_flash_pad_sends_note_on_then_note_off() {
        use crate::midi_feedback::MidiFeedback;
        let mut fb = MidiFeedback::new();
        // duration 0 → no real sleep; channel 1 → status 0x90/0x80.
        fb.flash_pad(40, 127, 1, 0).expect("flash_pad");
        let captured = fb.captured();
        assert_eq!(
            captured.len(),
            2,
            "flash_pad must emit note-on AND note-off (#1464); got {captured:?}"
        );
        assert_eq!(captured[0], [0x90, 40, 127], "note-on");
        assert_eq!(captured[1], [0x80, 40, 0], "note-off");
    }

    /// The PadFeedback (trait) flash path also turns the LED off, using the
    /// configured channel/velocity, with the duration honoured.
    #[test]
    fn midi_padfeedback_flash_pad_turns_off() {
        use crate::midi_feedback::MidiFeedback;
        let mut fb = MidiFeedback::new();
        PadFeedback::flash_pad(&mut fb, 0, RGB { r: 255, g: 0, b: 0 }, 0).expect("flash_pad");
        let captured = fb.captured();
        assert_eq!(
            captured.len(),
            2,
            "trait flash must emit on+off (#1464); got {captured:?}"
        );
        assert_eq!(captured[0][0] & 0xF0, 0x90, "first is a note-on");
        assert_eq!(captured[1][0] & 0xF0, 0x80, "second is a note-off");
        // Same note for on and off.
        assert_eq!(captured[0][1], captured[1][1]);
    }
}
