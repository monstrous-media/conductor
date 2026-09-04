// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Port resolver for binding MIDI ports to device identities (ADR-009 Phase 1;
//! ADR-035 — consumes the unified endpoint set natively).
//!
//! Pure logic: `(Vec<PortInfo>, Vec<EndpointConfig>) → Vec<BindingResult>`.
//! Only endpoints that participate in INPUT — `direction ∈ {Input,
//! Bidirectional}` — bind a physical MIDI input port; their input-side
//! matchers come from `EndpointKind::effective_matchers(Input)` (which yields
//! `&[]` for non-`Matcher` kinds, so OSC/Art-Net/virtual endpoints are
//! naturally inert here). The daemon feeds this the *normalized* endpoint set
//! (`normalize_to_endpoints`), so authored `[[endpoints]]` and lowered
//! `[[bindings]]` are matched through one path.

use crate::config::types::{ConnectorDirection, EndpointConfig};
use crate::device_intelligence::probe::IdentityConfidence;
use crate::device_intelligence::sysex_identity::SysExIdentity;
use crate::identity::DeviceId;

/// Information about an available MIDI port.
///
/// Prefer constructors (`PortInfo::new()`, `new_with_usb()`) over struct literals
/// to avoid breakage when new metadata fields are added.
#[derive(Debug, Clone)]
pub struct PortInfo {
    pub name: String,
    pub index: usize,
    /// USB Vendor ID (populated when available from platform APIs)
    pub vendor_id: Option<u16>,
    /// USB Product ID (populated when available from platform APIs)
    pub product_id: Option<u16>,
    /// SysEx Identity Reply data (populated by probing, when available)
    pub sysex_identity: Option<SysExIdentity>,
    /// Confidence label for the cached `sysex_identity`. ADR-026
    /// Phase 3.A: populated alongside `sysex_identity` from
    /// `ProbeCoordinator::cached()`, which always returns the pair
    /// atomically. `None` means either the port has not been probed
    /// or the cached identity has been invalidated.
    ///
    /// Convention: kept in sync with `sysex_identity` (both `Some` or
    /// both `None`) by the daemon's cache-lookup site. The pairing is
    /// **not enforced by the type** — split fields kept rather than
    /// `Option<(SysExIdentity, IdentityConfidence)>` so existing
    /// `port.sysex_identity.as_ref()` callers (e.g. the SysEx matcher
    /// in `PortResolver::resolve`) don't break. Test fixtures may
    /// intentionally set these independently to verify edge-case
    /// behaviour at the boundary.
    pub sysex_identity_confidence: Option<IdentityConfidence>,
}

impl PortInfo {
    /// Construct a PortInfo when no metadata is available.
    pub fn new(name: impl Into<String>, index: usize) -> Self {
        Self {
            name: name.into(),
            index,
            vendor_id: None,
            product_id: None,
            sysex_identity: None,
            sysex_identity_confidence: None,
        }
    }

    /// Construct a PortInfo with known USB Vendor and Product IDs.
    pub fn new_with_usb(
        name: impl Into<String>,
        index: usize,
        vendor_id: u16,
        product_id: u16,
    ) -> Self {
        Self {
            name: name.into(),
            index,
            vendor_id: Some(vendor_id),
            product_id: Some(product_id),
            sysex_identity: None,
            sysex_identity_confidence: None,
        }
    }
}

/// Result of resolving a single port against device identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingResult {
    /// Port bound to a device identity.
    Bound {
        device_id: DeviceId,
        port_name: String,
        port_index: usize,
    },
    /// Port has no matching device identity.
    Unbound {
        port_name: String,
        port_index: usize,
    },
    /// Port matches an identity that was already claimed by another port (D7).
    Ambiguous {
        port_name: String,
        port_index: usize,
        claimed_by: DeviceId,
    },
}

/// A device candidate that can be resolved against `[[endpoints]]` matchers.
///
/// Implemented by [`PortInfo`] (MIDI ports) and [`GamepadInfo`] (HID game
/// controllers) so the specificity-matching algorithm is **shared, not
/// duplicated** (ADR-047 §D2) — and crucially **no `PortInfo` is synthesized for
/// gamepads**. Metadata accessors default to `None`; each candidate overrides
/// only what it actually carries (a MIDI port has no GUID; a gamepad has no USB
/// VID/PID or SysEx identity, per §D2 — GUID bytes are bus-dependent and must
/// not be reinterpreted as VID/PID).
pub trait ResolvableCandidate {
    /// Device/port name used by name-based matchers.
    fn name(&self) -> &str;
    /// Stable index of this candidate within its enumeration.
    fn index(&self) -> usize;
    /// USB vendor id, when known (MIDI ports only).
    fn vendor_id(&self) -> Option<u16> {
        None
    }
    /// USB product id, when known (MIDI ports only).
    fn product_id(&self) -> Option<u16> {
        None
    }
    /// Cached SysEx identity, when probed (MIDI ports only).
    fn sysex_identity(&self) -> Option<&SysExIdentity> {
        None
    }
    /// SDL controller GUID, when this is a game controller (ADR-047 §D2).
    fn controller_guid(&self) -> Option<[u8; 16]> {
        None
    }
}

impl ResolvableCandidate for PortInfo {
    fn name(&self) -> &str {
        &self.name
    }
    fn index(&self) -> usize {
        self.index
    }
    fn vendor_id(&self) -> Option<u16> {
        self.vendor_id
    }
    fn product_id(&self) -> Option<u16> {
        self.product_id
    }
    fn sysex_identity(&self) -> Option<&SysExIdentity> {
        self.sysex_identity.as_ref()
    }
    // controller_guid: a MIDI port has no SDL GUID → trait default None.
}

/// A connected game controller, for GUID/name resolution against `[[endpoints]]`
/// (ADR-047 §D2). Lightweight by design: a controller has a name and an SDL
/// **model** GUID, but no USB VID/PID or SysEx identity in this path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GamepadInfo {
    pub name: String,
    pub index: usize,
    pub guid: [u8; 16],
}

impl GamepadInfo {
    pub fn new(name: impl Into<String>, index: usize, guid: [u8; 16]) -> Self {
        Self {
            name: name.into(),
            index,
            guid,
        }
    }
}

impl ResolvableCandidate for GamepadInfo {
    fn name(&self) -> &str {
        &self.name
    }
    fn index(&self) -> usize {
        self.index
    }
    fn controller_guid(&self) -> Option<[u8; 16]> {
        Some(self.guid)
    }
    // vendor_id/product_id/sysex_identity: gamepads carry none here → defaults.
}

/// Resolve any [`ResolvableCandidate`] slice against device identities (ADR-047
/// §D2 — the shared specificity-matching core extracted from `PortResolver`).
///
/// For each candidate, find the highest-specificity matching endpoint. On a
/// specificity tie the earlier-declared endpoint wins (strict `>`), matching the
/// regression-tested `equal_specificity_preserves_config_order` behaviour; a true
/// structurally-identical collision is rejected earlier by config validation, not
/// here. If an endpoint is already claimed by a previous candidate, the second one
/// gets `Ambiguous` (D7: first-come-first-served). Behaviour for `PortInfo` is
/// byte-for-byte the pre-extraction `PortResolver::resolve` — the only new
/// dispatch arm is `ControllerGuid`, which never matches a candidate whose
/// `controller_guid()` is `None` (i.e. MIDI ports are unaffected).
///
/// **Claim scope:** `claimed_aliases` is per-call, i.e. exclusivity holds within
/// one candidate class (all MIDI ports, or all gamepads), not across a MIDI and a
/// gamepad pass. This is intentional and safe: endpoint aliases are globally
/// unique (a duplicate is a hard load-time error in `normalize_to_endpoints`) and
/// each endpoint is single-protocol, so one alias cannot legitimately bind both a
/// MIDI port and a controller in the same config.
pub fn resolve_candidates<C: ResolvableCandidate>(
    candidates: &[C],
    endpoints: &[EndpointConfig],
) -> Vec<BindingResult> {
    use crate::identity::DeviceMatcher;
    let mut claimed_aliases: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut results = Vec::new();

    for candidate in candidates {
        // Find the best matching endpoint (highest specificity)
        let mut best_match: Option<(&EndpointConfig, u32)> = None;

        for identity in endpoints {
            // Skip disabled endpoints (ADR-009 Gap 4).
            if !identity.enabled {
                continue;
            }
            // Only INPUT / BIDIRECTIONAL endpoints bind an input device
            // (ADR-035 §4.4). Output-only endpoints never claim an input.
            if !matches!(
                identity.direction,
                ConnectorDirection::Input | ConnectorDirection::Bidirectional
            ) {
                continue;
            }
            // `effective_matchers(Input)` yields the input-side matchers —
            // `input_matchers` when set, else the symmetric `matchers`
            // (ADR-035 §4.1) — and `&[]` for non-`Matcher` kinds, so OSC /
            // Art-Net / virtual-port endpoints contribute nothing here.
            for matcher in identity.kind.effective_matchers(ConnectorDirection::Input) {
                // Dispatch by matcher type to the metadata it needs:
                // SysEx → cached identity; ControllerGuid → SDL GUID (ADR-047
                // §D2); everything else → name/USB. A `ControllerGuid` matcher
                // only binds a candidate that actually has a GUID, so MIDI
                // ports (guid = None) are never claimed by it, and gamepads
                // (vendor/sysex = None) are never claimed by USB/SysEx matchers.
                let matched = match matcher {
                    DeviceMatcher::SysExIdentity { .. } => {
                        matcher.matches_with_sysex(candidate.name(), candidate.sysex_identity())
                    }
                    DeviceMatcher::ControllerGuid { .. } => {
                        matcher.matches_with_guid(candidate.controller_guid())
                    }
                    _ => matcher.matches_with_usb(
                        candidate.name(),
                        candidate.vendor_id(),
                        candidate.product_id(),
                    ),
                };
                if matched {
                    let specificity = matcher.specificity();
                    if best_match
                        .as_ref()
                        .is_none_or(|(_, best_s)| specificity > *best_s)
                    {
                        best_match = Some((identity, specificity));
                    }
                }
            }
        }

        match best_match {
            Some((identity, _)) => {
                if claimed_aliases.contains(&identity.alias) {
                    // D7: Identity already claimed → Ambiguous
                    results.push(BindingResult::Ambiguous {
                        port_name: candidate.name().to_string(),
                        port_index: candidate.index(),
                        claimed_by: DeviceId::from_alias(&identity.alias),
                    });
                } else {
                    // Bind this candidate to the identity
                    claimed_aliases.insert(identity.alias.clone());
                    results.push(BindingResult::Bound {
                        device_id: DeviceId::from_alias(&identity.alias),
                        port_name: candidate.name().to_string(),
                        port_index: candidate.index(),
                    });
                }
            }
            None => {
                results.push(BindingResult::Unbound {
                    port_name: candidate.name().to_string(),
                    port_index: candidate.index(),
                });
            }
        }
    }

    results
}

pub struct PortResolver;

impl PortResolver {
    /// Resolve MIDI ports against device identities (thin wrapper over
    /// [`resolve_candidates`]).
    ///
    /// For each port, find the highest-specificity matching identity.
    /// If an identity is already claimed by a previous port, the second
    /// port gets Ambiguous (D7: first-come-first-served).
    pub fn resolve(ports: &[PortInfo], endpoints: &[EndpointConfig]) -> Vec<BindingResult> {
        resolve_candidates(ports, endpoints)
    }
}

/// Resolves connected game controllers against `[[endpoints]]` (ADR-047 §D2).
/// Shares the [`resolve_candidates`] core with [`PortResolver`].
pub struct GamepadResolver;

impl GamepadResolver {
    /// Resolve game controllers against device identities (GUID- and name-based
    /// matchers). Thin wrapper over [`resolve_candidates`].
    pub fn resolve(gamepads: &[GamepadInfo], endpoints: &[EndpointConfig]) -> Vec<BindingResult> {
        resolve_candidates(gamepads, endpoints)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_intelligence::probe::IdentityConfidence;

    /// Phase 3.A: PortInfo carries a `sysex_identity_confidence` field
    /// that is `None` for freshly-constructed ports. The daemon
    /// populates it from `ProbeCoordinator::cached()` only after a
    /// successful probe.
    #[test]
    fn port_info_new_initialises_confidence_to_none() {
        let port = PortInfo::new("Mikro IN", 0);
        assert!(port.sysex_identity_confidence.is_none());
        assert!(port.sysex_identity.is_none());
    }

    #[test]
    fn port_info_new_with_usb_initialises_confidence_to_none() {
        let port = PortInfo::new_with_usb("Mikro IN", 0, 0x17cc, 0x1700);
        assert!(port.sysex_identity_confidence.is_none());
    }

    /// `sysex_identity_confidence` is a public field so daemon-side
    /// construction can populate it from the probe cache without going
    /// through a builder. Phase 3.A pins this so the daemon's
    /// engine_manager (and any future test fixtures) can write the
    /// field directly.
    #[test]
    fn port_info_carries_direct_paired_port_confidence_when_set() {
        let mut port = PortInfo::new("Mikro IN", 0);
        port.sysex_identity_confidence = Some(IdentityConfidence::DirectPairedPort);
        assert_eq!(
            port.sysex_identity_confidence,
            Some(IdentityConfidence::DirectPairedPort)
        );
    }

    #[test]
    fn port_info_carries_shared_route_confidence_when_set() {
        // Phase 3.B will produce SharedRoute confidence; the field
        // accepts it today so 3.B's engine wiring doesn't need to
        // touch core types.
        let mut port = PortInfo::new("Mikro IN", 0);
        port.sysex_identity_confidence = Some(IdentityConfidence::SharedRoute);
        assert_eq!(
            port.sysex_identity_confidence,
            Some(IdentityConfidence::SharedRoute)
        );
    }

    /// Pins the spec-recommended idiom for splitting the cache tuple
    /// `Option<(SysExIdentity, IdentityConfidence)>` into the two
    /// PortInfo fields. The spec (`docs/sysex-device-identity/implementation-spec.md`
    /// §3.1) shows this exact pattern; this test ensures the example
    /// stays compilable Rust as types evolve.
    ///
    /// Naive `.map(...)` twice on the same `Option` won't compile —
    /// `Option::map` consumes the receiver. `unzip()` (stabilised
    /// Rust 1.66) splits `Option<(A, B)>` into `(Option<A>, Option<B>)`
    /// in one expression, which is the cleanest fit here.
    #[test]
    fn port_info_can_be_built_from_cache_tuple_via_unzip() {
        use crate::device_intelligence::sysex_identity::SysExIdentity;

        let identity = SysExIdentity {
            manufacturer_id: vec![0x42],
            family: 0x0034,
            model: 0x0001,
            version: [0, 0, 0, 1],
        };
        let cached: Option<(SysExIdentity, IdentityConfidence)> =
            Some((identity.clone(), IdentityConfidence::DirectPairedPort));

        // The exact idiom the spec recommends Phase 3.C use.
        let (sysex_identity, sysex_identity_confidence) = cached.unzip();
        let port = PortInfo {
            name: "Mikro IN".to_string(),
            index: 0,
            vendor_id: None,
            product_id: None,
            sysex_identity,
            sysex_identity_confidence,
        };

        assert_eq!(port.sysex_identity.as_ref(), Some(&identity));
        assert_eq!(
            port.sysex_identity_confidence,
            Some(IdentityConfidence::DirectPairedPort)
        );
    }

    /// Cache miss: `unzip()` must produce `(None, None)` so both
    /// PortInfo fields stay paired in the absent state.
    #[test]
    fn port_info_built_from_empty_cache_has_both_fields_none() {
        let cached: Option<(
            crate::device_intelligence::sysex_identity::SysExIdentity,
            IdentityConfidence,
        )> = None;
        let (sysex_identity, sysex_identity_confidence) = cached.unzip();
        let port = PortInfo {
            name: "Mikro IN".to_string(),
            index: 0,
            vendor_id: None,
            product_id: None,
            sysex_identity,
            sysex_identity_confidence,
        };
        assert!(port.sysex_identity.is_none());
        assert!(port.sysex_identity_confidence.is_none());
    }
}
