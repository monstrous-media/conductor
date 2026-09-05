// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use hidapi::{HidApi, HidDevice};
use std::error::Error;
use tracing::{debug, error, warn};

// Native Instruments Vendor ID
const NI_VENDOR_ID: u16 = 0x17CC;
const MIKRO_MK3_PRODUCT_ID: u16 = 0x1700;
const LED_REPORT_ID: u8 = 0x80;

// MK3 uses indexed colors (not RGB!), based on r00tman's driver
const PAD_LED_OFFSET: usize = 39; // Pads start at buffer[39]

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum PadColor {
    Off = 0,
    Red = 1,
    Orange = 2,
    LightOrange = 3,
    WarmYellow = 4,
    Yellow = 5,
    Lime = 6,
    Green = 7,
    Mint = 8,
    Cyan = 9,
    Turquoise = 10,
    Blue = 11,
    Plum = 12,
    Violet = 13,
    Purple = 14,
    Magenta = 15,
    Fuchsia = 16,
    White = 17,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum Brightness {
    Off = 0x00,
    Dim = 0x7c,
    Normal = 0x7e,
    Bright = 0x7f,
}

// RGB struct for API compatibility
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(clippy::upper_case_acronyms)]
pub struct RGB {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl RGB {
    pub const OFF: RGB = RGB { r: 0, g: 0, b: 0 };
}

impl From<crate::config::types::RgbColor> for RGB {
    fn from(c: crate::config::types::RgbColor) -> Self {
        RGB {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

impl From<RGB> for crate::config::types::RgbColor {
    fn from(c: RGB) -> Self {
        crate::config::types::RgbColor {
            r: c.r,
            g: c.g,
            b: c.b,
        }
    }
}

pub struct MikroMK3LEDs {
    device: Option<HidDevice>,
    buffer: [u8; 80],
}

impl Default for MikroMK3LEDs {
    fn default() -> Self {
        Self {
            device: None,
            buffer: [0; 80],
        }
    }
}

impl MikroMK3LEDs {
    pub fn new() -> Self {
        Self::default()
    }

    fn encode_pad(color: PadColor, brightness: Brightness) -> u8 {
        if matches!(brightness, Brightness::Off) {
            return 0;
        }
        let c = color as u8;
        let b = brightness as u8;
        (c << 2) | (b & 0b11)
    }

    fn velocity_to_color(velocity: u8) -> PadColor {
        match velocity {
            0..=39 => PadColor::Green,
            40..=79 => PadColor::Yellow,
            80..=127 => PadColor::Red,
            _ => PadColor::Green,
        }
    }

    fn velocity_to_brightness(velocity: u8) -> Brightness {
        if velocity == 0 {
            Brightness::Off
        } else {
            let factor = 0.4 + (velocity as f32 / 127.0) * 0.6;
            if factor < 0.5 {
                Brightness::Dim
            } else if factor < 0.75 {
                Brightness::Normal
            } else {
                Brightness::Bright
            }
        }
    }

    pub fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        debug!("Connecting to Mikro MK3 LEDs...");

        let api = HidApi::new()?;

        for device_info in api.device_list() {
            if device_info.vendor_id() == NI_VENDOR_ID
                && device_info.product_id() == MIKRO_MK3_PRODUCT_ID
            {
                let interface_number = device_info.interface_number();
                debug!(interface = interface_number, "Found Mikro MK3 device");

                if interface_number == 0 {
                    match device_info.open_device(&api) {
                        Ok(dev) => {
                            self.device = Some(dev);
                            debug!("Successfully connected to Mikro MK3 LED interface");
                            return Ok(());
                        }
                        Err(e) => {
                            error!("Failed to open Mikro MK3: {}", e);
                        }
                    }
                }
            }
        }

        Err("Mikro MK3 not found or could not be opened".into())
    }

    pub fn set_pad_color(&mut self, pad_index: u8, color: RGB) -> Result<(), Box<dyn Error>> {
        // This previously ignored `color` and always wrote Off, so any
        // PadFeedback colour path for legacy Mikro devices — including
        // LightingScheme::Custom — turned the pad off. Map the RGB onto the
        // device's fixed indexed palette + a brightness so the requested
        // colour is actually shown. Black maps to Off (clears the pad).
        let pad_color = Self::rgb_to_pad_color(color);
        let brightness = Self::rgb_to_brightness(color);
        self.set_pad_indexed(pad_index, pad_color, brightness)
    }

    /// Best-effort map of an RGB colour to the Mikro MK3's fixed indexed
    /// palette. The hardware has ~17 discrete colours, not full RGB, so
    /// this buckets by dominant channel(s); near-grey bright colours map to
    /// White, and black maps to Off.
    fn rgb_to_pad_color(color: RGB) -> PadColor {
        let RGB { r, g, b } = color;
        let max = r.max(g).max(b);
        if max == 0 {
            return PadColor::Off;
        }
        let min = r.min(g).min(b);
        // Low saturation (all channels close) but lit → White.
        if max - min <= 24 {
            return PadColor::White;
        }
        // A channel is "strong" if at least half the brightest channel.
        let half = max / 2;
        let sr = r >= half;
        let sg = g >= half;
        let sb = b >= half;
        match (sr, sg, sb) {
            (true, true, false) => PadColor::Yellow,
            (false, true, true) => PadColor::Cyan,
            (true, false, true) => PadColor::Magenta,
            (true, false, false) => PadColor::Red,
            (false, true, false) => PadColor::Green,
            (false, false, true) => PadColor::Blue,
            // Any other combination is near-grey-ish; fall back to White.
            _ => PadColor::White,
        }
    }

    /// Map an RGB colour's intensity to a discrete brightness.
    fn rgb_to_brightness(color: RGB) -> Brightness {
        match color.r.max(color.g).max(color.b) {
            0 => Brightness::Off,
            1..=84 => Brightness::Dim,
            85..=170 => Brightness::Normal,
            _ => Brightness::Bright,
        }
    }

    /// Maps logical pad index (0-15) to physical LED position
    /// The MK3 hardware has pads numbered top-to-bottom, but logical indices are bottom-to-top
    /// This creates a vertical flip: rows are swapped (0↔3, 1↔2)
    fn map_pad_to_led_position(pad_index: u8) -> u8 {
        let row = pad_index / 4; // 0-3 (bottom to top in logical layout)
        let col = pad_index % 4; // 0-3 (left to right)
        let flipped_row = 3 - row; // Flip vertically: 0→3, 1→2, 2→1, 3→0
        flipped_row * 4 + col
    }

    pub fn set_pad_indexed(
        &mut self,
        pad_index: u8,
        color: PadColor,
        brightness: Brightness,
    ) -> Result<(), Box<dyn Error>> {
        if pad_index >= 16 {
            return Err(format!("Pad index {} out of range (0-15)", pad_index).into());
        }

        // Map logical pad index to physical LED position
        let led_position = Self::map_pad_to_led_position(pad_index);
        let offset = PAD_LED_OFFSET + led_position as usize;
        self.buffer[offset] = Self::encode_pad(color, brightness);

        debug!(
            pad = pad_index,
            led = led_position,
            offset = offset,
            ?color,
            ?brightness,
            "LED update"
        );

        self.write_buffer()
    }

    pub fn set_pad_velocity(&mut self, pad_index: u8, velocity: u8) -> Result<(), Box<dyn Error>> {
        let color = Self::velocity_to_color(velocity);
        let brightness = Self::velocity_to_brightness(velocity);
        self.set_pad_indexed(pad_index, color, brightness)
    }

    pub fn show_velocity_feedback(
        &mut self,
        pad_index: u8,
        velocity: u8,
    ) -> Result<(), Box<dyn Error>> {
        self.set_pad_velocity(pad_index, velocity)
    }

    fn write_buffer(&self) -> Result<(), Box<dyn Error>> {
        if self.device.is_none() {
            return Ok(());
        }

        let device = self.device.as_ref().unwrap();

        let mut data = vec![LED_REPORT_ID];
        data.extend_from_slice(&self.buffer);

        debug!(bytes = data.len(), "Writing LED buffer to device");

        match device.write(&data) {
            Ok(bytes_written) => {
                debug!(bytes_written, "Successfully wrote LED buffer");
                Ok(())
            }
            Err(e) => {
                error!("Failed to write LED buffer: {}", e);
                Err(e.into())
            }
        }
    }

    pub fn set_mode_colors(&mut self, _mode: u8) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    pub fn flash_pad(&mut self, _pad_index: u8) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    pub fn ripple_effect(&mut self, _start_pad: u8) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    pub fn show_long_press_feedback(&mut self, _pad_index: u8) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    pub fn set_pad_colors(&mut self, colors: &[RGB; 16]) -> Result<(), Box<dyn Error>> {
        // Convert RGB to indexed colors (map to nearest available color)
        for (i, _color) in colors.iter().enumerate() {
            // For now, just set all pads to dim white
            self.buffer[PAD_LED_OFFSET + i] = Self::encode_pad(PadColor::White, Brightness::Dim);
        }
        self.write_buffer()
    }

    pub fn clear_all(&mut self) -> Result<(), Box<dyn Error>> {
        // Clear all pads
        for i in 0..16 {
            self.buffer[PAD_LED_OFFSET + i] = 0;
        }
        self.write_buffer()
    }

    pub fn breathing_effect(&mut self) -> Result<(), Box<dyn Error>> {
        for i in 0..16 {
            let led_pos = Self::map_pad_to_led_position(i as u8) as usize;
            self.buffer[PAD_LED_OFFSET + led_pos] =
                Self::encode_pad(PadColor::Blue, Brightness::Dim);
        }
        self.write_buffer()
    }

    pub fn pulse_effect(&mut self) -> Result<(), Box<dyn Error>> {
        for i in 0..16 {
            let led_pos = Self::map_pad_to_led_position(i as u8) as usize;
            self.buffer[PAD_LED_OFFSET + led_pos] =
                Self::encode_pad(PadColor::Cyan, Brightness::Normal);
        }
        self.write_buffer()
    }

    pub fn rainbow_effect(&mut self) -> Result<(), Box<dyn Error>> {
        let colors = [
            PadColor::Red,
            PadColor::Orange,
            PadColor::Yellow,
            PadColor::Green,
            PadColor::Cyan,
            PadColor::Blue,
            PadColor::Purple,
            PadColor::Magenta,
            PadColor::Red,
            PadColor::Orange,
            PadColor::Yellow,
            PadColor::Green,
            PadColor::Cyan,
            PadColor::Blue,
            PadColor::Purple,
            PadColor::Magenta,
        ];
        for (i, &color) in colors.iter().enumerate() {
            let led_pos = Self::map_pad_to_led_position(i as u8) as usize;
            self.buffer[PAD_LED_OFFSET + led_pos] = Self::encode_pad(color, Brightness::Normal);
        }
        self.write_buffer()
    }

    pub fn wave_effect(&mut self) -> Result<(), Box<dyn Error>> {
        let brightnesses = [
            Brightness::Dim,
            Brightness::Normal,
            Brightness::Bright,
            Brightness::Bright,
            Brightness::Bright,
            Brightness::Bright,
            Brightness::Normal,
            Brightness::Dim,
            Brightness::Dim,
            Brightness::Normal,
            Brightness::Bright,
            Brightness::Bright,
            Brightness::Bright,
            Brightness::Bright,
            Brightness::Normal,
            Brightness::Dim,
        ];
        for (i, &brightness) in brightnesses.iter().enumerate() {
            let led_pos = Self::map_pad_to_led_position(i as u8) as usize;
            self.buffer[PAD_LED_OFFSET + led_pos] = Self::encode_pad(PadColor::Blue, brightness);
        }
        self.write_buffer()
    }

    pub fn sparkle_effect(&mut self) -> Result<(), Box<dyn Error>> {
        use rand::RngExt;
        let mut rng = rand::rng();

        for i in 0..16 {
            let led_pos = Self::map_pad_to_led_position(i as u8) as usize;
            if rng.random_bool(0.2) {
                let brightness = if rng.random_bool(0.5) {
                    Brightness::Normal
                } else {
                    Brightness::Bright
                };
                self.buffer[PAD_LED_OFFSET + led_pos] =
                    Self::encode_pad(PadColor::White, brightness);
            } else {
                self.buffer[PAD_LED_OFFSET + led_pos] = 0;
            }
        }
        self.write_buffer()
    }

    pub fn vumeter_effect(&mut self) -> Result<(), Box<dyn Error>> {
        let colors = [
            PadColor::Green,
            PadColor::Green,
            PadColor::Green,
            PadColor::Green,
            PadColor::Green,
            PadColor::Green,
            PadColor::Yellow,
            PadColor::Yellow,
            PadColor::Yellow,
            PadColor::Yellow,
            PadColor::Orange,
            PadColor::Orange,
            PadColor::Red,
            PadColor::Red,
            PadColor::Red,
            PadColor::Red,
        ];
        for (i, &color) in colors.iter().enumerate() {
            let led_pos = Self::map_pad_to_led_position(i as u8) as usize;
            self.buffer[PAD_LED_OFFSET + led_pos] = Self::encode_pad(color, Brightness::Dim);
        }
        self.write_buffer()
    }

    pub fn spiral_effect(&mut self) -> Result<(), Box<dyn Error>> {
        let pattern = [
            (PadColor::Purple, Brightness::Bright),
            (PadColor::Purple, Brightness::Normal),
            (PadColor::Purple, Brightness::Normal),
            (PadColor::Purple, Brightness::Dim),
            (PadColor::Magenta, Brightness::Dim),
            (PadColor::Off, Brightness::Off),
            (PadColor::Off, Brightness::Off),
            (PadColor::Blue, Brightness::Dim),
            (PadColor::Magenta, Brightness::Normal),
            (PadColor::Off, Brightness::Off),
            (PadColor::Off, Brightness::Off),
            (PadColor::Blue, Brightness::Normal),
            (PadColor::Magenta, Brightness::Normal),
            (PadColor::Cyan, Brightness::Normal),
            (PadColor::Cyan, Brightness::Normal),
            (PadColor::Blue, Brightness::Normal),
        ];
        for (i, &(color, brightness)) in pattern.iter().enumerate() {
            let led_pos = Self::map_pad_to_led_position(i as u8) as usize;
            self.buffer[PAD_LED_OFFSET + led_pos] = Self::encode_pad(color, brightness);
        }
        self.write_buffer()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::{ChaseStep, LightingScheme, PadFeedback};

    /// Buffer slot for a logical pad (mirrors set_pad_indexed's addressing).
    fn pad_slot(pad: u8) -> usize {
        PAD_LED_OFFSET + MikroMK3LEDs::map_pad_to_led_position(pad) as usize
    }

    /// Regression test: a non-black `set_pad_color` must write a non-zero
    /// encoded LED value — previously it ignored the colour and always wrote
    /// Off, leaving legacy Mikro pads dark.
    #[test]
    fn set_pad_color_nonblack_writes_nonzero() {
        let mut leds = MikroMK3LEDs::new();
        leds.set_pad_color(0, RGB { r: 255, g: 0, b: 0 })
            .expect("set_pad_color");
        assert_ne!(
            leds.buffer[pad_slot(0)],
            0,
            "non-black set_pad_color must light the pad"
        );
    }

    /// Black still clears the pad (Off).
    #[test]
    fn set_pad_color_black_writes_zero() {
        let mut leds = MikroMK3LEDs::new();
        // Light it, then clear it.
        leds.set_pad_color(0, RGB { r: 0, g: 255, b: 0 }).unwrap();
        leds.set_pad_color(0, RGB::OFF).unwrap();
        assert_eq!(leds.buffer[pad_slot(0)], 0, "black must clear the pad");
    }

    /// A Custom lighting scheme run through the PadFeedback impl lights
    /// the legacy Mikro pad (the Custom branch calls set_pad_color).
    #[test]
    fn custom_scheme_lights_legacy_mikro_pad() {
        let mut leds = MikroMK3LEDs::new();
        PadFeedback::run_scheme(
            &mut leds,
            &LightingScheme::Custom(vec![ChaseStep {
                pad: 2,
                color: RGB { r: 0, g: 0, b: 255 },
                delay_ms: 0,
            }]),
        )
        .expect("run_scheme Custom");
        assert_ne!(
            leds.buffer[pad_slot(2)],
            0,
            "a Custom scheme step must light the legacy Mikro pad"
        );
    }
}
