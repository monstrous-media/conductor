// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! #2398 (epic #2395): config-load detection of MIDI feedback-loop topologies.
//!
//! A user can configure a route `to` / `SendMidi` / `MidiForward` whose output
//! target is also a port Conductor listens on as an input. The output then
//! echoes straight back into the input — a feedback loop. The runtime recursion
//! guard (ADR-015 D8) papers over it, but it's a misconfiguration that should
//! be surfaced at config load.
//!
//! This is a **pure** detector over `&Config`: no live port enumeration, no I/O.
//! It models the runtime's actual "what is listened as input" behaviour so it
//! has **zero false positives for legitimately distinct in/out ports**:
//!
//!   * Only `EndpointKind::Matcher` endpoints listen on real ports — the
//!     `PortResolver` binds inputs from `effective_matchers(Input)`, which is
//!     empty for `MidiVirtualPort`/OSC/Art-Net kinds. So a `MidiVirtualPort`
//!     endpoint is **not** treated as a listened input.
//!   * A target is excluded from the input scan exactly as the daemon excludes
//!     it (`input_manager::filter_ports` + `build_input_ignore`): a port name
//!     containing any `ignore_ports` pattern, **or** matching one of Conductor's
//!     own enabled `MidiVirtualPort` outputs (ADR-009 D21 auto-exclude), is not
//!     listened — routing to it is not a loop.
//!   * Under `listen_mode = "All"` a declared input endpoint's port is opened
//!     even when the endpoint is `enabled = false` (a disabled device is still
//!     listened; it just doesn't bind its alias). Under `"Configured"` only
//!     enabled endpoints bind, so disabled ones are excluded.
//!
//! The implicit `listen_mode = "All"` case where Conductor opens an *external*
//! port that has **no** declared endpoint cannot be detected without live
//! ports (warning on every external send would false-positive on output-only
//! gear), so it stays the recursion guard's / `ignore_ports`' job — see the
//! remediation hint in the warning.

use crate::config::types::{
    ActionConfig, Config, ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
    ListenMode,
};
use crate::config::validation::{Severity, ValidationFinding};
use crate::identity::DeviceMatcher;

/// Bounds recursion into nested actions (Sequence/Conditional/Repeat) from a
/// user-controlled config — matches the depth-guarding stance elsewhere in
/// validation.
const MAX_ACTION_DEPTH: usize = 32;

fn is_midi(ep: &EndpointConfig) -> bool {
    ep.effective_protocol() == ConnectorProtocol::Midi
}

/// A listened MIDI input: the endpoint alias plus the name-based matcher
/// identities it binds (`ExactName` exact, `NameContains` substring). USB /
/// SysEx / regex matchers need live metadata and are skipped (never a false
/// positive). Only `Matcher`-kind endpoints reach here.
struct ListenedInput {
    alias: String,
    exact: Vec<String>,
    contains: Vec<String>,
}

impl ListenedInput {
    /// Would this input listen on a port named `out_name`?
    fn matches(&self, out_name: &str) -> bool {
        self.exact.iter().any(|e| e == out_name)
            || self.contains.iter().any(|c| out_name.contains(c.as_str()))
    }
}

/// Input-side name identities of a `Matcher` endpoint.
fn input_identities(ep: &EndpointConfig) -> (Vec<String>, Vec<String>) {
    let mut exact = Vec::new();
    let mut contains = Vec::new();
    for m in ep.kind.effective_matchers(ConnectorDirection::Input) {
        match m {
            DeviceMatcher::ExactName { value } => exact.push(value.clone()),
            DeviceMatcher::NameContains { value } => contains.push(value.clone()),
            _ => {}
        }
    }
    (exact, contains)
}

/// Concrete output port names an `Output`/`Bidirectional` endpoint targets: a
/// `MidiVirtualPort` name, or `ExactName` output matchers. (A `NameContains`
/// output is not a concrete destination.)
fn output_port_names(ep: &EndpointConfig) -> Vec<String> {
    match &ep.kind {
        EndpointKind::MidiVirtualPort { port_name } => vec![port_name.clone()],
        EndpointKind::Matcher { .. } => ep
            .kind
            .effective_matchers(ConnectorDirection::Output)
            .iter()
            .filter_map(|m| match m {
                DeviceMatcher::ExactName { value } => Some(value.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Collect every `SendMidi.port` / `MidiForward.target` reachable from `action`
/// (recursing the common nesting wrappers) as `(path, target_string)`.
fn collect_midi_targets(
    action: &ActionConfig,
    path: &str,
    depth: usize,
    out: &mut Vec<(String, String)>,
) {
    if depth > MAX_ACTION_DEPTH {
        return;
    }
    match action {
        ActionConfig::SendMidi { port, .. } => out.push((path.to_string(), port.clone())),
        ActionConfig::MidiForward { target, .. } => out.push((path.to_string(), target.clone())),
        ActionConfig::Sequence { actions } => {
            for (i, a) in actions.iter().enumerate() {
                collect_midi_targets(a, &format!("{path}.sequence[{i}]"), depth + 1, out);
            }
        }
        ActionConfig::Conditional {
            then_action,
            else_action,
            ..
        } => {
            collect_midi_targets(then_action, &format!("{path}.then_action"), depth + 1, out);
            if let Some(e) = else_action {
                collect_midi_targets(e, &format!("{path}.else_action"), depth + 1, out);
            }
        }
        ActionConfig::Repeat { action, .. } => {
            collect_midi_targets(action, &format!("{path}.repeat"), depth + 1, out);
        }
        _ => {}
    }
}

fn warning(path: String, out_name: &str, input_alias: &str) -> ValidationFinding {
    ValidationFinding {
        severity: Severity::Warning,
        path,
        message: format!(
            "MIDI output target '{out_name}' is also listened as an input (endpoint \
             '{input_alias}') — output sent there echoes straight back into Conductor as input, \
             creating a feedback loop. Add '{out_name}' to advanced_settings.ignore_ports, set \
             advanced_settings.listen_mode = \"Configured\", or disable the '{input_alias}' input."
        ),
    }
}

/// Detect config-level MIDI feedback-loop topologies. Returns `Warning` findings
/// (never errors — a loop is a footgun the recursion guard survives, not a
/// load-blocking misconfiguration).
pub(crate) fn detect_feedback_loops(config: &Config) -> Vec<ValidationFinding> {
    let listen_all = config.advanced_settings.listen_mode == ListenMode::All;

    // Listened MIDI inputs: only `Matcher` endpoints actually bind input ports
    // (PortResolver reads `effective_matchers(Input)`; non-Matcher kinds yield
    // none). Under `Configured` only enabled endpoints bind; under `All` a
    // declared port is opened even when disabled (#2406 review finding 1).
    let listened: Vec<ListenedInput> = config
        .endpoints
        .iter()
        .filter(|ep| is_midi(ep) && matches!(ep.kind, EndpointKind::Matcher { .. }))
        .filter(|ep| {
            matches!(
                ep.direction,
                ConnectorDirection::Input | ConnectorDirection::Bidirectional
            )
        })
        .filter(|ep| listen_all || ep.enabled)
        .map(|ep| {
            let (exact, contains) = input_identities(ep);
            ListenedInput {
                alias: ep.alias.clone(),
                exact,
                contains,
            }
        })
        .collect();

    if listened.is_empty() {
        return Vec::new();
    }

    // Ports excluded from the input scan — mirrors daemon
    // `input_manager::{build_input_ignore, filter_ports}`: a name CONTAINING
    // any `ignore_ports` pattern, or one of Conductor's own enabled
    // `MidiVirtualPort` outputs (ADR-009 D21), is never listened. Matching is
    // substring (`port.contains(pattern)`), same as the runtime predicate.
    let mut ignore_patterns: Vec<String> = config.advanced_settings.ignore_ports.clone();
    for ep in &config.endpoints {
        if ep.enabled
            && let EndpointKind::MidiVirtualPort { port_name } = &ep.kind
        {
            ignore_patterns.push(port_name.clone());
        }
    }
    let excluded = |out_name: &str| -> bool {
        ignore_patterns
            .iter()
            .any(|pat| out_name.contains(pat.as_str()))
    };

    // alias → output port names, for resolving route `to` / action targets that
    // reference an endpoint by alias. Only ENABLED output endpoints can actually
    // send, so a route into a disabled output never loops (#2406 Council review).
    // We also track ALL output aliases (incl. disabled) so a disabled alias
    // resolves to "no output" rather than being misread as a literal port name.
    let output_endpoints = || {
        config.endpoints.iter().filter(|ep| {
            is_midi(ep)
                && matches!(
                    ep.direction,
                    ConnectorDirection::Output | ConnectorDirection::Bidirectional
                )
        })
    };
    let output_by_alias: std::collections::HashMap<&str, Vec<String>> = output_endpoints()
        .filter(|ep| ep.enabled)
        .map(|ep| (ep.alias.as_str(), output_port_names(ep)))
        .collect();
    let all_output_aliases: std::collections::HashSet<&str> =
        output_endpoints().map(|ep| ep.alias.as_str()).collect();

    // A target may be an enabled output alias, a disabled output alias (→ no
    // Action targets (`SendMidi.port` / `MidiForward.target`) use a UNIFIED
    // namespace — an enabled output alias, a disabled output alias (→ no send),
    // or a raw literal port name.
    let resolve_action_target = |target: &str| -> Vec<String> {
        if let Some(names) = output_by_alias.get(target) {
            names.clone()
        } else if all_output_aliases.contains(target) {
            Vec::new() // declared output alias but disabled → cannot send
        } else {
            vec![target.to_string()] // literal port name
        }
    };

    let mut findings = Vec::new();
    let check = |path: &str, out_name: &str, findings: &mut Vec<ValidationFinding>| {
        if excluded(out_name) {
            return; // not listened (ignore_ports / own virtual output)
        }
        if let Some(li) = listened.iter().find(|li| li.matches(out_name)) {
            findings.push(warning(path.to_string(), out_name, &li.alias));
        }
    };

    // Routes: `route.to` is STRICTLY an endpoint alias (ADR-035), never a
    // literal port name — so resolve it ONLY against enabled output endpoints.
    // An unknown alias is a config error caught by `validate_routes`; a
    // disabled or input-only alias simply can't send, so neither loops here
    // (no literal fallback → no false positives, #2406 Council review).
    for (idx, route) in config.routes.iter().enumerate() {
        if route.enabled
            && let Some(names) = output_by_alias.get(route.to.as_str())
        {
            for out_name in names {
                check(&format!("routes[{idx}].to"), out_name, &mut findings);
            }
        }
    }

    // Mapping actions (per-mode + global): SendMidi / MidiForward targets.
    let mut targets: Vec<(String, String)> = Vec::new();
    for (m_idx, mode) in config.modes.iter().enumerate() {
        for (map_idx, mapping) in mode.mappings.iter().enumerate() {
            collect_midi_targets(
                &mapping.action,
                &format!("modes[{m_idx}].mappings[{map_idx}].action"),
                0,
                &mut targets,
            );
        }
    }
    for (g_idx, mapping) in config.global_mappings.iter().enumerate() {
        collect_midi_targets(
            &mapping.action,
            &format!("global_mappings[{g_idx}].action"),
            0,
            &mut targets,
        );
    }
    for (path, target) in targets {
        for out_name in resolve_action_target(&target) {
            check(&path, &out_name, &mut findings);
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{
        AdvancedSettings, Config, ConnectorDirection, EndpointConfig, EndpointKind, ListenMode,
        Mapping, RouteConfig, Trigger,
    };

    fn vport(alias: &str, dir: ConnectorDirection, port_name: &str) -> EndpointConfig {
        EndpointConfig {
            alias: alias.to_string(),
            direction: dir,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::MidiVirtualPort {
                port_name: port_name.to_string(),
            },
        }
    }

    fn matcher(alias: &str, dir: ConnectorDirection, m: Vec<DeviceMatcher>) -> EndpointConfig {
        matcher_enabled(alias, dir, m, true)
    }

    fn matcher_enabled(
        alias: &str,
        dir: ConnectorDirection,
        m: Vec<DeviceMatcher>,
        enabled: bool,
    ) -> EndpointConfig {
        EndpointConfig {
            alias: alias.to_string(),
            direction: dir,
            protocol: None,
            description: None,
            enabled,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers: m,
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }
    }

    fn exact(v: &str) -> DeviceMatcher {
        DeviceMatcher::ExactName {
            value: v.to_string(),
        }
    }
    fn contains(v: &str) -> DeviceMatcher {
        DeviceMatcher::NameContains {
            value: v.to_string(),
        }
    }

    fn cfg(endpoints: Vec<EndpointConfig>, routes: Vec<RouteConfig>) -> Config {
        Config {
            endpoints,
            routes,
            ..Config::default_config()
        }
    }

    fn route(from: &str, to: &str) -> RouteConfig {
        RouteConfig {
            from: from.to_string(),
            to: to.to_string(),
            transform: None,
            filter: None,
            enabled: true,
            description: None,
            modes: vec![],
        }
    }

    fn sendmidi(port: &str) -> ActionConfig {
        ActionConfig::SendMidi {
            port: port.to_string(),
            message_type: "NoteOn".into(),
            channel: 0,
            note: Some(60),
            velocity: Some(100),
            controller: None,
            value: None,
            program: None,
            pitch: None,
            pressure: None,
        }
    }

    fn with_global(mut c: Config, action: ActionConfig) -> Config {
        c.global_mappings = vec![Mapping {
            trigger: Trigger::Note {
                note: 36,
                velocity_min: None,
                channel: None,
                device: None,
            },
            description: None,
            let_through: false,
            action,
        }];
        c
    }

    #[test]
    fn external_output_matcher_matched_by_input_is_a_loop() {
        // The real footgun: a route to an external port (Matcher Output ExactName)
        // that an Input endpoint also listens on.
        let c = cfg(
            vec![
                matcher("iac_in", ConnectorDirection::Input, vec![contains("IAC")]),
                matcher(
                    "iac_out",
                    ConnectorDirection::Output,
                    vec![exact("IAC Driver Bus 1")],
                ),
            ],
            vec![route("iac_in", "iac_out")],
        );
        let f = detect_feedback_loops(&c);
        assert_eq!(f.len(), 1, "got {f:?}");
        assert!(f[0].message.contains("IAC Driver Bus 1") && f[0].message.contains("iac_in"));
    }

    #[test]
    fn sendmidi_literal_port_matched_by_input_warns_including_nested() {
        // Literal external port name, Input ExactName on it. Nested in a Sequence.
        let c = with_global(
            cfg(
                vec![matcher(
                    "listen",
                    ConnectorDirection::Input,
                    vec![exact("Bus 1")],
                )],
                vec![],
            ),
            ActionConfig::Sequence {
                actions: vec![sendmidi("Bus 1")],
            },
        );
        let f = detect_feedback_loops(&c);
        assert_eq!(
            f.len(),
            1,
            "nested SendMidi to a listened port must warn, got {f:?}"
        );
        assert!(f[0].path.contains("sequence"));
    }

    #[test]
    fn routing_to_own_virtual_output_is_not_a_loop() {
        // #2406 finding 3: Conductor's own virtual output ports are auto-excluded
        // from the input scan (ADR-009 D21), so routing to one is NOT a loop —
        // even if an input matcher would nominally match the name.
        let c = cfg(
            vec![
                matcher(
                    "any_in",
                    ConnectorDirection::Input,
                    vec![contains("Conductor")],
                ),
                vport("vout", ConnectorDirection::Output, "Conductor: Synth"),
            ],
            vec![route("any_in", "vout")],
        );
        assert!(
            detect_feedback_loops(&c).is_empty(),
            "own virtual output is excluded from input scan — not a loop"
        );
    }

    #[test]
    fn ignore_ports_suppresses_the_warning() {
        // #2406 finding 2: if the target is already in ignore_ports it isn't
        // listened, so there is no loop and no warning (it's the remediation).
        let mut c = with_global(
            cfg(
                vec![matcher(
                    "listen",
                    ConnectorDirection::Input,
                    vec![exact("IAC Bus 1")],
                )],
                vec![],
            ),
            sendmidi("IAC Bus 1"),
        );
        c.advanced_settings = AdvancedSettings {
            ignore_ports: vec!["IAC".into()],
            ..c.advanced_settings
        };
        assert!(
            detect_feedback_loops(&c).is_empty(),
            "a target already excluded via ignore_ports must not warn"
        );
    }

    #[test]
    fn empty_ignore_pattern_matches_runtime_and_suppresses_warning() {
        // The runtime filter uses `port_name.contains(pattern)` directly, so an
        // empty ignore_ports entry excludes every input port. The detector must
        // mirror that exact predicate to avoid false-positive loop warnings.
        let mut c = with_global(
            cfg(
                vec![matcher(
                    "listen",
                    ConnectorDirection::Input,
                    vec![exact("Bus 1")],
                )],
                vec![],
            ),
            sendmidi("Bus 1"),
        );
        c.advanced_settings = AdvancedSettings {
            ignore_ports: vec!["".into()],
            ..c.advanced_settings
        };
        assert!(
            detect_feedback_loops(&c).is_empty(),
            "an empty ignore_ports pattern excludes all inputs at runtime, so the detector must not warn"
        );
    }

    #[test]
    fn disabled_input_still_loops_under_listen_all_but_not_configured() {
        // #2406 finding 1: under listen_mode=All a disabled input endpoint's port
        // is still opened/listened, so the loop is real; under Configured it is not.
        let mk = |mode: ListenMode| {
            let mut c = with_global(
                cfg(
                    vec![matcher_enabled(
                        "listen",
                        ConnectorDirection::Input,
                        vec![exact("Bus 1")],
                        false,
                    )],
                    vec![],
                ),
                sendmidi("Bus 1"),
            );
            c.advanced_settings = AdvancedSettings {
                listen_mode: mode,
                ..c.advanced_settings
            };
            c
        };
        assert_eq!(
            detect_feedback_loops(&mk(ListenMode::All)).len(),
            1,
            "disabled input under listen_mode=All still listens → loop"
        );
        assert!(
            detect_feedback_loops(&mk(ListenMode::Configured)).is_empty(),
            "disabled input under Configured does not bind → no loop"
        );
    }

    #[test]
    fn route_to_input_only_alias_is_not_treated_as_literal_port() {
        // #2406 Council review: route.to is strictly an endpoint alias (never a
        // literal port name). An input-only alias can't send, so routing to it
        // must not self-match and false-warn (the old literal fallback bug).
        let c = cfg(
            vec![
                matcher("src", ConnectorDirection::Input, vec![contains("Src")]),
                matcher("Mikro", ConnectorDirection::Input, vec![contains("Mikro")]),
            ],
            vec![route("src", "Mikro")],
        );
        assert!(
            detect_feedback_loops(&c).is_empty(),
            "route.to pointing at an input-only alias must not be read as a literal port"
        );
    }

    #[test]
    fn route_to_disabled_output_endpoint_does_not_warn() {
        // #2406 Council review: a disabled output endpoint can't actually send,
        // so a route into it is not a loop even if its port matches an input.
        let c = cfg(
            vec![
                matcher("in", ConnectorDirection::Input, vec![exact("IAC Bus 1")]),
                matcher_enabled(
                    "out",
                    ConnectorDirection::Output,
                    vec![exact("IAC Bus 1")],
                    false,
                ),
            ],
            vec![route("in", "out")],
        );
        assert!(
            detect_feedback_loops(&c).is_empty(),
            "a disabled output endpoint cannot send — not a loop"
        );
    }

    #[test]
    fn distinct_in_and_out_ports_do_not_warn() {
        let c = cfg(
            vec![
                matcher("in", ConnectorDirection::Input, vec![contains("Mikro")]),
                matcher("out", ConnectorDirection::Output, vec![exact("Synth A")]),
            ],
            vec![route("in", "out")],
        );
        assert!(detect_feedback_loops(&c).is_empty());
    }

    #[test]
    fn no_listened_inputs_means_no_warnings() {
        let c = cfg(
            vec![matcher(
                "out",
                ConnectorDirection::Output,
                vec![exact("Bus 1")],
            )],
            vec![],
        );
        assert!(detect_feedback_loops(&c).is_empty());
    }
}
