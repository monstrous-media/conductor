// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Device identity types for multi-device architecture (ADR-009 Phase 1)
//!
//! This module provides types for identifying and matching MIDI devices
//! in a multi-device "listen-to-all" architecture.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

/// Thread-safe cache for compiled regexes, keyed by pattern string.
static REGEX_CACHE: LazyLock<Mutex<HashMap<String, regex::Regex>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Unique identifier for a device in the multi-device system.
///
/// Wraps an `Arc<str>` for cheap cloning and hashing. Created from
/// either a user-defined alias (from config) or a port name with
/// instance disambiguation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(Arc<str>);

impl Serialize for DeviceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(DeviceId(Arc::from(s.as_str())))
    }
}

impl DeviceId {
    /// Create a DeviceId from a user-defined alias.
    pub fn from_alias(alias: &str) -> Self {
        Self(Arc::from(alias))
    }

    /// Create a DeviceId from a port name with instance disambiguation.
    ///
    /// Instance 0 gets no suffix; instance 1 gets " #2", instance 2 gets " #3", etc.
    pub fn from_port_instance(port_name: &str, instance: usize) -> Self {
        if instance == 0 {
            Self(Arc::from(port_name))
        } else {
            Self(Arc::from(format!("{} #{}", port_name, instance + 1)))
        }
    }

    /// Create a DeviceId from a raw port name (unmatched port, no alias).
    ///
    /// Semantically equivalent to `from_alias` but clearer intent for
    /// ports that didn't match any configured device identity.
    pub fn raw(port_name: &str) -> Self {
        Self(Arc::from(port_name))
    }

    /// Get the string representation of this DeviceId.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An event paired with its source device identity.
#[derive(Debug, Clone)]
pub struct DeviceEvent<T> {
    device_id: DeviceId,
    event: T,
}

impl<T> DeviceEvent<T> {
    pub fn new(device_id: DeviceId, event: T) -> Self {
        Self { device_id, event }
    }

    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    pub fn event(&self) -> &T {
        &self.event
    }

    pub fn into_parts(self) -> (DeviceId, T) {
        (self.device_id, self.event)
    }
}

/// Matcher for identifying a MIDI device by various criteria.
///
/// Matchers have a specificity ordering (D2):
/// `CoreMidiUniqueId > SysExIdentity > UsbIdentifier > UsbTopology > ExactName > PlatformId > NameContains > NameRegex`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DeviceMatcher {
    /// Match by exact port name string.
    ExactName { value: String },
    /// Match by substring contained in port name.
    NameContains { value: String },
    /// Match by regex pattern on port name. Patterns > 256 chars are rejected.
    NameRegex { value: String },
    /// Match by USB vendor/product ID pair.
    UsbIdentifier { vendor_id: u16, product_id: u16 },
    /// Match by USB topology path (e.g., "1-2.3").
    UsbTopology { value: String },
    /// Match by platform-specific device ID string.
    PlatformId { value: String },
    /// Match by macOS CoreMIDI unique ID.
    CoreMidiUniqueId { value: i32 },
    /// Match by SysEx Identity Reply data (ADR-022 D6, #752).
    /// Requires probing the device; matches against stored identity metadata.
    SysExIdentity {
        manufacturer_id: Vec<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        family: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<u16>,
    },
    /// Match a game controller by its SDL GUID (ADR-047 §D2).
    ///
    /// Opaque 16-byte **model** identity (not per-unit — two identical pads share
    /// a GUID). Serialized as a 32-char lowercase hex string at the config/UI
    /// boundary. Do **not** derive VID/PID from these bytes: the layout is
    /// bus-dependent and XInput / virtual / Bluetooth controllers zero those
    /// fields. Requires gamepad metadata, so `matches()` / `matches_with_usb()`
    /// return false — resolution uses `matches_with_guid()`.
    ControllerGuid {
        #[serde(with = "guid_hex")]
        value: [u8; 16],
    },
}

/// Serde helper for [`DeviceMatcher::ControllerGuid`] — a `[u8; 16]` SDL GUID is
/// represented as a 32-char lowercase hex string in TOML/JSON (ADR-047 §D2).
mod guid_hex {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &[u8; 16], s: S) -> Result<S::Ok, S::Error> {
        use std::fmt::Write;
        let mut hex = String::with_capacity(32);
        for b in value {
            let _ = write!(hex, "{b:02x}");
        }
        s.serialize_str(&hex)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 16], D::Error> {
        let s = String::deserialize(d)?;
        // Require ASCII *before* slicing by byte index: a 32-byte string with a
        // multi-byte UTF-8 char would pass a bare `len() == 32` check yet panic
        // when `&s[i*2..i*2+2]` splits a codepoint (Council review). For ASCII,
        // byte length == char count and every 2-byte slice is on a boundary.
        if !s.is_ascii() || s.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "controller GUID must be 32 ASCII hex characters, got {} byte(s)",
                s.len()
            )));
        }
        let mut out = [0u8; 16];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|e| serde::de::Error::custom(format!("invalid hex in GUID: {e}")))?;
        }
        Ok(out)
    }
}

impl DeviceMatcher {
    /// Test whether this matcher matches a given port name.
    ///
    /// Note: Only name-based matchers (ExactName, NameContains, NameRegex) work here.
    /// UsbIdentifier, UsbTopology, PlatformId, CoreMidiUniqueId, and SysExIdentity
    /// always return false — they require metadata via matches_with_usb() or
    /// matches_with_sysex() respectively.
    pub fn matches(&self, port_name: &str) -> bool {
        match self {
            Self::ExactName { value } => port_name == value,
            Self::NameContains { value } => port_name.contains(value.as_str()),
            Self::NameRegex { value } => {
                if value.len() > 256 {
                    return false;
                }
                let cache = REGEX_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(re) = cache.get(value.as_str()) {
                    return re.is_match(port_name);
                }
                drop(cache);
                // Compile and cache on miss
                match regex::Regex::new(value) {
                    Ok(re) => {
                        let result = re.is_match(port_name);
                        let mut cache = REGEX_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                        cache.insert(value.clone(), re);
                        result
                    }
                    Err(_) => false,
                }
            }
            // These matchers require metadata beyond port name
            Self::UsbIdentifier { .. } => false,
            Self::UsbTopology { .. } => false,
            Self::PlatformId { .. } => false,
            Self::CoreMidiUniqueId { .. } => false,
            Self::SysExIdentity { .. } => false,
            // ADR-047 §D2: needs the controller's GUID, not a port name.
            Self::ControllerGuid { .. } => false,
        }
    }

    /// Test whether this matcher matches a game controller's SDL GUID (ADR-047 §D2).
    ///
    /// Only `ControllerGuid` matchers compare here (opaque 16-byte equality);
    /// every other matcher returns false — gamepad *name* matching goes through
    /// [`matches`](Self::matches) / [`matches_with_usb`](Self::matches_with_usb)
    /// in the resolver. `guid` is `None` for non-gamepad candidates (e.g. MIDI
    /// ports), so a `ControllerGuid` matcher never binds them.
    pub fn matches_with_guid(&self, guid: Option<[u8; 16]>) -> bool {
        match self {
            Self::ControllerGuid { value } => guid == Some(*value),
            _ => false,
        }
    }

    /// Test whether this matcher matches a port with optional USB metadata.
    ///
    /// Extends `matches()` with USB vendor/product ID support.
    /// - `UsbIdentifier`: matches when both VID and PID match the provided metadata.
    ///   Returns false if USB metadata is absent (same as `matches()`).
    /// - Name-based matchers (`ExactName`, `NameContains`, `NameRegex`): delegate to
    ///   `matches(port_name)` — USB metadata is ignored.
    /// - Other matchers (`UsbTopology`, `PlatformId`, `CoreMidiUniqueId`): still return
    ///   false — they require platform-specific APIs not yet plumbed through.
    ///
    /// Note: `PortResolver::resolve()` calls this method, passing
    /// `port.vendor_id` and `port.product_id` from `PortInfo`.
    pub fn matches_with_usb(
        &self,
        port_name: &str,
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    ) -> bool {
        match self {
            Self::UsbIdentifier {
                vendor_id: vid,
                product_id: pid,
            } => {
                matches!((vendor_id, product_id), (Some(v), Some(p)) if v == *vid && p == *pid)
            }
            // Fall back to matches(port_name): works for name-based matchers,
            // returns false for UsbTopology/PlatformId/CoreMidiUniqueId
            _ => self.matches(port_name),
        }
    }

    /// Test whether this matcher matches a port with optional SysEx Identity data (#752).
    ///
    /// For `SysExIdentity` matchers, compares manufacturer_id, family, and model
    /// against the provided identity. Returns false if identity is absent.
    /// For all other matchers, delegates to `matches(port_name)`.
    ///
    /// Note: `PortResolver::resolve()` dispatches by matcher type: `SysExIdentity`
    /// matchers use this method with `port.sysex_identity` from `PortInfo`; all other
    /// matchers use `matches_with_usb()`. Active in resolution when identity data exists.
    pub fn matches_with_sysex(
        &self,
        port_name: &str,
        identity: Option<&crate::device_intelligence::sysex_identity::SysExIdentity>,
    ) -> bool {
        match self {
            Self::SysExIdentity {
                manufacturer_id,
                family,
                model,
            } => {
                let Some(id) = identity else {
                    return false;
                };
                if *manufacturer_id != id.manufacturer_id {
                    return false;
                }
                if let Some(f) = family
                    && *f != id.family
                {
                    return false;
                }
                if let Some(m) = model
                    && *m != id.model
                {
                    return false;
                }
                true
            }
            _ => self.matches(port_name),
        }
    }

    /// Return a numeric specificity score for priority ordering.
    ///
    /// Higher values = more specific matcher. Used by PortResolver to
    /// break ties when multiple identities match the same port.
    pub fn specificity(&self) -> u32 {
        match self {
            Self::NameRegex { .. } => 10,
            Self::NameContains { .. } => 20,
            Self::PlatformId { .. } => 30,
            Self::ExactName { .. } => 40,
            // ADR-047 §D2: model-level GUID — more specific than ExactName (40),
            // less than the USB topology/identifier pair (50/60).
            Self::ControllerGuid { .. } => 45,
            Self::UsbTopology { .. } => 50,
            Self::UsbIdentifier { .. } => 60,
            Self::SysExIdentity { .. } => 65,
            Self::CoreMidiUniqueId { .. } => 70,
        }
    }
}

// Constructor helpers for test ergonomics and code that builds matchers directly
impl DeviceMatcher {
    pub fn exact_name(name: impl Into<String>) -> Self {
        Self::ExactName { value: name.into() }
    }

    pub fn name_contains(substring: impl Into<String>) -> Self {
        Self::NameContains {
            value: substring.into(),
        }
    }

    pub fn name_regex(pattern: impl Into<String>) -> Self {
        Self::NameRegex {
            value: pattern.into(),
        }
    }

    pub fn usb_identifier(vendor_id: u16, product_id: u16) -> Self {
        Self::UsbIdentifier {
            vendor_id,
            product_id,
        }
    }

    pub fn usb_topology(path: impl Into<String>) -> Self {
        Self::UsbTopology { value: path.into() }
    }

    pub fn platform_id(id: impl Into<String>) -> Self {
        Self::PlatformId { value: id.into() }
    }

    pub fn core_midi_unique_id(id: i32) -> Self {
        Self::CoreMidiUniqueId { value: id }
    }

    /// Construct a `ControllerGuid` matcher from a 16-byte SDL GUID (ADR-047 §D2).
    pub fn controller_guid(value: [u8; 16]) -> Self {
        Self::ControllerGuid { value }
    }
}

/// State of a port's binding to a device identity.
#[derive(Debug, Clone)]
pub enum BindingState {
    /// Port is successfully bound to a device identity.
    Bound {
        device_id: DeviceId,
        port_name: String,
    },
    /// Port has no matching device identity.
    Unbound { port_name: String },
    /// Port matches multiple device identities (D7).
    Ambiguous {
        port_name: String,
        candidates: Vec<DeviceId>,
    },
}
