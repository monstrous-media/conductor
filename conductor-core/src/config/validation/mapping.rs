// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Cross-references, ModeIs deprecation, and per-mapping validation.

use super::*;

pub(super) fn validate_cross_references(config: &Config, ctx: &mut ValidationCtx) {
    let mode_names: HashSet<&str> = config.modes.iter().map(|m| m.name.as_str()).collect();

    // Check ModeChange actions reference existing modes
    let all_mappings = config
        .global_mappings
        .iter()
        .chain(config.modes.iter().flat_map(|m| m.mappings.iter()));

    for mapping in all_mappings {
        check_mode_references(&mapping.action, &mode_names, ctx, 0);
    }
}

pub(super) fn check_mode_references(
    action: &ActionConfig,
    mode_names: &HashSet<&str>,
    ctx: &mut ValidationCtx,
    depth: usize,
) {
    if depth > MAX_ACTION_DEPTH {
        ctx.error(
            "action",
            format!(
                "Action nesting exceeds maximum depth of {}",
                MAX_ACTION_DEPTH
            ),
        );
        return;
    }
    match action {
        ActionConfig::ModeChange { mode }
            if !mode.is_empty() && !mode_names.contains(mode.as_str()) =>
        {
            // Check for case-insensitive near-match
            let suggestion = mode_names
                .iter()
                .find(|name| name.eq_ignore_ascii_case(mode))
                .map(|name| format!(" (did you mean '{}'?)", name));
            ctx.error(
                "action.mode_change",
                format!(
                    "ModeChange references non-existent mode '{}'{}",
                    mode,
                    suggestion.unwrap_or_default()
                ),
            );
        }
        ActionConfig::ModeChange { .. } => {}
        ActionConfig::Sequence { actions } => {
            for a in actions {
                check_mode_references(a, mode_names, ctx, depth + 1);
            }
        }
        ActionConfig::Conditional {
            then_action,
            else_action,
            ..
        } => {
            check_mode_references(then_action, mode_names, ctx, depth + 1);
            if let Some(ea) = else_action {
                check_mode_references(ea, mode_names, ctx, depth + 1);
            }
        }
        ActionConfig::Repeat { action, .. } => {
            check_mode_references(action, mode_names, ctx, depth + 1);
        }
        _ => {}
    }
}

/// Migration hint emitted for a deprecated `Conditional`+`ModeIs` dispatch (§4.4).
const MODEIS_DEPRECATION_HINT: &str = "Conditional with a top-level `ModeIs` condition is mode-scoping expressed the \
     hard way and is deprecated (ADR-040 §D6 — Phase 1 = warning, later a hard \
     error). Prefer a mode-scoped mapping: move this mapping into the named mode's \
     `[[modes.mappings]]`. NOTE: composite conditions (`And`/`Or`/`Not` wrapping \
     `ModeIs`, e.g. `And(ModeIs, AppFrontmost)`) are NOT deprecated — they express \
     mode∩app and remain valid.";

/// ADR-040 §4.4 / §D6 Phase 1 — warn (non-fatal `Severity::Warning`) on a
/// `Conditional` action used as mode-scoping "the hard way": its **outermost**
/// condition is `ModeIs` and its `else_action` is absent or itself a `ModeIs`
/// dispatch chain. Composite conditions (`And`/`Or`/`Not` wrapping `ModeIs`)
/// express something mode-scoping can't (mode∩app, etc.) and are left silent.
///
/// (§4.6 — the "app in both `[per_app_profiles]` and `[per_app_modes]`" warning —
/// is deliberately NOT here: `[per_app_profiles]` is the GUI's `profiles.json`
/// manifest, not a core `Config` field, so the core validator can't see it. That
/// warning belongs at the daemon's manifest-load layer; tracked as a follow-up.)
pub(super) fn validate_conditional_modeis_deprecation(config: &Config, ctx: &mut ValidationCtx) {
    for (mi, mode) in config.modes.iter().enumerate() {
        for (ai, mapping) in mode.mappings.iter().enumerate() {
            let path = format!("modes[{mi}].mappings[{ai}].action");
            warn_modeis_dispatch(&mapping.action, &path, ctx, 0);
        }
    }
    for (ai, mapping) in config.global_mappings.iter().enumerate() {
        let path = format!("global_mappings[{ai}].action");
        warn_modeis_dispatch(&mapping.action, &path, ctx, 0);
    }
}

/// Whether `condition` is directly `ModeIs` (the "outermost" test — a `ModeIs`
/// nested inside `And`/`Or`/`Not` is NOT directly `ModeIs`, so composites are
/// excluded).
fn is_outermost_modeis(condition: &crate::actions::Condition) -> bool {
    matches!(condition, crate::actions::Condition::ModeIs { .. })
}

/// True iff the else branch is absent, or is itself a `Conditional` whose
/// outermost condition is `ModeIs` and whose else recurses the same way — i.e. a
/// pure `ModeIs` dispatch chain (§4.4). A plain (non-conditional) else, or one
/// guarded by a non-`ModeIs`/composite condition, breaks the chain and means the
/// action is doing real branching → not part of the deprecation.
///
/// Depth-bounded: action configs are user-controlled, so a
/// pathologically deep `else if ModeIs(…)` chain must not overflow the stack. At
/// the cap we return `false` — we can't confirm a *pure* `ModeIs` chain, so we
/// don't claim the deprecation (under-warn rather than risk a crash). Such a
/// config is already flagged by `check_mode_references`'s depth error.
fn else_is_modeis_chain_or_absent(else_action: Option<&ActionConfig>, depth: usize) -> bool {
    if depth > MAX_ACTION_DEPTH {
        return false;
    }
    match else_action {
        None => true,
        Some(ActionConfig::Conditional {
            condition,
            else_action,
            ..
        }) => {
            is_outermost_modeis(condition)
                && else_is_modeis_chain_or_absent(else_action.as_deref(), depth + 1)
        }
        Some(_) => false,
    }
}

/// Walk an action tree, warning once per top-level-`ModeIs` `Conditional`
/// dispatch (§4.4). Recurses into `Sequence`/`Repeat` and the branches of a
/// *non*-deprecated `Conditional`. When a deprecated dispatch chain IS found it
/// warns once, then scans the chain's sub-actions for *nested* dispatches via
/// [`scan_chain_for_nested`] (so a deprecated dispatch buried in a chain link's
/// then-branch is still caught) without re-warning the chain itself.
pub(super) fn warn_modeis_dispatch(
    action: &ActionConfig,
    path: &str,
    ctx: &mut ValidationCtx,
    depth: usize,
) {
    // Depth bound mirrors check_mode_references; that walk (over the same actions)
    // already emits the depth error, so here we just stop descending.
    if depth > MAX_ACTION_DEPTH {
        return;
    }
    match action {
        ActionConfig::Conditional {
            condition,
            then_action,
            else_action,
        } => {
            if is_outermost_modeis(condition)
                && else_is_modeis_chain_or_absent(else_action.as_deref(), 0)
            {
                ctx.warning(path, MODEIS_DEPRECATION_HINT);
                // Warn ONCE for the chain, but still scan every link's sub-actions
                // for an unrelated nested dispatch.
                scan_chain_for_nested(action, path, ctx, depth);
            } else {
                warn_modeis_dispatch(then_action, &format!("{path}.then_action"), ctx, depth + 1);
                if let Some(ea) = else_action {
                    warn_modeis_dispatch(ea, &format!("{path}.else_action"), ctx, depth + 1);
                }
            }
        }
        ActionConfig::Sequence { actions } => {
            for (i, a) in actions.iter().enumerate() {
                warn_modeis_dispatch(a, &format!("{path}.sequence[{i}]"), ctx, depth + 1);
            }
        }
        ActionConfig::Repeat { action, .. } => {
            warn_modeis_dispatch(action, &format!("{path}.repeat"), ctx, depth + 1);
        }
        _ => {}
    }
}

/// Scan an already-warned `ModeIs` dispatch chain for *nested* deprecated
/// dispatches without re-warning the chain links themselves (preserves "warn once
/// per chain"). For each link: full-scan its then-branch (which
/// may contain an unrelated nested dispatch), then descend the else-chain to the
/// next link. Depth-bounded like the other walkers.
fn scan_chain_for_nested(action: &ActionConfig, path: &str, ctx: &mut ValidationCtx, depth: usize) {
    if depth > MAX_ACTION_DEPTH {
        return;
    }
    if let ActionConfig::Conditional {
        then_action,
        else_action,
        ..
    } = action
    {
        // then-branch: full scan (a nested dispatch here SHOULD warn separately).
        warn_modeis_dispatch(then_action, &format!("{path}.then_action"), ctx, depth + 1);
        // Caller only invokes this on an action `else_is_modeis_chain_or_absent`
        // already accepted, so every link's else is `None` or another `ModeIs`
        // `Conditional` — a non-conditional else is unreachable here. Descend the
        // next link; do nothing for `None` (no dead tail).
        if let Some(next @ ActionConfig::Conditional { .. }) = else_action.as_deref() {
            scan_chain_for_nested(next, &format!("{path}.else_action"), ctx, depth + 1);
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Per-mapping validation (merged from both layers)
// ────────────────────────────────────────────────────────────────

pub(super) fn validate_mapping(
    mapping: &Mapping,
    path: &str,
    device_aliases: &HashSet<&String>,
    device_protocols: &HashMap<&str, crate::config::protocol::Protocol>,
    ctx: &mut ValidationCtx,
) {
    let trigger_path = format!("{}.trigger", path);
    let action_path = format!("{}.action", path);

    validate_trigger(&mapping.trigger, &trigger_path, device_aliases, ctx);
    validate_action(&mapping.action, &action_path, ctx, 0);
    validate_let_through(mapping, path, device_protocols, ctx);

    // ADR-039-B: a `HidForward` action reads the structured
    // gamepad `InputEvent` that fired the mapping. If the mapping's trigger is
    // not exclusively HID, that event is absent (a MIDI trigger yields a MIDI
    // event whose `channel: Some(_)` the HID transforms reject) and the
    // forward would silently do nothing. Reject at load rather than silent-drop
    // at runtime.
    if action_contains_hid_forward(&mapping.action)
        && !trigger_is_exclusively_hid(&mapping.trigger, device_protocols)
    {
        ctx.error(
            &action_path,
            "HidForward requires an exclusively-HID trigger (a Gamepad* trigger, or one whose \
             `device` resolves to a `protocol = \"hid\"` endpoint): it forwards the structured \
             gamepad event that fired the mapping, which a MIDI/any-source trigger does not \
             produce — the forward would silently do nothing (ADR-039-B §6.2.1).",
        );
    }
}

/// Recursively test whether an action tree contains a `HidForward` (possibly
/// nested inside `Conditional`/`Sequence`/`Repeat`/context-switch branches).
///
/// Bounded by `MAX_ACTION_DEPTH` (same guard as `validate_action`) so a
/// pathologically deep config can't stack-overflow this post-pass — at the
/// limit we stop descending and return `false` (the deep action also trips
/// `validate_action`'s own depth error, so the config is rejected regardless).
fn action_contains_hid_forward(action: &ActionConfig) -> bool {
    action_contains_hid_forward_depth(action, 0)
}

fn action_contains_hid_forward_depth(action: &ActionConfig, depth: usize) -> bool {
    if depth > MAX_ACTION_DEPTH {
        return false;
    }
    let recurse = |a: &ActionConfig| action_contains_hid_forward_depth(a, depth + 1);
    match action {
        ActionConfig::HidForward { .. } => true,
        ActionConfig::Sequence { actions } => actions.iter().any(recurse),
        ActionConfig::Repeat { action, .. } => recurse(action),
        ActionConfig::Conditional {
            then_action,
            else_action,
            ..
        } => recurse(then_action) || else_action.as_deref().is_some_and(recurse),
        ActionConfig::PcContextSwitch {
            mappings, default, ..
        } => mappings.iter().any(|(_, a)| recurse(a)) || default.as_deref().is_some_and(recurse),
        ActionConfig::CcContextSwitch {
            ranges, default, ..
        } => ranges.iter().any(|r| recurse(&r.action)) || default.as_deref().is_some_and(recurse),
        _ => false,
    }
}

/// ADR-038 §4.3 let-through validator.
///
/// (1) Hard error: `let_through = true` on an *exclusively-HID* mapping —
///     a HID-only trigger (`GamepadButton`/`GamepadButtonChord`/
///     `GamepadAnalogStick`/`GamepadTrigger`) or a trigger whose `device`
///     resolves to an endpoint declared `protocol = "hid"`. Routes are
///     MIDI-only today, so the let-through is a silent no-op (ADR-039-B
///     territory). MIDI / any-device mappings are unprovable and do NOT error.
///     (Only the statically-detectable "explicit" tier from §4.3.1 is
///     enforced here; the live-USB vs cached-absent severity tiers need
///     runtime device state the config validator does not carry.)
///
/// (3) Warning: a `Tap` with `let_through = false` observes the event then
///     swallows it — almost never intended.
///
/// Check (2) (proof-based "ineffective let-through") is intentionally not
/// implemented (spec §4.3.2 / R2 P4 — droppable rather than risk a
/// false-positive linter).
pub(super) fn validate_let_through(
    mapping: &Mapping,
    path: &str,
    device_protocols: &HashMap<&str, crate::config::protocol::Protocol>,
    ctx: &mut ValidationCtx,
) {
    // (3) Tap that consumes.
    if matches!(mapping.action, ActionConfig::Tap { .. }) && !mapping.let_through {
        ctx.warning(
            path,
            "uses a Tap action with let_through = false: it observes the event and then \
             swallows it, which is almost never intended. Set let_through = true to observe \
             without intercepting, or use a different action to intercept.",
        );
    }

    // (1) HID hard error — only relevant when let-through is requested.
    if mapping.let_through && trigger_is_exclusively_hid(&mapping.trigger, device_protocols) {
        ctx.error(
            path,
            "sets let_through = true on a HID-only source. let_through forwards to routes, but \
             HID has no route path until ADR-039-B — this would silently do nothing. Remove \
             let_through, or change the source.",
        );
    }
}

/// True when the trigger is exclusively HID by *explicit* classification: a
/// HID-only trigger variant, or a `device` filter that resolves to an endpoint
/// declared `protocol = "hid"`. A trigger with no device filter (any input) or
/// one resolving to a MIDI/unspecified endpoint is NOT exclusively HID.
fn trigger_is_exclusively_hid(
    trigger: &Trigger,
    device_protocols: &HashMap<&str, crate::config::protocol::Protocol>,
) -> bool {
    use crate::config::protocol::Protocol;
    if matches!(
        trigger,
        Trigger::GamepadButton { .. }
            | Trigger::GamepadButtonChord { .. }
            | Trigger::GamepadAnalogStick { .. }
            | Trigger::GamepadTrigger { .. }
    ) {
        return true;
    }
    trigger
        .device()
        .is_some_and(|alias| device_protocols.get(alias.as_str()) == Some(&Protocol::Hid))
}

// ────────────────────────────────────────────────────────────────
// Trigger validation
// ────────────────────────────────────────────────────────────────
