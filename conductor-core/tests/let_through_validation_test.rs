// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-038 — let-through validator (spec §4.3).
//!
//! Two checks ship here:
//!  (1) **HID hard error** — `let_through = true` on an *exclusively-HID*
//!      mapping (a HID-only trigger like `GamepadButton`, or a trigger whose
//!      `device` resolves to an endpoint declared `protocol = "hid"`). Routes
//!      are MIDI-only today, so let-through there is a silent no-op (ADR-039-B
//!      territory). MIDI / any-device mappings do NOT error.
//!  (3) **Tap-consumes warning** — a `Tap` with `let_through = false` observes
//!      then swallows the event, almost never intended.
//!
//! Check (2) (proof-based "ineffective let-through" advisory) is intentionally
//! NOT shipped — per spec §4.3.2 / R2 P4 it is droppable rather than risk a
//! false-positive linter, and the resolved-endpoint proof is out of scope here.
//! The live-USB vs cached-absent severity tiers (§4.3.1) need runtime device
//! state the config validator does not have; the statically-detectable
//! "explicit" tier (hard error) is the non-negotiable safety net implemented.
//!
//! ADR-035: device identity now lives in the unified `[[endpoints]]` set; the
//! HID-protocol fixture is an `EndpointConfig` declared `protocol = "hid"`,
//! `direction = Input` (HID is input-only — ADR-039 D7).

use conductor_core::config::protocol::Protocol;
use conductor_core::config::types::{
    ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind,
};
use conductor_core::config::validation::{Severity, validate_config};
use conductor_core::identity::DeviceMatcher;
use conductor_core::{ActionConfig, Config, Mapping, Mode, Trigger};

/// Map the test-facing `Protocol` onto the endpoint-level `ConnectorProtocol`.
fn connector_protocol(p: Protocol) -> ConnectorProtocol {
    match p {
        Protocol::Midi => ConnectorProtocol::Midi,
        Protocol::Hid => ConnectorProtocol::Hid,
        Protocol::Osc => ConnectorProtocol::Osc,
        Protocol::ArtNet => ConnectorProtocol::ArtNet,
    }
}

/// An Input endpoint declaring `protocol` (e.g. HID) so the let-through
/// validator can classify a trigger's `device` reference. HID is input-only,
/// so `direction = Input` keeps the endpoint itself valid (ADR-039 D7).
fn endpoint(alias: &str, protocol: Option<Protocol>) -> EndpointConfig {
    EndpointConfig {
        alias: alias.to_string(),
        direction: ConnectorDirection::Input,
        protocol: protocol.map(connector_protocol),
        description: None,
        enabled: true,
        channels: vec![],
        kind: EndpointKind::Matcher {
            matchers: vec![DeviceMatcher::name_contains(alias)],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: true,
        },
    }
}

fn mapping(trigger: Trigger, action: ActionConfig, let_through: bool) -> Mapping {
    Mapping {
        trigger,
        action,
        description: None,
        let_through,
    }
}

fn keystroke() -> ActionConfig {
    ActionConfig::Keystroke {
        keys: "a".to_string(),
        modifiers: vec![],
    }
}

fn note(device: Option<&str>) -> Trigger {
    Trigger::Note {
        note: 60,
        velocity_min: None,
        channel: None,
        device: device.map(String::from),
    }
}

fn gamepad(device: Option<&str>) -> Trigger {
    Trigger::GamepadButton {
        button: 128,
        velocity_min: None,
        device: device.map(String::from),
    }
}

fn config_with(endpoints: Vec<EndpointConfig>, mappings: Vec<Mapping>) -> Config {
    Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints,
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings,
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    }
}

fn has_error(report: &conductor_core::config::validation::ValidationReport, needle: &str) -> bool {
    report
        .errors
        .iter()
        .any(|f| f.severity == Severity::Error && f.message.contains(needle))
}

fn has_warning(
    report: &conductor_core::config::validation::ValidationReport,
    needle: &str,
) -> bool {
    report
        .warnings
        .iter()
        .any(|f| f.severity == Severity::Warning && f.message.contains(needle))
}

// ── (1) HID hard error ───────────────────────────────────────────

#[test]
fn hid_only_trigger_with_let_through_is_error() {
    let cfg = config_with(vec![], vec![mapping(gamepad(None), keystroke(), true)]);
    let report = validate_config(&cfg);
    assert!(
        has_error(&report, "let_through"),
        "a HID-only trigger (GamepadButton) with let_through=true must be a hard error: {:?}",
        report.errors
    );
}

#[test]
fn hid_protocol_device_with_let_through_is_error() {
    // A trigger whose device resolves to an endpoint declared protocol="hid".
    let cfg = config_with(
        vec![endpoint("pad", Some(Protocol::Hid))],
        vec![mapping(note(Some("pad")), keystroke(), true)],
    );
    let report = validate_config(&cfg);
    assert!(
        has_error(&report, "let_through"),
        "let_through on a device declared protocol=hid must be a hard error: {:?}",
        report.errors
    );
}

#[test]
fn midi_note_with_let_through_is_not_an_error() {
    let cfg = config_with(
        vec![endpoint("mikro", Some(Protocol::Midi))],
        vec![mapping(note(Some("mikro")), keystroke(), true)],
    );
    let report = validate_config(&cfg);
    assert!(
        !has_error(&report, "let_through"),
        "let_through on a MIDI device must not error: {:?}",
        report.errors
    );
}

#[test]
fn any_device_let_through_is_not_an_error() {
    // No device filter (any input) — unprovable as HID-only, must not error.
    let cfg = config_with(vec![], vec![mapping(note(None), keystroke(), true)]);
    let report = validate_config(&cfg);
    assert!(
        !has_error(&report, "let_through"),
        "let_through on an any-device MIDI trigger must not error: {:?}",
        report.errors
    );
}

#[test]
fn hid_only_trigger_without_let_through_is_not_an_error() {
    // The error is specifically about let_through; a plain HID mapping is fine.
    let cfg = config_with(vec![], vec![mapping(gamepad(None), keystroke(), false)]);
    let report = validate_config(&cfg);
    assert!(
        !has_error(&report, "let_through"),
        "a HID trigger WITHOUT let_through must not error: {:?}",
        report.errors
    );
}

// ── (3) Tap-consumes warning ─────────────────────────────────────

fn tap() -> ActionConfig {
    ActionConfig::Tap {
        message: "observed".to_string(),
    }
}

#[test]
fn tap_with_let_through_false_warns() {
    let cfg = config_with(vec![], vec![mapping(note(None), tap(), false)]);
    let report = validate_config(&cfg);
    assert!(
        has_warning(&report, "Tap"),
        "a Tap with let_through=false should warn about swallowing the event: {:?}",
        report.warnings
    );
}

#[test]
fn tap_with_let_through_true_does_not_warn() {
    let cfg = config_with(vec![], vec![mapping(note(None), tap(), true)]);
    let report = validate_config(&cfg);
    assert!(
        !has_warning(&report, "Tap action with let_through"),
        "a Tap with let_through=true is the intended observe-and-pass-through use: {:?}",
        report.warnings
    );
}
