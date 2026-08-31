// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Multi-step confirmation for HardwareIO operations
//!
//! Provides a two-phase confirmation flow:
//! 1. Request operation → receive confirmation token with risk assessment
//! 2. Submit token → execute operation (if not expired)

use super::sysex::{SysExCategory, SysExValidation, SysExValidator};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use thiserror::Error;
use uuid::Uuid;

/// Token expiration time (60 seconds)
const TOKEN_EXPIRATION_SECS: u64 = 60;

/// A MIDI message to send via conductor_send_midi (v4.26.67)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidiSendMessage {
    /// Message type: "note_on", "note_off", "cc", "program_change"
    #[serde(rename = "type")]
    pub message_type: String,
    /// MIDI channel (1-16)
    pub channel: u8,
    /// Note number (0-127) for note_on/note_off
    pub note: Option<u8>,
    /// Velocity (0-127) for note_on/note_off
    pub velocity: Option<u8>,
    /// Controller number (0-127) for cc
    pub controller: Option<u8>,
    /// Value (0-127) for cc
    pub value: Option<u8>,
    /// Program number (0-127) for program_change
    pub program: Option<u8>,
}

impl MidiSendMessage {
    /// Validate the message and return an error string if invalid
    pub fn validate(&self) -> Result<(), String> {
        if self.channel < 1 || self.channel > 16 {
            return Err(format!("Channel {} out of range (1-16)", self.channel));
        }
        match self.message_type.as_str() {
            "note_on" | "note_off" => {
                let note = self.note.ok_or("Missing 'note' for note message")?;
                if note > 127 {
                    return Err(format!("Note {} out of range (0-127)", note));
                }
                let vel = self.velocity.unwrap_or(if self.message_type == "note_on" {
                    100
                } else {
                    0
                });
                if vel > 127 {
                    return Err(format!("Velocity {} out of range (0-127)", vel));
                }
            }
            "cc" => {
                let cc = self
                    .controller
                    .ok_or("Missing 'controller' for CC message")?;
                if cc > 127 {
                    return Err(format!("Controller {} out of range (0-127)", cc));
                }
                let val = self.value.ok_or("Missing 'value' for CC message")?;
                if val > 127 {
                    return Err(format!("Value {} out of range (0-127)", val));
                }
            }
            "program_change" => {
                let prog = self.program.ok_or("Missing 'program' for program_change")?;
                if prog > 127 {
                    return Err(format!("Program {} out of range (0-127)", prog));
                }
            }
            other => {
                return Err(format!(
                    "Unknown message type: '{}'. Valid: note_on, note_off, cc, program_change",
                    other
                ));
            }
        }
        Ok(())
    }

    /// Convert to raw MIDI bytes (3 bytes for most messages, 2 for program_change)
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let ch = self.channel - 1; // MIDI channels are 0-indexed in bytes
        match self.message_type.as_str() {
            "note_on" => {
                let note = self.note.unwrap_or(60);
                let vel = self.velocity.unwrap_or(100);
                Ok(vec![0x90 | ch, note, vel])
            }
            "note_off" => {
                let note = self.note.unwrap_or(60);
                let vel = self.velocity.unwrap_or(0);
                Ok(vec![0x80 | ch, note, vel])
            }
            "cc" => {
                let cc = self.controller.unwrap_or(0);
                let val = self.value.unwrap_or(0);
                Ok(vec![0xB0 | ch, cc, val])
            }
            "program_change" => {
                let prog = self.program.unwrap_or(0);
                Ok(vec![0xC0 | ch, prog])
            }
            _ => Err(format!("Unknown type: {}", self.message_type)),
        }
    }
}

/// Error types for HardwareIO operations
#[derive(Debug, Error)]
pub enum HardwareIoError {
    #[error("Operation blocked: {0}")]
    Blocked(String),

    #[error("Confirmation required - use provided token to confirm")]
    ConfirmationRequired,

    #[error("Invalid or expired confirmation token")]
    InvalidToken,

    #[error("Token has expired (valid for {0} seconds)")]
    TokenExpired(u64),

    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    #[error("MIDI output error: {0}")]
    MidiError(String),

    #[error("Invalid SysEx data: {0}")]
    InvalidSysEx(String),
}

/// Confirmation token for HardwareIO operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmationToken {
    /// Unique token ID
    pub id: String,
    /// Timestamp when token was created (Unix epoch seconds)
    pub created_at: i64,
    /// Timestamp when token expires (Unix epoch seconds)
    pub expires_at: i64,
}

impl ConfirmationToken {
    /// Create a new confirmation token
    pub fn new() -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id: Uuid::new_v4().to_string(),
            created_at: now,
            expires_at: now + TOKEN_EXPIRATION_SECS as i64,
        }
    }

    /// Check if the token has expired
    pub fn is_expired(&self) -> bool {
        chrono::Utc::now().timestamp() > self.expires_at
    }

    /// Get remaining validity time in seconds
    pub fn remaining_secs(&self) -> i64 {
        (self.expires_at - chrono::Utc::now().timestamp()).max(0)
    }
}

impl Default for ConfirmationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Status of a confirmation request
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum ConfirmationStatus {
    /// Confirmation is required - return this to user
    #[serde(rename = "requires_confirmation")]
    RequiresConfirmation {
        token: ConfirmationToken,
        risk_assessment: RiskAssessment,
        message: String,
    },

    /// Operation was confirmed and executed
    #[serde(rename = "confirmed")]
    Confirmed { result: String },

    /// Operation was blocked (not allowed)
    #[serde(rename = "blocked")]
    Blocked { reason: String },

    /// Token was invalid or expired
    #[serde(rename = "invalid_token")]
    InvalidToken { reason: String },
}

/// Risk assessment for a HardwareIO operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAssessment {
    /// Risk level (low, medium, high, critical)
    pub level: String,
    /// Category of operation
    pub category: String,
    /// Human-readable description of risks
    pub description: String,
    /// Warnings to display to user
    pub warnings: Vec<String>,
    /// Whether this operation is reversible
    pub reversible: bool,
}

/// Pending confirmation request
#[derive(Debug, Clone)]
pub struct ConfirmationRequest {
    /// The token for this request
    pub token: ConfirmationToken,
    /// Type of operation
    pub operation_type: String,
    /// Device target
    pub device: String,
    /// Operation data (e.g., SysEx bytes)
    pub data: Vec<u8>,
    /// When this request was created
    pub created: Instant,
    /// Validation result
    pub validation: Option<SysExValidation>,
}

/// Manager for confirmation tokens
pub struct ConfirmationManager {
    /// Pending confirmations
    pending: Arc<Mutex<HashMap<String, ConfirmationRequest>>>,
}

impl ConfirmationManager {
    /// Create a new confirmation manager
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Request confirmation for a SysEx operation
    ///
    /// Returns ConfirmationStatus indicating whether confirmation is needed
    pub fn request_sysex_confirmation(
        &self,
        device: &str,
        data: &[u8],
        existing_token: Option<&str>,
    ) -> Result<ConfirmationStatus, HardwareIoError> {
        // #1619: drop any expired pending tokens before any per-call
        // work so the HashMap stays bounded under abuse.
        self.cleanup_expired();

        // Validate the SysEx data
        let validation = SysExValidator::validate(data);

        // Check if blocked
        if !validation.allowed {
            return Ok(ConfirmationStatus::Blocked {
                reason: validation
                    .reason
                    .unwrap_or_else(|| "Operation not allowed".to_string()),
            });
        }

        // If we have an existing token, try to use it. #1477: the
        // token MUST be bound to the same (op type, device, payload)
        // it was issued for — `confirm_operation` enforces this and
        // rejects mismatches as `InvalidToken`.
        if let Some(token_id) = existing_token {
            return self.confirm_operation(token_id, "sysex", device, data);
        }

        // Check if confirmation is required
        if !validation.category.requires_confirmation() {
            // Low-risk operation, auto-approve
            return Ok(ConfirmationStatus::Confirmed {
                result: "Operation approved (low risk)".to_string(),
            });
        }

        // Create confirmation request
        let token = ConfirmationToken::new();
        let token_id = token.id.clone();

        let request = ConfirmationRequest {
            token: token.clone(),
            operation_type: "sysex".to_string(),
            device: device.to_string(),
            data: data.to_vec(),
            created: Instant::now(),
            validation: Some(validation.clone()),
        };

        // Store the request
        {
            let mut pending = self.pending.lock().unwrap();
            pending.insert(token_id, request);
        }

        // Build risk assessment
        let risk_assessment = RiskAssessment {
            level: validation.risk_level.clone(),
            category: format!("{:?}", validation.category),
            description: validation.description.clone(),
            warnings: self.build_warnings(&validation),
            reversible: !matches!(
                validation.category,
                SysExCategory::FirmwareUpdate | SysExCategory::FactoryReset
            ),
        };

        Ok(ConfirmationStatus::RequiresConfirmation {
            token,
            risk_assessment,
            message: format!(
                "This operation requires confirmation. Risk level: {}. {}",
                validation.risk_level, validation.description
            ),
        })
    }

    /// Request confirmation for a device reset operation
    pub fn request_reset_confirmation(
        &self,
        device: &str,
        reset_type: &str,
        existing_token: Option<&str>,
    ) -> Result<ConfirmationStatus, HardwareIoError> {
        // #1619: drop any expired pending tokens before any per-call
        // work so the HashMap stays bounded under abuse.
        self.cleanup_expired();

        // If we have an existing token, try to use it. #1477: bind
        // the token to (op_type="reset", device, payload=reset_type
        // bytes) — the same triple stored on the issue path below.
        if let Some(token_id) = existing_token {
            return self.confirm_operation(token_id, "reset", device, reset_type.as_bytes());
        }

        // All resets require confirmation
        let token = ConfirmationToken::new();
        let token_id = token.id.clone();

        let (risk_level, reversible) = match reset_type {
            "soft" => ("medium", true),
            "hard" => ("high", false),
            "factory" => ("high", false),
            _ => ("high", false),
        };

        let request = ConfirmationRequest {
            token: token.clone(),
            operation_type: "reset".to_string(),
            device: device.to_string(),
            data: reset_type.as_bytes().to_vec(),
            created: Instant::now(),
            validation: None,
        };

        // Store the request
        {
            let mut pending = self.pending.lock().unwrap();
            pending.insert(token_id, request);
        }

        let risk_assessment = RiskAssessment {
            level: risk_level.to_string(),
            category: "DeviceReset".to_string(),
            description: match reset_type {
                "soft" => "Soft reset - clears temporary state".to_string(),
                "hard" => "Hard reset - reinitializes device".to_string(),
                "factory" => {
                    "Factory reset - erases all settings and returns to defaults".to_string()
                }
                _ => format!("Unknown reset type: {}", reset_type),
            },
            warnings: match reset_type {
                "soft" => vec!["Device may briefly disconnect".to_string()],
                "hard" => vec![
                    "Device will restart".to_string(),
                    "Unsaved changes may be lost".to_string(),
                ],
                "factory" => vec![
                    "ALL settings will be erased".to_string(),
                    "Custom presets will be deleted".to_string(),
                    "This cannot be undone".to_string(),
                ],
                _ => vec![],
            },
            reversible,
        };

        Ok(ConfirmationStatus::RequiresConfirmation {
            token,
            risk_assessment,
            message: format!(
                "Device {} reset ({}) requires confirmation. This operation may not be reversible.",
                reset_type, device
            ),
        })
    }

    /// Confirm a pending operation, binding the token to its issued
    /// operation descriptor.
    ///
    /// `expected_op_type`, `expected_device`, and `expected_payload`
    /// MUST match the stored `ConfirmationRequest` exactly. If they
    /// don't, the token is consumed (removed) and `InvalidToken` is
    /// returned — preventing the confused-deputy attack from #1477
    /// where a token issued for one operation could approve another.
    ///
    /// Mismatched-descriptor tokens are consumed (not left in `pending`)
    /// so an attacker can't retry until they guess a matching
    /// descriptor. The user will see the legitimate operation as
    /// "token not found" on their retry; that's the correct posture —
    /// the token was tampered with and is no longer trustworthy.
    fn confirm_operation(
        &self,
        token_id: &str,
        expected_op_type: &str,
        expected_device: &str,
        expected_payload: &[u8],
    ) -> Result<ConfirmationStatus, HardwareIoError> {
        let mut pending = self.pending.lock().unwrap();

        let request = match pending.remove(token_id) {
            Some(r) => r,
            None => {
                return Ok(ConfirmationStatus::InvalidToken {
                    reason: "Token not found or already used".to_string(),
                });
            }
        };

        // Check if expired (before descriptor match, so callers see
        // the more specific expiration reason rather than a generic
        // mismatch when both are true).
        if request.token.is_expired() {
            return Ok(ConfirmationStatus::InvalidToken {
                reason: format!(
                    "Token expired (was valid for {} seconds)",
                    TOKEN_EXPIRATION_SECS
                ),
            });
        }

        // #1477: bind the token to its issued operation. Reject any
        // mismatch on operation type, target device, or payload bytes.
        // The mismatch reason names what the token was for vs. what
        // was attempted so legitimate flow errors are diagnosable
        // without leaking the stored payload bytes.
        if request.operation_type != expected_op_type
            || request.device != expected_device
            || request.data != expected_payload
        {
            return Ok(ConfirmationStatus::InvalidToken {
                reason: format!(
                    "Token does not match the operation it was issued for \
                     (issued: {} on '{}', attempted: {} on '{}')",
                    request.operation_type, request.device, expected_op_type, expected_device,
                ),
            });
        }

        Ok(ConfirmationStatus::Confirmed {
            result: format!(
                "Operation {} on device '{}' confirmed and ready to execute",
                request.operation_type, request.device
            ),
        })
    }

    /// Get a pending request by token ID
    pub fn get_pending(&self, token_id: &str) -> Option<ConfirmationRequest> {
        let pending = self.pending.lock().unwrap();
        pending.get(token_id).cloned()
    }

    /// Clean up expired tokens
    pub fn cleanup_expired(&self) -> usize {
        let mut pending = self.pending.lock().unwrap();
        let before_count = pending.len();

        pending.retain(|_, request| !request.token.is_expired());

        before_count - pending.len()
    }

    /// Build warning messages for a SysEx validation
    fn build_warnings(&self, validation: &SysExValidation) -> Vec<String> {
        let mut warnings = Vec::new();

        match validation.category {
            SysExCategory::ParameterChange => {
                warnings.push("This will modify device settings".to_string());
            }
            SysExCategory::PresetDump => {
                warnings.push("This may overwrite existing presets on the device".to_string());
            }
            SysExCategory::SampleDump => {
                warnings.push("This may overwrite existing samples on the device".to_string());
                warnings.push("Large transfers may take time to complete".to_string());
            }
            SysExCategory::FactoryReset => {
                warnings.push("ALL device settings will be erased".to_string());
                warnings.push("Custom presets and samples will be deleted".to_string());
                warnings.push("This cannot be undone".to_string());
            }
            SysExCategory::UnknownManufacturer => {
                warnings.push("Unknown manufacturer - cannot verify safety".to_string());
                warnings.push("Proceed with caution".to_string());
            }
            _ => {}
        }

        if validation.manufacturer_name.is_none()
            && validation.category != SysExCategory::UnknownManufacturer
        {
            warnings.push("Manufacturer could not be identified".to_string());
        }

        warnings
    }

    /// Request confirmation for a MIDI send operation (v4.26.67)
    ///
    /// Standard MIDI messages (note_on, note_off, cc, program_change) are low risk
    /// and auto-confirm. They still get audit-logged via the HardwareIO path.
    pub fn request_midi_send_confirmation(
        &self,
        port: &str,
        messages: &[MidiSendMessage],
        existing_token: Option<&str>,
    ) -> Result<ConfirmationStatus, HardwareIoError> {
        // #1619: drop any expired pending tokens before any per-call
        // work so the HashMap stays bounded under abuse.
        self.cleanup_expired();

        // If a token was supplied, validate it via the bound-descriptor
        // path. MIDI send auto-confirms and never stores a pending
        // request, so any stored token must be for a different
        // operation (sysex or reset) — and `confirm_operation` will
        // reject the op_type mismatch as `InvalidToken`. This blocks
        // the #1477 replay where a SysEx/reset token would have been
        // accepted here to confirm an unrelated MIDI send.
        //
        // The payload bytes don't matter for the rejection (op_type
        // mismatch alone catches it), but we pass a canonical
        // serialisation so that a future MIDI flow that DOES store
        // pending requests gets a stable descriptor for free.
        if let Some(token_id) = existing_token {
            // `MidiSendMessage` has a derived `Serialize` impl over
            // primitives, so `to_vec` is infallible — make the
            // invariant explicit rather than silently producing an
            // empty descriptor on a phantom error path.
            let payload = serde_json::to_vec(messages)
                .expect("MidiSendMessage uses derived Serialize over primitives — infallible");
            return self.confirm_operation(token_id, "midi_send", port, &payload);
        }

        // #1618: validate each message at the gate. A malformed message
        // (channel out of range, missing required field) used to slip
        // past here and surface later in the executor's `to_bytes()`
        // call as a less-contextual error. Block at the gate with the
        // offending index so the client knows exactly which message to
        // fix.
        for (i, msg) in messages.iter().enumerate() {
            if let Err(e) = msg.validate() {
                return Ok(ConfirmationStatus::Blocked {
                    reason: format!("message {i}: {e}"),
                });
            }
        }

        // Standard MIDI messages are low risk — auto-confirm
        Ok(ConfirmationStatus::Confirmed {
            result: format!(
                "Sending {} MIDI message(s) to port '{}'",
                messages.len(),
                port
            ),
        })
    }

    /// Get pending confirmation count
    pub fn pending_count(&self) -> usize {
        let pending = self.pending.lock().unwrap();
        pending.len()
    }
}

impl Default for ConfirmationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_creation() {
        let token = ConfirmationToken::new();
        assert!(!token.is_expired());
        assert!(token.remaining_secs() > 0);
        assert!(token.remaining_secs() <= TOKEN_EXPIRATION_SECS as i64);
    }

    #[test]
    fn test_low_risk_auto_approved() {
        let manager = ConfirmationManager::new();

        // Identity request is low risk
        let data = &[0x7E, 0x00, 0x06, 0x01];
        let result = manager
            .request_sysex_confirmation("Test Device", data, None)
            .unwrap();

        match result {
            ConfirmationStatus::Confirmed { .. } => {}
            _ => panic!("Expected auto-approval for low-risk operation"),
        }
    }

    #[test]
    fn test_high_risk_requires_confirmation() {
        let manager = ConfirmationManager::new();

        // Unknown manufacturer with data
        let data = &[0x55, 0x01, 0x02, 0x03];
        let result = manager
            .request_sysex_confirmation("Test Device", data, None)
            .unwrap();

        match result {
            ConfirmationStatus::RequiresConfirmation {
                token,
                risk_assessment,
                ..
            } => {
                assert!(!token.id.is_empty());
                assert_eq!(risk_assessment.level, "high");
            }
            _ => panic!("Expected confirmation requirement"),
        }
    }

    #[test]
    fn test_confirmation_flow() {
        let manager = ConfirmationManager::new();

        // First request - get token
        let data = &[0x55, 0x01, 0x02, 0x03];
        let result1 = manager
            .request_sysex_confirmation("Test Device", data, None)
            .unwrap();

        let token_id = match result1 {
            ConfirmationStatus::RequiresConfirmation { token, .. } => token.id,
            _ => panic!("Expected confirmation requirement"),
        };

        // Second request with token - should confirm
        let result2 = manager
            .request_sysex_confirmation("Test Device", data, Some(&token_id))
            .unwrap();

        match result2 {
            ConfirmationStatus::Confirmed { .. } => {}
            _ => panic!("Expected confirmed status"),
        }
    }

    #[test]
    fn test_invalid_token() {
        let manager = ConfirmationManager::new();

        let data = &[0x55, 0x01, 0x02, 0x03];
        let result = manager
            .request_sysex_confirmation("Test Device", data, Some("invalid-token"))
            .unwrap();

        match result {
            ConfirmationStatus::InvalidToken { .. } => {}
            _ => panic!("Expected invalid token status"),
        }
    }

    #[test]
    fn test_blocked_operation() {
        let manager = ConfirmationManager::new();

        // Large data block looks like firmware update
        let mut data = vec![0x00, 0x21, 0x09]; // NI manufacturer
        data.extend(vec![0x00; 2000]); // Large block

        let result = manager
            .request_sysex_confirmation("Test Device", &data, None)
            .unwrap();

        match result {
            ConfirmationStatus::Blocked { reason } => {
                assert!(reason.contains("Blocked"));
            }
            _ => panic!("Expected blocked status"),
        }
    }

    #[test]
    fn test_reset_confirmation() {
        let manager = ConfirmationManager::new();

        let result = manager
            .request_reset_confirmation("Test Device", "factory", None)
            .unwrap();

        match result {
            ConfirmationStatus::RequiresConfirmation {
                risk_assessment, ..
            } => {
                assert_eq!(risk_assessment.level, "high");
                assert!(!risk_assessment.reversible);
                assert!(!risk_assessment.warnings.is_empty());
            }
            _ => panic!("Expected confirmation requirement for factory reset"),
        }
    }

    #[test]
    fn test_token_reuse_prevented() {
        let manager = ConfirmationManager::new();

        // Get a token
        let data = &[0x55, 0x01, 0x02, 0x03];
        let result1 = manager
            .request_sysex_confirmation("Test Device", data, None)
            .unwrap();

        let token_id = match result1 {
            ConfirmationStatus::RequiresConfirmation { token, .. } => token.id,
            _ => panic!("Expected confirmation requirement"),
        };

        // Use the token
        let _ = manager
            .request_sysex_confirmation("Test Device", data, Some(&token_id))
            .unwrap();

        // Try to use it again - should fail
        let result3 = manager
            .request_sysex_confirmation("Test Device", data, Some(&token_id))
            .unwrap();

        match result3 {
            ConfirmationStatus::InvalidToken { .. } => {}
            _ => panic!("Expected invalid token on reuse"),
        }
    }

    #[test]
    fn test_cleanup_expired() {
        let manager = ConfirmationManager::new();

        // Create a request
        let data = &[0x55, 0x01, 0x02, 0x03];
        let _ = manager
            .request_sysex_confirmation("Test Device", data, None)
            .unwrap();

        assert_eq!(manager.pending_count(), 1);

        // Cleanup shouldn't remove non-expired tokens
        let cleaned = manager.cleanup_expired();
        assert_eq!(cleaned, 0);
        assert_eq!(manager.pending_count(), 1);
    }

    // MidiSendMessage tests (v4.26.67)

    #[test]
    fn test_midi_send_note_on_to_bytes() {
        let msg = MidiSendMessage {
            message_type: "note_on".to_string(),
            channel: 1,
            note: Some(60),
            velocity: Some(100),
            controller: None,
            value: None,
            program: None,
        };
        assert!(msg.validate().is_ok());
        assert_eq!(msg.to_bytes().unwrap(), vec![0x90, 60, 100]);
    }

    #[test]
    fn test_midi_send_cc_to_bytes() {
        let msg = MidiSendMessage {
            message_type: "cc".to_string(),
            channel: 10,
            note: None,
            velocity: None,
            controller: Some(7),
            value: Some(64),
            program: None,
        };
        assert!(msg.validate().is_ok());
        assert_eq!(msg.to_bytes().unwrap(), vec![0xB9, 7, 64]); // 0xB0 | 9 (ch10 zero-indexed)
    }

    #[test]
    fn test_midi_send_program_change_to_bytes() {
        let msg = MidiSendMessage {
            message_type: "program_change".to_string(),
            channel: 2,
            note: None,
            velocity: None,
            controller: None,
            value: None,
            program: Some(5),
        };
        assert!(msg.validate().is_ok());
        assert_eq!(msg.to_bytes().unwrap(), vec![0xC1, 5]); // 0xC0 | 1 (ch2 zero-indexed)
    }

    #[test]
    fn test_midi_send_invalid_channel() {
        let msg = MidiSendMessage {
            message_type: "note_on".to_string(),
            channel: 17,
            note: Some(60),
            velocity: Some(100),
            controller: None,
            value: None,
            program: None,
        };
        assert!(msg.validate().is_err());
        assert!(msg.validate().unwrap_err().contains("Channel 17"));
    }

    #[test]
    fn test_midi_send_unknown_type() {
        let msg = MidiSendMessage {
            message_type: "pitch_bend".to_string(),
            channel: 1,
            note: None,
            velocity: None,
            controller: None,
            value: None,
            program: None,
        };
        assert!(msg.validate().is_err());
        assert!(msg.validate().unwrap_err().contains("Unknown message type"));
    }

    #[test]
    fn test_midi_send_confirmation_auto_confirms() {
        let manager = ConfirmationManager::new();
        let msgs = vec![MidiSendMessage {
            message_type: "note_on".to_string(),
            channel: 1,
            note: Some(60),
            velocity: Some(100),
            controller: None,
            value: None,
            program: None,
        }];
        let result = manager
            .request_midi_send_confirmation("Virtual Output", &msgs, None)
            .unwrap();
        match result {
            ConfirmationStatus::Confirmed { result } => {
                assert!(result.contains("1 MIDI message"));
                assert!(result.contains("Virtual Output"));
            }
            _ => panic!("Expected auto-confirm for MIDI send"),
        }
    }

    // ────────────────────────────────────────────────────────────────
    // #1477 — token binding (descriptor-match) sentinels.
    //
    // The exhaustive matrix of cross-operation, cross-device and
    // cross-payload replay rejections lives in the integration test
    // crate at `conductor-daemon/tests/confirmation_token_binding_1477.rs`.
    // These two in-module tests are kept here so a reviewer reading
    // `confirmation.rs` in isolation can see the binding behaviour is
    // covered. Keep them minimal — extend the integration tests instead.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn binding_1477_sysex_token_cannot_confirm_reset() {
        // Roland parameter-change SysEx — `SysExValidator` classifies
        // it as `ParameterChange`, which `requires_confirmation()`, so
        // a token is issued (and stored) rather than auto-approved.
        let manager = ConfirmationManager::new();
        let data = &[0xF0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x01, 0xF7];
        let token_id = match manager
            .request_sysex_confirmation("Device", data, None)
            .unwrap()
        {
            ConfirmationStatus::RequiresConfirmation { token, .. } => token.id,
            other => panic!("expected RequiresConfirmation, got {other:?}"),
        };

        // Replay the SysEx token against a reset on the same device.
        // Pre-#1477 this Confirmed the reset using the stored SysEx
        // request's identity — the confused-deputy bug.
        let result = manager
            .request_reset_confirmation("Device", "factory", Some(&token_id))
            .unwrap();
        assert!(
            matches!(result, ConfirmationStatus::InvalidToken { .. }),
            "sysex token must not confirm reset, got {result:?}"
        );
    }

    #[test]
    fn binding_1477_exact_match_still_confirms() {
        // Regression guard for the fix itself — a legitimate two-step
        // flow (same op type, same device, same payload) must continue
        // to confirm successfully.
        let manager = ConfirmationManager::new();
        let data = &[0xF0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7F, 0x01, 0xF7];
        let token_id = match manager
            .request_sysex_confirmation("Device", data, None)
            .unwrap()
        {
            ConfirmationStatus::RequiresConfirmation { token, .. } => token.id,
            other => panic!("expected RequiresConfirmation, got {other:?}"),
        };
        let result = manager
            .request_sysex_confirmation("Device", data, Some(&token_id))
            .unwrap();
        assert!(
            matches!(result, ConfirmationStatus::Confirmed { .. }),
            "exact-match descriptor must still confirm, got {result:?}"
        );
    }
}
