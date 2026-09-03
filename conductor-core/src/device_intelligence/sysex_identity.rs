// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! SysEx Identity probing and parsing (ADR-022 Phase 5A)
//!
//! Sends MIDI Identity Request (F0 7E 7F 06 01 F7) and parses the reply
//! into a structured SysExIdentity.

use serde::{Deserialize, Serialize};

/// MIDI Identity Request message (Universal SysEx).
pub const IDENTITY_REQUEST: [u8; 6] = [0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7];

/// Cheap structural check for the Identity Reply frame shape — covers
/// the universal `F0 7E <dev> 06 02 ...` / `F7`-terminated form without
/// a full parse. Intended for hot-path SysEx filtering where avoiding
/// even the small allocation inside [`SysExIdentity::parse`] is worth
/// the code duplication. Call this BEFORE [`SysExIdentity::parse`] to
/// short-circuit unrelated SysEx. Keep the two in sync — if `parse`
/// grows stricter, tighten this predicate too.
pub fn is_identity_reply_shape(bytes: &[u8]) -> bool {
    // Valid lengths: exactly 15 (1-byte mfr) or 17 (3-byte mfr).
    //   15 bytes: F0 7E dev 06 02 mfr(1) fam(2) mod(2) ver(4) F7
    //   17 bytes: F0 7E dev 06 02 00 mfr(2) fam(2) mod(2) ver(4) F7
    //
    // Anything else is rejected per ADR-026 D2 strict rules. Vendor-
    // extended Identity Replies (e.g. Roland's GS Device Inquiry +
    // extra state bytes) fall outside the universal spec; the target
    // out-of-scope is called out in ADR-026 D10. A future vendor-
    // specific parser, if added, should slot in AFTER this universal
    // filter so well-behaved hardware continues to identify normally.
    if bytes.len() != 15 && bytes.len() != 17 {
        return false;
    }
    if bytes[0] != 0xF0
        || bytes[1] != 0x7E
        || bytes[3] != 0x06
        || bytes[4] != 0x02
        || bytes.last() != Some(&0xF7)
    {
        return false;
    }
    // Length-vs-manufacturer-form cross-check:
    //   len==15 MUST mean 1-byte mfr, so bytes[5] != 0x00.
    //   len==17 MUST mean 3-byte mfr, so bytes[5] == 0x00.
    match (bytes.len(), bytes[5]) {
        (15, 0x00) => return false, // truncated 3-byte-mfr frame masquerading as 1-byte
        (17, b) if b != 0x00 => return false, // overlong 1-byte-mfr frame with junk bytes
        _ => {}
    }
    // All payload bytes between the header and the F7 terminator must
    // be 7-bit clean (MIDI data-byte invariant). A single bit-7 byte in
    // the mfr / family / model / version region means the reply is
    // either truncated mid-message or coming from non-compliant firmware;
    // either way, we refuse to cache a bogus identity. `bytes[0..5]` are
    // already verified as the fixed header; `bytes.last()` is the F7
    // terminator; everything else must be <= 0x7F.
    if bytes[5..bytes.len() - 1].iter().any(|&b| b > 0x7F) {
        return false;
    }
    true
}

/// Parsed SysEx Identity Reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SysExIdentity {
    /// Manufacturer ID (1 or 3 bytes)
    pub manufacturer_id: Vec<u8>,
    /// Device family (2 bytes)
    pub family: u16,
    /// Device model (2 bytes)
    pub model: u16,
    /// Software/firmware version (4 bytes)
    pub version: [u8; 4],
}

impl SysExIdentity {
    /// Parse a SysEx Identity Reply message.
    ///
    /// Expected format: F0 7E <channel> 06 02 <manufacturer> <family_lo> <family_hi>
    ///                   <model_lo> <model_hi> <v1> <v2> <v3> <v4> F7
    pub fn parse(data: &[u8]) -> Option<Self> {
        // Delegate structural validation to `is_identity_reply_shape` so
        // the shape predicate and the parser can never drift. Any frame
        // that doesn't pass the shape check fails here too.
        if !is_identity_reply_shape(data) {
            return None;
        }

        // Manufacturer-form branch: length cross-check is already
        // guaranteed by the shape predicate (len==17 ↔ bytes[5]==0x00,
        // len==15 ↔ bytes[5]!=0x00), so we just read without re-checking.
        let (manufacturer_id, offset) = if data[5] == 0x00 {
            (vec![data[5], data[6], data[7]], 8)
        } else {
            (vec![data[5]], 6)
        };

        let family = u16::from_le_bytes([data[offset], data[offset + 1]]);
        let model = u16::from_le_bytes([data[offset + 2], data[offset + 3]]);
        let version = [
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ];

        Some(SysExIdentity {
            manufacturer_id,
            family,
            model,
            version,
        })
    }

    /// Get manufacturer name from ID (common manufacturers).
    pub fn manufacturer_name(&self) -> &str {
        match self.manufacturer_id.as_slice() {
            [0x00, 0x21, 0x09] => "Native Instruments",
            [0x00, 0x20, 0x29] => "Novation",
            [0x42] => "KORG",
            [0x47] => "Akai",
            [0x41] => "Roland",
            [0x43] => "Yamaha",
            _ => "Unknown",
        }
    }
}

/// Matcher for SysEx Identity — matches against stored identity data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SysExIdentityMatcher {
    pub manufacturer_id: Vec<u8>,
    pub family: Option<u16>,
    pub model: Option<u16>,
}

impl SysExIdentityMatcher {
    /// Check if this matcher matches an identity.
    pub fn matches(&self, identity: &SysExIdentity) -> bool {
        if self.manufacturer_id != identity.manufacturer_id {
            return false;
        }
        if let Some(f) = self.family
            && f != identity.family
        {
            return false;
        }
        if let Some(m) = self.model
            && m != identity.model
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_identity_reply_1byte_mfr() {
        // KORG identity reply
        let data = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, // header
            0x42, // KORG
            0x34, 0x00, // family
            0x01, 0x00, // model
            0x01, 0x02, 0x03, 0x04, // version
            0xF7,
        ];
        let id = SysExIdentity::parse(&data).unwrap();
        assert_eq!(id.manufacturer_id, vec![0x42]);
        assert_eq!(id.family, 0x0034);
        assert_eq!(id.model, 0x0001);
        assert_eq!(id.manufacturer_name(), "KORG");
    }

    #[test]
    fn test_parse_identity_reply_3byte_mfr() {
        // Native Instruments identity reply
        let data = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, // header
            0x00, 0x21, 0x09, // NI
            0x00, 0x16, // family
            0x00, 0x00, // model
            0x01, 0x00, 0x05, 0x00, // version
            0xF7,
        ];
        let id = SysExIdentity::parse(&data).unwrap();
        assert_eq!(id.manufacturer_id, vec![0x00, 0x21, 0x09]);
        assert_eq!(id.manufacturer_name(), "Native Instruments");
    }

    #[test]
    fn test_parse_invalid_too_short() {
        assert!(SysExIdentity::parse(&[0xF0, 0x7E]).is_none());
    }

    #[test]
    fn test_parse_rejects_truncated_3byte_mfr_frame() {
        // bytes[5] == 0x00 indicates 3-byte manufacturer ID, so the
        // minimum valid frame is 17 bytes. A 15-byte frame with a
        // leading 0x00 is malformed — must return None (prior impl
        // silently produced `manufacturer_id = [0x00]`, which is not a
        // valid 1-byte MMA manufacturer).
        let truncated_15 = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x00, 0x21, 0x09, 0x00, 0x16, 0x00, 0x00, 0x01, 0x00,
            0xF7,
        ];
        assert_eq!(truncated_15.len(), 15);
        assert!(SysExIdentity::parse(&truncated_15).is_none());

        let truncated_16 = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x00, 0x21, 0x09, 0x00, 0x16, 0x00, 0x00, 0x01, 0x00,
            0x05, 0xF7,
        ];
        assert_eq!(truncated_16.len(), 16);
        assert!(SysExIdentity::parse(&truncated_16).is_none());
    }

    #[test]
    fn test_parse_rejects_length_other_than_15_or_17() {
        // ADR-026 D2: exactly 15 or 17 bytes. 16, 18, 20, 100 must all
        // be rejected — previously any `len >= 15` passed through with
        // truncated fields. Vendor-extended replies fall here too; the
        // universal-only parser deliberately doesn't handle them.
        let base_16 = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0x00, 0xF7,
        ];
        assert_eq!(base_16.len(), 16);
        assert!(SysExIdentity::parse(&base_16).is_none());

        let base_18 = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0x00, 0x00, 0x00, 0xF7,
        ];
        assert_eq!(base_18.len(), 18);
        assert!(SysExIdentity::parse(&base_18).is_none());

        let base_20 = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0x00, 0x00, 0x00, 0x00, 0x00, 0xF7,
        ];
        assert_eq!(base_20.len(), 20);
        assert!(SysExIdentity::parse(&base_20).is_none());
    }

    #[test]
    fn test_parse_rejects_non_7bit_payload_byte() {
        // Any byte between bytes[5] and the final F7 with bit 7 set is
        // a MIDI-protocol violation. Must be rejected without allocating
        // a bogus identity.
        //
        // Put 0x80 in the manufacturer slot of an otherwise-valid 15-byte frame.
        let bad_mfr = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x80, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0xF7,
        ];
        assert!(SysExIdentity::parse(&bad_mfr).is_none());

        // Put 0xFF in the version region.
        let bad_version = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0xFF, 0x03, 0x04,
            0xF7,
        ];
        assert!(SysExIdentity::parse(&bad_version).is_none());
    }

    #[test]
    fn test_parse_rejects_overlong_1byte_mfr_frame() {
        // A 17-byte frame with bytes[5] != 0x00 would be junk-extended
        // 1-byte-mfr — reject rather than misinterpret as 3-byte-mfr.
        let bogus_17 = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0x00, 0x00, 0xF7,
        ];
        assert_eq!(bogus_17.len(), 17);
        assert_ne!(bogus_17[5], 0x00); // 1-byte mfr form
        assert!(SysExIdentity::parse(&bogus_17).is_none());
    }

    #[test]
    fn test_is_identity_reply_shape_rejects_truncated_3byte_mfr() {
        // Matches the parse rejection above — the hot-path predicate
        // must also reject 15/16-byte frames with bytes[5] == 0x00.
        let truncated_15 = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x00, 0x21, 0x09, 0x00, 0x16, 0x00, 0x00, 0x01, 0x00,
            0xF7,
        ];
        assert!(!is_identity_reply_shape(&truncated_15));

        // Sanity: the valid 17-byte 3-byte-mfr frame still passes.
        let valid_17 = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x00, 0x21, 0x09, 0x00, 0x16, 0x00, 0x00, 0x01, 0x00,
            0x05, 0x00, 0xF7,
        ];
        assert!(is_identity_reply_shape(&valid_17));

        // Sanity: 15-byte 1-byte-mfr frame still passes (bytes[5] != 0x00).
        let valid_15 = [
            0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04,
            0xF7,
        ];
        assert!(is_identity_reply_shape(&valid_15));
    }

    #[test]
    fn test_matcher_matches() {
        let id = SysExIdentity {
            manufacturer_id: vec![0x42],
            family: 0x0034,
            model: 0x0001,
            version: [1, 2, 3, 4],
        };
        let matcher = SysExIdentityMatcher {
            manufacturer_id: vec![0x42],
            family: Some(0x0034),
            model: None,
        };
        assert!(matcher.matches(&id));
    }

    #[test]
    fn test_matcher_mismatch() {
        let id = SysExIdentity {
            manufacturer_id: vec![0x42],
            family: 0x0034,
            model: 0x0001,
            version: [1, 2, 3, 4],
        };
        let matcher = SysExIdentityMatcher {
            manufacturer_id: vec![0x41], // Roland, not KORG
            family: None,
            model: None,
        };
        assert!(!matcher.matches(&id));
    }
}
