// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Plugin capability flags for permission system

use serde::{Deserialize, Serialize};

/// Plugin capability flags for permission system
///
/// Capabilities represent permissions that plugins require to function.
/// The user is prompted to approve dangerous capabilities before the
/// plugin can execute.
///
/// # Example
///
/// ```rust
/// use conductor_core::plugin::Capability;
///
/// let caps = vec![Capability::Network, Capability::Filesystem];
/// assert!(caps.contains(&Capability::Network));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    /// Can make HTTP/HTTPS requests
    ///
    /// Required for plugins that communicate with web services,
    /// APIs, or external servers.
    ///
    /// **Risk**: Network access (data exfiltration)
    Network,

    /// Can read/write files
    ///
    /// Required for plugins that save state, cache data, or
    /// interact with the filesystem.
    ///
    /// **Risk**: File system access (data theft, file corruption)
    Filesystem,

    /// Can access audio devices
    ///
    /// Required for plugins that play sounds, record audio, or
    /// control audio routing.
    ///
    /// **Risk**: Audio device control
    Audio,

    /// Can send/receive MIDI
    ///
    /// Required for plugins that send MIDI output or receive
    /// MIDI input from additional devices.
    ///
    /// **Risk**: MIDI device control
    Midi,

    /// Can spawn processes
    ///
    /// Required for plugins that execute shell commands or
    /// launch external applications.
    ///
    /// **Risk**: Arbitrary code execution
    Subprocess,

    /// Can control system (volume, display, etc.)
    ///
    /// Required for plugins that adjust system settings like
    /// volume, brightness, or power management.
    ///
    /// **Risk**: System control
    SystemControl,
}

impl Capability {
    /// Get human-readable name
    pub fn name(&self) -> &'static str {
        match self {
            Capability::Network => "Network Access",
            Capability::Filesystem => "Filesystem Access",
            Capability::Audio => "Audio Device Access",
            Capability::Midi => "MIDI Device Access",
            Capability::Subprocess => "Process Execution",
            Capability::SystemControl => "System Control",
        }
    }

    /// Get description of what this capability allows
    pub fn description(&self) -> &'static str {
        match self {
            Capability::Network => "Make HTTP/HTTPS requests to external servers",
            Capability::Filesystem => "Read and write files on your computer",
            Capability::Audio => "Access and control audio devices",
            Capability::Midi => "Send and receive MIDI messages",
            Capability::Subprocess => "Execute shell commands and launch applications",
            Capability::SystemControl => "Control system settings (volume, display, etc.)",
        }
    }

    /// Get risk level (Low, Medium, High)
    pub fn risk_level(&self) -> RiskLevel {
        match self {
            Capability::Network => RiskLevel::Medium,
            Capability::Filesystem => RiskLevel::High,
            Capability::Audio => RiskLevel::Low,
            Capability::Midi => RiskLevel::Low,
            Capability::Subprocess => RiskLevel::High,
            Capability::SystemControl => RiskLevel::Medium,
        }
    }

    /// List all capabilities
    pub fn all() -> Vec<Capability> {
        vec![
            Capability::Network,
            Capability::Filesystem,
            Capability::Audio,
            Capability::Midi,
            Capability::Subprocess,
            Capability::SystemControl,
        ]
    }

    /// Map a registry/manifest capability string to a `Capability`.
    ///
    /// Lookup is case-insensitive and accepts the legacy aliases the
    /// registry uses (`storage` → `Filesystem`, `system_control` /
    /// `systemcontrol` → `SystemControl`). This is the single source
    /// of truth for the registry-string → enum mapping; both
    /// `enrich_capability` (here) and
    /// `plugin_registry::capabilities_from_registry` route through it
    /// so a new capability or alias is added in exactly one place.
    pub fn from_registry_str(raw: &str) -> Option<Capability> {
        match raw.to_lowercase().as_str() {
            "network" => Some(Capability::Network),
            "filesystem" | "storage" => Some(Capability::Filesystem),
            "audio" => Some(Capability::Audio),
            "midi" => Some(Capability::Midi),
            "subprocess" => Some(Capability::Subprocess),
            "system_control" | "systemcontrol" => Some(Capability::SystemControl),
            _ => None,
        }
    }
}

/// Enriched capability metadata for GUI display.
///
/// Plugin manifests declare capabilities as raw strings (e.g. `"Network"`,
/// `"storage"`). The Plugin Manager UI renders rich cards with risk
/// badges and descriptions; the Marketplace did not, because the wire
/// format only carried the raw strings. This struct is what the
/// GUI commands return so both surfaces share the same data shape.
///
/// `raw` preserves the original spelling for display in the unknown-
/// capability case so the user sees exactly what the registry declared.
/// `known: false` is the GUI's signal to render an "unknown capability"
/// warning treatment instead of pretending it's safe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichedCapability {
    /// Original capability string from the registry / manifest.
    pub raw: String,
    /// Human-readable display name (`Capability::name()` if known,
    /// falls back to `raw`).
    pub display_name: String,
    /// One-line description of what the capability allows.
    ///
    /// For known capabilities this is `Capability::description()`. For
    /// unknown capabilities this is a "this version doesn't recognise
    /// it" warning string (NOT empty), so the GUI's unknown-capability
    /// row carries an explanatory line for the user.
    pub description: String,
    /// Risk tier for UI colour cue. Typed (not `String`) so the
    /// Rust↔GUI contract is compiler-enforced and there's no risk of
    /// emitting a typoed/mis-cased tier. Wire format is the same lowercase JSON
    /// (`"low"`/`"medium"`/`"high"`/`"unknown"`) — frontend
    /// `normaliseRiskLevel` accepts those verbatim.
    pub risk_level: CapabilityRiskTier,
    /// `true` when `raw` mapped to a known `Capability` variant;
    /// `false` for forward-compat unknown strings.
    pub known: bool,
}

/// Risk tier for `EnrichedCapability` — a superset of `RiskLevel`
/// that adds an `Unknown` variant for forward-compat capability
/// strings the daemon doesn't recognise.
///
/// Kept separate from `RiskLevel` (which is for known `Capability`
/// variants only — every known cap has Low/Medium/High, never
/// Unknown) so the two enums document their respective contracts:
/// `RiskLevel` is a property of a known capability, `CapabilityRiskTier`
/// is what we tell the GUI about whatever string the registry shipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityRiskTier {
    Low,
    Medium,
    High,
    Unknown,
}

impl From<RiskLevel> for CapabilityRiskTier {
    fn from(r: RiskLevel) -> Self {
        match r {
            RiskLevel::Low => CapabilityRiskTier::Low,
            RiskLevel::Medium => CapabilityRiskTier::Medium,
            RiskLevel::High => CapabilityRiskTier::High,
        }
    }
}

/// Convert a raw capability string into rich GUI metadata.
///
/// Mapping rules are owned by `Capability::from_registry_str` (case-
/// insensitive, accepts `storage` and `system_control`/`systemcontrol`
/// aliases). This function adds the display strings + risk tier on
/// top so the GUI gets a single self-describing payload per capability.
pub fn enrich_capability(raw: &str) -> EnrichedCapability {
    match Capability::from_registry_str(raw) {
        Some(cap) => EnrichedCapability {
            raw: raw.to_string(),
            display_name: cap.name().to_string(),
            description: cap.description().to_string(),
            risk_level: cap.risk_level().into(),
            known: true,
        },
        None => EnrichedCapability {
            raw: raw.to_string(),
            display_name: raw.to_string(),
            description: format!(
                "Unknown capability '{raw}' — this version of Conductor doesn't recognise it"
            ),
            risk_level: CapabilityRiskTier::Unknown,
            known: false,
        },
    }
}

/// Risk level for capabilities
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl RiskLevel {
    /// Check if this risk level is safe for auto-granting
    ///
    /// Safe capabilities (Low risk) can be auto-granted without user confirmation.
    /// Medium and High risk capabilities require explicit user approval.
    pub fn is_safe(&self) -> bool {
        matches!(self, RiskLevel::Low)
    }

    /// Get color for UI display (CSS class or color name)
    pub fn color(&self) -> &'static str {
        match self {
            RiskLevel::Low => "green",
            RiskLevel::Medium => "yellow",
            RiskLevel::High => "red",
        }
    }

    /// Get emoji indicator
    pub fn emoji(&self) -> &'static str {
        match self {
            RiskLevel::Low => "✅",
            RiskLevel::Medium => "⚠️",
            RiskLevel::High => "🚨",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_metadata() {
        let cap = Capability::Network;
        assert_eq!(cap.name(), "Network Access");
        assert!(!cap.description().is_empty());
        assert_eq!(cap.risk_level(), RiskLevel::Medium);
    }

    #[test]
    fn test_all_capabilities() {
        let caps = Capability::all();
        assert_eq!(caps.len(), 6);
        assert!(caps.contains(&Capability::Network));
        assert!(caps.contains(&Capability::Filesystem));
    }

    #[test]
    fn test_risk_levels() {
        assert_eq!(Capability::Audio.risk_level(), RiskLevel::Low);
        assert_eq!(Capability::Network.risk_level(), RiskLevel::Medium);
        assert_eq!(Capability::Filesystem.risk_level(), RiskLevel::High);
        assert_eq!(Capability::Subprocess.risk_level(), RiskLevel::High);
    }

    #[test]
    fn test_risk_level_ordering() {
        assert!(RiskLevel::Low < RiskLevel::Medium);
        assert!(RiskLevel::Medium < RiskLevel::High);
    }

    #[test]
    fn test_risk_level_display() {
        assert_eq!(RiskLevel::Low.color(), "green");
        assert_eq!(RiskLevel::Medium.color(), "yellow");
        assert_eq!(RiskLevel::High.color(), "red");
    }

    #[test]
    fn test_capability_equality() {
        let cap1 = Capability::Network;
        let cap2 = Capability::Network;
        let cap3 = Capability::Filesystem;

        assert_eq!(cap1, cap2);
        assert_ne!(cap1, cap3);
    }

    #[test]
    fn test_capability_serialization() {
        let cap = Capability::Network;
        let json = serde_json::to_string(&cap).unwrap();
        assert_eq!(json, "\"network\"");

        let deserialized: Capability = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, cap);
    }

    // ─── Registry-string enrichment ─────────────────────────

    #[test]
    fn test_enrich_known_lowercase_capability() {
        let out = enrich_capability("network");
        assert_eq!(out.raw, "network");
        assert_eq!(out.display_name, "Network Access");
        assert_eq!(
            out.description,
            "Make HTTP/HTTPS requests to external servers"
        );
        assert_eq!(out.risk_level, CapabilityRiskTier::Medium);
        assert!(out.known);
    }

    #[test]
    fn test_enrich_handles_pascalcase() {
        // Real registry data uses PascalCase ("Subprocess", "Network").
        // Match `capabilities_from_registry`'s case-insensitive lookup.
        let out = enrich_capability("Subprocess");
        assert_eq!(out.display_name, "Process Execution");
        assert_eq!(out.risk_level, CapabilityRiskTier::High);
        assert!(out.known);
    }

    #[test]
    fn test_enrich_handles_storage_alias() {
        // "storage" is the registry-side spelling for "filesystem"
        // (see Spotify entry in plugins/registry/registry.json).
        let out = enrich_capability("storage");
        assert_eq!(out.display_name, "Filesystem Access");
        assert_eq!(out.risk_level, CapabilityRiskTier::High);
        assert!(out.known);
    }

    #[test]
    fn test_enrich_handles_systemcontrol_variants() {
        for input in ["system_control", "systemcontrol", "SystemControl"] {
            let out = enrich_capability(input);
            assert_eq!(out.display_name, "System Control", "input: {input}");
            assert_eq!(out.risk_level, CapabilityRiskTier::Medium);
            assert!(out.known);
        }
    }

    #[test]
    fn test_enrich_unknown_capability_marked_unknown() {
        let out = enrich_capability("telepathy");
        assert!(!out.known, "unknown capability must be flagged as such");
        assert_eq!(out.raw, "telepathy");
        assert_eq!(
            out.display_name, "telepathy",
            "fallback to raw string preserves what the user actually sees in the registry"
        );
        assert!(
            out.description.contains("Unknown"),
            "description should hint at the unknown status"
        );
        assert_eq!(out.risk_level, CapabilityRiskTier::Unknown);
    }

    #[test]
    fn test_enrich_preserves_raw_case() {
        // "raw" is the literal string the registry shipped — used by
        // GUI to show the user exactly what was declared.
        let out = enrich_capability("Network");
        assert_eq!(out.raw, "Network");
        // Display name still normalised to canonical.
        assert_eq!(out.display_name, "Network Access");
    }

    #[test]
    fn test_enrich_capability_serialization() {
        let out = enrich_capability("network");
        let json = serde_json::to_string(&out).unwrap();
        // Field shape contract for the GUI consumer.
        assert!(json.contains("\"raw\":\"network\""));
        assert!(json.contains("\"display_name\":\"Network Access\""));
        assert!(json.contains("\"risk_level\":\"medium\""));
        assert!(json.contains("\"known\":true"));
    }
}
