// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Action validators (all ActionConfig variants).

use super::*;

pub(super) fn validate_action(
    action: &ActionConfig,
    path: &str,
    ctx: &mut ValidationCtx,
    depth: usize,
) {
    if depth > MAX_ACTION_DEPTH {
        ctx.error(
            path,
            format!(
                "Action nesting exceeds maximum depth of {}",
                MAX_ACTION_DEPTH
            ),
        );
        return;
    }
    match action {
        ActionConfig::Keystroke { keys, modifiers } => {
            if keys.is_empty() {
                ctx.error(path, "Keystroke requires keys");
            }
            let valid_modifiers = ["cmd", "shift", "alt", "ctrl", "fn"];
            for modifier in modifiers {
                if !valid_modifiers.contains(&modifier.as_str()) {
                    ctx.error(
                        path,
                        format!(
                            "Unknown modifier: '{}'. Valid modifiers: {}",
                            modifier,
                            valid_modifiers.join(", ")
                        ),
                    );
                }
            }
        }
        ActionConfig::Text { text } => {
            if text.is_empty() {
                ctx.error(path, "Text action requires text");
            }
        }
        ActionConfig::Launch { app } => {
            if app.is_empty() {
                ctx.error(path, "Launch action requires app name");
            } else {
                validate_app_name(app, path, ctx);
            }
        }
        ActionConfig::Shell {
            command,
            args,
            timeout_ms,
            // ADR-027 §D10b sandbox override — applied by the daemon at
            // spawn time (the daemon `~`-expands and absolute-filters the
            // paths; it does NOT canonicalise them); no core-side validation
            // needed.
            sandbox: _,
        } => {
            // ADR-027 D7 — clamp timeout to [1000, 300000] so
            // sub-second timeouts can't kill kid-script shells before
            // their first sigchld and multi-minute timeouts can't
            // defeat the watchdog. `None` is the default-fallthrough
            // signal and stays untouched here.
            if let Some(ms) = timeout_ms
                && !(1_000..=300_000).contains(ms)
            {
                ctx.error(
                    path,
                    format!("Shell timeout_ms must be in [1000, 300000], got {}", ms).as_str(),
                );
            }
            // Reject inputs that the runtime would silently no-op on.
            // Three classes:
            //   - Empty: `command = ""`
            //   - Whitespace-only: `command = "   "` — `execute_shell`
            //     trims and aborts.
            //   - Quote-only legacy commands (args = None): `"'"`,
            //     `"''"`, `'""'`, etc. — `parse_command_line` toggles
            //     quote-state but emits zero tokens, executor logs a
            //     "failed to parse" warning and aborts. These would
            //     otherwise pass validation only to disappoint at
            //     runtime; reject at load with the standard "requires
            //     command" diagnostic instead. The quote-only check
            //     applies only to the legacy single-string form —
            //     argv-form `command` is a binary path that's already
            //     metacharacter-blocklisted by `validate_shell_command`,
            //     and a single `'` or `"` in an argv-form command would
            //     fail to open as a binary at spawn time with an
            //     informative OS-level error rather than a silent
            //     no-op.
            let runnable = !command.trim().is_empty()
                && (args.is_some() || command_has_runnable_token(command));
            if !runnable {
                ctx.error(path, "Shell action requires command");
            } else {
                validate_shell_command(command, path, ctx);
            }
            // ADR-027 D3 §3.1 — argv-form `args` get the same
            // metacharacter blocklist applied. Without this, configs
            // like `command = "/bin/sh", args = ["-c", "env > /tmp/x"]`
            // would smuggle the dangerous-pattern set past
            // `validate_shell_command` simply by moving the redirect /
            // pipe / `&&` chain into argv.
            //
            // **Deliberately broad.** Every blocklist entry (`>`, `|`,
            // `&&`, trailing `&`, `$(`, `${`, `\``, etc.) is dangerous
            // only IF the program receiving the arg is a shell
            // interpreter — for non-interpreter binaries those bytes
            // are inert data. We block all of them at Phase 1 anyway
            // because the validator can't tell the program's class
            // from the schema alone (a path like `./my-script` could
            // be a shell wrapper or a compiled binary). Phase 2's
            // `allow_interpreters` policy resolves the program's
            // effective binary and lifts the blocklist for known-safe
            // (non-interpreter) cases; until that lands, the false-
            // positive cost of rejecting some legitimate-but-shell-
            // looking args is a worthwhile trade for closing the
            // smuggling vector.
            if let Some(args) = args {
                for (i, arg) in args.iter().enumerate() {
                    let arg_path = format!("{}.args[{}]", path, i);
                    validate_shell_arg(arg, &arg_path, ctx);
                }
            }
            // ADR-027 D3 §3.2: apply the
            // `allow_interpreters` policy. Wrapper resolution +
            // interpreter classification already happens in
            // `capabilities_for_action`; here we re-run it to drive
            // the user-facing diagnostic, since validation is the
            // earliest layer the user sees feedback from.
            validate_interpreter_policy(command, args.as_deref(), path, ctx);
        }
        ActionConfig::Sequence { actions } => {
            if actions.is_empty() {
                ctx.error(path, "Sequence requires at least one action");
            }
            for (i, sub_action) in actions.iter().enumerate() {
                validate_action(sub_action, &format!("{}[{}]", path, i), ctx, depth + 1);
            }
        }
        ActionConfig::Delay { ms } => {
            if *ms == 0 {
                ctx.error(path, "Delay must be > 0 ms");
            }
        }
        ActionConfig::MouseClick { button, .. } => {
            let valid_buttons = ["left", "right", "middle"];
            if !valid_buttons.contains(&button.as_str()) {
                ctx.error(
                    path,
                    format!(
                        "Invalid mouse button: '{}'. Valid buttons: {}",
                        button,
                        valid_buttons.join(", ")
                    ),
                );
            }
        }
        ActionConfig::VolumeControl { operation, value } => {
            let valid_ops = ["Up", "Down", "Mute", "Unmute", "Set"];
            if !valid_ops.contains(&operation.as_str()) {
                ctx.error(
                    path,
                    format!(
                        "Invalid volume operation: '{}'. Valid operations: {}",
                        operation,
                        valid_ops.join(", ")
                    ),
                );
            }
            if operation == "Set" && value.is_none() {
                ctx.error(path, "VolumeControl Set operation requires value");
            }
        }
        ActionConfig::ModeChange { mode } => {
            if mode.is_empty() {
                ctx.error(path, "ModeChange requires mode name");
            }
            // Cross-field mode existence check is done in validate_cross_references
        }
        ActionConfig::Repeat {
            action,
            count,
            delay_ms: _,
        } => {
            if *count == 0 {
                ctx.error(path, "Repeat count must be > 0");
            }
            validate_action(action, &format!("{}.action", path), ctx, depth + 1);
        }
        ActionConfig::Conditional {
            condition,
            then_action,
            else_action,
        } => {
            // ADR-025 Phase 2: validate the condition tree so silently-
            // impossible conditions (e.g. CcValueInRange with min > max,
            // MIDI channel / note / cc out of range) surface at load
            // time rather than quietly always evaluating false at runtime.
            validate_condition(condition, &format!("{}.condition", path), ctx);
            validate_action(then_action, &format!("{}.then", path), ctx, depth + 1);
            if let Some(else_act) = else_action {
                validate_action(else_act, &format!("{}.else", path), ctx, depth + 1);
            }
        }
        ActionConfig::SendMidi {
            port,
            message_type,
            channel,
            note,
            velocity,
            controller,
            value,
            program,
            pitch,
            pressure,
        } => {
            ctx.midi_features.push("SendMIDI".to_string());

            if port.is_empty() {
                ctx.error(path, "SendMidi requires port name");
            }

            let valid_types = [
                "NoteOn",
                "NoteOff",
                "CC",
                "ControlChange",
                "ProgramChange",
                "PitchBend",
                "Aftertouch",
            ];
            if !valid_types.iter().any(|t| {
                message_type.eq_ignore_ascii_case(t)
                    || message_type.replace('_', "").eq_ignore_ascii_case(t)
                    || message_type.replace('-', "").eq_ignore_ascii_case(t)
            }) {
                ctx.error(
                    path,
                    format!(
                        "Invalid MIDI message type: '{}'. Valid types: {}",
                        message_type,
                        valid_types.join(", ")
                    ),
                );
            }

            if *channel > 15 {
                ctx.error(path, format!("MIDI channel must be 0-15, got {}", channel));
            }

            let msg_type_lower = message_type.to_lowercase();
            if msg_type_lower.contains("note") {
                if let Some(n) = note
                    && *n > 127
                {
                    ctx.error(path, format!("MIDI note must be 0-127, got {}", n));
                }
                if let Some(v) = velocity
                    && *v > 127
                {
                    ctx.error(path, format!("MIDI velocity must be 0-127, got {}", v));
                }
            } else if msg_type_lower.contains("cc") || msg_type_lower.contains("control") {
                if let Some(c) = controller
                    && *c > 127
                {
                    ctx.error(path, format!("MIDI controller must be 0-127, got {}", c));
                }
                if let Some(v) = value
                    && *v > 127
                {
                    ctx.error(path, format!("MIDI value must be 0-127, got {}", v));
                }
            } else if msg_type_lower.contains("program") {
                if let Some(p) = program
                    && *p > 127
                {
                    ctx.error(path, format!("MIDI program must be 0-127, got {}", p));
                }
            } else if msg_type_lower.contains("pitch") {
                if let Some(p) = pitch
                    && (*p < -8192 || *p > 8191)
                {
                    ctx.error(
                        path,
                        format!("MIDI pitch bend must be -8192 to +8191, got {}", p),
                    );
                }
            } else if msg_type_lower.contains("aftertouch")
                && let Some(p) = pressure
                && *p > 127
            {
                ctx.error(path, format!("MIDI pressure must be 0-127, got {}", p));
            }
        }
        ActionConfig::MidiForward {
            target, transform, ..
        } => {
            ctx.midi_features.push("MidiForward".to_string());
            if target.is_empty() {
                ctx.error(path, "MidiForward requires target port name");
            } else if !ctx.device_known(target) {
                // ADR-035: a `target` that doesn't match any
                // `[[endpoints]]` alias is a raw port name. It still works,
                // but the hot-plug rescan loop only refreshes the device
                // output map for aliased outputs — raw-port-name targets
                // bypass it (action_executor falls through to
                // connect_by_name). So they get no hot-plug liveness, no
                // status pill, no mute affordance. Warn (non-blocking) so the
                // operator can choose to define an endpoint.
                ctx.warning(
                    path,
                    format!(
                        "MidiForward target '{0}' does not match any [[endpoints]] alias, so it's \
                         treated as a raw port name. It will still forward, but receives no \
                         hot-plug status, mute affordance, or device-status integration. \
                         Define an [[endpoints]] entry with alias = \"{0}\" for full runtime \
                         tracking, or ignore this if a raw port name is intentional.",
                        target
                    ),
                );
            }
            if let Some(t) = transform {
                let errs = t.validate();
                if !errs.is_empty() {
                    ctx.error(path, format!("MidiForward transform: {}", errs.join("; ")));
                }
            }
        }
        ActionConfig::HidForward { target, transform } => {
            use crate::config::protocol::Protocol;
            use crate::config::types::SignalTransform;
            ctx.hid_features.push("HidForward".to_string());

            // Target must be a declared endpoint: unlike MidiForward (which
            // tolerates raw port names), HidForward MUST resolve the target's
            // protocol to validate the transform variant against it.
            if target.is_empty() {
                ctx.error(path, "HidForward requires a target endpoint alias");
            } else {
                match ctx.device_protocols.get(target).copied() {
                    None => ctx.error(
                        path,
                        format!(
                            "HidForward target '{target}' does not match any [[endpoints]] alias. \
                             HidForward needs a declared output endpoint so its transform variant \
                             can be checked against the target protocol."
                        ),
                    ),
                    // V1: HidForward forwards to a MIDI output only (HidToMidi
                    // → MIDI). HID→OSC and HID→Art-Net stay route-only: routing
                    // those endpoints by alias needs output-endpoint resolution
                    // the action executor does not carry, and there is no
                    // Art-Net output capability yet. Reject the cross-protocol
                    // variants at load with a pointer to routes. Centralized
                    // protocol/variant check (parity with route validation).
                    Some(Protocol::Midi)
                        if matches!(transform, SignalTransform::HidToMidi { .. }) =>
                    {
                        // OK: HidToMidi → MIDI output. Range-validate the
                        // transform values at load (reject, don't mask), the
                        // same gate routes apply to HidToMidi.
                        if let SignalTransform::HidToMidi {
                            trigger_to_cc,
                            channel,
                        } = transform
                        {
                            if *channel > 15 {
                                ctx.error(
                                    path,
                                    format!(
                                        "HidForward HidToMidi channel {channel} out of range \
                                         (must be 0-15)"
                                    ),
                                );
                            }
                            for (trigger, cc) in trigger_to_cc {
                                if *cc > 127 {
                                    ctx.error(
                                        path,
                                        format!(
                                            "HidForward HidToMidi CC {cc} for trigger '{trigger}' \
                                             out of range (must be 0-127)"
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    Some(_) => match transform {
                        SignalTransform::HidToOsc { .. } | SignalTransform::HidToArtNet { .. } => {
                            ctx.error(
                                path,
                                format!(
                                    "HidForward to '{target}' uses {}, but HidForward V1 only \
                                     forwards to a MIDI output (HidToMidi). For HID→OSC or \
                                     HID→Art-Net, use a route instead — those already work.",
                                    transform_variant_name(transform)
                                ),
                            )
                        }
                        t => ctx.error(
                            path,
                            format!(
                                "HidForward transform {} does not match target '{target}'. \
                                 HidForward V1 requires a HidToMidi transform and a MIDI output \
                                 target.",
                                transform_variant_name(t)
                            ),
                        ),
                    },
                }
            }
        }
        ActionConfig::OscForward { target, transform } => {
            use crate::config::protocol::Protocol;
            ctx.osc_features.push("OscForward".to_string());

            // V1 is pass-through: a transform is reserved for a future
            // OSC→OSC remap and must be absent (mirrors HidForward V1's
            // transform restriction).
            if transform.is_some() {
                ctx.error(
                    path,
                    "OscForward transform is not supported in V1 — omit it (the inbound \
                     OSC message is forwarded verbatim). An OSC→OSC remap is a follow-up.",
                );
            }

            // Target must resolve to a declared, *enabled OSC output*
            // (Output/Bidirectional) endpoint — exactly the criteria the daemon
            // uses to build its runtime `osc_output_endpoints` map, so a config
            // that loads is one whose target can actually be sent to.
            // Reachability over the wire is still enforced at send time by the
            // connector registry. The OSC-*source* gate is enforced at dispatch
            // (parity with HidForward): the executor requires the inbound OSC
            // message in the trigger context, so a non-OSC-triggered mapping is
            // a runtime no-op.
            use crate::config::types::ConnectorDirection;
            if target.is_empty() {
                ctx.error(
                    path,
                    "OscForward requires a target OSC output endpoint alias",
                );
            } else {
                match ctx.device_protocols.get(target).copied() {
                    None => ctx.error(
                        path,
                        format!(
                            "OscForward target '{target}' does not match any [[endpoints]] alias. \
                             Declare an OSC output endpoint with that alias."
                        ),
                    ),
                    Some(Protocol::Osc) => {
                        // Protocol matches; now require an *enabled output* so the
                        // runtime map will actually contain it.
                        match ctx.endpoint_dir_enabled.get(target).copied() {
                            Some((_, false)) => ctx.error(
                                path,
                                format!(
                                    "OscForward target '{target}' is a disabled endpoint; enable \
                                     it (enabled = true) to forward to it."
                                ),
                            ),
                            Some((dir, true))
                                if !matches!(
                                    dir,
                                    ConnectorDirection::Output | ConnectorDirection::Bidirectional
                                ) =>
                            {
                                ctx.error(
                                    path,
                                    format!(
                                        "OscForward target '{target}' is an Input-only OSC \
                                         endpoint; OscForward requires an OSC output (direction = \
                                         \"Output\" or \"Bidirectional\")."
                                    ),
                                );
                            }
                            Some(_) => {}
                            // Both maps seed from the same endpoints list, so a
                            // protocol hit with no direction entry is unreachable;
                            // fail closed if it ever happens.
                            None => ctx.error(
                                path,
                                format!(
                                    "OscForward target '{target}' could not be resolved to an \
                                     endpoint direction."
                                ),
                            ),
                        }
                    }
                    Some(other) => ctx.error(
                        path,
                        format!(
                            "OscForward target '{target}' is a {other:?} endpoint; OscForward \
                             requires an OSC output endpoint."
                        ),
                    ),
                }
            }
        }
        ActionConfig::OscSend {
            host,
            port,
            address,
            ..
        } => {
            ctx.osc_features.push("OscSend".to_string());
            if host.is_empty() {
                ctx.error(path, "OscSend requires host");
            }
            if *port == 0 {
                ctx.error(path, "OscSend requires non-zero port");
            }
            if address.is_empty() || !address.starts_with('/') {
                ctx.error(path, "OscSend address must start with '/'");
            }
        }
        ActionConfig::Plugin { plugin, params } => {
            if plugin.is_empty() {
                ctx.error(path, "Plugin action requires non-empty plugin name");
            } else if plugin == "." || plugin == ".." || plugin.contains("..") {
                ctx.error(
                    path,
                    format!("Plugin name '{}' is not allowed (path traversal)", plugin),
                );
            } else if !plugin
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            {
                ctx.error(
                    path,
                    format!(
                        "Plugin name '{}' contains invalid characters (only ASCII alphanumeric, hyphens, underscores, dots allowed)",
                        plugin
                    ),
                );
            }
            if params.is_null() || params.as_object().is_some_and(|o| o.is_empty()) {
                ctx.warning(
                    path,
                    format!("Plugin '{}' has no parameters (may be intentional)", plugin),
                );
            }
        }
        // ADR-025 Phase 2.D: typed-surface arms. Field-level bounds
        // checks (device non-empty, channel 0-15, cc 0-127, range
        // min/max in 0-127, min <= max) are done HERE so obviously-
        // invalid configs surface at load time rather than at runtime.
        // Cross-cutting structural checks (range overlap, DeviceRef
        // resolution against the binding registry) remain in task #26.
        //
        // PC-key bounds (0-127) are already enforced at the
        // deserialisation boundary by `types::string_keyed_pc_map` so
        // no extra check is needed here for `mappings` keys.
        ActionConfig::PcContextSwitch {
            channel,
            device,
            mappings,
            default,
        } => {
            if device.is_empty() {
                ctx.error(path, "PcContextSwitch: device is required");
            } else if !ctx.device_known(device) {
                ctx.error(
                    path,
                    format!("PcContextSwitch: unknown device '{}'", device),
                );
            }
            if *channel > 15 {
                ctx.error(
                    path,
                    format!(
                        "PcContextSwitch: channel out of range {} (must be 0-15)",
                        channel
                    ),
                );
            }
            for (pc, inner) in mappings {
                validate_action(inner, &format!("{}.mappings[{}]", path, pc), ctx, depth + 1);
            }
            if let Some(def) = default {
                validate_action(def, &format!("{}.default", path), ctx, depth + 1);
            }
        }
        ActionConfig::CcContextSwitch {
            cc,
            channel,
            device,
            ranges,
            default,
        } => {
            if device.is_empty() {
                ctx.error(path, "CcContextSwitch: device is required");
            } else if !ctx.device_known(device) {
                ctx.error(
                    path,
                    format!("CcContextSwitch: unknown device '{}'", device),
                );
            }
            if *cc > 127 {
                ctx.error(
                    path,
                    format!("CcContextSwitch: cc out of range {} (must be 0-127)", cc),
                );
            }
            if *channel > 15 {
                ctx.error(
                    path,
                    format!(
                        "CcContextSwitch: channel out of range {} (must be 0-15)",
                        channel
                    ),
                );
            }
            for (i, r) in ranges.iter().enumerate() {
                let range_path = format!("{}.ranges[{}]", path, i);
                if r.min > 127 {
                    ctx.error(
                        &range_path,
                        format!("CcContextSwitch range: min out of range {} (0-127)", r.min),
                    );
                }
                if r.max > 127 {
                    ctx.error(
                        &range_path,
                        format!("CcContextSwitch range: max out of range {} (0-127)", r.max),
                    );
                }
                if r.min > r.max {
                    ctx.error(
                        &range_path,
                        format!(
                            "CcContextSwitch range: min ({}) > max ({}) is unsatisfiable",
                            r.min, r.max
                        ),
                    );
                }
                validate_action(&r.action, &format!("{}.action", range_path), ctx, depth + 1);
            }

            // Pairwise overlap detection (order-independent).
            // Runtime dispatch is first-match-wins, so an overlap
            // silently masks the later branch — catch it at load
            // time. Only compare well-formed ranges (min <= max) to
            // avoid cascading noise from already-reported unsatisfiable
            // ranges. O(n²) is fine because meaningful `ranges` is
            // bounded by the MIDI CC value space (128); oversize
            // branch tables still lower to `ContextSwitchTable` so
            // this loop runs on the same data either way.
            for i in 0..ranges.len() {
                for j in (i + 1)..ranges.len() {
                    let a = &ranges[i];
                    let b = &ranges[j];
                    if a.min > a.max || b.min > b.max {
                        continue;
                    }
                    if a.min <= b.max && b.min <= a.max {
                        // Anchor the finding to the later (masked)
                        // range — that's the branch that won't ever
                        // fire under first-match-wins. Both indices
                        // appear in the message so tooling can still
                        // locate the earlier offender.
                        ctx.error(
                            format!("{}.ranges[{}]", path, j),
                            format!(
                                "CcContextSwitch: ranges[{}] ({}-{}) overlaps ranges[{}] ({}-{}); first-match-wins will mask the later branch",
                                i, a.min, a.max, j, b.min, b.max
                            ),
                        );
                    }
                }
            }

            if let Some(def) = default {
                validate_action(def, &format!("{}.default", path), ctx, depth + 1);
            }
        }
        // ADR-038 §4.1: observation sugar. Only the schema-level check
        // (non-empty message) lives here currently; the let-through
        // advisories (Tap-consumes, HID hard error) are not yet implemented.
        ActionConfig::Tap { message } => {
            // Treat whitespace-only messages as blank, mirroring the
            // alias/command `trim()` checks elsewhere in this module.
            if message.trim().is_empty() {
                ctx.error(path, "Tap action requires a non-empty message");
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Security: Shell command validation (from former loader.rs)
// ────────────────────────────────────────────────────────────────
