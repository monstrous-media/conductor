// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Structure, trigger-duplicate, endpoint, network-listener, and per-app-mode validators.

use super::*;

pub(super) fn validate_structure(config: &Config, ctx: &mut ValidationCtx) {
    // Duplicate mode names
    let mut mode_names = HashSet::new();
    for mode in &config.modes {
        if !mode_names.insert(&mode.name) {
            ctx.error("modes", format!("Duplicate mode name: '{}'", mode.name));
        }
        // Empty mode name (from former validator.rs)
        if mode.name.is_empty() {
            ctx.error("modes", "Mode name cannot be empty");
        }
    }

    // ── ADR-031 § 4.3 — `[[routes]]` reject rules (resolve against
    //    the unified [[endpoints]] set per ADR-035) ──
    validate_routes(config, ctx);

    // ── Warn on MIDI feedback-loop topologies: a route /
    //    SendMidi / MidiForward output target that is also a listened input. ──
    ctx.warnings
        .extend(crate::config::feedback_loops::detect_feedback_loops(config));
}

/// ADR-037 D2: within a single scope (one mode, or the global mappings),
/// two structurally identical triggers put the second rule permanently in
/// the shadow of the first (first-match-wins on identical match sets), so
/// it can never fire.
///
/// "Identical" here means the full trigger (variant + all values + device)
/// is the same — NOT merely the same constraint *dimensions*. Two `Note`
/// triggers on notes 36 and 37 share dimensions but are legitimately
/// distinct, so they are not flagged.
pub(super) fn validate_trigger_duplicates(config: &Config, ctx: &mut ValidationCtx) {
    fn check_scope(
        mappings: &[crate::config::types::Mapping],
        scope_path: &str,
        ctx: &mut ValidationCtx,
    ) {
        // Compare triggers by their canonical JSON form (handles the enum +
        // nested values without needing PartialEq on Trigger).
        let mut seen: Vec<(serde_json::Value, usize)> = Vec::new();
        for (idx, mapping) in mappings.iter().enumerate() {
            let key = serde_json::to_value(&mapping.trigger).unwrap_or(serde_json::Value::Null);
            if let Some((_, first_idx)) = seen.iter().find(|(k, _)| *k == key) {
                ctx.error(
                    format!("{}.mappings[{}]", scope_path, idx),
                    format!(
                        "Trigger is structurally identical to mappings[{}] in the same scope — \
                         the second rule can never fire (first-match-wins on identical match \
                         sets). Remove the duplicate or differentiate its trigger (ADR-037 D2).",
                        first_idx
                    ),
                );
            } else {
                seen.push((key, idx));
            }
        }
    }

    for (mi, mode) in config.modes.iter().enumerate() {
        check_scope(&mode.mappings, &format!("modes[{}]", mi), ctx);
    }
    check_scope(&config.global_mappings, "global_mappings", ctx);
}

/// Validate the unified `[[endpoints]]` set (ADR-035 §4.1, §5).
/// Type↔field consistency is already enforced by the hand-written
/// `EndpointConfig` deserializer (§4.1); this adds the semantic checks:
/// the non-empty-matchers invariant (hard error) and direction legality
/// per kind (warning).
pub(super) fn validate_endpoints(config: &Config, ctx: &mut ValidationCtx) {
    use crate::config::types::{ConnectorDirection, ConnectorProtocol, EndpointKind};
    for (i, ep) in config.endpoints.iter().enumerate() {
        let path = format!("endpoints[{}] ('{}')", i, ep.alias);

        // ADR-042 Phase A — loopback-only network-listener gate +
        // ACL shape validation for OSC/Art-Net Input/Bidirectional endpoints.
        validate_network_listener(ep, &path, ctx);

        // HID is Input-only (ADR-039 D7 — HID output was dropped entirely).
        // A HID endpoint declaring Output/Bidirectional would silently never
        // produce output (the output resolver gates non-MIDI out of the MIDI
        // port map, ADR-035) — reject it at load instead.
        if ep.effective_protocol() == ConnectorProtocol::Hid
            && ep.direction != ConnectorDirection::Input
        {
            ctx.error(
                path.clone(),
                format!(
                    "endpoint '{}' is HID with direction = {:?} — HID is input-only \
                     (HID output was dropped, ADR-039 D7). Set direction = Input.",
                    ep.alias, ep.direction
                ),
            );
        }

        // Channel-scope validation: channels are 0-indexed MIDI
        // channels (0-15) and only meaningful for MIDI endpoints.
        for &ch in &ep.channels {
            if ch > 15 {
                ctx.error(
                    format!("endpoints[{}].channels", i),
                    format!(
                        "channel {} is out of range (must be 0-15) in endpoint '{}'",
                        ch, ep.alias
                    ),
                );
            }
        }
        if !ep.channels.is_empty() && ep.effective_protocol() != ConnectorProtocol::Midi {
            ctx.warning(
                format!("endpoints[{}].channels", i),
                format!(
                    "endpoint '{}' has channels configured but protocol is {:?} — \
                     channels only apply to MIDI endpoints",
                    ep.alias,
                    ep.effective_protocol()
                ),
            );
        }

        // Non-empty-matchers invariant (R3): an `EndpointKind::Matcher` must
        // carry at least one matcher across matchers / input_matchers /
        // output_matchers — else nothing can ever resolve the endpoint.
        if !ep.kind.has_any_matcher() {
            ctx.error(
                path.clone(),
                format!(
                    "endpoint '{}' is a Matcher with no matchers — set at least one of \
                     `matchers`, `input_matchers`, or `output_matchers`.",
                    ep.alias
                ),
            );
        }

        // Direction legality: a Conductor-created virtual MIDI port is an
        // output/bidirectional concept — `direction = Input` is not meaningful.
        if matches!(ep.kind, EndpointKind::MidiVirtualPort { .. })
            && ep.direction == ConnectorDirection::Input
        {
            ctx.warning(
                path.clone(),
                format!(
                    "endpoint '{}' is a MidiVirtualPort with direction = Input — a virtual \
                     port Conductor creates is output/bidirectional; Input is not meaningful.",
                    ep.alias
                ),
            );
        }

        // Direction↔matcher consistency (ADR-035 §4.1): `effective_matchers`
        // only consults `output_matchers` for Output and `input_matchers` for
        // Input, so an asymmetric matcher set that doesn't match the declared
        // direction is silently ignored at resolve time — surprising/broken.
        // Reject the contradiction at load instead. (A
        // Bidirectional endpoint legitimately uses both sides.)
        if let EndpointKind::Matcher {
            input_matchers,
            output_matchers,
            ..
        } = &ep.kind
        {
            if ep.direction == ConnectorDirection::Input && !output_matchers.is_empty() {
                ctx.error(
                    path.clone(),
                    format!(
                        "endpoint '{}' has direction = Input but defines `output_matchers` — \
                         output matchers are only used for Output/Bidirectional endpoints and \
                         would be silently ignored. Set direction = Bidirectional or remove \
                         `output_matchers`.",
                        ep.alias
                    ),
                );
            }
            if ep.direction == ConnectorDirection::Output && !input_matchers.is_empty() {
                ctx.error(
                    path,
                    format!(
                        "endpoint '{}' has direction = Output but defines `input_matchers` — \
                         input matchers are only used for Input/Bidirectional endpoints and \
                         would be silently ignored. Set direction = Bidirectional or remove \
                         `input_matchers`.",
                        ep.alias
                    ),
                );
            }
        }
    }
}

/// ADR-042 Phase A — validate the network-security policy on an
/// OSC/Art-Net *listener* endpoint (`direction = Input` or `Bidirectional`).
///
/// Output endpoints *send* to a remote host (a lighting rig at `10.0.0.5` is
/// normal) and are intentionally untouched. For a listener:
///
/// - **Loopback-only (R6):** any non-loopback `host` is a config-load error
///   pointing at Phase B-early. This supersedes the old "non-loopback without
///   `allow_network`" rule; `allow_network` does **not** lift the gate in
///   Phase A. The `allow_network`/`network_acl` schema is still shape-checked
///   so configs stay forward-compatible.
/// - **Shape:** `allow_network = true` requires a non-empty `network_acl`.
/// - **D11:** any populated `network_acl` is parsed through
///   [`NetworkAcl::parse`] — rejecting `0.0.0.0/0` / `::/0` and (for Art-Net
///   `allow_broadcast`) the **aggregate** amplification budget.
pub(super) fn validate_network_listener(
    ep: &crate::config::types::EndpointConfig,
    path: &str,
    ctx: &mut ValidationCtx,
) {
    use crate::config::types::{ConnectorDirection, EndpointKind, NetworkSecurityConfig};
    use crate::security::NetworkAcl;

    // Only listeners (Input / Bidirectional) are inbound-attack surface.
    if !matches!(
        ep.direction,
        ConnectorDirection::Input | ConnectorDirection::Bidirectional
    ) {
        return;
    }

    // Extract (host, security, allow_broadcast) for the network kinds only.
    let (host, security, allow_broadcast): (&str, &NetworkSecurityConfig, bool) = match &ep.kind {
        EndpointKind::OscEndpoint { host, security, .. } => (host.as_str(), security, false),
        EndpointKind::ArtNetEndpoint {
            host,
            security,
            allow_broadcast,
            ..
        } => (host.as_str(), security, *allow_broadcast),
        _ => return,
    };

    // ── Loopback gate + B-early A.2 lift ──────────────────────────────
    // "localhost" is accepted as an unambiguous loopback alias; any other
    // non-IP host can't be proven loopback-only at config-load time.
    //
    // R6 Phase A was loopback-only. ADR-042 Phase B-early **lifts** that gate:
    // a non-loopback host is permitted at config-load IFF the operator opts in
    // with `allow_network = true` (and a `network_acl`, enforced below) — in
    // which case the bind is gated at RUNTIME on an HMAC-verified approval
    // rather than rejected here. A non-loopback host WITHOUT `allow_network`
    // remains a config-load error. An opted-in non-loopback host must be a
    // concrete IP literal: network listeners bind an explicit address and the
    // bind gate keys approval on (host, port, acl) — DNS names are never
    // resolved.
    let parsed_host = host.parse::<std::net::IpAddr>().ok();
    let host_is_loopback = host == "localhost"
        || parsed_host
            .as_ref()
            .is_some_and(NetworkAcl::is_loopback_address);
    if !host_is_loopback {
        if !security.allow_network {
            ctx.error(
                format!("{path}.host"),
                format!(
                    "endpoint '{}' is a network listener bound to non-loopback host '{}'; \
                     enable network binding with `allow_network = true` + a `network_acl` \
                     (Phase B-early gates the bind on keychain-HMAC approval), or use \
                     127.0.0.1 / ::1 for a loopback-only listener.",
                    ep.alias, host
                ),
            );
        } else if parsed_host.is_none() {
            ctx.error(
                format!("{path}.host"),
                format!(
                    "endpoint '{}' has a non-loopback listener host '{}' that is not an IP \
                     literal; a network listener binds a concrete address and DNS names are \
                     not resolved. Use an explicit IPv4/IPv6 address.",
                    ep.alias, host
                ),
            );
        }
        // else: opted-in non-loopback IP literal → permitted at config-load;
        // the runtime bind gate requires an HMAC-verified approval to bind.
    }

    // ── ACL shape + D11 hardening (forward-compat) ────────────────────
    if security.allow_network && security.network_acl.is_empty() {
        ctx.error(
            format!("{path}.network_acl"),
            format!(
                "endpoint '{}' sets allow_network = true but network_acl is empty; \
                 an allow-list of source CIDRs is required.",
                ep.alias
            ),
        );
    }

    if !security.network_acl.is_empty() {
        match NetworkAcl::parse(
            &security.network_acl,
            allow_broadcast,
            security.i_understand_amplification_risk,
        ) {
            Ok((_, warnings)) => {
                for w in warnings {
                    let crate::security::AclWarning::Ipv6LinkLocal(entry) = w;
                    ctx.warning(
                        format!("{path}.network_acl"),
                        format!(
                            "endpoint '{}' network_acl entry '{}' is IPv6 link-local \
                             (reachable from the whole L2 segment).",
                            ep.alias, entry
                        ),
                    );
                }
            }
            Err(e) => {
                ctx.error(
                    format!("{path}.network_acl"),
                    format!("endpoint '{}' has an invalid network_acl: {}", ep.alias, e),
                );
            }
        }
    }
}

/// ADR-040 D3/D5 — validate `[per_app_modes]`:
///   - `default`, every `rules` value, and every `window_rules[].mode` must
///     name a declared `[[modes]]` block (a typo yields a rule that can never
///     activate — fail loudly at load, like the route mode-scope check).
///   - a `WindowRule` may set `title_pattern` *or* `title_regex`, not both.
///   - `title_regex` must compile.
pub(super) fn validate_per_app_modes(config: &Config, ctx: &mut ValidationCtx) {
    let Some(pam) = config.per_app_modes.as_ref() else {
        return;
    };
    let mode_names: HashSet<&str> = config.modes.iter().map(|m| m.name.as_str()).collect();

    // `default` references a real mode.
    if let Some(default) = pam.default.as_deref()
        && !mode_names.contains(default)
    {
        ctx.error(
            "per_app_modes.default",
            format!(
                "[per_app_modes].default references unknown mode '{default}' — must match a \
                 declared [[modes]] block (ADR-040 D3)."
            ),
        );
    }

    // App-name `rules`: every target mode is real.
    for (app, mode) in &pam.rules {
        if !mode_names.contains(mode.as_str()) {
            ctx.error(
                format!("per_app_modes.rules.\"{app}\""),
                format!(
                    "[per_app_modes] rule for app '{app}' references unknown mode '{mode}' — must \
                     match a declared [[modes]] block (ADR-040 D3)."
                ),
            );
        }
    }

    // Window rules: mode ref, mutual exclusivity, regex compile.
    for (idx, wr) in pam.window_rules.iter().enumerate() {
        let path = format!("per_app_modes.window_rules[{idx}]");

        if !mode_names.contains(wr.mode.as_str()) {
            ctx.error(
                format!("{path}.mode"),
                format!(
                    "[per_app_modes] window_rule for app '{}' references unknown mode '{}' — must \
                     match a declared [[modes]] block (ADR-040 D5).",
                    wr.app, wr.mode
                ),
            );
        }

        if wr.title_pattern.is_some() && wr.title_regex.is_some() {
            ctx.error(
                path.clone(),
                format!(
                    "[per_app_modes] window_rule for app '{}' sets both title_pattern and \
                     title_regex — these are mutually exclusive (ADR-040 §4.1). Use one.",
                    wr.app
                ),
            );
        }

        if let Some(re) = wr.title_regex.as_deref()
            && let Err(e) = regex::Regex::new(re)
        {
            ctx.error(
                format!("{path}.title_regex"),
                format!(
                    "[per_app_modes] window_rule for app '{}' has an invalid title_regex: {e}",
                    wr.app
                ),
            );
        }
    }
}
