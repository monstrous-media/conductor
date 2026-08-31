// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! SysEx message validation for safe hardware operations
//!
//! Validates SysEx messages against known patterns to categorize risk levels
//! and block potentially dangerous operations.

use serde::{Deserialize, Serialize};

/// Category of SysEx message based on its purpose and risk
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SysExCategory {
    /// Identity request - safe, read-only
    IdentityRequest,
    /// Device inquiry - safe, read-only
    DeviceInquiry,
    /// Parameter query - safe, read-only
    ParameterQuery,
    /// Parameter change - moderate risk, changes settings
    ParameterChange,
    /// Preset/program dump - moderate risk, can overwrite presets
    PresetDump,
    /// Sample dump - high risk, can overwrite samples
    SampleDump,
    /// Firmware update - BLOCKED, can brick device
    FirmwareUpdate,
    /// Factory reset - high risk, erases all settings
    FactoryReset,
    /// Unknown manufacturer - elevated risk
    UnknownManufacturer,
    /// Malformed message
    Malformed,
}

impl SysExCategory {
    /// Returns true if this category is blocked (never allowed)
    pub fn is_blocked(&self) -> bool {
        matches!(self, SysExCategory::FirmwareUpdate)
    }

    /// Returns true if this category requires explicit confirmation
    pub fn requires_confirmation(&self) -> bool {
        matches!(
            self,
            SysExCategory::ParameterChange
                | SysExCategory::PresetDump
                | SysExCategory::SampleDump
                | SysExCategory::FactoryReset
                | SysExCategory::UnknownManufacturer
        )
    }

    /// Returns a human-readable risk level
    pub fn risk_level(&self) -> &'static str {
        match self {
            SysExCategory::IdentityRequest
            | SysExCategory::DeviceInquiry
            | SysExCategory::ParameterQuery => "low",
            SysExCategory::ParameterChange | SysExCategory::PresetDump => "medium",
            SysExCategory::SampleDump
            | SysExCategory::FactoryReset
            | SysExCategory::UnknownManufacturer => "high",
            SysExCategory::FirmwareUpdate => "critical",
            SysExCategory::Malformed => "unknown",
        }
    }

    /// Returns a description of what this category does
    pub fn description(&self) -> &'static str {
        match self {
            SysExCategory::IdentityRequest => "Requests device identity (read-only)",
            SysExCategory::DeviceInquiry => "Queries device capabilities (read-only)",
            SysExCategory::ParameterQuery => "Queries device parameters (read-only)",
            SysExCategory::ParameterChange => "Changes device parameters/settings",
            SysExCategory::PresetDump => "Sends preset/program data to device",
            SysExCategory::SampleDump => "Sends sample data to device",
            SysExCategory::FirmwareUpdate => "BLOCKED: Attempts to update device firmware",
            SysExCategory::FactoryReset => "Resets device to factory settings",
            SysExCategory::UnknownManufacturer => "Unknown manufacturer ID - cannot verify safety",
            SysExCategory::Malformed => "Malformed SysEx message",
        }
    }
}

/// Result of SysEx validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SysExValidation {
    /// Category of the message
    pub category: SysExCategory,
    /// Manufacturer ID (if identifiable)
    pub manufacturer_id: Option<String>,
    /// Manufacturer name (if known)
    pub manufacturer_name: Option<String>,
    /// Human-readable description of what this message does
    pub description: String,
    /// Risk level (low, medium, high, critical)
    pub risk_level: String,
    /// Whether this message is allowed
    pub allowed: bool,
    /// Reason if not allowed
    pub reason: Option<String>,
}

/// Known manufacturer IDs (first byte after F0, or 3 bytes for extended)
mod manufacturers {
    pub const UNIVERSAL_NON_REALTIME: u8 = 0x7E;
    pub const UNIVERSAL_REALTIME: u8 = 0x7F;

    // Extended manufacturer IDs (0x00 followed by 2 bytes)
    pub const NATIVE_INSTRUMENTS: [u8; 3] = [0x00, 0x21, 0x09];
    pub const NOVATION: [u8; 3] = [0x00, 0x20, 0x29];
    pub const ABLETON: [u8; 3] = [0x00, 0x21, 0x1D];

    // Single-byte manufacturer IDs
    pub const ROLAND: u8 = 0x41;
    pub const YAMAHA: u8 = 0x43;
    pub const KORG: u8 = 0x42;
    pub const AKAI: u8 = 0x47;
}

/// SysEx message validator
pub struct SysExValidator;

impl SysExValidator {
    /// Validate a SysEx message and return its categorization
    ///
    /// # Arguments
    /// * `data` - SysEx data bytes (without F0 start and F7 end)
    pub fn validate(data: &[u8]) -> SysExValidation {
        if data.is_empty() {
            return SysExValidation {
                category: SysExCategory::Malformed,
                manufacturer_id: None,
                manufacturer_name: None,
                description: "Empty SysEx message".to_string(),
                risk_level: "unknown".to_string(),
                allowed: false,
                reason: Some("Message is empty".to_string()),
            };
        }

        // Parse manufacturer ID
        let (manufacturer_id, manufacturer_name, remaining) = Self::parse_manufacturer(data);

        // Categorize based on content
        let category = Self::categorize(data, remaining);

        // Check if blocked
        let (allowed, reason) = if category.is_blocked() {
            (
                false,
                Some(format!(
                    "Blocked: {} operations are not allowed",
                    category.description()
                )),
            )
        } else {
            (true, None)
        };

        SysExValidation {
            category,
            manufacturer_id: Some(manufacturer_id),
            manufacturer_name,
            description: category.description().to_string(),
            risk_level: category.risk_level().to_string(),
            allowed,
            reason,
        }
    }

    /// Parse manufacturer ID from SysEx data
    fn parse_manufacturer(data: &[u8]) -> (String, Option<String>, &[u8]) {
        if data.is_empty() {
            return ("unknown".to_string(), None, data);
        }

        let first_byte = data[0];

        // Universal SysEx
        if first_byte == manufacturers::UNIVERSAL_NON_REALTIME {
            return (
                "7E".to_string(),
                Some("Universal Non-Realtime".to_string()),
                &data[1..],
            );
        }
        if first_byte == manufacturers::UNIVERSAL_REALTIME {
            return (
                "7F".to_string(),
                Some("Universal Realtime".to_string()),
                &data[1..],
            );
        }

        // Extended manufacturer ID (0x00 followed by 2 bytes)
        if first_byte == 0x00 && data.len() >= 3 {
            let ext_id = &data[0..3];
            let id_str = format!("{:02X} {:02X} {:02X}", ext_id[0], ext_id[1], ext_id[2]);

            let name = match ext_id {
                _ if ext_id == manufacturers::NATIVE_INSTRUMENTS => {
                    Some("Native Instruments".to_string())
                }
                _ if ext_id == manufacturers::NOVATION => Some("Novation".to_string()),
                _ if ext_id == manufacturers::ABLETON => Some("Ableton".to_string()),
                _ => None,
            };

            return (id_str, name, &data[3..]);
        }

        // Single-byte manufacturer ID
        let name = match first_byte {
            manufacturers::ROLAND => Some("Roland".to_string()),
            manufacturers::YAMAHA => Some("Yamaha".to_string()),
            manufacturers::KORG => Some("Korg".to_string()),
            manufacturers::AKAI => Some("Akai".to_string()),
            _ => None,
        };

        (format!("{:02X}", first_byte), name, &data[1..])
    }

    /// Categorize SysEx message based on content
    fn categorize(full_data: &[u8], after_manufacturer: &[u8]) -> SysExCategory {
        // Check for Universal SysEx patterns first
        if !full_data.is_empty() {
            let first = full_data[0];

            // Universal Non-Realtime (7E)
            if first == manufacturers::UNIVERSAL_NON_REALTIME && full_data.len() >= 3 {
                let sub_id = full_data[2];
                return match sub_id {
                    0x06 => SysExCategory::IdentityRequest,   // Identity Request
                    0x07 => SysExCategory::DeviceInquiry,     // Dump Request
                    0x01..=0x03 => SysExCategory::SampleDump, // Sample Dump Standard
                    _ => SysExCategory::DeviceInquiry,
                };
            }

            // Universal Realtime (7F)
            if first == manufacturers::UNIVERSAL_REALTIME && full_data.len() >= 3 {
                let sub_id = full_data[2];
                return match sub_id {
                    0x06 => SysExCategory::ParameterQuery,  // MMC
                    0x04 => SysExCategory::ParameterChange, // Device Control
                    _ => SysExCategory::ParameterChange,
                };
            }
        }

        // Check for dangerous patterns in the data
        if Self::looks_like_firmware_update(full_data, after_manufacturer) {
            return SysExCategory::FirmwareUpdate;
        }

        if Self::looks_like_factory_reset(after_manufacturer) {
            return SysExCategory::FactoryReset;
        }

        // Check for known safe patterns
        if Self::is_parameter_query(after_manufacturer) {
            return SysExCategory::ParameterQuery;
        }

        // Unknown manufacturer with substantial data is higher risk
        if full_data.len() > 1 && Self::is_unknown_manufacturer(full_data) {
            return SysExCategory::UnknownManufacturer;
        }

        // Default to parameter change (moderate risk)
        if after_manufacturer.is_empty() {
            SysExCategory::DeviceInquiry
        } else {
            SysExCategory::ParameterChange
        }
    }

    /// Check if message looks like a firmware update
    fn looks_like_firmware_update(full_data: &[u8], after_manufacturer: &[u8]) -> bool {
        // Common firmware update indicators
        let firmware_keywords: [&[u8]; 4] = [
            &[0x01, 0x00], // Common firmware header
            &[0x7F, 0x00], // Bootloader mode
            b"FIRM",       // ASCII "FIRM"
            b"BOOT",       // ASCII "BOOT"
        ];

        for keyword in &firmware_keywords {
            if after_manufacturer
                .windows(keyword.len())
                .any(|w| w == *keyword)
            {
                return true;
            }
        }

        // Check for large data blocks (>1KB) which might be firmware
        if full_data.len() > 1024 {
            // Large blocks with high entropy might be firmware
            // For now, just flag large unknown blocks
            return true;
        }

        false
    }

    /// Check if message looks like a factory reset
    fn looks_like_factory_reset(after_manufacturer: &[u8]) -> bool {
        // Common factory reset patterns
        let reset_patterns: [&[u8]; 3] = [
            &[0x7F, 0x7F, 0x7F], // All-notes-off style reset
            &[0x09, 0x00],       // Common reset command
            &[0x0F, 0x7F],       // Full reset
        ];

        for pattern in &reset_patterns {
            if after_manufacturer.starts_with(pattern) {
                return true;
            }
        }

        false
    }

    /// Check if message is a parameter query (read-only)
    fn is_parameter_query(after_manufacturer: &[u8]) -> bool {
        // Common query patterns
        if after_manufacturer.is_empty() {
            return true; // Inquiry only
        }

        // Native Instruments parameter query
        if after_manufacturer.len() >= 2 && after_manufacturer[0] == 0x60 {
            return true;
        }

        false
    }

    /// Check if manufacturer is unknown
    fn is_unknown_manufacturer(data: &[u8]) -> bool {
        if data.is_empty() {
            return true;
        }

        let first = data[0];

        // Check if it's a known manufacturer
        if first == manufacturers::UNIVERSAL_NON_REALTIME
            || first == manufacturers::UNIVERSAL_REALTIME
            || first == manufacturers::ROLAND
            || first == manufacturers::YAMAHA
            || first == manufacturers::KORG
            || first == manufacturers::AKAI
        {
            return false;
        }

        // Check extended IDs
        if first == 0x00 && data.len() >= 3 {
            let ext_id = &data[0..3];
            if ext_id == manufacturers::NATIVE_INSTRUMENTS
                || ext_id == manufacturers::NOVATION
                || ext_id == manufacturers::ABLETON
            {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_request() {
        // Universal Non-Realtime Identity Request: F0 7E 00 06 01 F7
        let data = &[0x7E, 0x00, 0x06, 0x01];
        let result = SysExValidator::validate(data);

        assert_eq!(result.category, SysExCategory::IdentityRequest);
        assert!(result.allowed);
        assert_eq!(result.risk_level, "low");
    }

    #[test]
    fn test_roland_parameter_query() {
        // Roland parameter query
        let data = &[0x41, 0x10, 0x00, 0x00, 0x00, 0x11];
        let result = SysExValidator::validate(data);

        assert_eq!(result.manufacturer_name, Some("Roland".to_string()));
        assert!(result.allowed);
    }

    #[test]
    fn test_native_instruments_message() {
        // Native Instruments extended ID
        let data = &[0x00, 0x21, 0x09, 0x60, 0x01];
        let result = SysExValidator::validate(data);

        assert_eq!(
            result.manufacturer_name,
            Some("Native Instruments".to_string())
        );
        assert_eq!(result.category, SysExCategory::ParameterQuery);
        assert!(result.allowed);
    }

    #[test]
    fn test_firmware_update_blocked() {
        // Simulated firmware update (large data block)
        let mut data = vec![0x00, 0x21, 0x09]; // NI manufacturer ID
        data.extend(vec![0x00; 2000]); // Large data block

        let result = SysExValidator::validate(&data);

        assert_eq!(result.category, SysExCategory::FirmwareUpdate);
        assert!(!result.allowed);
        assert!(result.reason.is_some());
    }

    #[test]
    fn test_empty_message() {
        let result = SysExValidator::validate(&[]);

        assert_eq!(result.category, SysExCategory::Malformed);
        assert!(!result.allowed);
    }

    #[test]
    fn test_unknown_manufacturer() {
        // Unknown single-byte manufacturer ID with data
        let data = &[0x55, 0x01, 0x02, 0x03];
        let result = SysExValidator::validate(data);

        assert_eq!(result.category, SysExCategory::UnknownManufacturer);
        assert_eq!(result.risk_level, "high");
        assert!(result.allowed); // Allowed but requires confirmation
    }

    #[test]
    fn test_factory_reset_detection() {
        // Factory reset pattern - data after manufacturer starts with 0x7F, 0x7F, 0x7F
        let data = &[0x41, 0x7F, 0x7F, 0x7F];
        let result = SysExValidator::validate(data);

        assert_eq!(result.category, SysExCategory::FactoryReset);
        assert_eq!(result.risk_level, "high");
    }

    #[test]
    fn test_category_requires_confirmation() {
        assert!(!SysExCategory::IdentityRequest.requires_confirmation());
        assert!(!SysExCategory::DeviceInquiry.requires_confirmation());
        assert!(!SysExCategory::ParameterQuery.requires_confirmation());
        assert!(SysExCategory::ParameterChange.requires_confirmation());
        assert!(SysExCategory::PresetDump.requires_confirmation());
        assert!(SysExCategory::SampleDump.requires_confirmation());
        assert!(SysExCategory::FactoryReset.requires_confirmation());
        assert!(SysExCategory::UnknownManufacturer.requires_confirmation());
    }

    #[test]
    fn test_category_is_blocked() {
        assert!(!SysExCategory::IdentityRequest.is_blocked());
        assert!(!SysExCategory::ParameterChange.is_blocked());
        assert!(!SysExCategory::FactoryReset.is_blocked());
        assert!(SysExCategory::FirmwareUpdate.is_blocked());
    }
}
