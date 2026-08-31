// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Probe-on-connect orchestration helpers (ADR-026 Phases 3.C.2 + 3.C.3).
//!
//! When a previously-unbound MIDI input port becomes bound to a
//! configured `[[devices]]` identity, the daemon may auto-fire a
//! SysEx Universal Identity probe to capture the device's Identity
//! Reply and (when confidence is `DirectPairedPort`) trigger a
//! re-resolve so any `SysExIdentity` matchers that should match
//! actually do.
//!
//! This module is **pure logic only** — no I/O, no async, no
//! daemon state. The four exported helpers are:
//!
//! - [`should_probe_on_connect`]: gate read off `AdvancedSettings`
//!   flags. Phase 4 will extend this with per-device `no_probe`.
//! - [`compute_newly_opened_configured`]: diff helper —
//!   `(previous_known, current_bindings) → newly_opened`. Lets the
//!   `EngineManager` fire probes only for ports that just became
//!   bound, not every port on every rescan.
//! - [`classify_probe_outcome`] + [`ProbeOnConnectAction`]: turns
//!   a `Result<ProbeResult, ProbeStartError>` into a discrete action
//!   the caller's spawn task can apply (`AutoPromote`,
//!   `SurfaceConfirmation`, `LogNoReply`, `LogStartError`). Keeping
//!   the pattern-match here means the spawn task is just
//!   side-effect application, which keeps integration code thin.
//! - [`build_identity_needs_confirmation_event`] (Phase 3.C.3): turns
//!   a `SurfaceConfirmation` action into the `MonitorEvent` the
//!   daemon broadcasts to subscribed GUIs. Replaces the
//!   `tracing::warn!` placeholder used during 3.C.2 development.

use crate::daemon::types::MonitorEvent;
use conductor_core::config::Config;
use conductor_core::device_intelligence::probe::{
    IdentityConfidence, ProbeResult, ProbeStartError,
};
use conductor_core::device_intelligence::sysex_identity::SysExIdentity;
use conductor_core::identity::DeviceId;
use std::collections::HashSet;

/// Wire-format event_type discriminator for the `IdentityNeedsConfirmation`
/// MonitorEvent (ADR-026 Phase 3.C.3). Frontend stores match on this
/// string to route the payload into the pending-confirmation queue.
pub const IDENTITY_NEEDS_CONFIRMATION_EVENT_TYPE: &str = "identity_needs_confirmation";

/// Build the `MonitorEvent` the daemon broadcasts when probe-on-connect
/// produces a `SurfaceConfirmation` action — i.e. a `SharedRoute` or
/// `MultipleIdentified` outcome that the user must confirm before the
/// daemon auto-promotes any binding.
///
/// Frontend lookup matches on `event.event_type ==
/// IDENTITY_NEEDS_CONFIRMATION_EVENT_TYPE`. The structured payload
/// carries `{ port_name, candidates: [SysExIdentity] }`.
///
/// `MonitorEvent.device_id` is intentionally left unset (Copilot
/// #970 review): that field is documented as "Source device
/// identity (multi-device mode)" — a configured device alias from
/// `[[bindings]]`. The probe target here is identified by an OS
/// port name, not an alias yet — that's exactly the case the user
/// is being asked to confirm. Setting `device_id` to the port name
/// would confuse `EventFilter.device_id`-style consumers that
/// compare against the configured-alias set. Port info lives in
/// `payload.port_name` instead.
pub fn build_identity_needs_confirmation_event(
    port_name: &str,
    candidates: &[SysExIdentity],
    timestamp_ms: u64,
) -> MonitorEvent {
    let payload = serde_json::json!({
        "port_name": port_name,
        "candidates": candidates,
    });
    MonitorEvent {
        timestamp_ms,
        event_type: IDENTITY_NEEDS_CONFIRMATION_EVENT_TYPE.to_string(),
        detail: Some(format!(
            "Identity probe needs confirmation — {} candidate device(s) seen on port {}",
            candidates.len(),
            port_name
        )),
        payload: Some(payload),
        ..Default::default()
    }
}

/// Type alias for an `InputManager::get_device_bindings()` row:
/// `(device_id, port_name, connected, is_configured)`. Used by
/// [`ports_eligible_for_probe_on_connect`] so callers can pass the
/// bindings vec straight through without re-shaping.
pub type DeviceBinding = (DeviceId, String, bool, bool);

/// Returns `true` iff the global + per-feature gates allow firing a
/// probe-on-connect right now. Per ADR-026 D6 both flags default to
/// `true`; the global flag wins.
///
/// Phase 4 will extend this with a per-device `no_probe` field for
/// hardware that misbehaves under SysEx polling.
pub fn should_probe_on_connect(config: &Config) -> bool {
    let advanced = &config.advanced_settings;
    advanced.sysex_identity_probing && advanced.probe_on_connect
}

/// Compute the set of input port names that are bound to a
/// configured device *now* but were not bound on the previous
/// rescan. Pure set difference — `current` is the snapshot of
/// configured bindings (i.e. ports that resolved to a
/// `BindingResult::Bound`), `previous_known` is the equivalent
/// snapshot from the prior tick.
///
/// On the first call (`previous_known` empty) every currently-bound
/// port is returned. On a steady-state rescan with no churn the
/// result is empty.
pub fn compute_newly_opened_configured(
    previous_known: &HashSet<String>,
    current_configured_ports: &[String],
) -> HashSet<String> {
    current_configured_ports
        .iter()
        .filter(|p| !previous_known.contains(*p))
        .cloned()
        .collect()
}

/// Top-level gate combining [`should_probe_on_connect`] + binding
/// filter + diff-vs-last-known. Returns the set of input port names
/// for which the daemon should spawn a probe-on-connect task right
/// now. Pure function so the EngineManager wiring can stay thin and
/// the gating logic is fully unit-testable.
///
/// Filtering rules:
/// - Both flag gates must allow probing (otherwise empty result).
/// - Only `connected && is_configured` bindings are eligible —
///   probing a disconnected port can never succeed (no reply path),
///   and probing an unconfigured port has no paired output to send
///   to anyway.
/// - Only ports not in `previous_known` are returned, so steady-state
///   rescans don't burn the per-port rate-limit budget on probes
///   that already ran.
pub fn ports_eligible_for_probe_on_connect(
    config: &Config,
    bindings: &[DeviceBinding],
    previous_known: &HashSet<String>,
) -> HashSet<String> {
    if !should_probe_on_connect(config) {
        return HashSet::new();
    }
    let current_configured: Vec<String> = bindings
        .iter()
        .filter(|(_, _, connected, is_configured)| *connected && *is_configured)
        // Phase 4.2 — per-device opt-out filter. A binding with
        // `no_probe = true` skips the auto-probe path UNLESS its
        // matchers contain a `SysExIdentity` entry (the matcher would
        // never fire otherwise; nothing else populates the identity it
        // resolves against, see spec §4.2). Bindings without a matching
        // `[[devices]]` entry — or where the alias doesn't resolve —
        // pass through unchanged: those are typically raw / unmatched
        // ports, and `no_probe` is a configured-device concern.
        .filter(|(device_id, _, _, _)| !device_should_skip_auto_probe(config, device_id))
        .map(|(_, port, _, _)| port.clone())
        .collect();
    compute_newly_opened_configured(previous_known, &current_configured)
}

/// Per ADR-026 Phase 4.2 / ADR-035: returns `true` iff the resolved
/// `[[endpoints]]` entry for this `DeviceId` carries `no_probe = true`
/// AND none of its matchers are `SysExIdentity`. The SysExIdentity
/// override is on by design — a SysEx-keyed matcher cannot resolve
/// without a probe, so honouring `no_probe` for such endpoints would
/// permanently break them.
fn device_should_skip_auto_probe(config: &Config, device_id: &DeviceId) -> bool {
    let Some(endpoint) = config
        .endpoints
        .iter()
        .find(|e| e.alias == device_id.as_str())
    else {
        return false; // unmatched / raw port — let it through
    };
    endpoint.kind.no_probe() && !endpoint.kind.has_any_sysex_identity_matcher()
}

/// Atomic dispatch tick: combine the eligibility computation with
/// the `last_known` update, gated on the probing flag. The bug
/// without this gate is subtle but real:
///
/// 1. User has `probe_on_connect = false`, the daemon hot-plug loop
///    fires `dispatch_probe_on_connect_for_new_ports` on every tick.
/// 2. The dispatch ran `*last_known = current_configured` on every
///    call regardless of the gate, populating `last_known` with the
///    current ports.
/// 3. User flips `probe_on_connect = true` via config reload.
/// 4. Next dispatch tick: eligibility check sees current ports
///    already in `last_known` → empty set → no probes fire until a
///    port disconnects + reconnects.
///
/// Fix: only update `last_known` when probing is actually enabled.
/// On the toggle-on transition, `last_known` is whatever it was
/// the last time probing was on (empty if always off, or stale
/// from a prior on-period). Empty is the common case and gives
/// the expected "probe all current ports on enable" behaviour;
/// the off→on→off→on flip-flop edge case is acknowledged as a
/// minor known limitation (a port held connected across the
/// off period stays in `last_known` from the previous on-period
/// and won't re-probe until reconnect — acceptable since
/// reconnecting is the natural way to force a re-probe).
///
/// Returns the set of ports the caller should spawn probe tasks
/// for. Caller mutates `last_known` via the `&mut` argument; this
/// function's contract is "atomic decide-and-record".
pub fn process_dispatch_tick(
    config: &Config,
    bindings: &[DeviceBinding],
    last_known: &mut HashSet<String>,
) -> HashSet<String> {
    let eligible = ports_eligible_for_probe_on_connect(config, bindings, last_known);
    if should_probe_on_connect(config) {
        // PR #976 review (Copilot): `last_known` must reflect the
        // SAME filtered set the eligibility computation uses, not all
        // connected+configured bindings. Otherwise a `no_probe = true`
        // port lands in `last_known` and the user can't flip
        // `no_probe = false` via config reload to trigger a probe —
        // they'd have to physically reconnect the device. Keeping the
        // filter consistent makes the toggle a true two-way door.
        *last_known = bindings
            .iter()
            .filter(|(_, _, connected, is_configured)| *connected && *is_configured)
            .filter(|(device_id, _, _, _)| !device_should_skip_auto_probe(config, device_id))
            .map(|(_, port, _, _)| port.clone())
            .collect();
    }
    eligible
}

/// Action the spawn task should apply after a probe outcome lands.
/// Keeps the pattern-match pure — side effects (sending
/// `DaemonCommand::HotPlugCheck`, `tracing::warn!`, etc.) live at the
/// call site.
#[derive(Debug, PartialEq)]
pub enum ProbeOnConnectAction {
    /// Reply landed on the expected input port (`DirectPairedPort`).
    /// Trigger a `HotPlugCheck` so the resolver re-runs against the
    /// freshly-cached identity — any `SysExIdentity` matchers that
    /// should match will fire.
    AutoPromote {
        port_name: String,
        identity: SysExIdentity,
    },
    /// Probe identified the device but routing is ambiguous
    /// (`SharedRoute` for a single reply landing on a non-target
    /// port, OR `MultipleIdentified` for replies across multiple
    /// input ports). Caller should ask the user to confirm before
    /// binding (Phase 3.C.3 will surface this as
    /// `IdentityNeedsConfirmation`; for now the caller emits
    /// `tracing::warn!`).
    SurfaceConfirmation {
        port_name: String,
        candidates: Vec<SysExIdentity>,
    },
    /// Device didn't reply within the probe timeout. Common for
    /// non-SysEx-capable hardware; informational at debug level.
    LogNoReply { port_name: String, timeout_ms: u64 },
    /// Probe couldn't even be started (rate-limited, no paired
    /// output, send-error, etc.). Informational at debug level.
    LogStartError {
        port_name: String,
        error: ProbeStartError,
    },
}

/// Pure mapping from a `Result<ProbeResult, ProbeStartError>` to
/// the action the spawn task should apply. The `port_name` argument
/// is the original probe target — used for diagnostics when the
/// outcome doesn't carry it (e.g. `NoReply`).
pub fn classify_probe_outcome(
    outcome: Result<ProbeResult, ProbeStartError>,
    port_name: &str,
) -> ProbeOnConnectAction {
    match outcome {
        Ok(ProbeResult::Identified {
            identity,
            confidence: IdentityConfidence::DirectPairedPort,
            ..
        }) => ProbeOnConnectAction::AutoPromote {
            port_name: port_name.to_string(),
            identity,
        },
        Ok(ProbeResult::Identified {
            identity,
            confidence: IdentityConfidence::SharedRoute,
            ..
        }) => ProbeOnConnectAction::SurfaceConfirmation {
            port_name: port_name.to_string(),
            candidates: vec![identity],
        },
        Ok(ProbeResult::MultipleIdentified { identities, .. }) => {
            ProbeOnConnectAction::SurfaceConfirmation {
                port_name: port_name.to_string(),
                candidates: identities,
            }
        }
        Ok(ProbeResult::NoReply { timeout_ms }) => ProbeOnConnectAction::LogNoReply {
            port_name: port_name.to_string(),
            timeout_ms,
        },
        Err(error) => ProbeOnConnectAction::LogStartError {
            port_name: port_name.to_string(),
            error,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::config::types::AdvancedSettings;

    fn config_with_flags(global: bool, on_connect: bool) -> Config {
        let mut config = Config::default_config();
        config.advanced_settings = AdvancedSettings {
            sysex_identity_probing: global,
            probe_on_connect: on_connect,
            ..AdvancedSettings::default()
        };
        config
    }

    fn mock_identity(mfr: u8) -> SysExIdentity {
        SysExIdentity {
            manufacturer_id: vec![mfr],
            family: 0x0034,
            model: 0x0001,
            version: [0, 0, 0, 1],
        }
    }

    // ── should_probe_on_connect ───────────────────────────────────

    /// Default config: both flags on (per ADR-026 D6) → probe fires.
    #[test]
    fn should_probe_default_config_returns_true() {
        let config = Config::default_config();
        assert!(should_probe_on_connect(&config));
    }

    /// Global kill-switch off → no probe regardless of per-feature flag.
    #[test]
    fn should_probe_global_off_returns_false() {
        let config = config_with_flags(false, true);
        assert!(!should_probe_on_connect(&config));
    }

    /// Per-feature flag off, global on → no probe (auto-on-bind off).
    #[test]
    fn should_probe_on_connect_off_returns_false() {
        let config = config_with_flags(true, false);
        assert!(!should_probe_on_connect(&config));
    }

    /// Both off → no probe (redundant gate).
    #[test]
    fn should_probe_both_off_returns_false() {
        let config = config_with_flags(false, false);
        assert!(!should_probe_on_connect(&config));
    }

    // ── compute_newly_opened_configured ────────────────────────────

    /// First rescan: previous_known empty → every current port is
    /// "newly opened". Pins the cold-start behaviour (the daemon
    /// fires probes on initial setup, not just on hot-plug).
    #[test]
    fn newly_opened_first_rescan_returns_all_current() {
        let prev = HashSet::new();
        let current = vec!["port-A".to_string(), "port-B".to_string()];
        let new = compute_newly_opened_configured(&prev, &current);
        assert_eq!(
            new,
            ["port-A", "port-B"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>()
        );
    }

    /// Steady-state: same set in both → no new ports → no probes
    /// fire (avoid spurious re-probes on every rescan tick, which
    /// would burn the per-port rate-limit budget).
    #[test]
    fn newly_opened_steady_state_returns_empty() {
        let prev: HashSet<String> = ["port-A", "port-B"].iter().map(|s| s.to_string()).collect();
        let current = vec!["port-A".to_string(), "port-B".to_string()];
        let new = compute_newly_opened_configured(&prev, &current);
        assert!(new.is_empty());
    }

    /// Hot-plug add: one new port appears alongside existing ones →
    /// only the newcomer probes.
    #[test]
    fn newly_opened_hot_plug_add_returns_only_new_port() {
        let prev: HashSet<String> = ["port-A"].iter().map(|s| s.to_string()).collect();
        let current = vec!["port-A".to_string(), "port-B".to_string()];
        let new = compute_newly_opened_configured(&prev, &current);
        assert_eq!(
            new,
            ["port-B"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>()
        );
    }

    /// Hot-plug remove: a port disappears → result is empty (we don't
    /// probe ports that are no longer bound).
    #[test]
    fn newly_opened_hot_plug_remove_returns_empty() {
        let prev: HashSet<String> = ["port-A", "port-B"].iter().map(|s| s.to_string()).collect();
        let current = vec!["port-A".to_string()];
        let new = compute_newly_opened_configured(&prev, &current);
        assert!(new.is_empty());
    }

    /// Mixed churn: one removed, one added → only the added.
    #[test]
    fn newly_opened_mixed_churn_returns_only_added() {
        let prev: HashSet<String> = ["port-A"].iter().map(|s| s.to_string()).collect();
        let current = vec!["port-B".to_string()];
        let new = compute_newly_opened_configured(&prev, &current);
        assert_eq!(
            new,
            ["port-B"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>()
        );
    }

    // ── ports_eligible_for_probe_on_connect ─────────────────────────

    fn binding(alias: &str, port: &str, connected: bool, is_configured: bool) -> DeviceBinding {
        (
            DeviceId::from_alias(alias),
            port.to_string(),
            connected,
            is_configured,
        )
    }

    /// Default config + a fresh configured+connected binding → that
    /// port is eligible. Pins the basic happy path.
    #[test]
    fn eligible_default_config_returns_configured_connected_ports() {
        let config = Config::default_config();
        let bindings = vec![binding("mikro", "Mikro IN", true, true)];
        let prev = HashSet::new();
        let eligible = ports_eligible_for_probe_on_connect(&config, &bindings, &prev);
        assert_eq!(
            eligible,
            ["Mikro IN"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>()
        );
    }

    /// Global flag off → no ports eligible regardless of bindings.
    /// The global gate wins, so the EngineManager can short-circuit
    /// without iterating.
    #[test]
    fn eligible_global_flag_off_returns_empty() {
        let config = config_with_flags(false, true);
        let bindings = vec![binding("mikro", "Mikro IN", true, true)];
        let prev = HashSet::new();
        let eligible = ports_eligible_for_probe_on_connect(&config, &bindings, &prev);
        assert!(eligible.is_empty());
    }

    // ── Phase 4.2: per-device no_probe ──────────────────────────────
    // Spec §4.2 — bindings carrying `no_probe = true` skip the auto-
    // probe path. Manual Identify still works (test in this module
    // doesn't cover the manual path, that's `run_probe_device_identity`).
    // Exception: a binding whose matchers contain a `SysExIdentity`
    // entry IS still probed even with `no_probe = true`, otherwise the
    // matcher could never fire (the device is identified by SysEx,
    // there's no other way to know the matcher applies).

    /// Build an `EndpointConfig` test fixture with the given alias and
    /// `no_probe` value (ADR-035: the legacy `DeviceIdentityConfig` binding
    /// lowered to an input-direction `Matcher` endpoint). Helper exists only
    /// in tests; production endpoints come from `Config::default_config` /
    /// TOML loaders.
    fn device_with_no_probe(
        alias: &str,
        no_probe: bool,
        matchers: Vec<conductor_core::identity::DeviceMatcher>,
    ) -> conductor_core::config::types::EndpointConfig {
        use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
        EndpointConfig {
            alias: alias.to_string(),
            direction: ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers,
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe,
            },
        }
    }

    #[test]
    fn eligible_skips_bindings_with_per_device_no_probe() {
        // Default config (both global flags on) but the only configured
        // device carries `no_probe = true`. Eligibility set is empty —
        // the auto-probe path leaves this device alone.
        let mut config = Config::default_config();
        config.endpoints = vec![device_with_no_probe("mikro", true, vec![])];
        let bindings = vec![binding("mikro", "Mikro IN", true, true)];
        let prev = HashSet::new();
        let eligible = ports_eligible_for_probe_on_connect(&config, &bindings, &prev);
        assert!(
            eligible.is_empty(),
            "no_probe=true must skip auto-probe; got {:?}",
            eligible,
        );
    }

    #[test]
    fn eligible_no_probe_overridden_when_sysex_identity_matcher_present() {
        // Per spec: if a binding's matchers include a SysExIdentity
        // entry, the daemon ignores `no_probe` — otherwise the matcher
        // can never fire. The probe is the ONLY way to populate the
        // identity that the matcher then resolves against.
        use conductor_core::identity::DeviceMatcher;
        let mut config = Config::default_config();
        config.endpoints = vec![device_with_no_probe(
            "mikro",
            true, // user set no_probe
            vec![DeviceMatcher::SysExIdentity {
                manufacturer_id: vec![0x42],
                family: Some(0x0034),
                model: Some(0x0001),
            }],
        )];
        let bindings = vec![binding("mikro", "Mikro IN", true, true)];
        let prev = HashSet::new();
        let eligible = ports_eligible_for_probe_on_connect(&config, &bindings, &prev);
        assert_eq!(
            eligible,
            ["Mikro IN"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>(),
            "SysExIdentity matcher must override no_probe; otherwise matcher can never fire",
        );
    }

    #[test]
    fn eligible_skips_only_no_probe_bindings_when_others_are_eligible() {
        // Two devices, one with no_probe and one without — only the
        // permitted device should be probed. The opt-out is per-device,
        // not global.
        let mut config = Config::default_config();
        config.endpoints = vec![
            device_with_no_probe("mikro", true, vec![]),
            device_with_no_probe("nanok", false, vec![]),
        ];
        let bindings = vec![
            binding("mikro", "Mikro IN", true, true),
            binding("nanok", "nanoKONTROL IN", true, true),
        ];
        let prev = HashSet::new();
        let eligible = ports_eligible_for_probe_on_connect(&config, &bindings, &prev);
        assert_eq!(
            eligible,
            ["nanoKONTROL IN"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>(),
            "only the no_probe=false device should be eligible",
        );
    }

    /// Per-feature flag off → no ports eligible. Same shape as the
    /// global-off case from the caller's perspective.
    #[test]
    fn eligible_probe_on_connect_off_returns_empty() {
        let config = config_with_flags(true, false);
        let bindings = vec![binding("mikro", "Mikro IN", true, true)];
        let prev = HashSet::new();
        let eligible = ports_eligible_for_probe_on_connect(&config, &bindings, &prev);
        assert!(eligible.is_empty());
    }

    /// Disconnected configured binding → not eligible. Probing a
    /// disconnected port can never succeed (no reply path); skipping
    /// avoids burning the per-port 60 s rate-limit budget on a
    /// guaranteed NoReply.
    #[test]
    fn eligible_skips_disconnected_configured_ports() {
        let config = Config::default_config();
        let bindings = vec![binding("mikro", "Mikro IN", false, true)];
        let prev = HashSet::new();
        let eligible = ports_eligible_for_probe_on_connect(&config, &bindings, &prev);
        assert!(eligible.is_empty());
    }

    /// Unconfigured (opportunistic) binding → not eligible. There's
    /// no `[[devices]]` identity for it, hence no paired output to
    /// send the SysEx Identity Request to. (The probe coordinator
    /// would surface NoPairedOutput; better to short-circuit at the
    /// gate than spin up a task that immediately errors.)
    #[test]
    fn eligible_skips_unconfigured_ports() {
        let config = Config::default_config();
        let bindings = vec![binding("port:mikro:0", "Mikro IN", true, false)];
        let prev = HashSet::new();
        let eligible = ports_eligible_for_probe_on_connect(&config, &bindings, &prev);
        assert!(eligible.is_empty());
    }

    /// Steady state — port already in previous_known → not eligible.
    /// This is the dedup that prevents probing the same port on
    /// every rescan tick.
    #[test]
    fn eligible_skips_ports_already_known() {
        let config = Config::default_config();
        let bindings = vec![binding("mikro", "Mikro IN", true, true)];
        let prev: HashSet<String> = ["Mikro IN"].iter().map(|s| s.to_string()).collect();
        let eligible = ports_eligible_for_probe_on_connect(&config, &bindings, &prev);
        assert!(eligible.is_empty());
    }

    /// Mixed bindings: one configured+connected+new, one
    /// configured+disconnected, one unconfigured, one
    /// configured+connected+already-known. Only the first should be
    /// eligible.
    #[test]
    fn eligible_mixed_bindings_returns_only_new_configured_connected() {
        let config = Config::default_config();
        let bindings = vec![
            binding("mikro", "Mikro IN", true, true),   // ✓
            binding("nano", "nano IN", false, true),    // disconnected
            binding("port:0", "Other IN", true, false), // unconfigured
            binding("fcb", "FCB IN", true, true),       // already known
        ];
        let prev: HashSet<String> = ["FCB IN"].iter().map(|s| s.to_string()).collect();
        let eligible = ports_eligible_for_probe_on_connect(&config, &bindings, &prev);
        assert_eq!(
            eligible,
            ["Mikro IN"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>()
        );
    }

    // ── process_dispatch_tick ──────────────────────────────────────

    /// Probing on, fresh `last_known`: returns all
    /// configured+connected ports as eligible AND populates
    /// `last_known` with that set so the next tick won't re-probe.
    #[test]
    fn dispatch_tick_probing_on_populates_last_known_and_returns_eligible() {
        let config = Config::default_config();
        let bindings = vec![
            binding("mikro", "Mikro IN", true, true),
            binding("fcb", "FCB IN", true, true),
        ];
        let mut last_known = HashSet::new();
        let eligible = process_dispatch_tick(&config, &bindings, &mut last_known);
        let expected: HashSet<String> = ["Mikro IN", "FCB IN"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(eligible, expected);
        assert_eq!(last_known, expected);
    }

    /// Probing on, steady-state second tick: no new ports eligible,
    /// `last_known` stays correct (the dispatch refreshes it to the
    /// current set anyway).
    #[test]
    fn dispatch_tick_probing_on_steady_state_returns_empty_keeps_last_known() {
        let config = Config::default_config();
        let bindings = vec![binding("mikro", "Mikro IN", true, true)];
        let mut last_known: HashSet<String> = ["Mikro IN"].iter().map(|s| s.to_string()).collect();
        let eligible = process_dispatch_tick(&config, &bindings, &mut last_known);
        assert!(eligible.is_empty());
        assert_eq!(
            last_known,
            ["Mikro IN"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>()
        );
    }

    /// **Bug fix pin**: probing OFF must NOT pollute `last_known`.
    /// Otherwise a later toggle-on would see ports already-known and
    /// fail to probe them. PR #930 review caught this — pre-fix the
    /// dispatcher always wrote to `last_known` regardless of the
    /// gate.
    #[test]
    fn dispatch_tick_probing_off_does_not_update_last_known() {
        let config = config_with_flags(false, true);
        let bindings = vec![binding("mikro", "Mikro IN", true, true)];
        let mut last_known = HashSet::new();
        let eligible = process_dispatch_tick(&config, &bindings, &mut last_known);
        assert!(eligible.is_empty(), "probing off → no eligible");
        assert!(
            last_known.is_empty(),
            "probing off must NOT pollute last_known — got {:?}",
            last_known
        );
    }

    /// Toggle-on path: probing was off (last_known stayed empty),
    /// then user enables it via config reload. Currently-connected
    /// ports SHOULD be eligible on the first dispatch after enable.
    /// This test pins the user-visible promise that flipping the
    /// flag on probes the existing fleet, not just future
    /// hot-plug arrivals.
    #[test]
    fn dispatch_tick_no_probe_binding_not_added_to_last_known() {
        // PR #976 review (Copilot): bindings filtered out by `no_probe`
        // must NOT be recorded in `last_known`. Otherwise, flipping
        // `no_probe = false` via config reload would see the port
        // already-known and not probe it until physical reconnect —
        // making the toggle a one-way door at runtime.
        let mut config = Config::default_config();
        config.endpoints = vec![device_with_no_probe("mikro", true, vec![])];
        let bindings = vec![binding("mikro", "Mikro IN", true, true)];
        let mut last_known = HashSet::new();

        // Tick 1 — no_probe=true → port skipped, NOT recorded.
        let eligible = process_dispatch_tick(&config, &bindings, &mut last_known);
        assert!(
            eligible.is_empty(),
            "no_probe=true must yield no eligible ports"
        );
        assert!(
            !last_known.contains("Mikro IN"),
            "no_probe-skipped port must not leak into last_known; last_known = {:?}",
            last_known,
        );

        // Tick 2 — user flips no_probe=false via reload. Port was NOT
        // in last_known so it should be eligible now (fresh probe).
        if let conductor_core::config::types::EndpointKind::Matcher { no_probe, .. } =
            &mut config.endpoints[0].kind
        {
            *no_probe = false;
        }
        let eligible = process_dispatch_tick(&config, &bindings, &mut last_known);
        assert_eq!(
            eligible,
            ["Mikro IN"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>(),
            "flipping no_probe=false on reload must trigger probe without reconnect",
        );
    }

    #[test]
    fn dispatch_tick_toggle_off_then_on_probes_currently_connected_ports() {
        // Tick 1 — probing off, port already connected, last_known
        // stays empty (per the bug-fix pin above).
        let config_off = config_with_flags(false, true);
        let bindings = vec![binding("mikro", "Mikro IN", true, true)];
        let mut last_known = HashSet::new();
        let eligible = process_dispatch_tick(&config_off, &bindings, &mut last_known);
        assert!(eligible.is_empty());
        assert!(last_known.is_empty(), "off-tick must keep last_known empty");

        // Tick 2 — user enables probing via config reload, same port
        // still connected. Now the port should be eligible because
        // last_known stayed empty during the off period.
        let config_on = Config::default_config();
        let eligible = process_dispatch_tick(&config_on, &bindings, &mut last_known);
        assert_eq!(
            eligible,
            ["Mikro IN"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>(),
            "after enable, currently-connected port must be eligible"
        );
        assert_eq!(
            last_known,
            ["Mikro IN"]
                .iter()
                .map(|s| s.to_string())
                .collect::<HashSet<String>>(),
            "post-enable tick must populate last_known"
        );
    }

    // ── classify_probe_outcome ─────────────────────────────────────

    /// Identified + DirectPairedPort → AutoPromote. The canonical
    /// happy path: paired output sent the request, the expected
    /// input received it, no thru-box weirdness. Trigger a
    /// re-resolve to fire any SysExIdentity matchers.
    #[test]
    fn classify_direct_paired_port_returns_auto_promote() {
        let identity = mock_identity(0x42);
        let action = classify_probe_outcome(
            Ok(ProbeResult::Identified {
                identity: identity.clone(),
                confidence: IdentityConfidence::DirectPairedPort,
                rtt_ms: 5,
                probed_at_unix_ms: 1_700_000_000_000,
            }),
            "port-A",
        );
        assert_eq!(
            action,
            ProbeOnConnectAction::AutoPromote {
                port_name: "port-A".to_string(),
                identity,
            }
        );
    }

    /// Identified + SharedRoute → SurfaceConfirmation. The reply
    /// landed on a non-target port (thru-box echoed it onto a
    /// sibling) — auto-binding could pick the wrong port, so ask
    /// the user.
    #[test]
    fn classify_shared_route_returns_surface_confirmation() {
        let identity = mock_identity(0x42);
        let action = classify_probe_outcome(
            Ok(ProbeResult::Identified {
                identity: identity.clone(),
                confidence: IdentityConfidence::SharedRoute,
                rtt_ms: 5,
                probed_at_unix_ms: 1_700_000_000_000,
            }),
            "port-A",
        );
        assert_eq!(
            action,
            ProbeOnConnectAction::SurfaceConfirmation {
                port_name: "port-A".to_string(),
                candidates: vec![identity],
            }
        );
    }

    /// MultipleIdentified → SurfaceConfirmation with all candidates.
    /// The merger case: multiple devices answered the broadcast, or
    /// the same device's reply echoed onto multiple input ports.
    #[test]
    fn classify_multiple_identified_returns_surface_confirmation() {
        let id1 = mock_identity(0x42);
        let id2 = mock_identity(0x41);
        let action = classify_probe_outcome(
            Ok(ProbeResult::MultipleIdentified {
                identities: vec![id1.clone(), id2.clone()],
                rtt_ms: 5,
                probed_at_unix_ms: 1_700_000_000_000,
            }),
            "port-A",
        );
        assert_eq!(
            action,
            ProbeOnConnectAction::SurfaceConfirmation {
                port_name: "port-A".to_string(),
                candidates: vec![id1, id2],
            }
        );
    }

    /// NoReply → LogNoReply. Common case for non-SysEx-capable
    /// hardware; the binding stays in its pre-probe state. Caller
    /// emits `tracing::debug!` rather than user-facing noise.
    #[test]
    fn classify_no_reply_returns_log_no_reply() {
        let action =
            classify_probe_outcome(Ok(ProbeResult::NoReply { timeout_ms: 1000 }), "port-A");
        assert_eq!(
            action,
            ProbeOnConnectAction::LogNoReply {
                port_name: "port-A".to_string(),
                timeout_ms: 1000,
            }
        );
    }

    /// Err(start_error) → LogStartError. The probe couldn't even
    /// fire (rate-limited, no paired output, send error). Caller
    /// emits `tracing::debug!`. The error variant is preserved so
    /// observability tools can break it down by category.
    #[test]
    fn classify_start_error_returns_log_start_error() {
        let action = classify_probe_outcome(
            Err(ProbeStartError::RateLimited {
                retry_after_ms: 60_000,
            }),
            "port-A",
        );
        assert_eq!(
            action,
            ProbeOnConnectAction::LogStartError {
                port_name: "port-A".to_string(),
                error: ProbeStartError::RateLimited {
                    retry_after_ms: 60_000,
                },
            }
        );
    }

    // ── build_identity_needs_confirmation_event (Phase 3.C.3) ─────

    #[test]
    fn confirmation_event_carries_port_name_and_candidates_in_payload() {
        // Two identities on a thru-box → MultipleIdentified path. The
        // frontend's pending-confirmation store must be able to read
        // the full candidate list out of the payload to render a
        // confirmation dialog.
        let candidates = vec![mock_identity(0x42), mock_identity(0x41)];
        let event = build_identity_needs_confirmation_event(
            "Komplete Audio 6 MK2",
            &candidates,
            1_700_000_000_000,
        );

        assert_eq!(event.event_type, IDENTITY_NEEDS_CONFIRMATION_EVENT_TYPE);
        assert_eq!(event.timestamp_ms, 1_700_000_000_000);
        // device_id intentionally unset (#970 review) — port_name
        // lives in the payload so EventFilter.device_id consumers
        // don't get confused by an OS port name appearing where
        // they expect a configured device alias.
        assert!(event.device_id.is_none());

        let payload = event.payload.expect("payload must be set");
        assert_eq!(payload["port_name"], "Komplete Audio 6 MK2");
        let payload_candidates = payload["candidates"]
            .as_array()
            .expect("candidates must be an array");
        assert_eq!(payload_candidates.len(), 2);
        assert_eq!(payload_candidates[0]["manufacturer_id"][0], 0x42);
        assert_eq!(payload_candidates[1]["manufacturer_id"][0], 0x41);
    }

    #[test]
    fn confirmation_event_handles_single_candidate_for_shared_route() {
        // SharedRoute single-reply case — one candidate, but still
        // requires confirmation per ADR-026 D5.
        let candidates = vec![mock_identity(0x42)];
        let event =
            build_identity_needs_confirmation_event("Mikro IN", &candidates, 1_700_000_000_000);

        let payload = event.payload.unwrap();
        assert_eq!(
            payload["candidates"].as_array().unwrap().len(),
            1,
            "SharedRoute single-reply must still produce a confirmation event"
        );
        assert!(
            event.detail.unwrap().contains("1 candidate"),
            "human-readable detail should reflect the count"
        );
    }

    #[test]
    fn confirmation_event_detail_includes_port_name_for_log_correlation() {
        // Operators grepping logs need port_name in the message so
        // they can correlate with hardware setup actions. (Avoids
        // "which port was that?" when 6 controllers are plugged in.)
        let event =
            build_identity_needs_confirmation_event("FCB1010 IN", &[mock_identity(0x00)], 0);
        assert!(event.detail.unwrap().contains("FCB1010 IN"));
    }
}
