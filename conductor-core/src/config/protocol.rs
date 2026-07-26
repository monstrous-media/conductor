// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Protocol enum for multi-protocol binding support (ADR-022 Phase 4A)

use serde::{Deserialize, Serialize};

/// Communication protocol for a device binding.
///
/// Determines which port type the matchers should resolve against
/// and which event types are expected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// MIDI protocol (default) — note, CC, aftertouch, pitch bend events
    #[default]
    Midi,
    /// HID protocol — gamepad buttons, analog sticks, triggers
    Hid,
    /// OSC protocol — Open Sound Control messages over UDP
    Osc,
    /// Art-Net protocol — DMX over Ethernet for lighting
    #[serde(rename = "artnet")]
    ArtNet,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Midi => write!(f, "midi"),
            Protocol::Hid => write!(f, "hid"),
            Protocol::Osc => write!(f, "osc"),
            Protocol::ArtNet => write!(f, "artnet"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_midi() {
        assert_eq!(Protocol::default(), Protocol::Midi);
    }

    #[test]
    fn test_serde_roundtrip() {
        for protocol in [
            Protocol::Midi,
            Protocol::Hid,
            Protocol::Osc,
            Protocol::ArtNet,
        ] {
            let json = serde_json::to_string(&protocol).unwrap();
            let parsed: Protocol = serde_json::from_str(&json).unwrap();
            assert_eq!(protocol, parsed);
        }
    }

    #[test]
    fn test_serde_lowercase() {
        assert_eq!(serde_json::to_string(&Protocol::Midi).unwrap(), "\"midi\"");
        assert_eq!(serde_json::to_string(&Protocol::Hid).unwrap(), "\"hid\"");
        assert_eq!(serde_json::to_string(&Protocol::Osc).unwrap(), "\"osc\"");
        assert_eq!(
            serde_json::to_string(&Protocol::ArtNet).unwrap(),
            "\"artnet\""
        );
    }

    #[test]
    fn test_toml_roundtrip() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            protocol: Protocol,
        }
        let toml_str = "protocol = \"hid\"";
        let w: Wrapper = toml::from_str(toml_str).unwrap();
        assert_eq!(w.protocol, Protocol::Hid);
    }

    #[test]
    fn test_display() {
        assert_eq!(Protocol::Midi.to_string(), "midi");
        assert_eq!(Protocol::ArtNet.to_string(), "artnet");
    }
}
