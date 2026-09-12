// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! LED configuration: MIDI LED, HID LED, velocity color mapping, RGB.

use super::*;

/// LED configuration section
///
/// Controls LED feedback behavior for supported hardware devices.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LedConfig {
    /// Whether LED feedback is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Global brightness (0-127)
    #[serde(default = "default_brightness")]
    pub brightness: u8,
    /// Default lighting scheme
    #[serde(default = "default_scheme")]
    pub scheme: String,
    /// Idle timeout in seconds before dimming (0 = never)
    #[serde(default)]
    pub idle_timeout_secs: u32,
    /// Per-mode color overrides
    #[serde(default)]
    pub mode_colors: std::collections::BTreeMap<String, RgbColor>,
    /// MIDI LED configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi: Option<MidiLedConfig>,
    /// HID LED configuration (config-driven device profiles)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hid: Option<HidLedConfig>,
    /// Velocity-to-color mapping
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_colors: Option<VelocityColorMap>,
    /// Default fade time in milliseconds for reactive LED feedback
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_fade_ms: Option<u64>,
}

impl Default for LedConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            brightness: default_brightness(),
            scheme: default_scheme(),
            idle_timeout_secs: 0,
            mode_colors: std::collections::BTreeMap::new(),
            midi: None,
            hid: None,
            velocity_colors: None,
            default_fade_ms: None,
        }
    }
}

/// MIDI LED configuration
///
/// Configures how MIDI messages control device LEDs. Supports velocity-based
/// color mapping, custom per-pad overrides, and device-specific protocols.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MidiLedConfig {
    /// MIDI channel for LED control (1-16, R673)
    #[serde(default = "default_midi_led_channel")]
    pub channel: u8,
    /// Velocity value for LED on (0-127, R674)
    #[serde(default = "default_note_on_velocity")]
    pub note_on_velocity: u8,
    /// Velocity value for LED off (0-127, R675)
    #[serde(default)]
    pub note_off_velocity: u8,
    /// Velocity-based color palette (R676, R678)
    #[serde(default)]
    pub colors: MidiLedColors,
    /// Custom per-pad LED mappings (R679-R681)
    #[serde(default)]
    pub custom_mappings: Vec<MidiLedCustomMapping>,
}

impl Default for MidiLedConfig {
    fn default() -> Self {
        Self {
            channel: default_midi_led_channel(),
            note_on_velocity: default_note_on_velocity(),
            note_off_velocity: 0,
            colors: MidiLedColors::default(),
            custom_mappings: Vec::new(),
        }
    }
}

fn default_midi_led_channel() -> u8 {
    1
}

fn default_note_on_velocity() -> u8 {
    127
}

/// Velocity-based color values for MIDI LED control (R676, R678)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MidiLedColors {
    #[serde(default = "default_color_red")]
    pub red: u8,
    #[serde(default = "default_color_green")]
    pub green: u8,
    #[serde(default = "default_color_blue")]
    pub blue: u8,
    #[serde(default = "default_color_yellow")]
    pub yellow: u8,
    #[serde(default = "default_color_amber")]
    pub amber: u8,
    #[serde(default)]
    pub off: u8,
}

impl MidiLedColors {
    /// Map an RGB color to the nearest configured palette velocity.
    /// Uses simple hue-based matching: predominantly red → red velocity, etc.
    pub fn rgb_to_velocity(&self, r: u8, g: u8, b: u8) -> u8 {
        if r == 0 && g == 0 && b == 0 {
            return self.off;
        }
        // Blue dominant
        if b > r && b > g {
            return self.blue;
        }
        // Yellow: r and g both significant and close together, with blue low
        if r > 0 && g > 0 && r.abs_diff(g) < 30 && b < r.min(g) / 2 {
            return self.yellow;
        }
        if r > g && r > b {
            return self.red;
        }
        if g > r && g > b {
            return self.green;
        }
        self.amber
    }
}

impl Default for MidiLedColors {
    fn default() -> Self {
        Self {
            red: default_color_red(),
            green: default_color_green(),
            blue: default_color_blue(),
            yellow: default_color_yellow(),
            amber: default_color_amber(),
            off: 0,
        }
    }
}

fn default_color_red() -> u8 {
    5
}
fn default_color_green() -> u8 {
    21
}
fn default_color_yellow() -> u8 {
    13
}
fn default_color_blue() -> u8 {
    45
}
fn default_color_amber() -> u8 {
    9
}

/// A custom per-pad LED mapping (R679-R681)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MidiLedCustomMapping {
    /// Pad number this mapping applies to
    pub pad: u8,
    /// MIDI message to send when LED should be on
    pub led_on: MidiLedMessage,
    /// MIDI message to send when LED should be off
    pub led_off: MidiLedMessage,
}

// ────────────────────────────────────────────────────────────────
// HID LED Configuration
// ────────────────────────────────────────────────────────────────

/// HID LED configuration (config-driven device profiles)
///
/// Configures HID-based LED control for devices like NI Maschine Mikro MK3.
/// Fields use `Option<T>` so profile merging can distinguish "user set this"
/// from "use profile default". Call `resolve_profile()` to get a fully-populated
/// `ResolvedHidLedConfig` for runtime use.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HidLedConfig {
    /// Built-in device profile name (e.g. "mikro-mk3")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hid_profile: Option<String>,
    /// USB Vendor ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u16>,
    /// USB Product ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u16>,
    /// HID interface number to open (0-255)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_number: Option<u8>,
    /// HID report ID for LED output
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led_report_id: Option<u8>,
    /// Size of the LED output buffer (bytes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_size: Option<usize>,
    /// Byte offset where pad LEDs start in the buffer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_led_offset: Option<usize>,
    /// Number of pads on the device
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_count: Option<u8>,
    /// Indexed color palette (maps index to RGB for indexed-color devices)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_palette: Option<Vec<RgbColor>>,
    /// Pad layout: logical pad index → physical LED position.
    /// If None, identity mapping is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_layout: Option<Vec<u8>>,
}

/// Resolved HID LED config — all fields populated after profile merge.
/// Used by the HID feedback implementation at runtime.
#[derive(Debug, Clone)]
pub struct ResolvedHidLedConfig {
    pub vendor_id: u16,
    pub product_id: u16,
    pub interface_number: u8,
    pub led_report_id: u8,
    pub buffer_size: usize,
    pub pad_led_offset: usize,
    pub pad_count: u8,
    pub color_palette: Vec<RgbColor>,
    pub pad_layout: Vec<u8>,
}

impl HidLedConfig {
    /// Built-in Mikro MK3 device profile
    pub fn mikro_mk3() -> Self {
        Self {
            hid_profile: Some("mikro-mk3".to_string()),
            vendor_id: Some(0x17CC),
            product_id: Some(0x1700),
            interface_number: Some(0),
            led_report_id: Some(0x80),
            buffer_size: Some(80),
            pad_led_offset: Some(39),
            pad_count: Some(16),
            color_palette: Some(mikro_mk3_palette()),
            pad_layout: Some(mikro_mk3_pad_layout()),
        }
    }

    /// Look up a built-in profile by name.
    pub fn from_profile(name: &str) -> Option<Self> {
        match name {
            "mikro-mk3" => Some(Self::mikro_mk3()),
            _ => None,
        }
    }

    /// Merge profile defaults into unset fields and return a fully-populated
    /// `ResolvedHidLedConfig`. Validates buffer bounds, pad layout, etc.
    pub fn resolve_profile(&self) -> Result<ResolvedHidLedConfig, String> {
        let profile = self
            .hid_profile
            .as_ref()
            .map(|name| {
                Self::from_profile(name).ok_or_else(|| format!("Unknown HID profile: '{}'", name))
            })
            .transpose()?;

        // User values take priority; fall back to profile; then sensible defaults
        let vendor_id = self
            .vendor_id
            .or(profile.as_ref().and_then(|p| p.vendor_id))
            .ok_or("vendor_id is required (set explicitly or use hid_profile)")?;
        let product_id = self
            .product_id
            .or(profile.as_ref().and_then(|p| p.product_id))
            .ok_or("product_id is required (set explicitly or use hid_profile)")?;
        let interface_number = self
            .interface_number
            .or(profile.as_ref().and_then(|p| p.interface_number))
            .unwrap_or(0);
        let led_report_id = self
            .led_report_id
            .or(profile.as_ref().and_then(|p| p.led_report_id))
            .unwrap_or(0);
        let buffer_size = self
            .buffer_size
            .or(profile.as_ref().and_then(|p| p.buffer_size))
            .unwrap_or(80);
        let pad_led_offset = self
            .pad_led_offset
            .or(profile.as_ref().and_then(|p| p.pad_led_offset))
            .unwrap_or(0);
        let pad_count = self
            .pad_count
            .or(profile.as_ref().and_then(|p| p.pad_count))
            .unwrap_or(16);
        let color_palette = self
            .color_palette
            .clone()
            .or(profile.as_ref().and_then(|p| p.color_palette.clone()))
            .unwrap_or_default();
        let pad_layout = self
            .pad_layout
            .clone()
            .or(profile.as_ref().and_then(|p| p.pad_layout.clone()))
            .unwrap_or_default();

        // Validate buffer bounds (use checked_add for overflow safety)
        if buffer_size == 0 {
            return Err("buffer_size must be > 0".to_string());
        }
        if pad_led_offset >= buffer_size {
            return Err(format!(
                "pad_led_offset ({}) must be less than buffer_size ({})",
                pad_led_offset, buffer_size
            ));
        }
        // Note: assumes 1 byte per pad (indexed color). Devices needing
        // multi-byte LED data (e.g. RGB) would need a bytes_per_pad field.
        let pad_end = pad_led_offset
            .checked_add(pad_count as usize)
            .ok_or_else(|| "pad config causes integer overflow".to_string())?;
        if pad_end > buffer_size {
            return Err(format!(
                "pad range (offset {} + count {}) exceeds buffer_size ({})",
                pad_led_offset, pad_count, buffer_size
            ));
        }

        // Validate pad_layout or materialize identity mapping
        let pad_layout = if pad_layout.is_empty() {
            // Identity mapping: logical index == physical position
            (0..pad_count).collect()
        } else {
            if pad_layout.len() != pad_count as usize {
                return Err(format!(
                    "pad_layout has {} entries but pad_count is {}",
                    pad_layout.len(),
                    pad_count
                ));
            }
            let mut seen = std::collections::HashSet::new();
            for (i, &pos) in pad_layout.iter().enumerate() {
                if pos >= pad_count {
                    return Err(format!(
                        "pad_layout[{}]: position {} >= pad_count {}",
                        i, pos, pad_count
                    ));
                }
                if !seen.insert(pos) {
                    return Err(format!(
                        "pad_layout[{}]: duplicate physical position {}",
                        i, pos
                    ));
                }
            }
            pad_layout
        };

        Ok(ResolvedHidLedConfig {
            vendor_id,
            product_id,
            interface_number,
            led_report_id,
            buffer_size,
            pad_led_offset,
            pad_count,
            color_palette,
            pad_layout,
        })
    }
}

/// MK3 indexed color palette (maps PadColor values to approximate RGB)
fn mikro_mk3_palette() -> Vec<RgbColor> {
    vec![
        RgbColor { r: 0, g: 0, b: 0 },   // 0: Off
        RgbColor { r: 255, g: 0, b: 0 }, // 1: Red
        RgbColor {
            r: 255,
            g: 128,
            b: 0,
        }, // 2: Orange
        RgbColor {
            r: 255,
            g: 180,
            b: 0,
        }, // 3: LightOrange
        RgbColor {
            r: 255,
            g: 210,
            b: 0,
        }, // 4: WarmYellow
        RgbColor {
            r: 255,
            g: 255,
            b: 0,
        }, // 5: Yellow
        RgbColor {
            r: 128,
            g: 255,
            b: 0,
        }, // 6: Lime
        RgbColor { r: 0, g: 255, b: 0 }, // 7: Green
        RgbColor {
            r: 0,
            g: 255,
            b: 128,
        }, // 8: Mint
        RgbColor {
            r: 0,
            g: 255,
            b: 255,
        }, // 9: Cyan
        RgbColor {
            r: 0,
            g: 200,
            b: 255,
        }, // 10: Turquoise
        RgbColor {
            r: 0,
            g: 128,
            b: 255,
        }, // 11: Blue
        RgbColor {
            r: 128,
            g: 0,
            b: 255,
        }, // 12: Plum
        RgbColor {
            r: 160,
            g: 0,
            b: 255,
        }, // 13: Violet
        RgbColor {
            r: 200,
            g: 0,
            b: 255,
        }, // 14: Purple
        RgbColor {
            r: 255,
            g: 0,
            b: 200,
        }, // 15: Magenta
        RgbColor {
            r: 255,
            g: 0,
            b: 128,
        }, // 16: Fuchsia
        RgbColor {
            r: 255,
            g: 255,
            b: 255,
        }, // 17: White
    ]
}

/// MK3 pad layout: vertical flip (logical bottom-to-top → physical top-to-bottom)
pub(super) fn mikro_mk3_pad_layout() -> Vec<u8> {
    vec![12, 13, 14, 15, 8, 9, 10, 11, 4, 5, 6, 7, 0, 1, 2, 3]
}

// ────────────────────────────────────────────────────────────────
// Velocity-to-Color Mapping
// ────────────────────────────────────────────────────────────────

/// Velocity-to-color mapping
///
/// Maps velocity ranges to colors for LED feedback.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VelocityColorMap {
    pub ranges: Vec<VelocityRange>,
}

impl Default for VelocityColorMap {
    fn default() -> Self {
        Self {
            ranges: vec![
                VelocityRange {
                    min: 0,
                    max: 39,
                    color: RgbColor { r: 0, g: 255, b: 0 },
                },
                VelocityRange {
                    min: 40,
                    max: 79,
                    color: RgbColor {
                        r: 255,
                        g: 255,
                        b: 0,
                    },
                },
                VelocityRange {
                    min: 80,
                    max: 127,
                    color: RgbColor { r: 255, g: 0, b: 0 },
                },
            ],
        }
    }
}

impl VelocityColorMap {
    /// Look up the color for a given velocity. Returns None if no range matches.
    pub fn color_for_velocity(&self, velocity: u8) -> Option<&RgbColor> {
        self.ranges
            .iter()
            .find(|r| velocity >= r.min && velocity <= r.max)
            .map(|r| &r.color)
    }
}

/// A velocity range with associated color
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VelocityRange {
    pub min: u8,
    pub max: u8,
    pub color: RgbColor,
}

/// A MIDI message descriptor for LED control
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MidiLedMessage {
    NoteOn { note: u8, velocity: u8 },
    NoteOff { note: u8, velocity: u8 },
    Cc { cc: u8, value: u8 },
}

fn default_brightness() -> u8 {
    100
}

fn default_scheme() -> String {
    "reactive".to_string()
}

/// RGB color for LED configuration
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

// RgbColor ↔ RGB conversions live in mikro_leds.rs to avoid coupling config to hardware

// ────────────────────────────────────────────────────────
// Signal Routing Graph — ADR-031 D1
// ────────────────────────────────────────────────────────
