// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Signal routing graph (ADR-031): connectors, endpoints, routes, transforms.

use super::*;

/// Direction of a connector endpoint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum ConnectorDirection {
    Input,
    Output,
    #[default]
    Bidirectional,
}

/// Protocol spoken by a connector.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum ConnectorProtocol {
    #[default]
    Midi,
    Osc,
    ArtNet,
    Hid,
}

/// ADR-042 Phase A — per-listener network-security policy, shared by the
/// `OscEndpoint` and `ArtNetEndpoint` payloads (flattened onto the endpoint
/// table on the wire).
///
/// Phase A is **loopback-only**: these fields are *parsed and shape-validated*
/// (forward-compat so Phase B-early can lift the gate) but a non-loopback
/// listener `host` is a config-load error regardless of `allow_network`. The
/// fields only become operative once a non-loopback bind exists in Phase
/// B-early. `allow_sensitive_actions` (D17) is the exception — it is the
/// action-class gate that is **active in Phase A** for loopback OSC/Art-Net.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSecurityConfig {
    /// Operator intent to accept non-loopback traffic. Phase A still rejects
    /// the bind (loopback-only); shape-validated for forward compatibility.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_network: bool,

    /// Allow-list of source CIDRs (parsed via [`crate::security::NetworkAcl`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_acl: Vec<String>,

    /// Optional narrower allow-list of individual sender IPs (checked in
    /// addition to `network_acl` at the listener edge).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sender_acl: Vec<String>,

    /// Total inbound packet budget (token-bucket). `None` = default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_total: Option<u32>,

    /// Per-sender inbound packet budget (checked before the total).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_sender: Option<u32>,

    /// Acknowledge the amplification risk of a broad broadcast ACL (D11).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub i_understand_amplification_risk: bool,

    /// D17 action-class gate: when `false` (default), network-origin triggers
    /// from this listener — **including loopback OSC/Art-Net** — may NOT
    /// dispatch `Shell`/`Launch`/`Keystroke`. Active in Phase A.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_sensitive_actions: bool,

    /// Phase B-late per-listener strict-mode (session-token replay defence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_mode: Option<StrictMode>,
}

/// Phase B-late strict-mode policy (parsed in Phase A for forward-compat).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum StrictMode {
    /// Require a session-token nonce in a designated OSC argument.
    SessionToken {
        /// OSC argument index carrying the nonce.
        arg_index: usize,
        /// Validity window in seconds (default 30).
        #[serde(default = "default_strict_window_sec")]
        window_sec: u64,
        /// Replay-cache size (default 1000).
        #[serde(default = "default_strict_replay_window")]
        replay_window: usize,
    },
}

fn default_strict_window_sec() -> u64 {
    30
}
fn default_strict_replay_window() -> usize {
    1000
}

/// Protocol-specific endpoint identification (ADR-031 § 3.1 / ADR-035 §4.1).
///
/// Promoted from the former nested `EndpointConfig` enum (ADR-035):
/// the `EndpointConfig` name now belongs to the unified `[[endpoints]]`
/// wrapper struct below; this enum is the type-specific payload (`kind`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EndpointKind {
    /// MIDI/HID: match by port name, USB ID, SysEx identity.
    /// Reuses existing `DeviceMatcher` infrastructure (ADR-022).
    Matcher {
        /// Symmetric matchers — used in both directions, or as the sole
        /// direction. Empty is permitted only when an asymmetric
        /// `input_matchers`/`output_matchers` is populated (validated via
        /// the non-empty invariant).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        matchers: Vec<DeviceMatcher>,
        /// Asymmetric override for a Bidirectional endpoint whose input
        /// port differs from its output (ADR-035 §4.4, R2 — replaces the
        /// synthetic `-out` alias). Empty = fall back to `matchers`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        input_matchers: Vec<DeviceMatcher>,
        /// Asymmetric output-side matchers. Empty = fall back to `matchers`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        output_matchers: Vec<DeviceMatcher>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        no_probe: bool,
    },
    /// OSC: network endpoint.
    OscEndpoint {
        host: String,
        port: u16,
        /// ADR-042 Phase A network-security policy (loopback-only in Phase A;
        /// flattened so the fields sit at the endpoint level on the wire).
        #[serde(flatten)]
        security: NetworkSecurityConfig,
    },
    /// Art-Net: universe on a network interface.
    ArtNetEndpoint {
        universe: u16,
        #[serde(default = "default_artnet_host")]
        host: String,
        #[serde(default = "default_artnet_port")]
        port: u16,
        /// Art-Net broadcast (255.255.255.255 / directed broadcast). When a
        /// listener (`direction = Input`) sets this, the ACL amplification
        /// budget (ADR-042 D11) is enforced.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        allow_broadcast: bool,
        /// ADR-042 Phase A network-security policy (shared with OSC).
        #[serde(flatten)]
        security: NetworkSecurityConfig,
    },
    /// Virtual MIDI port created by Conductor (ADR-031 D10 — DAW proxy model).
    ///
    /// The daemon creates a virtual MIDI port with this name via CoreMIDI
    /// (macOS) or ALSA (Linux). DAWs and other apps see it as a standard
    /// MIDI port. Virtual ports are lazily created only when referenced
    /// by an enabled route.
    MidiVirtualPort {
        /// Name of the virtual port as it appears to the OS and other apps.
        /// Convention: `"Conductor: {device_alias}"` (e.g., `"Conductor: Mikro"`).
        port_name: String,
    },
}

fn default_artnet_host() -> String {
    "255.255.255.255".to_string()
}
fn default_artnet_port() -> u16 {
    6454
}

impl EndpointKind {
    /// Direction-aware matcher selection (ADR-035 §4.1, R3 "empty-matchers
    /// hazard"). `Input` → `input_matchers` (falling back to `matchers`);
    /// `Output` → `output_matchers` (falling back to `matchers`);
    /// `Bidirectional` → `matchers`. Non-`Matcher` kinds carry no matchers.
    ///
    /// Probing, conflict-overlap detection, and metrics/labels MUST use this
    /// instead of reading `.matchers` directly — an output-only endpoint
    /// lowers to `Matcher` with empty `matchers` + populated
    /// `output_matchers`, so `matchers[0]` would panic.
    pub fn effective_matchers(&self, dir: ConnectorDirection) -> &[DeviceMatcher] {
        match self {
            EndpointKind::Matcher {
                matchers,
                input_matchers,
                output_matchers,
                ..
            } => match dir {
                ConnectorDirection::Input if !input_matchers.is_empty() => input_matchers,
                ConnectorDirection::Output if !output_matchers.is_empty() => output_matchers,
                _ => matchers,
            },
            _ => &[],
        }
    }

    /// The `EndpointKind::Matcher` non-empty invariant (ADR-035 §4.1, R3):
    /// a `Matcher` must carry at least one matcher across the three lists.
    /// Returns `true` when the invariant holds (vacuously `true` for
    /// non-`Matcher` kinds). The validator turns a `false` here
    /// into a clear load-time "endpoint has no matchers" error.
    pub fn has_any_matcher(&self) -> bool {
        match self {
            EndpointKind::Matcher {
                matchers,
                input_matchers,
                output_matchers,
                ..
            } => !matchers.is_empty() || !input_matchers.is_empty() || !output_matchers.is_empty(),
            _ => true,
        }
    }

    /// `no_probe` flag for a `Matcher` endpoint (ADR-026 Phase 4.2). Always
    /// `false` for non-`Matcher` kinds (probing only applies to MIDI/HID).
    pub fn no_probe(&self) -> bool {
        matches!(self, EndpointKind::Matcher { no_probe: true, .. })
    }

    /// `true` iff any matcher across `matchers` / `input_matchers` /
    /// `output_matchers` is `DeviceMatcher::SysExIdentity`. Used by the
    /// `no_probe` override warning + `device_should_skip_auto_probe`
    /// (probe_on_connect). Phase 4.2.
    pub fn has_any_sysex_identity_matcher(&self) -> bool {
        let is_sysex = |m: &DeviceMatcher| matches!(m, DeviceMatcher::SysExIdentity { .. });
        match self {
            EndpointKind::Matcher {
                matchers,
                input_matchers,
                output_matchers,
                ..
            } => {
                matchers.iter().any(is_sysex)
                    || input_matchers.iter().any(is_sysex)
                    || output_matchers.iter().any(is_sysex)
            }
            _ => false,
        }
    }

    /// Protocol implied by this endpoint kind, used when an
    /// [`EndpointConfig`] omits an explicit `protocol` override.
    /// `OscEndpoint` → `Osc`, `ArtNetEndpoint` → `ArtNet`,
    /// `Matcher`/`MidiVirtualPort` → `Midi` (the default). A `Matcher`
    /// can also be HID, but that distinction only arrives via the explicit
    /// `protocol` override — kind-inference alone defaults it to `Midi`.
    pub fn protocol(&self) -> ConnectorProtocol {
        match self {
            EndpointKind::OscEndpoint { .. } => ConnectorProtocol::Osc,
            EndpointKind::ArtNetEndpoint { .. } => ConnectorProtocol::ArtNet,
            EndpointKind::Matcher { .. } | EndpointKind::MidiVirtualPort { .. } => {
                ConnectorProtocol::Midi
            }
        }
    }
}

/// A single unified I/O endpoint (ADR-035). Collapses the legacy
/// `[[bindings]]` (`DeviceIdentityConfig`, input-only) and `[[connectors]]`
/// (`ConnectorConfig`) blocks into one `[[endpoints]]` schema with an
/// explicit `direction` + `type` discriminator.
///
/// Authored under `[[endpoints]]`; legacy blocks are lowered into this shape
/// in memory (ADR-035), never parsed through this struct's strict
/// deserializer directly (which requires `direction`).
///
/// **Deserialization is hand-written** (see the `Deserialize` impl below):
/// `#[serde(flatten)]` over an internally-tagged enum is unsafe with the
/// `toml` crate — it silently disables unknown-field detection (a `prot =`
/// typo would be dropped) and mangles scalar parsing. The derived `Serialize`
/// keeps `#[serde(flatten)]` for output; a parity test covers the round-trip.
#[derive(Debug, Clone, Serialize)]
pub struct EndpointConfig {
    /// Unique across the merged endpoint set (endpoints + lowered bindings +
    /// lowered connectors).
    pub alias: String,

    /// Input / Output / Bidirectional. REQUIRED when authored as
    /// `[[endpoints]]` (no serde default — R2 P1): forcing it avoids
    /// accidentally binding a network listener as implicitly Bidirectional.
    pub direction: ConnectorDirection,

    /// Override protocol auto-detection. Inferred from `kind` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<ConnectorProtocol>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default = "default_true")]
    pub enabled: bool,

    /// MIDI channel scope (0–15). Empty = all channels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<u8>,

    /// Type-specific payload. On the wire (`Serialize`), `#[serde(flatten)]`
    /// lifts the `type = "..."` tag and the variant's fields to the same
    /// TOML level as the common fields above.
    #[serde(flatten)]
    pub kind: EndpointKind,
}

impl EndpointConfig {
    /// Effective protocol: the explicit `protocol` override when authored,
    /// otherwise inferred from `kind` (see [`EndpointKind::protocol`]).
    ///
    /// This is the single source of truth for "what protocol does this
    /// endpoint speak" — used by the connector registry (runtime
    /// projection) and the output resolver (MIDI-output-map filter) so an
    /// `OscEndpoint`/`ArtNetEndpoint`/HID endpoint never lands in the MIDI
    /// output port map.
    pub fn effective_protocol(&self) -> ConnectorProtocol {
        self.protocol.unwrap_or_else(|| self.kind.protocol())
    }
}

impl<'de> Deserialize<'de> for EndpointConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // ADR-035 §4.1 / R3: route through `toml::Value` rather than
        // `#[serde(flatten)]`. Config is TOML-only; a `toml::Value` round-trip
        // preserves typed scalars (unlike serde's flatten Content buffer) and
        // lets us reject stray/typo'd keys with a contextual error. See the
        // repo's `toml::from_str::<Value>` gotcha note.
        //
        // The GUI's `save_config` round-trips config through serde_json,
        // and TOML has no `null` — so a plain `toml::Value::deserialize` rejects
        // ANY JSON `null` outright ("invalid type: null, expected any valid TOML
        // value"), even for an optional field the GUI legitimately sends as null.
        // Deserialize each top-level field as `Option<toml::Value>` (a JSON null
        // → `None`) and hand the map — nulls included — to the strict parser,
        // which treats a null as absent for value extraction but still rejects a
        // null on an unknown/typo'd key (null is not a strictness
        // escape hatch). The config-load (TOML) path has no nulls, so every entry
        // is `Some(_)` and behaviour there is byte-for-byte unchanged.
        let raw: std::collections::BTreeMap<String, Option<toml::Value>> =
            std::collections::BTreeMap::deserialize(deserializer)?;
        endpoint_from_toml_value(raw).map_err(serde::de::Error::custom)
    }
}

/// Strict, contextual parse of one `[[endpoints]]` table into an
/// [`EndpointConfig`]. Factored out of the `Deserialize` impl so the
/// strictness rules are unit-testable directly. Returns a human-readable
/// error string (the impl wraps it via `serde::de::Error::custom`).
fn endpoint_from_toml_value(
    mut table: std::collections::BTreeMap<String, Option<toml::Value>>,
) -> Result<EndpointConfig, String> {
    // `table` maps field name → `Some(value)` for a present field, or `None` for
    // a field the caller sent as JSON `null`. A null is treated as
    // absent for value extraction (`take_opt` → `None`; `take` → missing-field
    // error), but the KEY stays in the map until consumed, so the strict
    // leftover-key check below still rejects a null on an unknown/typo'd field
    // (null must not bypass strictness). The TOML config-load
    // path produces no nulls, so every entry is `Some(_)` and behaviour is
    // unchanged. (A non-table endpoint errors earlier, when the caller
    // deserializes into the map.)
    type RawTable = std::collections::BTreeMap<String, Option<toml::Value>>;

    fn take<T: serde::de::DeserializeOwned>(t: &mut RawTable, key: &str) -> Result<T, String> {
        match t.remove(key) {
            // Absent, or present-but-null → the field is missing.
            None | Some(None) => Err(format!("missing field `{key}`")),
            Some(Some(v)) => v.try_into().map_err(|e| format!("invalid `{key}`: {e}")),
        }
    }
    fn take_opt<T: serde::de::DeserializeOwned>(
        t: &mut RawTable,
        key: &str,
    ) -> Result<Option<T>, String> {
        match t.remove(key) {
            // Absent OR explicit null → `None`.
            None | Some(None) => Ok(None),
            Some(Some(v)) => v
                .try_into()
                .map(Some)
                .map_err(|e| format!("invalid `{key}`: {e}")),
        }
    }

    // ADR-042 Phase A — the shared network-security fields (flattened on the
    // wire) for OSC / Art-Net listeners. Each is taken individually so the
    // strict "leftover key" check below still catches typos.
    fn take_network_security(t: &mut RawTable) -> Result<NetworkSecurityConfig, String> {
        Ok(NetworkSecurityConfig {
            allow_network: take_opt(t, "allow_network")?.unwrap_or(false),
            network_acl: take_opt(t, "network_acl")?.unwrap_or_default(),
            sender_acl: take_opt(t, "sender_acl")?.unwrap_or_default(),
            rate_limit_total: take_opt(t, "rate_limit_total")?,
            rate_limit_per_sender: take_opt(t, "rate_limit_per_sender")?,
            i_understand_amplification_risk: take_opt(t, "i_understand_amplification_risk")?
                .unwrap_or(false),
            allow_sensitive_actions: take_opt(t, "allow_sensitive_actions")?.unwrap_or(false),
            strict_mode: take_opt(t, "strict_mode")?,
        })
    }

    let alias: String = take(&mut table, "alias")?;
    let direction: ConnectorDirection = take(&mut table, "direction")?;
    let protocol: Option<ConnectorProtocol> = take_opt(&mut table, "protocol")?;
    let description: Option<String> = take_opt(&mut table, "description")?;
    let enabled: bool = take_opt(&mut table, "enabled")?.unwrap_or(true);
    let channels: Vec<u8> = take_opt(&mut table, "channels")?.unwrap_or_default();

    let type_tag: String = take(&mut table, "type")?;
    let kind = match type_tag.as_str() {
        "Matcher" => EndpointKind::Matcher {
            matchers: take_opt(&mut table, "matchers")?.unwrap_or_default(),
            input_matchers: take_opt(&mut table, "input_matchers")?.unwrap_or_default(),
            output_matchers: take_opt(&mut table, "output_matchers")?.unwrap_or_default(),
            no_probe: take_opt(&mut table, "no_probe")?.unwrap_or(false),
        },
        "MidiVirtualPort" => EndpointKind::MidiVirtualPort {
            port_name: take(&mut table, "port_name")?,
        },
        "OscEndpoint" => EndpointKind::OscEndpoint {
            host: take(&mut table, "host")?,
            port: take(&mut table, "port")?,
            security: take_network_security(&mut table)?,
        },
        "ArtNetEndpoint" => EndpointKind::ArtNetEndpoint {
            universe: take(&mut table, "universe")?,
            host: take_opt(&mut table, "host")?.unwrap_or_else(default_artnet_host),
            port: take_opt(&mut table, "port")?.unwrap_or_else(default_artnet_port),
            allow_broadcast: take_opt(&mut table, "allow_broadcast")?.unwrap_or(false),
            security: take_network_security(&mut table)?,
        },
        other => {
            return Err(format!(
                "unknown endpoint `type` \"{other}\" (expected one of: Matcher, \
                 MidiVirtualPort, OscEndpoint, ArtNetEndpoint)"
            ));
        }
    };

    // Strict: any leftover key is an unknown / typo'd field — the whole
    // reason this impl is hand-written (§4.1). `prot =` instead of
    // `protocol =`, or a stray `host` on `MidiVirtualPort`, errors here
    // instead of being silently dropped. A leftover key whose value was
    // `null` is rejected too (it stayed in the map), so JSON null can't be a
    // strictness escape hatch on an unknown field.
    if let Some(unknown) = table.keys().next() {
        return Err(format!(
            "unknown field `{unknown}` for endpoint type \"{type_tag}\" (alias \"{alias}\")"
        ));
    }

    Ok(EndpointConfig {
        alias,
        direction,
        protocol,
        description,
        enabled,
        channels,
        kind,
    })
}

/// A named I/O endpoint in the signal routing graph (ADR-031 D1).
///
/// Connectors extend `DeviceIdentityConfig` (ADR-022 bindings, input-only)
/// to cover output endpoints and bidirectional devices. Validation
/// (per spec § 3.3) enforces alias uniqueness across bindings + connectors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorConfig {
    /// User-defined alias (unique across connectors AND bindings).
    pub alias: String,

    /// Connector direction.
    #[serde(default)]
    pub direction: ConnectorDirection,

    /// Protocol this connector speaks.
    #[serde(default)]
    pub protocol: ConnectorProtocol,

    /// How to find this connector's physical port(s).
    pub endpoint: EndpointKind,

    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Whether this connector is active.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Per-connector MIDI channel scope (ADR-031 § 3.1).
    /// Empty = match all channels. Values are 0-indexed (0-15).
    /// Events on channels not in this list are dropped at the connector
    /// boundary. Channels are only meaningful for `protocol = "Midi"`;
    /// the validator warns when set on non-MIDI protocols.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<u8>,
}

// ────────────────────────────────────────────────────────
// Signal Routing Graph — ADR-031 D2 / Phase 2 (Routes)
// ────────────────────────────────────────────────────────

/// A signal path from one connector/binding to another (ADR-031 D2).
///
/// Routes operate below the mapping engine — signals flow through routes
/// unless intercepted by a trigger/action mapping. ADR-036 unifies routes
/// with the legacy `Trigger::Raw` mechanism by adding `modes` (scope).
/// Bare routes (no `modes`) remain mode-independent for backward
/// compatibility. All routes are post-mapping (ADR-036 Phase 3 removed the
/// `pre_mapping` escape hatch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteConfig {
    /// Source connector or binding alias.
    pub from: String,
    /// Destination connector or binding alias. (Validation accepts
    /// either — bindings can host MIDI output ports too.)
    pub to: String,
    /// Optional transform applied to signals in transit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<SignalTransform>,
    /// Optional filter. Only signals matching the filter are routed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<SignalFilter>,
    /// Whether this route is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Mode scope (ADR-036 D1). Empty = fires in all modes (legacy
    /// bare-route behaviour). Non-empty = fires only when one of the
    /// listed mode names is active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<String>,
}

/// Filter applied to signals in a route (ADR-031 D4).
/// Only signals matching ALL populated criteria pass through.
/// Empty/None fields are unconstrained (match everything).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalFilter {
    /// MIDI message types to include (empty = all).
    /// Reuses `MidiMessageType` from ADR-030 P1 — same enum used by
    /// `Trigger::Raw.message_types` so users learn one vocabulary.
    /// The validator (Phase 2A § 4.3) inherits ADR-030 §D7's restriction:
    /// `ChannelPressure` and `SysEx` are rejected (not yet emitted by the
    /// input pipeline). When/if those land, drop the restriction in both
    /// places at once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_types: Vec<MidiMessageType>,
    /// MIDI channel filter (empty = all channels). 0-indexed (0-15).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<u8>,
    /// MIDI CC range to include `[min, max]` (inclusive).
    /// Validator MUST reject `min > max` (per spec § 4.1 — same pattern
    /// as `CcValueInRange` rejection at config load). Otherwise the
    /// filter would silently match nothing and a route would never
    /// fire — the same failure mode, but harder to diagnose because no
    /// trigger is involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc_range: Option<(u8, u8)>,
    /// MIDI note range to include `[min, max]` (inclusive). Same
    /// `min > max` rejection applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_range: Option<(u8, u8)>,
    /// OSC address prefix to match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc_address_prefix: Option<String>,
}

/// Transform applied to signals in a route (ADR-031 D3).
/// Protocol-specific, not generic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SignalTransform {
    /// MIDI → MIDI transform (reuses existing `MidiTransform` from
    /// ADR-009 Gap 2 — channel/CC/note remap, velocity
    /// scale/offset, value invert, value curve).
    Midi(crate::transform::MidiTransform),

    /// MIDI → OSC cross-protocol translation (Phase 5).
    MidiToOsc {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cc_to_address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note_to_address: Option<String>,
        #[serde(default)]
        value_to_float: bool,
    },

    /// OSC → MIDI cross-protocol translation (Phase 5).
    OscToMidi {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address_to_cc: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address_to_note: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel: Option<u8>,
    },

    /// MIDI → Art-Net (CC/note values to DMX channel levels, Phase 5).
    ///
    /// `cc_to_dmx` / `note_to_dmx` are `HashMap<u8, u16>` in Rust
    /// (typed for safety + 7-bit MIDI semantics), but TOML requires
    /// string-typed table keys — so we serialise/deserialise via the
    /// `u8_string_map` helper which emits decimal-string keys
    /// (e.g. `7u8` → `"7"`) and parses them back via `u8::from_str`.
    MidiToArtNet {
        #[serde(with = "crate::config::u8_string_map")]
        cc_to_dmx: std::collections::HashMap<u8, u16>,
        #[serde(
            default,
            with = "crate::config::u8_string_map",
            skip_serializing_if = "std::collections::HashMap::is_empty"
        )]
        note_to_dmx: std::collections::HashMap<u8, u16>,
    },

    /// HID → Art-Net (analog axis values to DMX channel levels, Phase 5).
    HidToArtNet {
        trigger_to_channel: std::collections::HashMap<String, u16>,
    },

    /// OSC → Art-Net (ADR-039-A): extract the DMX channel from
    /// the OSC **address** via a template carrying a single `{dmx}` placeholder
    /// (same fallible-extraction convention as `OscToMidi`'s `{cc}`/`{note}` —
    /// the capture is attacker-controlled, so it is parsed fallibly and
    /// range-checked to the DMX universe 1-512 before any update is built).
    /// The first OSC argument becomes the 8-bit DMX level: `Float` is treated
    /// as normalised 0.0-1.0 → 0-255, `Int` clamps to 0-255.
    OscToArtNet {
        /// Address template with a `{dmx}` placeholder, e.g. `"/dmx/{dmx}"`.
        /// Validated at config-load (must start with `/` and contain exactly
        /// one `{dmx}`).
        address_to_dmx: String,
    },

    /// HID → MIDI (ADR-039-B): map a gamepad trigger name to a MIDI
    /// Control Change. The trigger's 7-bit value (button velocity / axis 0-127)
    /// becomes the CC value verbatim; emitted on `channel` (0-indexed 0-15).
    /// Keys are canonical gamepad trigger names (`south`, `left_stick_x`, …);
    /// `String` keys so TOML tables work directly (no `u8_string_map` needed).
    HidToMidi {
        trigger_to_cc: std::collections::HashMap<String, u8>,
        /// MIDI channel (0-indexed 0-15) for the emitted CC. Defaults to 0.
        #[serde(default)]
        channel: u8,
    },

    /// HID → OSC (ADR-039-B): map a gamepad trigger name to an OSC
    /// address; the trigger's 7-bit value (button velocity / axis 0-127) is the
    /// single OSC argument — a normalized `Float` 0.0-1.0 when `value_to_float`
    /// (the OSC convention), else a raw `Int`. Mirrors `MidiToOsc`'s
    /// `value_to_float` toggle. Keys are canonical gamepad trigger names; values
    /// are OSC addresses (must start with `/`, validated at config-load).
    HidToOsc {
        trigger_to_address: std::collections::HashMap<String, String>,
        /// Emit the value as a normalized `Float` (0.0-1.0); else a raw `Int`.
        #[serde(default)]
        value_to_float: bool,
    },
}
