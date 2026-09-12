// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Unified config validation system
//!
//! Merges the two previous validation layers:
//! - Structural + security validation (formerly in `loader.rs` `Config::validate()`)
//! - Protocol coverage validation (formerly in `validator.rs` `validate_config()`)
//!
//! All config validation now flows through this single module.

use crate::config::types::{
    ActionConfig, Config, ConnectorDirection, ConnectorProtocol, Mapping, Trigger,
};
use crate::error::ConfigError;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

// ────────────────────────────────────────────────────────────────
// Public types (preserved from former validator.rs)
// ────────────────────────────────────────────────────────────────

/// Severity level for validation findings
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// A single validation finding
#[derive(Debug, Clone, Serialize)]
pub struct ValidationFinding {
    pub severity: Severity,
    pub path: String,
    pub message: String,
}

/// Protocol coverage metrics
#[derive(Debug, Clone, Serialize)]
pub struct ProtocolCoverage {
    /// MIDI features used vs available
    pub midi: CoverageMetric,
    /// HID features used vs available
    pub hid: CoverageMetric,
    /// OSC features used vs available
    pub osc: CoverageMetric,
}

/// A single coverage metric
#[derive(Debug, Clone, Serialize)]
pub struct CoverageMetric {
    pub used: Vec<String>,
    pub available: Vec<String>,
    pub percentage: f64,
}

/// Full validation report
#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub errors: Vec<ValidationFinding>,
    pub warnings: Vec<ValidationFinding>,
    pub coverage: ProtocolCoverage,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn total_findings(&self) -> usize {
        self.errors.len() + self.warnings.len()
    }

    /// Format all errors into a single string (for ConfigError conversion)
    pub fn format_errors(&self) -> String {
        self.errors
            .iter()
            .map(|f| format!("{}: {}", f.path, f.message))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

// ────────────────────────────────────────────────────────────────
// Internal accumulator used during validation
// ────────────────────────────────────────────────────────────────

struct ValidationCtx {
    errors: Vec<ValidationFinding>,
    warnings: Vec<ValidationFinding>,
    midi_features: Vec<String>,
    hid_features: Vec<String>,
    osc_features: Vec<String>,
    /// Declared device aliases, captured once at the top of
    /// `validate_config`. State conditions and context-switch actions
    /// ERROR on unknown aliases because the state store only observes
    /// events from declared devices. Triggers only WARN (ListenMode::All
    /// auto-discovers), so we keep the stricter check local to
    /// state-bearing nodes.
    device_aliases: HashSet<String>,
    /// Declared endpoint alias → protocol, captured alongside
    /// `device_aliases`. Used by the `HidForward` action validator to check
    /// the transform variant matches the target endpoint's protocol
    /// (ADR-039-B), reusing the same protocol vocabulary as
    /// route validation.
    device_protocols: std::collections::HashMap<String, crate::config::protocol::Protocol>,
    /// Declared endpoint alias → (direction, enabled), captured alongside
    /// `device_protocols`. `OscForward` uses this to require its target be an
    /// *enabled OSC output* (Output/Bidirectional) endpoint — mirroring the
    /// daemon's runtime `osc_output_endpoints` map criteria so a config that
    /// loads is exactly one whose target can actually be sent to.
    endpoint_dir_enabled:
        std::collections::HashMap<String, (crate::config::types::ConnectorDirection, bool)>,
    /// ADR-027 D3 §3.2: policy applied to Shell
    /// actions whose resolved binary is a known interpreter family.
    /// Populated from `config.advanced_settings.allow_interpreters` at
    /// the entry to `validate_config`. Defaults to `Warn` for
    /// freshly-constructed `ValidationCtx` (matches the
    /// `AdvancedSettings::default()` and the Shell-validation tests
    /// that don't go through `validate_config`).
    allow_interpreters: crate::config::types::InterpreterPolicy,
}

impl ValidationCtx {
    fn new() -> Self {
        Self {
            errors: Vec::new(),
            warnings: Vec::new(),
            midi_features: Vec::new(),
            hid_features: Vec::new(),
            osc_features: Vec::new(),
            device_aliases: HashSet::new(),
            device_protocols: std::collections::HashMap::new(),
            endpoint_dir_enabled: std::collections::HashMap::new(),
            allow_interpreters: crate::config::types::InterpreterPolicy::default(),
        }
    }

    fn device_known(&self, alias: &str) -> bool {
        self.device_aliases.contains(alias)
    }

    fn error(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(ValidationFinding {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
        });
    }

    fn warning(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.warnings.push(ValidationFinding {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
        });
    }
}

// ────────────────────────────────────────────────────────────────
// Public API
// ────────────────────────────────────────────────────────────────

/// Run full validation on a config and produce a report.
///
/// This is the single entry-point that replaces both the old
/// `Config::validate()` and `validator::validate_config()`.
pub fn validate_config(config: &Config) -> ValidationReport {
    let mut ctx = ValidationCtx::new();

    // Seed declared device aliases for ERROR-level checks inside
    // state conditions and context-switch actions (ADR-025 §2.4).
    // ADR-035: device references resolve against the unified `[[endpoints]]`
    // set — the only authored I/O source.
    ctx.device_aliases = config.endpoints.iter().map(|e| e.alias.clone()).collect();
    ctx.device_protocols = config
        .endpoints
        .iter()
        .map(|e| {
            (
                e.alias.clone(),
                connector_proto_to_proto(e.effective_protocol()),
            )
        })
        .collect();
    ctx.endpoint_dir_enabled = config
        .endpoints
        .iter()
        .map(|e| (e.alias.clone(), (e.direction, e.enabled)))
        .collect();

    // ADR-027 D3 §3.2: seed the interpreter
    // policy so the Shell validation arm can emit warnings/errors per
    // user preference. Defaults to Warn; existing configs without the
    // field set get the default via serde.
    ctx.allow_interpreters = config.advanced_settings.allow_interpreters;

    // ── Structural checks (from former loader.rs) ────────────
    validate_structure(config, &mut ctx);

    // ── Per-app mode auto-switching (ADR-040 D3/D5) ──────────
    validate_per_app_modes(config, &mut ctx);

    // ── Conditional+ModeIs deprecation (ADR-040 §4.4 / §D6 Phase 1) ──
    validate_conditional_modeis_deprecation(config, &mut ctx);

    // ── LED config validation ──
    validate_led_config(config, &mut ctx);
    validate_midi_led_config(config, &mut ctx);
    validate_hid_led_config(config, &mut ctx);
    validate_velocity_color_map(config, &mut ctx);

    // ── Cross-field validation (NEW) ─────────────────────────
    validate_cross_references(config, &mut ctx);

    // ── Specificity duplicates (ADR-037 D2) ──────────────────
    validate_trigger_duplicates(config, &mut ctx);

    // ── Unified endpoints (ADR-035) ──────────────────
    // Endpoint channel-scope + protocol validation lives in
    // `validate_endpoints`.
    validate_endpoints(config, &mut ctx);

    // ── Dispatch-trace ring buffer bounds (spec §10 Open Item #3) ──
    // 0 would make the ring unable to retain any trace; values above
    // MAX_TRACE_BUFFER_SIZE (~500 MB) are a clear misconfiguration.
    {
        let n = config.advanced_settings.trace_buffer_size;
        if n == 0 {
            ctx.error(
                "advanced_settings.trace_buffer_size",
                "trace_buffer_size must be at least 1 (0 disables retention entirely)".to_string(),
            );
        } else if n > crate::config::types::MAX_TRACE_BUFFER_SIZE {
            ctx.error(
                "advanced_settings.trace_buffer_size",
                format!(
                    "trace_buffer_size {} exceeds the maximum of {} (~500 MB) — pick a smaller buffer",
                    n,
                    crate::config::types::MAX_TRACE_BUFFER_SIZE
                ),
            );
        }
    }

    // ── Per-mapping validation (merged from both layers) ─────
    // ADR-035: endpoint aliases are the only authored I/O source.
    let device_aliases: HashSet<&String> = config.endpoints.iter().map(|e| &e.alias).collect();
    // ADR-038 §4.3.1: alias → declared protocol, for the HID let-through error.
    // An endpoint's effective protocol surfaces HID-only sources so
    // `trigger_is_exclusively_hid` still rejects `let_through = true` on a
    // HID mapping. Uses the shared `connector_proto_to_proto`.
    let device_protocols: HashMap<&str, crate::config::protocol::Protocol> = config
        .endpoints
        .iter()
        .map(|e| {
            (
                e.alias.as_str(),
                connector_proto_to_proto(e.effective_protocol()),
            )
        })
        .collect();

    for (map_idx, mapping) in config.global_mappings.iter().enumerate() {
        let path = format!("global_mappings[{}]", map_idx);
        validate_mapping(mapping, &path, &device_aliases, &device_protocols, &mut ctx);
    }
    for (mode_idx, mode) in config.modes.iter().enumerate() {
        let mode_path = format!("modes[{}]", mode_idx);
        for (map_idx, mapping) in mode.mappings.iter().enumerate() {
            let path = format!("{}.mappings[{}]", mode_path, map_idx);
            validate_mapping(mapping, &path, &device_aliases, &device_protocols, &mut ctx);
        }
        // Detect mappings shadowed by an earlier same-mode mapping.
        warn_shadowed_mappings(
            &mode.mappings,
            &format!("Mode '{}'", mode.name),
            &mode_path,
            &mut ctx,
        );
    }

    // ── ADR-047 §D3a: frozen legacy sentinel id 255 ──────────
    // A `GamepadButton`/`GamepadButtonChord` bound on id 255 is the old
    // unknown-control collision sink and is permanently invalid. Warn loudly;
    // `Config::load` disables the bind (it never matches). Not silently migrated.
    validate_gamepad_legacy_sentinel(config, &mut ctx);

    // ── Build coverage metrics (from former validator.rs) ────
    ctx.midi_features.sort();
    ctx.midi_features.dedup();
    ctx.hid_features.sort();
    ctx.hid_features.dedup();
    ctx.osc_features.sort();
    ctx.osc_features.dedup();

    let midi_available = vec![
        "Note".to_string(),
        "VelocityRange".to_string(),
        "LongPress".to_string(),
        "DoubleTap".to_string(),
        "NoteChord".to_string(),
        "EncoderTurn".to_string(),
        "CC".to_string(),
        "Aftertouch".to_string(),
        "PitchBend".to_string(),
        "SendMIDI".to_string(),
        "MidiForward".to_string(),
    ];
    let hid_available = vec![
        "GamepadButton".to_string(),
        "GamepadButtonChord".to_string(),
        "GamepadAnalogStick".to_string(),
        "GamepadTrigger".to_string(),
        // ADR-039-B: HidForward is pushed to `hid_features` by
        // validate_action, so it must appear here too or HID coverage can
        // exceed 100%.
        "HidForward".to_string(),
    ];
    let osc_available = vec!["OscSend".to_string()];

    fn pct(used: usize, total: usize) -> f64 {
        if total == 0 {
            0.0
        } else {
            (used as f64 / total as f64) * 100.0
        }
    }

    // Sort findings by path for deterministic output (avoids HashMap iteration order issues)
    let mut errors = ctx.errors;
    let mut warnings = ctx.warnings;
    errors.sort_by(|a, b| a.path.cmp(&b.path));
    warnings.sort_by(|a, b| a.path.cmp(&b.path));

    ValidationReport {
        errors,
        warnings,
        coverage: ProtocolCoverage {
            midi: CoverageMetric {
                percentage: pct(ctx.midi_features.len(), midi_available.len()),
                used: ctx.midi_features,
                available: midi_available,
            },
            hid: CoverageMetric {
                percentage: pct(ctx.hid_features.len(), hid_available.len()),
                used: ctx.hid_features,
                available: hid_available,
            },
            osc: CoverageMetric {
                percentage: pct(ctx.osc_features.len(), osc_available.len()),
                used: ctx.osc_features,
                available: osc_available,
            },
        },
    }
}

/// Thin adapter for `Config::load()` / `Config::save()`.
///
/// Runs full validation and converts any errors into a `ConfigError`.
pub fn validate_for_loading(config: &Config) -> Result<(), ConfigError> {
    let report = validate_config(config);
    // Surface warnings even on successful validation
    for w in &report.warnings {
        tracing::warn!("Config warning: {}: {}", w.path, w.message);
    }
    if !report.is_valid() {
        return Err(ConfigError::ValidationError(report.format_errors()));
    }
    Ok(())
}

// ────────────────────────────────────────────────────────────────
// ADR-047 §D3a — frozen legacy gamepad sentinel (id 255)
// ────────────────────────────────────────────────────────────────

/// Permanently-invalid gamepad control id. Before ADR-047 §D3a every unmapped
/// gilrs control aliased onto this single id, so distinct controls collided.
/// It is now a frozen sentinel: a `GamepadButton`/`GamepadButtonChord` bind on
/// 255 is reported and disabled at load (never silently migrated).
pub const LEGACY_GAMEPAD_SENTINEL: u8 = 255;

/// True when a trigger binds the frozen legacy sentinel id 255 — either a
/// `GamepadButton { button: 255 }` or a `GamepadButtonChord` whose `buttons`
/// contains 255. Shared by validation (warn) and `Config::load` (disable).
pub fn trigger_binds_legacy_gamepad_sentinel(trigger: &Trigger) -> bool {
    match trigger {
        Trigger::GamepadButton { button, .. } => *button == LEGACY_GAMEPAD_SENTINEL,
        Trigger::GamepadButtonChord { buttons, .. } => buttons.contains(&LEGACY_GAMEPAD_SENTINEL),
        _ => false,
    }
}

/// Disable (drop) every mapping whose trigger binds the frozen legacy sentinel
/// id 255, across global and per-mode scopes. Returns the number of mappings
/// removed. Called by `Config::load` after validation has warned — the bind is
/// permanently invalid (ADR-047 §D3a), so we drop the whole mapping rather than
/// silently rewrite it. A chord that merely *includes* 255 is dropped wholesale
/// (re-binding the 255 element would change the chord's meaning unannounced).
pub fn disable_legacy_gamepad_sentinel_binds(config: &mut Config) -> usize {
    let mut removed = 0;
    let before = config.global_mappings.len();
    config
        .global_mappings
        .retain(|m| !trigger_binds_legacy_gamepad_sentinel(&m.trigger));
    removed += before - config.global_mappings.len();
    for mode in &mut config.modes {
        let before = mode.mappings.len();
        mode.mappings
            .retain(|m| !trigger_binds_legacy_gamepad_sentinel(&m.trigger));
        removed += before - mode.mappings.len();
    }
    removed
}

/// Warn on any gamepad bind referencing the frozen legacy sentinel id 255.
fn validate_gamepad_legacy_sentinel(config: &Config, ctx: &mut ValidationCtx) {
    let mut check = |trigger: &Trigger, path: &str| {
        if trigger_binds_legacy_gamepad_sentinel(trigger) {
            ctx.warning(
                path,
                format!(
                    "Gamepad bind references id {LEGACY_GAMEPAD_SENTINEL}, the frozen legacy \
                     'unknown control' sentinel (ADR-047 §D3a). It no longer maps to any physical \
                     control and is DISABLED on load — this mapping will never fire. Re-bind to the \
                     control's real id (run `gamepad_diagnostic` to discover it). Not auto-migrated."
                ),
            );
        }
    };
    for (i, m) in config.global_mappings.iter().enumerate() {
        check(&m.trigger, &format!("global_mappings[{i}]"));
    }
    for (mode_idx, mode) in config.modes.iter().enumerate() {
        for (i, m) in mode.mappings.iter().enumerate() {
            check(&m.trigger, &format!("modes[{mode_idx}].mappings[{i}]"));
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Structural validation (from former loader.rs Config::validate())
// ────────────────────────────────────────────────────────────────

mod action;
mod led;
mod mapping;
mod routes;
mod security;
mod structure;
mod trigger;

use action::*;
use led::*;
use mapping::*;
use routes::*;
use security::*;
use structure::*;
use trigger::*;

/// Maximum recursion depth for nested action validation (prevents stack overflow
/// from deeply nested Sequence/Conditional/Repeat in user-controlled configs).
const MAX_ACTION_DEPTH: usize = 64;

#[cfg(test)]
mod tests;
