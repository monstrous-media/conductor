// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Output port enumeration and auto-pairing for multi-device architecture (ADR-021 Phase 1B)
//!
//! Provides MIDI output port enumeration, name-based auto-pairing between input
//! and output ports, and explicit matcher-based output resolution.

use conductor_core::config::types::{
    ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
};
use midir::MidiOutput;
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

/// Maximum number of MIDI output ports to enumerate, preventing unbounded allocation.
const MAX_MIDI_DEVICES: usize = 64;

/// Common suffixes for MIDI input port names.
/// Ordered longest-first so `" MIDI In"` is tried before `" In"` (avoids partial strip).
const INPUT_SUFFIXES: &[&str] = &[" MIDI IN", " MIDI In", " Input", " In"];

/// Common suffixes for MIDI output port names.
/// Ordered longest-first to match the corresponding input suffix ordering.
const OUTPUT_SUFFIXES: &[&str] = &[" MIDI OUT", " MIDI Out", " Output", " Out"];

/// Result of resolving an output port for a device.
#[derive(Debug, Clone)]
pub struct OutputResolution {
    /// The matched output port name.
    pub port_name: String,
    /// Whether this was auto-paired (true) or explicitly matched (false).
    pub auto_paired: bool,
}

/// Enumerate available MIDI output port names.
///
/// Creates a transient `MidiOutput` client to scan for output ports.
/// Results are capped at [`MAX_MIDI_DEVICES`] (64) to prevent unbounded allocation.
///
/// Note: `MidiOutputManager::connect_by_name()` uses a CoreMIDI warmup/sleep
/// when *opening* an output connection, but port *enumeration* via `midir` does
/// not require it — `MidiOutput::new()` + `.ports()` returns the current list
/// without the stale-cache issue that affects `MidiInput` on macOS (#108/#110).
pub fn enumerate_output_ports() -> Vec<String> {
    match MidiOutput::new("Conductor Output Scanner") {
        Ok(midi_out) => {
            let ports = midi_out.ports();
            if ports.len() > MAX_MIDI_DEVICES {
                warn!(
                    "MIDI output port count ({}) exceeds limit ({}), some ports will not be resolved",
                    ports.len(),
                    MAX_MIDI_DEVICES
                );
            }
            let mut names = Vec::new();
            for (i, port) in ports.iter().enumerate().take(MAX_MIDI_DEVICES) {
                match midi_out.port_name(port) {
                    Ok(name) => names.push(name),
                    Err(e) => {
                        warn!("Failed to get name for MIDI output port {}: {}", i, e);
                    }
                }
            }
            names
        }
        Err(e) => {
            warn!("Failed to create MIDI output for port enumeration: {}", e);
            Vec::new()
        }
    }
}

/// Auto-pair an input port to an output port by name similarity.
///
/// Algorithm:
/// 1. Strip known input suffix from `input_port_name` to get the base name.
/// 2. Try exact suffix match: for each output suffix, check if `"{base}{suffix}"` exists.
/// 3. Substring fallback: find output ports containing `base`. Return if exactly 1 match.
pub fn auto_pair_output(input_port_name: &str, available_outputs: &[String]) -> Option<String> {
    if available_outputs.is_empty() {
        return None;
    }

    // Step 1: Strip input suffix to get base name
    let base = INPUT_SUFFIXES
        .iter()
        .find_map(|suffix| input_port_name.strip_suffix(suffix))
        .unwrap_or(input_port_name);

    // Step 2: Try exact suffix match
    for suffix in OUTPUT_SUFFIXES {
        let candidate = format!("{}{}", base, suffix);
        if available_outputs.contains(&candidate) {
            return Some(candidate);
        }
    }

    // Step 3: Substring fallback — return only if exactly 1 match
    let matches: Vec<&String> = available_outputs
        .iter()
        .filter(|o| o.contains(base))
        .collect();
    if matches.len() == 1 {
        return Some(matches[0].clone());
    }

    None
}

/// Build the unified alias → MIDI-output-port map from the normalized
/// endpoint set (ADR-035 Slice 9.5).
///
/// Replaces the legacy `build_device_output_map` (`[[bindings]]`) +
/// `build_connector_output_map` (`[[connectors]]`) pair: both authored
/// `[[endpoints]]` and lowered legacy blocks arrive here as one
/// [`EndpointConfig`] slice (via `normalize_to_endpoints`), so output
/// resolution runs through a single path.
///
/// Only **MIDI** endpoints produce entries — `effective_protocol()` filters
/// out `OscEndpoint`/`ArtNetEndpoint` (which dispatch through
/// `ConnectorRegistry::send_osc`/`send_artnet`) and HID endpoints (input-only,
/// no MIDI output). This is the uniform protocol gate that the legacy device
/// path lacked: a HID `[[bindings]]` device can no longer accidentally
/// auto-pair to a like-named MIDI output port.
///
/// Resolution per endpoint:
/// 1. **Explicit output** (direction ∈ {Output, Bidirectional}):
///    - `Matcher` → highest-`specificity()` match of
///      `effective_matchers(Output)` against `available_outputs`
///      (`auto_paired = false`).
///    - `MidiVirtualPort` → the declared `port_name` directly
///      (`auto_paired = false`; the daemon creates a virtual port of
///      exactly that name).
/// 2. **Auto-pair** (any endpoint with an input binding that did not resolve
///    an explicit output): suffix-heuristic pairing from the bound input port
///    name (`auto_paired = true`). This is what preserves LED-feedback output
///    for input-only endpoints.
///
/// Endpoints that are disabled, non-MIDI, or have neither an input binding nor
/// an output-capable direction are skipped silently (nothing to resolve —
/// logging "no output found" would be noise).
pub fn build_output_map(
    endpoints: &[EndpointConfig],
    input_bindings: &[(String, String)],
    available_outputs: &[String],
) -> HashMap<String, OutputResolution> {
    let mut map = HashMap::new();

    for ep in endpoints {
        // Skip disabled endpoints (consistent with PortResolver::resolve()).
        if !ep.enabled {
            continue;
        }
        // Uniform protocol gate: only MIDI endpoints reach the MIDI output
        // map. OSC / Art-Net dispatch through ConnectorRegistry; HID is
        // input-only and has no MIDI output.
        if ep.effective_protocol() != ConnectorProtocol::Midi {
            continue;
        }

        let input_port = input_bindings
            .iter()
            .find(|(alias, _)| alias == &ep.alias)
            .map(|(_, port)| port.as_str());

        let output_capable = matches!(
            ep.direction,
            ConnectorDirection::Output | ConnectorDirection::Bidirectional
        );

        // Nothing to resolve: an input-only endpoint that isn't currently
        // bound has neither an output direction nor a port to auto-pair from.
        if input_port.is_none() && !output_capable {
            continue;
        }

        // Priority 1: explicit output (output-capable endpoints only).
        let mut resolution: Option<OutputResolution> = None;
        if output_capable {
            match &ep.kind {
                EndpointKind::MidiVirtualPort { port_name } => {
                    // Direct mapping — the daemon creates a virtual port of
                    // exactly this name; no port-list lookup.
                    resolution = Some(OutputResolution {
                        port_name: port_name.clone(),
                        auto_paired: false,
                    });
                }
                EndpointKind::Matcher { .. } => {
                    let matchers = ep.kind.effective_matchers(ConnectorDirection::Output);
                    let mut best: Option<(&String, u32)> = None;
                    for port in available_outputs {
                        for matcher in matchers {
                            if matcher.matches(port) {
                                let score = matcher.specificity();
                                if best.is_none_or(|(_, s)| score > s) {
                                    best = Some((port, score));
                                }
                            }
                        }
                    }
                    if let Some((port, _)) = best {
                        resolution = Some(OutputResolution {
                            port_name: port.clone(),
                            auto_paired: false,
                        });
                    }
                }
                // Non-MIDI kinds are unreachable here (filtered by the
                // protocol gate above), but be exhaustive.
                EndpointKind::OscEndpoint { .. } | EndpointKind::ArtNetEndpoint { .. } => {}
            }
        }

        // Priority 2: auto-pair from the bound input port name.
        if resolution.is_none()
            && let Some(input_name) = input_port
            && let Some(output_name) = auto_pair_output(input_name, available_outputs)
        {
            resolution = Some(OutputResolution {
                port_name: output_name,
                auto_paired: true,
            });
        }

        match resolution {
            Some(res) => {
                let method = if res.auto_paired {
                    "auto-paired"
                } else {
                    "explicit"
                };
                info!(
                    "Endpoint '{}': output port '{}' ({})",
                    ep.alias, res.port_name, method
                );
                map.insert(ep.alias.clone(), res);
            }
            None => {
                if available_outputs.is_empty() {
                    info!(
                        "Endpoint '{}': no output port found (no MIDI output ports available)",
                        ep.alias
                    );
                } else {
                    info!(
                        "Endpoint '{}': no output port found — add output matchers for explicit binding",
                        ep.alias
                    );
                }
            }
        }
    }

    map
}

/// The set of OS virtual-MIDI-port names the daemon should keep created for a
/// given endpoint set (#2063 / ADR-035, ADR-031 D10 "DAW proxy model").
///
/// Each enabled `MidiVirtualPort` endpoint declares a `port_name` that the
/// daemon must materialize as a real OS MIDI port — without it a route to that
/// alias fails with "port not found" and external apps (DAWs) can't see it.
/// This is the desired-state input to [`crate::action_executor::ActionExecutor::sync_virtual_ports`];
/// keeping it a pure function (no I/O) makes the create/teardown decision
/// testable without a CoreMIDI/ALSA session. Disabled endpoints are excluded so
/// a reload that flips `enabled = false` tears the port down. Order-preserving
/// and de-duplicated (two endpoints can't both own the same OS port name).
pub fn desired_virtual_port_names(endpoints: &[EndpointConfig]) -> Vec<String> {
    let mut names = Vec::new();
    for ep in endpoints {
        if !ep.enabled {
            continue;
        }
        if let EndpointKind::MidiVirtualPort { port_name } = &ep.kind
            && !names.contains(port_name)
        {
            names.push(port_name.clone());
        }
    }
    names
}

/// The set of MIDI output port names a dispatch can currently reach: the live
/// OS enumeration ([`enumerate_output_ports`]) UNION the enabled
/// `MidiVirtualPort` endpoints this daemon materializes
/// ([`desired_virtual_port_names`]).
///
/// #2421: a daemon-created virtual output port is reachable but does NOT appear
/// in `MidiOutput::ports()` from the creating process — that enumeration lists
/// ports owned by *other* processes. Gating an endpoint's `connected` flag
/// purely on the live enumeration (#2203) therefore reported daemon virtual
/// ports red in the Endpoints + Routing Graph views, while the Discovered Ports
/// view — a separate enumeration that *does* see them — showed them green.
/// Folding the daemon's own materialized virtual ports back in makes the three
/// views agree.
///
/// This does NOT weaken the #2203 guard: only *enabled* `MidiVirtualPort`
/// endpoints contribute (the same desired-set `sync_virtual_ports` actually
/// creates), so a disabled or never-created virtual port is still absent from
/// the reachable set and stays disconnected.
pub fn reachable_output_ports(
    enumerated: impl IntoIterator<Item = String>,
    endpoints: &[EndpointConfig],
    virtual_ports_supported: bool,
) -> HashSet<String> {
    let mut reachable: HashSet<String> = enumerated.into_iter().collect();
    // Only fold in config virtual ports on platforms where the daemon can
    // actually create them. On Windows `MidiOutputManager` has no virtual-port
    // registry (`sync_virtual_ports` is a no-op), so a configured
    // `MidiVirtualPort` is never materialized and must NOT be reported reachable
    // (Copilot review on PR #2443). Callers pass
    // `MidiOutputManager::virtual_ports_available()`.
    if virtual_ports_supported {
        reachable.extend(desired_virtual_port_names(endpoints));
    }
    reachable
}

/// Async wrapper that runs output port enumeration in a blocking thread.
pub async fn enumerate_output_ports_async() -> Vec<String> {
    match tokio::task::spawn_blocking(enumerate_output_ports).await {
        Ok(ports) => ports,
        Err(e) => {
            warn!("Output port enumeration task failed: {}", e);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conductor_core::identity::DeviceMatcher;

    // --- auto_pair_output tests ---

    #[test]
    fn test_auto_pair_ni_naming() {
        let outputs = vec![
            "Maschine Mikro MK3 Output".to_string(),
            "Other Device".to_string(),
        ];
        let result = auto_pair_output("Maschine Mikro MK3 Input", &outputs);
        assert_eq!(result, Some("Maschine Mikro MK3 Output".to_string()));
    }

    #[test]
    fn test_auto_pair_novation_naming() {
        let outputs = vec!["Launchpad MIDI Out".to_string(), "Other Device".to_string()];
        let result = auto_pair_output("Launchpad MIDI In", &outputs);
        assert_eq!(result, Some("Launchpad MIDI Out".to_string()));
    }

    #[test]
    fn test_auto_pair_generic_usb_midi() {
        let outputs = vec!["USB MIDI Device".to_string()];
        // No suffix to strip, base = "USB MIDI Device", substring match finds exactly 1
        let result = auto_pair_output("USB MIDI Device", &outputs);
        assert_eq!(result, Some("USB MIDI Device".to_string()));
    }

    #[test]
    fn test_auto_pair_ambiguous() {
        let outputs = vec![
            "Mikro MK3 Output".to_string(),
            "Mikro MK2 Output".to_string(),
        ];
        // base = "Mikro" (after stripping " Input"), both outputs contain "Mikro"
        let result = auto_pair_output("Mikro Input", &outputs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_auto_pair_no_match() {
        let outputs = vec![
            "Completely Different Device".to_string(),
            "Another Device".to_string(),
        ];
        let result = auto_pair_output("Synth Controller Input", &outputs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_auto_pair_empty_outputs() {
        let result = auto_pair_output("Any Device Input", &[]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_auto_pair_midi_in_suffix() {
        let outputs = vec!["Controller MIDI Out".to_string()];
        let result = auto_pair_output("Controller MIDI In", &outputs);
        assert_eq!(result, Some("Controller MIDI Out".to_string()));
    }

    #[test]
    fn test_auto_pair_midi_in_uppercase_suffix() {
        let outputs = vec!["Controller MIDI OUT".to_string()];
        let result = auto_pair_output("Controller MIDI IN", &outputs);
        assert_eq!(result, Some("Controller MIDI OUT".to_string()));
    }

    #[test]
    fn test_auto_pair_long_suffix_preferred_over_short() {
        // "Launchpad MIDI In" should strip " MIDI In" (not just " In") to get base "Launchpad"
        // Then match "Launchpad MIDI Out" via " MIDI Out" suffix
        let outputs = vec!["Launchpad MIDI Out".to_string()];
        let result = auto_pair_output("Launchpad MIDI In", &outputs);
        assert_eq!(result, Some("Launchpad MIDI Out".to_string()));

        // Verify the base is "Launchpad", not "Launchpad MIDI"
        // If base were "Launchpad MIDI", it would try "Launchpad MIDI MIDI Out" (no match)
        // and fall back to substring, which also works — but the exact suffix path is correct
        let outputs_with_extra = vec![
            "Launchpad MIDI Out".to_string(),
            "Launchpad MIDI OUT".to_string(), // 2 matches for substring "Launchpad"
        ];
        // With correct longest-first ordering, exact suffix match finds "Launchpad MIDI OUT" first
        // (OUTPUT_SUFFIXES tries " MIDI OUT" before " MIDI Out")
        let result = auto_pair_output("Launchpad MIDI In", &outputs_with_extra);
        // Should match via exact suffix, not fail as ambiguous substring
        assert!(result.is_some());
    }

    // --- build_output_map tests (ADR-035 Slice 9.5) ---

    fn endpoint(alias: &str, direction: ConnectorDirection, kind: EndpointKind) -> EndpointConfig {
        EndpointConfig {
            alias: alias.to_string(),
            direction,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind,
        }
    }

    /// Symmetric `Matcher` kind (matchers used for both directions).
    fn matcher_kind(matchers: Vec<DeviceMatcher>) -> EndpointKind {
        EndpointKind::Matcher {
            matchers,
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        }
    }

    /// Asymmetric `Matcher` kind (distinct input/output matchers).
    fn asym_kind(input: Vec<DeviceMatcher>, output: Vec<DeviceMatcher>) -> EndpointKind {
        EndpointKind::Matcher {
            matchers: vec![],
            input_matchers: input,
            output_matchers: output,
            no_probe: false,
        }
    }

    #[test]
    fn input_only_endpoint_auto_pairs_feedback_output() {
        // An Input-direction endpoint with a bound input port still gets an
        // auto-paired output so LED feedback keeps working (legacy device
        // auto-pair behaviour preserved).
        let ep = endpoint(
            "mikro",
            ConnectorDirection::Input,
            matcher_kind(vec![DeviceMatcher::name_contains("Mikro")]),
        );
        let outputs = vec!["Maschine Mikro MK3 Output".to_string()];
        let bindings = vec![("mikro".to_string(), "Maschine Mikro MK3 Input".to_string())];
        let map = build_output_map(&[ep], &bindings, &outputs);
        assert_eq!(map.len(), 1);
        assert_eq!(map["mikro"].port_name, "Maschine Mikro MK3 Output");
        assert!(
            map["mikro"].auto_paired,
            "input-only feedback is auto-paired"
        );
    }

    #[test]
    fn output_only_endpoint_resolves_via_explicit_matcher() {
        let ep = endpoint(
            "leds",
            ConnectorDirection::Output,
            asym_kind(vec![], vec![DeviceMatcher::name_contains("LED")]),
        );
        let outputs = vec!["LED Controller Output".to_string()];
        let bindings: Vec<(String, String)> = vec![]; // not an input device
        let map = build_output_map(&[ep], &bindings, &outputs);
        assert_eq!(map.len(), 1);
        assert_eq!(map["leds"].port_name, "LED Controller Output");
        assert!(
            !map["leds"].auto_paired,
            "explicit output is not auto-paired"
        );
    }

    #[test]
    fn bidirectional_symmetric_matcher_resolves_explicit_output() {
        // Symmetric matchers double as output matchers via
        // effective_matchers(Output); when they match an output port the
        // explicit path wins over auto-pair.
        let ep = endpoint(
            "iac",
            ConnectorDirection::Bidirectional,
            matcher_kind(vec![DeviceMatcher::name_contains("IAC")]),
        );
        let outputs = vec!["IAC Driver Bus 1".to_string()];
        let bindings = vec![("iac".to_string(), "IAC Driver Bus 1".to_string())];
        let map = build_output_map(&[ep], &bindings, &outputs);
        assert_eq!(map["iac"].port_name, "IAC Driver Bus 1");
        assert!(!map["iac"].auto_paired);
    }

    #[test]
    fn bidirectional_asymmetric_uses_output_matchers() {
        let ep = endpoint(
            "split",
            ConnectorDirection::Bidirectional,
            asym_kind(
                vec![DeviceMatcher::name_contains("Input Only")],
                vec![DeviceMatcher::name_contains("Split Output")],
            ),
        );
        let outputs = vec!["Split Output Port".to_string()];
        let bindings = vec![("split".to_string(), "Input Only Port".to_string())];
        let map = build_output_map(&[ep], &bindings, &outputs);
        assert_eq!(
            map.get("split").map(|r| r.port_name.as_str()),
            Some("Split Output Port"),
            "output side must resolve via output_matchers"
        );
        assert!(!map["split"].auto_paired);
    }

    #[test]
    fn explicit_output_wins_over_auto_pair() {
        // Bidirectional endpoint whose output_matchers point at an explicit
        // port — must NOT auto-pair to the like-named input mate.
        let ep = endpoint(
            "mikro",
            ConnectorDirection::Bidirectional,
            asym_kind(
                vec![DeviceMatcher::name_contains("Mikro")],
                vec![DeviceMatcher::name_contains("Explicit Out")],
            ),
        );
        let outputs = vec![
            "Mikro Output".to_string(),      // would auto-pair from "Mikro Input"
            "Explicit Out Port".to_string(), // explicit matcher hits this
        ];
        let bindings = vec![("mikro".to_string(), "Mikro Input".to_string())];
        let map = build_output_map(&[ep], &bindings, &outputs);
        assert_eq!(map["mikro"].port_name, "Explicit Out Port");
        assert!(!map["mikro"].auto_paired, "explicit wins over auto-pair");
    }

    #[test]
    fn midi_virtual_port_maps_directly() {
        let ep = endpoint(
            "daw_proxy",
            ConnectorDirection::Output,
            EndpointKind::MidiVirtualPort {
                port_name: "Conductor DAW Input".to_string(),
            },
        );
        // Empty available_outputs — virtual mapping doesn't depend on it.
        let map = build_output_map(&[ep], &[], &[]);
        assert_eq!(map["daw_proxy"].port_name, "Conductor DAW Input");
        assert!(!map["daw_proxy"].auto_paired);
    }

    #[test]
    fn desired_virtual_port_names_collects_enabled_virtual_ports() {
        // #2063: only enabled MidiVirtualPort endpoints contribute a desired OS
        // port name; disabled ones are excluded (so a reload tears them down),
        // non-virtual kinds never do, and duplicates collapse.
        let mut disabled = endpoint(
            "off_proxy",
            ConnectorDirection::Output,
            EndpointKind::MidiVirtualPort {
                port_name: "Should Not Appear".to_string(),
            },
        );
        disabled.enabled = false;
        let endpoints = vec![
            endpoint(
                "daw_proxy",
                ConnectorDirection::Output,
                EndpointKind::MidiVirtualPort {
                    port_name: "Conductor DAW Input".to_string(),
                },
            ),
            // An input-direction virtual port still needs the OS port created
            // (external apps send into it), so direction is not a filter.
            endpoint(
                "synth_in",
                ConnectorDirection::Input,
                EndpointKind::MidiVirtualPort {
                    port_name: "Conductor Synth In".to_string(),
                },
            ),
            // Duplicate name (same OS port) must collapse to one entry.
            endpoint(
                "daw_proxy_dup",
                ConnectorDirection::Output,
                EndpointKind::MidiVirtualPort {
                    port_name: "Conductor DAW Input".to_string(),
                },
            ),
            disabled,
            endpoint(
                "mikro",
                ConnectorDirection::Output,
                matcher_kind(vec![DeviceMatcher::name_contains("Mikro")]),
            ),
        ];
        let names = desired_virtual_port_names(&endpoints);
        assert_eq!(names, vec!["Conductor DAW Input", "Conductor Synth In"]);
    }

    #[test]
    fn reachable_output_ports_includes_daemon_created_virtual_ports() {
        // #2421: a daemon-materialized virtual output port is reachable for
        // dispatch even though it is ABSENT from the live midir enumeration —
        // `MidiOutput::ports()` lists ports created by OTHER processes, not the
        // ones this daemon created. Gating `connected` purely on the live
        // enumeration (#2203) therefore reported such ports red in the Endpoints
        // + Routing Graph views while the Discovered Ports view (a separate
        // enumeration that DOES see them) showed them green. The reachable set
        // must fold the daemon's own enabled MidiVirtualPort endpoints back in.
        let endpoints = vec![endpoint(
            "daw_proxy",
            ConnectorDirection::Output,
            EndpointKind::MidiVirtualPort {
                port_name: "Conductor: DAW".to_string(),
            },
        )];
        // The virtual port is NOT among the live OS output ports.
        let enumerated = vec!["IAC Driver Bus 1".to_string()];
        // virtual_ports_supported = true: this platform can create them.
        let reachable = reachable_output_ports(enumerated, &endpoints, true);

        assert!(
            reachable.contains("IAC Driver Bus 1"),
            "live-enumerated ports must remain reachable"
        );
        assert!(
            reachable.contains("Conductor: DAW"),
            "#2421: a daemon-created virtual output must be reachable even when \
             absent from the live enumeration"
        );
    }

    #[test]
    fn reachable_output_ports_skips_virtual_ports_when_unsupported() {
        // Copilot review on PR #2443: on platforms where the daemon cannot
        // create virtual MIDI ports (Windows — `MidiOutputManager` has no
        // `virtual_ports` and `sync_virtual_ports` is a no-op; or
        // CONDUCTOR_DISABLE_VIRTUAL_MIDI), a configured MidiVirtualPort is never
        // materialized, so folding it into the reachable set would falsely mark
        // it connected. When `virtual_ports_supported` is false the config union
        // must be skipped — only the live enumeration counts.
        let endpoints = vec![endpoint(
            "daw_proxy",
            ConnectorDirection::Output,
            EndpointKind::MidiVirtualPort {
                port_name: "Conductor: DAW".to_string(),
            },
        )];
        let enumerated = vec!["IAC Driver Bus 1".to_string()];
        let reachable = reachable_output_ports(enumerated, &endpoints, false);

        assert!(
            reachable.contains("IAC Driver Bus 1"),
            "live-enumerated ports must remain reachable regardless of platform"
        );
        assert!(
            !reachable.contains("Conductor: DAW"),
            "a virtual port must NOT be reachable when the platform cannot create it"
        );
    }

    #[test]
    fn reachable_output_ports_excludes_disabled_virtual_ports() {
        // Guards the #2203 boundary: a DISABLED MidiVirtualPort is never
        // materialized (excluded from `desired_virtual_port_names`), so it must
        // NOT be reported reachable — it stays disconnected, exactly as #2203
        // intends for a virtual port that was never created.
        let mut disabled = endpoint(
            "off_proxy",
            ConnectorDirection::Output,
            EndpointKind::MidiVirtualPort {
                port_name: "Conductor: Off".to_string(),
            },
        );
        disabled.enabled = false;

        let reachable = reachable_output_ports(Vec::<String>::new(), &[disabled], true);
        assert!(
            !reachable.contains("Conductor: Off"),
            "a disabled (never-created) virtual port must not be reachable"
        );
    }

    #[test]
    fn osc_endpoint_skipped() {
        let ep = endpoint(
            "lighting",
            ConnectorDirection::Output,
            EndpointKind::OscEndpoint {
                host: "127.0.0.1".to_string(),
                port: 9000,
                security: Default::default(),
            },
        );
        let map = build_output_map(&[ep], &[], &[]);
        assert!(
            map.is_empty(),
            "OSC dispatches via registry, not the MIDI map"
        );
    }

    #[test]
    fn artnet_endpoint_skipped() {
        let ep = endpoint(
            "dmx",
            ConnectorDirection::Output,
            EndpointKind::ArtNetEndpoint {
                universe: 0,
                host: "255.255.255.255".to_string(),
                port: 6454,
                allow_broadcast: false,
                security: Default::default(),
            },
        );
        let map = build_output_map(&[ep], &[], &[]);
        assert!(
            map.is_empty(),
            "Art-Net dispatches via registry, not the MIDI map"
        );
    }

    #[test]
    fn hid_endpoint_produces_no_phantom_output() {
        // ADR-035 Slice 9.5 cleanup: a HID endpoint must NOT auto-pair to a
        // like-named MIDI output port. The legacy device path lacked this
        // protocol gate; the unified path adds it.
        let mut ep = endpoint(
            "xbox",
            ConnectorDirection::Bidirectional,
            matcher_kind(vec![DeviceMatcher::name_contains("Xbox")]),
        );
        ep.protocol = Some(ConnectorProtocol::Hid);
        // Output port name that would match the matcher if the protocol gate
        // weren't applied — proves the gate, not the matcher, skips it.
        let outputs = vec!["Xbox One Controller".to_string()];
        let bindings = vec![("xbox".to_string(), "Xbox One Controller".to_string())];
        let map = build_output_map(&[ep], &bindings, &outputs);
        assert!(
            map.is_empty(),
            "HID endpoint must not land in the MIDI output map"
        );
    }

    #[test]
    fn disabled_endpoint_skipped() {
        let mut ep = endpoint(
            "mikro",
            ConnectorDirection::Bidirectional,
            asym_kind(vec![], vec![DeviceMatcher::name_contains("Mikro")]),
        );
        ep.enabled = false;
        let outputs = vec!["Mikro Output".to_string()];
        let bindings = vec![("mikro".to_string(), "Mikro Input".to_string())];
        let map = build_output_map(&[ep], &bindings, &outputs);
        assert!(map.is_empty(), "disabled endpoint is excluded");
    }

    #[test]
    fn unbound_input_only_endpoint_skipped() {
        // Input-only endpoint not currently bound and not output-capable:
        // nothing to resolve, no noise.
        let ep = endpoint(
            "unplugged",
            ConnectorDirection::Input,
            matcher_kind(vec![DeviceMatcher::name_contains("Ghost")]),
        );
        let outputs = vec!["Some Output".to_string()];
        let map = build_output_map(&[ep], &[], &outputs);
        assert!(map.is_empty());
    }

    #[test]
    fn output_matcher_picks_highest_specificity() {
        // exact_name (40) must beat name_contains (20) when both match.
        let ep = endpoint(
            "pads",
            ConnectorDirection::Output,
            asym_kind(
                vec![],
                vec![
                    DeviceMatcher::name_contains("Mikro"),
                    DeviceMatcher::exact_name("Maschine Mikro MK3 Out"),
                ],
            ),
        );
        let outputs = vec![
            "Mikro Companion Output".to_string(), // name_contains only (sp 20)
            "Maschine Mikro MK3 Out".to_string(), // exact + contains (sp 40)
        ];
        let map = build_output_map(&[ep], &[], &outputs);
        assert_eq!(
            map.get("pads").map(|r| r.port_name.as_str()),
            Some("Maschine Mikro MK3 Out"),
            "highest-specificity matcher must win"
        );
    }

    #[test]
    fn ambiguous_auto_pair_returns_no_entry() {
        let ep = endpoint(
            "mikro",
            ConnectorDirection::Input,
            matcher_kind(vec![DeviceMatcher::name_contains("Mikro")]),
        );
        let outputs = vec![
            "Mikro MK3 Pro Output".to_string(),
            "Mikro MK2 Pro Output".to_string(),
        ];
        let bindings = vec![("mikro".to_string(), "Mikro Input".to_string())];
        let map = build_output_map(&[ep], &bindings, &outputs);
        assert!(map.is_empty(), "ambiguous auto-pair resolves nothing");
    }

    #[test]
    fn multi_endpoint_map_mixes_explicit_and_auto_pair() {
        let explicit = endpoint(
            "mikro",
            ConnectorDirection::Bidirectional,
            asym_kind(
                vec![DeviceMatcher::name_contains("Mikro")],
                vec![DeviceMatcher::name_contains("Maschine Mikro MK3 Output")],
            ),
        );
        let auto = endpoint(
            "nano",
            ConnectorDirection::Input,
            matcher_kind(vec![DeviceMatcher::name_contains("nano")]),
        );
        let outputs = vec![
            "Maschine Mikro MK3 Output".to_string(),
            "nanoKONTROL Output".to_string(),
        ];
        let bindings = vec![
            ("mikro".to_string(), "Maschine Mikro MK3 Input".to_string()),
            ("nano".to_string(), "nanoKONTROL Input".to_string()),
        ];
        let map = build_output_map(&[explicit, auto], &bindings, &outputs);
        assert_eq!(map.len(), 2);
        assert!(!map["mikro"].auto_paired, "explicit");
        assert!(map["nano"].auto_paired, "auto-paired");
    }

    #[test]
    fn no_match_means_no_entry() {
        let ep = endpoint(
            "ghost",
            ConnectorDirection::Output,
            asym_kind(vec![], vec![DeviceMatcher::name_contains("Nonexistent")]),
        );
        let outputs = vec!["IAC Driver Bus 1".to_string()];
        let map = build_output_map(&[ep], &[], &outputs);
        assert!(map.is_empty());
    }

    // --- enumerate_output_ports test ---

    #[test]
    fn test_enumerate_output_ports_no_panic() {
        // Should not panic even with no MIDI hardware
        let ports = enumerate_output_ports();
        let _ = ports.len();
    }
}
