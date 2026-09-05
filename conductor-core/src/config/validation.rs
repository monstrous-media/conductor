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

fn validate_led_config(config: &Config, ctx: &mut ValidationCtx) {
    if let Some(ref led) = config.led {
        if led.brightness > 127 {
            ctx.error(
                "led.brightness",
                format!("LED brightness must be 0-127, got {}", led.brightness),
            );
        }

        let valid_schemes = crate::feedback::LightingScheme::list_all();
        if led.scheme.is_empty() {
            ctx.warning(
                "led.scheme",
                "LED scheme is empty; defaults to 'reactive'".to_string(),
            );
        } else if !valid_schemes.contains(&led.scheme.as_str()) {
            ctx.error(
                "led.scheme",
                format!(
                    "Unknown LED scheme '{}'. Valid schemes: {}",
                    led.scheme,
                    valid_schemes.join(", ")
                ),
            );
        }

        let mode_names: std::collections::HashSet<&str> =
            config.modes.iter().map(|m| m.name.as_str()).collect();
        for mode_name in led.mode_colors.keys() {
            if !mode_names.contains(mode_name.as_str()) {
                let suggestion = mode_names
                    .iter()
                    .find(|name| name.eq_ignore_ascii_case(mode_name))
                    .map(|name| format!(" (did you mean '{}'?)", name));
                ctx.error(
                    "led.mode_colors",
                    format!(
                        "LED mode_colors references non-existent mode '{}'{}",
                        mode_name,
                        suggestion.unwrap_or_default()
                    ),
                );
            }
        }
    }
}

fn validate_midi_led_config(config: &Config, ctx: &mut ValidationCtx) {
    let midi_cfg = match config.led.as_ref().and_then(|l| l.midi.as_ref()) {
        Some(c) => c,
        None => return,
    };

    if midi_cfg.channel == 0 || midi_cfg.channel > 16 {
        ctx.error(
            "led.midi.channel",
            format!("MIDI LED channel must be 1-16, got {}", midi_cfg.channel),
        );
    }

    if midi_cfg.note_on_velocity > 127 {
        ctx.error(
            "led.midi.note_on_velocity",
            format!(
                "note_on_velocity must be 0-127, got {}",
                midi_cfg.note_on_velocity
            ),
        );
    }

    if midi_cfg.note_off_velocity > 127 {
        ctx.error(
            "led.midi.note_off_velocity",
            format!(
                "note_off_velocity must be 0-127, got {}",
                midi_cfg.note_off_velocity
            ),
        );
    }

    // Validate color velocities are in MIDI range
    let colors = &midi_cfg.colors;
    for (name, val) in [
        ("red", colors.red),
        ("green", colors.green),
        ("blue", colors.blue),
        ("yellow", colors.yellow),
        ("amber", colors.amber),
        ("off", colors.off),
    ] {
        if val > 127 {
            ctx.error(
                format!("led.midi.colors.{}", name),
                format!("Color velocity must be 0-127, got {}", val),
            );
        }
    }

    // Validate custom mappings
    for (i, mapping) in midi_cfg.custom_mappings.iter().enumerate() {
        let path = format!("led.midi.custom_mappings[{}]", i);
        if mapping.pad > 127 {
            ctx.error(
                &path,
                format!("Pad number must be 0-127, got {}", mapping.pad),
            );
        }
        validate_midi_led_message(&mapping.led_on, &format!("{}.led_on", path), ctx);
        validate_midi_led_message(&mapping.led_off, &format!("{}.led_off", path), ctx);
    }

    // Check for duplicate pad numbers in custom_mappings
    let mut seen_pads = std::collections::HashSet::new();
    for (i, mapping) in midi_cfg.custom_mappings.iter().enumerate() {
        if !seen_pads.insert(mapping.pad) {
            ctx.error(
                format!("led.midi.custom_mappings[{}]", i),
                format!("Duplicate custom mapping for pad {}", mapping.pad),
            );
        }
    }
}

fn validate_midi_led_message(
    msg: &crate::config::types::MidiLedMessage,
    path: &str,
    ctx: &mut ValidationCtx,
) {
    use crate::config::types::MidiLedMessage;
    match msg {
        MidiLedMessage::NoteOn { note, velocity } | MidiLedMessage::NoteOff { note, velocity } => {
            if *note > 127 {
                ctx.error(path, format!("Note must be 0-127, got {}", note));
            }
            if *velocity > 127 {
                ctx.error(path, format!("Velocity must be 0-127, got {}", velocity));
            }
        }
        MidiLedMessage::Cc { cc, value } => {
            if *cc > 127 {
                ctx.error(path, format!("CC must be 0-127, got {}", cc));
            }
            if *value > 127 {
                ctx.error(path, format!("Value must be 0-127, got {}", value));
            }
        }
    }
}

fn validate_hid_led_config(config: &Config, ctx: &mut ValidationCtx) {
    let hid_cfg = match config.led.as_ref().and_then(|l| l.hid.as_ref()) {
        Some(c) => c,
        None => return,
    };

    // Check profile name is valid first
    if let Some(ref profile_name) = hid_cfg.hid_profile
        && crate::config::types::HidLedConfig::from_profile(profile_name).is_none()
    {
        ctx.error(
            "led.hid.hid_profile",
            format!(
                "Unknown HID device profile '{}'. Available profiles: mikro-mk3",
                profile_name
            ),
        );
        return; // Can't validate further
    }

    // Validate against the *resolved* config (profile merged in)
    match hid_cfg.resolve_profile() {
        Ok(resolved) => {
            if resolved.vendor_id == 0 {
                ctx.error("led.hid.vendor_id", "vendor_id must be non-zero");
            }
            if resolved.product_id == 0 {
                ctx.error("led.hid.product_id", "product_id must be non-zero");
            }
            if resolved.pad_count == 0 {
                ctx.error("led.hid.pad_count", "pad_count must be > 0");
            }
            // encode_pad_indexed packs color_index into 6 bits, so max 64 palette entries
            if resolved.color_palette.len() > 64 {
                ctx.error(
                    "led.hid.color_palette",
                    format!(
                        "color_palette has {} entries but HID LED encoding format supports at \
                         most 64 (6-bit color index)",
                        resolved.color_palette.len()
                    ),
                );
            }
        }
        Err(e) => {
            ctx.error("led.hid", e);
        }
    }
}

fn validate_velocity_color_map(config: &Config, ctx: &mut ValidationCtx) {
    let vcm = match config.led.as_ref().and_then(|l| l.velocity_colors.as_ref()) {
        Some(c) => c,
        None => return,
    };

    if vcm.ranges.is_empty() {
        ctx.error(
            "led.velocity_colors.ranges",
            "Velocity color map must have at least one range",
        );
        return;
    }

    for (i, range) in vcm.ranges.iter().enumerate() {
        let path = format!("led.velocity_colors.ranges[{}]", i);
        if range.min > 127 {
            ctx.error(&path, format!("min must be 0-127, got {}", range.min));
        }
        if range.max > 127 {
            ctx.error(&path, format!("max must be 0-127, got {}", range.max));
        }
        if range.min > range.max {
            ctx.error(
                &path,
                format!("min ({}) > max ({}) — invalid range", range.min, range.max),
            );
        }
    }

    // Overlap and gap detection on sorted ranges
    let mut sorted: Vec<_> = vcm.ranges.iter().collect();
    sorted.sort_by_key(|r| r.min);

    for window in sorted.windows(2) {
        let prev = window[0];
        let next = window[1];

        if prev.max >= next.min {
            ctx.error(
                "led.velocity_colors.ranges",
                format!(
                    "Overlapping ranges: [{}-{}] and [{}-{}]",
                    prev.min, prev.max, next.min, next.max
                ),
            );
        } else if (prev.max as u16) + 1 < next.min as u16 {
            ctx.warning(
                "led.velocity_colors.ranges",
                format!(
                    "Gap in velocity coverage: {}-{} is unmapped",
                    (prev.max as u16) + 1,
                    (next.min as u16) - 1
                ),
            );
        }
    }
}

fn validate_structure(config: &Config, ctx: &mut ValidationCtx) {
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
fn validate_trigger_duplicates(config: &Config, ctx: &mut ValidationCtx) {
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
fn validate_endpoints(config: &Config, ctx: &mut ValidationCtx) {
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
fn validate_network_listener(
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
fn validate_per_app_modes(config: &Config, ctx: &mut ValidationCtx) {
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

fn validate_routes(config: &Config, ctx: &mut ValidationCtx) {
    // Routes resolve against the unified `[[endpoints]]` set (ADR-035).
    let endpoint_aliases: HashSet<&str> =
        config.endpoints.iter().map(|e| e.alias.as_str()).collect();

    // Build an `alias → Protocol` map for cross-protocol detection.
    // Connector protocols map 1:1 onto binding-side Protocol (same 4
    // variants — Midi/Hid/Osc/ArtNet).
    use crate::config::protocol::Protocol;
    use crate::config::types::ConnectorProtocol;
    let protocol_for: std::collections::HashMap<&str, Protocol> = config
        .endpoints
        .iter()
        .map(|e| {
            (
                e.alias.as_str(),
                connector_proto_to_proto(e.effective_protocol()),
            )
        })
        .collect();

    // ADR-036 D1: set of declared mode names, for validating
    // each route's `modes` scope references something real.
    let mode_names: HashSet<&str> = config.modes.iter().map(|m| m.name.as_str()).collect();

    // Track forward edges so we can detect A→B + B→A direct cycles.
    let mut forward_edges: HashSet<(&str, &str)> = HashSet::new();

    for (idx, route) in config.routes.iter().enumerate() {
        let path = format!("routes[{}]", idx);

        // Rule 6 (ADR-036 D1): every name in `route.modes` must reference
        // a declared [[modes]] block. A typo or stale name yields a route
        // that can never become active — fail loudly at load.
        for (m_idx, mode_name) in route.modes.iter().enumerate() {
            if !mode_names.contains(mode_name.as_str()) {
                ctx.error(
                    format!("{}.modes[{}]", path, m_idx),
                    format!(
                        "Route mode scope references unknown mode '{}' — must match a declared \
                         [[modes]] block (ADR-036 D1). The route would never fire.",
                        mode_name
                    ),
                );
            }
        }

        // (ADR-036 Phase 3) The route `phase` field was removed — all routes
        // are post-mapping. A lingering `phase = "..."` is rejected at config
        // load (`Config::check_removed_route_phase`), so there is nothing to
        // validate here.

        // Rule 1a: `from` references a known endpoint
        let from_known = endpoint_aliases.contains(route.from.as_str());
        if !from_known {
            ctx.error(
                format!("{}.from", path),
                format!(
                    "Route 'from' references unknown alias '{}' — must match an [[endpoints]] \
                     entry (ADR-035).",
                    route.from
                ),
            );
        }

        // Rule 1b: `to` references a known endpoint
        let to_known = endpoint_aliases.contains(route.to.as_str());
        if !to_known {
            ctx.error(
                format!("{}.to", path),
                format!(
                    "Route 'to' references unknown alias '{}' — must match an [[endpoints]] \
                     entry (ADR-035).",
                    route.to
                ),
            );
        }

        // Rule 2: self-reference
        if route.from == route.to {
            ctx.error(
                path.clone(),
                format!(
                    "Route from '{0}' to '{0}' is a self-loop — pick distinct endpoints \
                     (ADR-031 § 4.3).",
                    route.from
                ),
            );
        }

        // Rule 3: A→B + B→A direct (depth-1) cycle.
        //
        // SCOPE: per ADR-031 spec § 4.3, only direct 2-cycles are
        // detected at config load. Multi-hop cycles (A→B→C→A) are
        // intentionally OUT OF SCOPE for Phase 2A — they'd need a
        // graph walk + cycle search, which has its own cost/complexity
        // trade-offs and would need a separate spec section. The
        // runtime route engine (Phase 2B § 4.5) is the second line of
        // defence: it can break cycles via per-event recursion guards
        // (mirrors `MidiRecursionGuard` from ADR-015 D8). Multi-hop
        // static detection is tracked as a future hardening item.
        //
        // Gate on both endpoints being known + non-self-ref so we don't
        // cascade an extra "cycle" error onto an already-broken route.
        if from_known && to_known && route.from != route.to {
            if forward_edges.contains(&(route.to.as_str(), route.from.as_str())) {
                ctx.error(
                    path.clone(),
                    format!(
                        "Route '{}' → '{}' forms a direct cycle with an earlier '{}' → '{}' route — \
                         would feedback-loop on every event (ADR-031 § 4.3).",
                        route.from, route.to, route.to, route.from
                    ),
                );
            }
            forward_edges.insert((route.from.as_str(), route.to.as_str()));
        }

        // Rule 4: cross-protocol transform compatibility.
        //
        // Three cases per `ExpectedTransform`:
        //   - SameProtocol: no requirement; any transform or None is fine.
        //   - Required(variant): cross-protocol pair with a defined
        //     SignalTransform variant. Route MUST declare that exact
        //     variant; missing or mismatched is an error.
        //   - Unsupported: cross-protocol pair with NO defined variant
        //     in ADR-031 (e.g. HID→OSC, ArtNet→MIDI). Route is rejected
        //     regardless of transform value.
        //
        // (Historically, a prior `Some("MidiToOsc")` fallback let HID→OSC
        // routes silently validate; that gap is now closed.)
        if from_known && to_known {
            let from_proto = protocol_for.get(route.from.as_str()).copied();
            let to_proto = protocol_for.get(route.to.as_str()).copied();
            if let (Some(fp), Some(tp)) = (from_proto, to_proto) {
                match expected_transform_variant(fp, tp) {
                    ExpectedTransform::SameProtocol => {
                        // No transform required; nothing to check here.
                    }
                    ExpectedTransform::Unsupported => {
                        ctx.error(
                            path.clone(),
                            format!(
                                "Route '{}' ({:?}) → '{}' ({:?}): unsupported protocol pair — \
                                 no matching SignalTransform variant exists in ADR-031 for \
                                 this direction. Either re-route through an intermediate \
                                 protocol (e.g. HID→MIDI→OSC via two routes) or wait for \
                                 a future ADR to add the variant.",
                                route.from, fp, route.to, tp
                            ),
                        );
                    }
                    ExpectedTransform::Required(expected) => {
                        match &route.transform {
                            None => {
                                ctx.error(
                                    path.clone(),
                                    format!(
                                        "Cross-protocol route '{}' ({:?}) → '{}' ({:?}) must \
                                         declare `transform.type = \"{}\"` — without one the \
                                         payload bytes are forwarded raw to a wire that doesn't \
                                         speak that protocol (ADR-031 § 4.3).",
                                        route.from, fp, route.to, tp, expected
                                    ),
                                );
                            }
                            Some(t) if transform_variant_name(t) != expected => {
                                ctx.error(
                                    path.clone(),
                                    format!(
                                        "Route '{}' ({:?}) → '{}' ({:?}) declares \
                                         `transform.type = \"{}\"` but the (from_protocol, \
                                         to_protocol) pair requires '{}'. A wrong transform \
                                         variant is a runtime no-op for the protocol gap — \
                                         payloads still hit the wrong wire (ADR-031 § 4.3).",
                                        route.from,
                                        fp,
                                        route.to,
                                        tp,
                                        transform_variant_name(t),
                                        expected
                                    ),
                                );
                            }
                            Some(t) => {
                                // Variant matches expected — validate its
                                // value ranges so out-of-range config is
                                // REJECTED at load, not silently masked at
                                // runtime. Mirrors the
                                // existing CC/channel range checks elsewhere.
                                if let crate::config::types::SignalTransform::HidToMidi {
                                    trigger_to_cc,
                                    channel,
                                } = t
                                {
                                    if *channel > 15 {
                                        ctx.error(
                                            path.clone(),
                                            format!(
                                                "HidToMidi channel {} out of range (must be \
                                                 0-15) on route '{}' → '{}'",
                                                channel, route.from, route.to
                                            ),
                                        );
                                    }
                                    for (trigger, cc) in trigger_to_cc {
                                        if *cc > 127 {
                                            ctx.error(
                                                path.clone(),
                                                format!(
                                                    "HidToMidi CC {} for trigger '{}' out of \
                                                     range (must be 0-127) on route '{}' → '{}'",
                                                    cc, trigger, route.from, route.to
                                                ),
                                            );
                                        }
                                    }
                                }
                                // HidToOsc: OSC addresses must
                                // start with '/' — reject malformed ones at
                                // config-load rather than emit invalid packets.
                                if let crate::config::types::SignalTransform::HidToOsc {
                                    trigger_to_address,
                                    ..
                                } = t
                                {
                                    for (trigger, address) in trigger_to_address {
                                        if !address.starts_with('/') {
                                            ctx.error(
                                                path.clone(),
                                                format!(
                                                    "HidToOsc address '{}' for trigger '{}' is \
                                                     invalid (OSC addresses must start with '/') \
                                                     on route '{}' → '{}'",
                                                    address, trigger, route.from, route.to
                                                ),
                                            );
                                        }
                                    }
                                }
                                // OscToArtNet (ADR-039-A): the
                                // address template must be a valid OSC address
                                // (starts with '/') carrying exactly one
                                // `{dmx}` placeholder — without it the
                                // transform can never extract a channel and
                                // the route would silently never fire.
                                if let crate::config::types::SignalTransform::OscToArtNet {
                                    address_to_dmx,
                                } = t
                                    && (!address_to_dmx.starts_with('/')
                                        || address_to_dmx.matches("{dmx}").count() != 1)
                                {
                                    ctx.error(
                                        path.clone(),
                                        format!(
                                            "OscToArtNet address_to_dmx '{}' is invalid \
                                             (must start with '/' and contain exactly one \
                                             '{{dmx}}' placeholder) on route '{}' → '{}'",
                                            address_to_dmx, route.from, route.to
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }

                // Rule 4b (ADR-039-B §6.2.1): byte-filters on
                // HID-source routes are rejected. A HID event serializes to
                // MIDI bytes lossily (gamepad button 128 → `pad & 0x7F` = MIDI
                // note 0), so a `channels`/`cc_range`/`note_range`/
                // `message_types` filter evaluated against that serialization
                // fires on non-deterministic ghost triggers. V1 allows only
                // catch-all (no-filter) HID routes; structured HID filters are
                // deferred (to be designed with OSC-input routing).
                if fp == crate::config::protocol::Protocol::Hid
                    && route.filter.as_ref().is_some_and(signal_filter_is_active)
                {
                    ctx.error(
                        format!("{}.filter", path),
                        format!(
                            "Route '{}' ({:?}) → '{}' has a byte-filter, but HID-source routes \
                             must be catch-all (no filter): a gamepad event serializes to MIDI \
                             bytes lossily (button 128 → note 0), so the filter would match \
                             non-deterministic ghost triggers (ADR-039-B §6.2.1). Remove the \
                             `[routes.filter]` block; structured HID filters are deferred.",
                            route.from, fp, route.to
                        ),
                    );
                }

                // ADR-039-A: OSC-source routes must currently be catch-all.
                // MIDI byte-filters are meaningless for OSC (there are
                // no MIDI bytes); OSC-address filtering arrives with typed
                // triggers separately. Reject any active filter on an OSC source.
                if fp == crate::config::protocol::Protocol::Osc
                    && route.filter.as_ref().is_some_and(signal_filter_is_active)
                {
                    ctx.error(
                        format!("{}.filter", path),
                        format!(
                            "Route '{}' (Osc) → '{}' has a filter, but OSC-source routes must be \
                             catch-all (no filter) in Slice 1: OSC carries no MIDI bytes, so a \
                             MIDI/HID byte-filter cannot apply. OSC-address filtering arrives with \
                             typed triggers (ADR-039-A Slice 2). Remove the `[routes.filter]` block.",
                            route.from, route.to
                        ),
                    );
                }

                // ADR-039-A D8: cross-protocol feedback-loop guard. An
                // OSC-source route whose MIDI output is ALSO a Conductor MIDI
                // input forms a system-level loop (OSC → OscToMidi → MIDI out →
                // OS/virtual loopback → MIDI in → mapping engine → Action) that
                // the route-only D17 argument cannot otherwise see — the
                // re-entrant bytes carry no OSC provenance, so taint-tracking
                // would never catch them. Fail closed at config-load. Detectable
                // in-app cases: a Bidirectional MIDI target, or a distinct MIDI
                // input endpoint with identical matchers. (Partial-overlap and
                // cross-application loopback are an accepted, documented Phase-A
                // residual — ADR-042.)
                if fp == crate::config::protocol::Protocol::Osc
                    && tp == crate::config::protocol::Protocol::Midi
                    && let Some(to_ep) = config.endpoints.iter().find(|e| e.alias == route.to)
                    && midi_output_is_self_ingested(config, to_ep)
                {
                    ctx.error(
                        format!("{}.to", path),
                        format!(
                            "Route '{}' (Osc) → '{}' targets a MIDI output that Conductor also \
                             ingests as a MIDI input, forming an OSC→MIDI→input feedback loop that \
                             could reach actions (ADR-039-A D8 / ADR-042 D17). Point the route at a \
                             MIDI output Conductor does not also listen on, or split the device into \
                             distinct in/out endpoints.",
                            route.from, route.to
                        ),
                    );
                }
            }
        }

        // Rule 5: cc_range / note_range min > max
        if let Some(filter) = &route.filter {
            if let Some((min, max)) = filter.cc_range
                && min > max
            {
                ctx.error(
                    format!("{}.filter.cc_range", path),
                    format!(
                        "Route filter `cc_range = [{}, {}]` has min > max — would silently \
                         match nothing (per spec § 4.1, same diagnose-class).",
                        min, max
                    ),
                );
            }
            if let Some((min, max)) = filter.note_range
                && min > max
            {
                ctx.error(
                    format!("{}.filter.note_range", path),
                    format!(
                        "Route filter `note_range = [{}, {}]` has min > max — would silently \
                         match nothing (per spec § 4.1, same diagnose-class).",
                        min, max
                    ),
                );
            }

            // Rule 5b: channel values must be 0-15 (parity with
            // `devices[*].channels` per ADR-022).
            for &ch in &filter.channels {
                if ch > 15 {
                    ctx.error(
                        format!("{}.filter.channels", path),
                        format!("Route filter channel {} is out of range (must be 0-15)", ch),
                    );
                }
            }

            // Rule 5c: reject SysEx / ChannelPressure in `message_types`
            // — the input pipeline doesn't emit them yet, so a route with
            // such a filter would silently never match. Mirrors the Raw
            // trigger validator's identical check.
            for mt in &filter.message_types {
                if matches!(
                    mt,
                    crate::config::MidiMessageType::SysEx
                        | crate::config::MidiMessageType::ChannelPressure
                ) {
                    ctx.error(
                        format!("{}.filter.message_types", path),
                        format!(
                            "Route filter message_type '{:?}' is not supported by the current \
                             event pipeline. SysEx and ChannelPressure are reserved for a future \
                             ADR-030 phase; remove them or use NoteOn/NoteOff/CC/ProgramChange/\
                             Aftertouch/PitchBend.",
                            mt
                        ),
                    );
                }
            }
        }
    }

    // ── ADR-031 § 4.3 Phase 2A — overlap warnings (non-fatal) ──
    //
    // Two classes (ADR-031 § 4.3 Phase 2A):
    //   (b) Route shadowed by a specific trigger (Note/CC/...) on the
    //       route's source device — the trigger fires first; the route
    //       only sees what the trigger doesn't intercept.
    //   (c) Exact-duplicate route — same `from`+`to` + same filter shape.
    //       Wasted CPU + same event sent twice.
    warn_route_overlaps(config, ctx);
}

fn warn_route_overlaps(config: &Config, ctx: &mut ValidationCtx) {
    use crate::config::types::Trigger;

    if config.routes.is_empty() {
        return;
    }

    // Triggers fire on input events from physical input endpoints. A route
    // whose source is an output-only endpoint (e.g. an OSC output, an
    // MCP-created output, a virtual MIDI port) doesn't emit trigger events,
    // so trigger-shadowing warnings against those routes are false positives.
    // Build the input-capable endpoint-alias set once and gate the scan on it.
    // (ADR-035: input = direction Input or Bidirectional.)
    use crate::config::types::ConnectorDirection;
    let binding_aliases: std::collections::HashSet<&str> = config
        .endpoints
        .iter()
        .filter(|e| {
            matches!(
                e.direction,
                ConnectorDirection::Input | ConnectorDirection::Bidirectional
            )
        })
        .map(|e| e.alias.as_str())
        .collect();

    // Collect every mapping (mode + global) so we can scan once per route.
    let mut all_triggers: Vec<&Trigger> =
        config.global_mappings.iter().map(|m| &m.trigger).collect();
    for mode in &config.modes {
        all_triggers.extend(mode.mappings.iter().map(|m| &m.trigger));
    }

    for (idx, route) in config.routes.iter().enumerate() {
        let path = format!("routes[{}]", idx);
        let from = route.from.as_str();
        let is_binding_source = binding_aliases.contains(from);

        // (a) + (b): scan triggers for source-device overlap, but only
        //     when the route source is a binding alias (triggers don't
        //     fire on connector sources). The route source `X` is
        //     shadowed by any trigger with `device = X` OR
        //     `device = None` (any-device).
        if is_binding_source {
            for trig in &all_triggers {
                let trig_device = trig.device();
                // Borrow-compare to avoid the per-trigger `to_string()`
                // allocation.
                let device_matches =
                    trig_device.is_none() || trig_device.map(|s| s.as_str()) == Some(from);
                if !device_matches {
                    continue;
                }
                ctx.warning(
                    path.clone(),
                    format!(
                        "Route source '{}' overlaps a specific trigger — the specific \
                         rule fires first; the route only sees events that don't match \
                         the trigger (ADR-031 §D11).",
                        from
                    ),
                );
                break;
            }
        }

        // (c): exact-duplicate route — compare against earlier routes.
        //
        // ADR-036 D1 refinement: two routes are duplicates only when their
        // mode scopes overlap. Disjoint mode scopes (e.g. ["Drums"] vs
        // ["Keys"]) never both fire for the same event, so they're
        // legitimate — not a duplicate. An empty scope means "all modes" and
        // overlaps anything. (Phase 3 removed the `phase` axis — all routes
        // are post-mapping.)
        for (prev_idx, prev) in config.routes.iter().enumerate().take(idx) {
            if route_shapes_equal(prev, route) && mode_scopes_overlap(&prev.modes, &route.modes) {
                ctx.warning(
                    path.clone(),
                    format!(
                        "Route is a duplicate of routes[{}] (same from/to/filter/transform, \
                         overlapping mode scope) — events would be forwarded twice. \
                         Either remove one, differentiate their filters, or narrow their modes.",
                        prev_idx
                    ),
                );
                break; // one warning per duplicate
            }
        }
    }
}

/// Routes are "equal in shape" when their `from`, `to`, filter, AND
/// transform all match. `description` is ignored (it's a label, not a
/// behavior). `enabled` is also ignored (toggle is a state change, not
/// a shape change).
///
/// Including transform in the comparison preserves the "different
/// transforms on the same source/dest is legitimate fan-out" intent:
/// two routes with same from/to/filter but DIFFERENT transforms are
/// correctly distinguished and don't false-warn as duplicates.
/// (Historically the implementation excluded transform, which inverted
/// the intended logic.)
/// Two route mode scopes "overlap" when at least one mode could be
/// active for both. An empty scope means "all modes" (legacy bare-route
/// behaviour), so it overlaps any other scope. Two non-empty scopes
/// overlap iff they share at least one mode name. (ADR-036 D1.)
fn mode_scopes_overlap(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    a.iter().any(|m| b.contains(m))
}

fn route_shapes_equal(
    a: &crate::config::types::RouteConfig,
    b: &crate::config::types::RouteConfig,
) -> bool {
    if a.from != b.from || a.to != b.to {
        return false;
    }
    // Compare filter + transform shape via JSON serialization
    // (cheap, exact — handles the nested `MidiTransform` and the
    // tagged `SignalTransform` enum uniformly).
    let af = serde_json::to_value(&a.filter).unwrap_or(serde_json::Value::Null);
    let bf = serde_json::to_value(&b.filter).unwrap_or(serde_json::Value::Null);
    if af != bf {
        return false;
    }
    let at = serde_json::to_value(&a.transform).unwrap_or(serde_json::Value::Null);
    let bt = serde_json::to_value(&b.transform).unwrap_or(serde_json::Value::Null);
    at == bt
}

/// Result of looking up the required `SignalTransform` variant for a
/// (from_protocol, to_protocol) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedTransform {
    /// Same protocol — no transform required (any transform or `None`
    /// is acceptable; the same-protocol pass-through is the canonical case).
    SameProtocol,
    /// Cross-protocol with a defined variant — the route must declare
    /// `transform.type = <variant>` exactly.
    Required(&'static str),
    /// Cross-protocol with NO defined variant in ADR-031 — the route
    /// should be rejected as unsupported regardless of transform.
    /// Returning a "best-guess" variant (the prior behavior) silently
    /// validated nonsense like HID→OSC + transform.type = "MidiToOsc".
    Unsupported,
}

/// Map a `ConnectorProtocol` (the endpoint's wire protocol) to the routing
/// `Protocol` vocabulary. The 4 variants map 1:1. Shared by endpoint-protocol
/// seeding, route validation, and the `HidForward` action validator so they
/// can't drift.
fn connector_proto_to_proto(
    p: crate::config::types::ConnectorProtocol,
) -> crate::config::protocol::Protocol {
    use crate::config::protocol::Protocol;
    use crate::config::types::ConnectorProtocol;
    match p {
        ConnectorProtocol::Midi => Protocol::Midi,
        ConnectorProtocol::Hid => Protocol::Hid,
        ConnectorProtocol::Osc => Protocol::Osc,
        ConnectorProtocol::ArtNet => Protocol::ArtNet,
    }
}

/// Whether a `SignalFilter` actually constrains anything. An all-empty
/// filter (`[routes.filter]` with no fields set) is equivalent to no filter
/// — a catch-all — so it does not trip the HID byte-filter ban (Rule 4b).
fn signal_filter_is_active(f: &crate::config::types::SignalFilter) -> bool {
    !f.message_types.is_empty()
        || !f.channels.is_empty()
        || f.cc_range.is_some()
        || f.note_range.is_some()
        || f.osc_address_prefix.is_some()
}

/// ADR-039-A D8: does this MIDI-output endpoint also get ingested as a
/// MIDI input by Conductor? Detects the two config-load-visible self-loop
/// shapes: a Bidirectional MIDI endpoint (both out and in), or a distinct
/// enabled MIDI input/bidirectional endpoint with an identical matcher set
/// (the same physical device wired in and out). Partial-overlap and
/// cross-application loopback are an accepted Phase-A residual.
fn midi_output_is_self_ingested(
    config: &Config,
    to_ep: &crate::config::types::EndpointConfig,
) -> bool {
    use crate::config::types::{ConnectorDirection, ConnectorProtocol, EndpointKind};
    if to_ep.effective_protocol() != ConnectorProtocol::Midi {
        return false;
    }
    // A bidirectional MIDI endpoint used as the route target is itself both the
    // output and an input — a guaranteed loop.
    if to_ep.direction == ConnectorDirection::Bidirectional {
        return true;
    }

    // Virtual MIDI port output: a `MidiVirtualPort`
    // has NO DeviceMatchers — it is matched by `port_name` — so the
    // matcher-signature path below would always miss it (empty signature). Yet a
    // Conductor-created virtual port is the *classic* loopback vector: a route
    // to it loops if any enabled MIDI input/bidir endpoint also targets that
    // port name (another virtual port with the same name, or a matcher that
    // names it). Check that explicitly, by port name.
    if let EndpointKind::MidiVirtualPort { port_name } = &to_ep.kind {
        return config.endpoints.iter().any(|e| {
            e.enabled
                && e.alias != to_ep.alias
                && e.effective_protocol() == ConnectorProtocol::Midi
                && matches!(
                    e.direction,
                    ConnectorDirection::Input | ConnectorDirection::Bidirectional
                )
                && endpoint_targets_port_name(e, port_name)
        });
    }

    let out_sig = endpoint_matcher_signature(to_ep, ConnectorDirection::Output);
    if out_sig.is_empty() {
        return false;
    }
    config.endpoints.iter().any(|e| {
        e.enabled
            && e.alias != to_ep.alias
            && e.effective_protocol() == ConnectorProtocol::Midi
            && matches!(
                e.direction,
                ConnectorDirection::Input | ConnectorDirection::Bidirectional
            )
            && endpoint_matcher_signature(e, ConnectorDirection::Input) == out_sig
    })
}

/// Whether endpoint `e` (an input/bidir MIDI endpoint) would bind a port called
/// `name` — i.e. it is a `MidiVirtualPort` of that exact name, or a `Matcher`
/// whose input-side matchers name it (`ExactName`, or a `NameContains`
/// substring). Used by the D8 virtual-port self-loop check.
fn endpoint_targets_port_name(e: &crate::config::types::EndpointConfig, name: &str) -> bool {
    use crate::config::types::{ConnectorDirection, EndpointKind};
    use crate::identity::DeviceMatcher;
    match &e.kind {
        EndpointKind::MidiVirtualPort { port_name } => port_name == name,
        EndpointKind::Matcher { .. } => e
            .kind
            .effective_matchers(ConnectorDirection::Input)
            .iter()
            .any(|m| match m {
                DeviceMatcher::ExactName { value } => value == name,
                DeviceMatcher::NameContains { value } => name.contains(value.as_str()),
                _ => false,
            }),
        _ => false,
    }
}

/// Sorted debug signatures of an endpoint's effective matchers for `dir`, so
/// two endpoints targeting the same device compare equal regardless of order.
fn endpoint_matcher_signature(
    ep: &crate::config::types::EndpointConfig,
    dir: crate::config::types::ConnectorDirection,
) -> Vec<String> {
    let mut sig: Vec<String> = ep
        .kind
        .effective_matchers(dir)
        .iter()
        .map(|m| format!("{m:?}"))
        .collect();
    sig.sort();
    sig
}

/// Look up the required `SignalTransform` variant name for a
/// (from_protocol, to_protocol) pair. See `ExpectedTransform`.
fn expected_transform_variant(
    from: crate::config::protocol::Protocol,
    to: crate::config::protocol::Protocol,
) -> ExpectedTransform {
    use crate::config::protocol::Protocol;
    if from == to {
        return ExpectedTransform::SameProtocol;
    }
    match (from, to) {
        (Protocol::Midi, Protocol::Osc) => ExpectedTransform::Required("MidiToOsc"),
        (Protocol::Osc, Protocol::Midi) => ExpectedTransform::Required("OscToMidi"),
        (Protocol::Midi, Protocol::ArtNet) => ExpectedTransform::Required("MidiToArtNet"),
        (Protocol::Hid, Protocol::ArtNet) => ExpectedTransform::Required("HidToArtNet"),
        (Protocol::Hid, Protocol::Midi) => ExpectedTransform::Required("HidToMidi"),
        (Protocol::Hid, Protocol::Osc) => ExpectedTransform::Required("HidToOsc"),
        (Protocol::Osc, Protocol::ArtNet) => ExpectedTransform::Required("OscToArtNet"),
        // Pairs without a defined SignalTransform variant (ArtNet→*, etc.)
        // are explicitly Unsupported. A future ADR/slice may
        // add variants; until then the validator must reject rather than guess.
        _ => ExpectedTransform::Unsupported,
    }
}

/// Tag name of a `SignalTransform` variant — used in error messages.
fn transform_variant_name(t: &crate::config::types::SignalTransform) -> &'static str {
    use crate::config::types::SignalTransform;
    match t {
        SignalTransform::Midi(_) => "Midi",
        SignalTransform::MidiToOsc { .. } => "MidiToOsc",
        SignalTransform::OscToMidi { .. } => "OscToMidi",
        SignalTransform::MidiToArtNet { .. } => "MidiToArtNet",
        SignalTransform::HidToArtNet { .. } => "HidToArtNet",
        SignalTransform::HidToMidi { .. } => "HidToMidi",
        SignalTransform::HidToOsc { .. } => "HidToOsc",
        SignalTransform::OscToArtNet { .. } => "OscToArtNet",
    }
}

// ────────────────────────────────────────────────────────────────
// Cross-field validation (NEW)
// ────────────────────────────────────────────────────────────────

fn validate_cross_references(config: &Config, ctx: &mut ValidationCtx) {
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

/// Maximum recursion depth for nested action validation (prevents stack overflow
/// from deeply nested Sequence/Conditional/Repeat in user-controlled configs).
const MAX_ACTION_DEPTH: usize = 64;

fn check_mode_references(
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
fn validate_conditional_modeis_deprecation(config: &Config, ctx: &mut ValidationCtx) {
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
fn warn_modeis_dispatch(action: &ActionConfig, path: &str, ctx: &mut ValidationCtx, depth: usize) {
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

fn validate_mapping(
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
fn validate_let_through(
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

fn validate_trigger(
    trigger: &Trigger,
    path: &str,
    device_aliases: &HashSet<&String>,
    ctx: &mut ValidationCtx,
) {
    // Device reference validation (from former loader.rs)
    //
    // The original wording was misleading — it said "mapping will only
    // match if this device connects", implying the issue would resolve when
    // the hardware appeared. It won't: the alias is a config-level identifier.
    // Even with the hardware connected, without a matching `[[endpoints]]`
    // entry the daemon assigns the port a generated DeviceId (port name with a
    // `#N` instance suffix when duplicates appear, via
    // `DeviceId::from_port_instance` in `input_manager`), not the trigger's
    // alias. The trigger never matches until either an `[[endpoints]]` entry
    // with this alias exists or the `device` filter is removed.
    if let Some(ref_alias) = trigger.device()
        && !device_aliases.contains(ref_alias)
    {
        ctx.warning(
            path,
            format!(
                "Trigger references device alias '{0}', but no [[endpoints]] entry \
                 defines this alias. This mapping will never match until you \
                 either (a) add an [[endpoints]] entry with alias = \"{0}\" or \
                 (b) remove the `device` filter from the trigger.",
                ref_alias
            ),
        );
    }

    // Validate MIDI channel range (0-indexed: 0-15, displayed as 1-16)
    if let Some(ch) = trigger.channel()
        && ch > 15
    {
        ctx.error(
            path,
            format!("MIDI channel out of range: {} (must be 0-15)", ch),
        );
    }

    match trigger {
        Trigger::Note { note, .. } => {
            ctx.midi_features.push("Note".to_string());
            if *note > 127 {
                ctx.error(
                    path,
                    format!("Note number out of range: {} (must be 0-127)", note),
                );
            }
        }
        Trigger::VelocityRange {
            note,
            soft_max,
            medium_max,
            ..
        } => {
            ctx.midi_features.push("VelocityRange".to_string());
            if *note > 127 {
                ctx.error(
                    path,
                    format!("Note number out of range: {} (must be 0-127)", note),
                );
            }
            // Velocity zone overlap warning (from former validator.rs)
            if let (Some(soft), Some(med)) = (soft_max, medium_max)
                && soft >= med
            {
                ctx.warning(
                    path,
                    format!(
                        "soft_max ({}) >= medium_max ({}) — velocity zones overlap",
                        soft, med
                    ),
                );
            }
        }
        Trigger::LongPress { note, .. } => {
            ctx.midi_features.push("LongPress".to_string());
            if *note > 127 {
                ctx.error(
                    path,
                    format!("Note number out of range: {} (must be 0-127)", note),
                );
            }
        }
        Trigger::DoubleTap { note, .. } => {
            ctx.midi_features.push("DoubleTap".to_string());
            if *note > 127 {
                ctx.error(
                    path,
                    format!("Note number out of range: {} (must be 0-127)", note),
                );
            }
        }
        Trigger::NoteChord { notes, .. } => {
            ctx.midi_features.push("NoteChord".to_string());
            for (i, note) in notes.iter().enumerate() {
                if *note > 127 {
                    ctx.error(
                        format!("{}.notes[{}]", path, i),
                        format!("Note number out of range: {} (must be 0-127)", note),
                    );
                }
            }
            if notes.is_empty() {
                ctx.error(path, "NoteChord must have at least one note");
            }
            // Chord size warning (from former validator.rs)
            if notes.len() == 1 {
                ctx.warning(
                    path,
                    "NoteChord with fewer than 2 notes — use Note trigger instead",
                );
            }
        }
        Trigger::EncoderTurn { cc, direction, .. } => {
            ctx.midi_features.push("EncoderTurn".to_string());
            if *cc > 127 {
                ctx.error(
                    path,
                    format!("CC number out of range: {} (must be 0-127)", cc),
                );
            }
            if let Some(dir) = direction
                && dir != "Clockwise"
                && dir != "CounterClockwise"
            {
                ctx.error(
                    path,
                    format!(
                        "Invalid direction: '{}' (must be 'Clockwise' or 'CounterClockwise')",
                        dir
                    ),
                );
            }
        }
        Trigger::CC { cc, .. } => {
            ctx.midi_features.push("CC".to_string());
            if *cc > 127 {
                ctx.error(
                    path,
                    format!("CC number out of range: {} (must be 0-127)", cc),
                );
            }
        }
        Trigger::ProgramChange { pc, .. } => {
            ctx.midi_features.push("ProgramChange".to_string());
            if let Some(p) = pc
                && *p > 127
            {
                ctx.error(
                    path,
                    format!("Program Change number out of range: {} (must be 0-127)", p),
                );
            }
        }
        Trigger::Aftertouch { .. } => {
            ctx.midi_features.push("Aftertouch".to_string());
        }
        Trigger::PolyAftertouch { note, .. } => {
            ctx.midi_features.push("PolyAftertouch".to_string());
            if *note > 127 {
                ctx.error(
                    path,
                    format!("Note number out of range: {} (must be 0-127)", note),
                );
            }
        }
        Trigger::PitchBend { .. } => {
            ctx.midi_features.push("PitchBend".to_string());
        }
        Trigger::GamepadButton { button, .. } => {
            ctx.hid_features.push("GamepadButton".to_string());
            if *button < 128 {
                // Layer 1 treated this as error; Layer 2 as warning.
                // Unified: Error (prevents MIDI conflicts)
                ctx.error(
                    path,
                    format!(
                        "Gamepad button ID out of range: {} (must be 128-255 to avoid MIDI conflicts)",
                        button
                    ),
                );
            }
        }
        Trigger::GamepadButtonChord { buttons, .. } => {
            ctx.hid_features.push("GamepadButtonChord".to_string());
            for (i, button) in buttons.iter().enumerate() {
                if *button < 128 {
                    ctx.error(
                        format!("{}.buttons[{}]", path, i),
                        format!(
                            "Gamepad button ID out of range: {} (must be 128-255 to avoid MIDI conflicts)",
                            button
                        ),
                    );
                }
            }
            if buttons.is_empty() {
                ctx.error(path, "GamepadButtonChord must have at least one button");
            }
        }
        Trigger::GamepadAnalogStick {
            axis, direction, ..
        } => {
            ctx.hid_features.push("GamepadAnalogStick".to_string());
            // 128-131 = analog sticks; ADR-047 §D3b adds the d-pad-as-axis
            // encoder ids 147/148 (kept in sync with the matcher in mapping.rs).
            if !((128..=131).contains(axis) || matches!(*axis, 147 | 148)) {
                ctx.error(
                    path,
                    format!(
                        "Gamepad analog stick axis out of range: {} (must be 128-131, or 147/148 for d-pad-as-axis)",
                        axis
                    ),
                );
            }
            if let Some(dir) = direction
                && dir != "Clockwise"
                && dir != "CounterClockwise"
            {
                ctx.error(
                    path,
                    format!(
                        "Invalid direction: '{}' (must be 'Clockwise' or 'CounterClockwise')",
                        dir
                    ),
                );
            }
        }
        Trigger::GamepadTrigger { trigger, .. } => {
            ctx.hid_features.push("GamepadTrigger".to_string());
            if *trigger != 132 && *trigger != 133 {
                ctx.error(
                    path,
                    format!(
                        "Gamepad trigger ID out of range: {} (must be 132 or 133)",
                        trigger
                    ),
                );
            }
        }
        // OSC triggers (ADR-039-A)
        Trigger::OscMessage { address, .. } => {
            if address.is_empty() || !address.starts_with('/') {
                ctx.error(
                    path,
                    format!(
                        "OscMessage address '{}' is invalid (OSC addresses must start with '/')",
                        address
                    ),
                );
            }
        }
        Trigger::OscAddressPattern { pattern, .. } => {
            if let Err(e) = crate::osc_pattern::OscPattern::compile(pattern) {
                ctx.error(
                    path,
                    format!("OscAddressPattern '{}' is invalid: {}", pattern, e),
                );
            }
        }
        Trigger::OscArgRange {
            arg_index,
            min,
            max,
            ..
        } => {
            if !min.is_finite() || !max.is_finite() {
                ctx.error(
                    path,
                    "OscArgRange min/max must be finite numbers".to_string(),
                );
            } else if min > max {
                ctx.error(path, format!("OscArgRange min {} exceeds max {}", min, max));
            }
            // Cap the index defensively: the OSC parser bounds args per
            // message, and a huge index is certainly a config mistake.
            if *arg_index > 63 {
                ctx.error(
                    path,
                    format!("OscArgRange arg_index {} is out of range (0-63)", arg_index),
                );
            }
        }
    }
}

/// Within each mode, the rule engine matches first-match-wins. If
/// two mappings have overlapping triggers and the broader one appears
/// first, the narrower one will never fire — a class of bug that is
/// silent today (the user sees no error, just an action that doesn't
/// happen). Walk each mode's mappings in order and emit a warning for
/// every pair (i, j) where i < j and `mappings[i].trigger.shadows(
/// &mappings[j].trigger)`.
///
/// Scope is intentionally narrow in v1: only the four trigger types
/// most often involved in shadow bugs (Note, CC, Aftertouch,
/// PolyAftertouch) are analysed by `Trigger::shadows`. Cross-type pairs
/// and uncovered variants are not flagged. Future follow-ups will
/// extend coverage; the validator wiring stays the same.
fn warn_shadowed_mappings(
    mappings: &[crate::config::Mapping],
    scope_label: &str,
    base_path: &str,
    ctx: &mut ValidationCtx,
) {
    for (i, earlier) in mappings.iter().enumerate() {
        for (j, later) in mappings.iter().enumerate().skip(i + 1) {
            if !earlier.trigger.shadows(&later.trigger) {
                continue;
            }
            let later_path = format!("{}.mappings[{}]", base_path, j);
            let earlier_desc = earlier.description.as_deref().unwrap_or("(no description)");
            let later_desc = later.description.as_deref().unwrap_or("(no description)");
            ctx.warning(
                &later_path,
                format!(
                    "{0}: mapping #{1} ('{2}') is shadowed by mapping #{3} ('{4}'), \
                     which has the same or broader trigger and appears earlier. \
                     Mapping #{1} will never fire because the rule engine matches \
                     first-match-wins. Either reorder the mappings (move the more-\
                     specific one earlier) or narrow the broader trigger.",
                    scope_label, j, later_desc, i, earlier_desc
                ),
            );
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Condition validation (ADR-025 Phase 2)
// ────────────────────────────────────────────────────────────────

/// Walk a `Condition` tree, emitting errors for bounds violations and
/// structurally-impossible expressions (e.g. `CcValueInRange { min: 80,
/// max: 20 }` which can never be satisfied at runtime).
///
/// Surfacing at load time prevents the silent-always-false failure mode:
/// a broken condition would otherwise just send every trigger down the
/// `else_action` branch with no user-visible feedback.
fn validate_condition(condition: &crate::actions::Condition, path: &str, ctx: &mut ValidationCtx) {
    use crate::actions::Condition;
    match condition {
        Condition::ActivePcIs {
            pc,
            channel,
            device,
        } => {
            if device.is_empty() {
                ctx.error(path, "ActivePcIs: device is required");
            } else if !ctx.device_known(device) {
                ctx.error(path, format!("ActivePcIs: unknown device '{}'", device));
            }
            if *pc > 127 {
                ctx.error(path, format!("ActivePcIs: pc out of range {} (0-127)", pc));
            }
            if *channel > 15 {
                ctx.error(
                    path,
                    format!("ActivePcIs: channel out of range {} (0-15)", channel),
                );
            }
        }
        Condition::CcValueInRange {
            cc,
            channel,
            min,
            max,
            device,
        } => {
            if device.is_empty() {
                ctx.error(path, "CcValueInRange: device is required");
            } else if !ctx.device_known(device) {
                ctx.error(path, format!("CcValueInRange: unknown device '{}'", device));
            }
            if *cc > 127 {
                ctx.error(
                    path,
                    format!("CcValueInRange: cc out of range {} (0-127)", cc),
                );
            }
            if *channel > 15 {
                ctx.error(
                    path,
                    format!("CcValueInRange: channel out of range {} (0-15)", channel),
                );
            }
            if *min > 127 {
                ctx.error(
                    path,
                    format!("CcValueInRange: min out of range {} (0-127)", min),
                );
            }
            if *max > 127 {
                ctx.error(
                    path,
                    format!("CcValueInRange: max out of range {} (0-127)", max),
                );
            }
            if *min > *max {
                // ADR-025 P2: a CcValueInRange with
                // min > max is unsatisfiable. Catching at load time
                // avoids the silent always-false branch at runtime.
                ctx.error(
                    path,
                    format!(
                        "CcValueInRange: min ({}) > max ({}) is unsatisfiable; swap the bounds or widen the range",
                        min, max
                    ),
                );
            }
        }
        Condition::NoteHeld {
            note,
            channel,
            device,
            ..
        } => {
            if device.is_empty() {
                ctx.error(path, "NoteHeld: device is required");
            } else if !ctx.device_known(device) {
                ctx.error(path, format!("NoteHeld: unknown device '{}'", device));
            }
            if *note > 127 {
                ctx.error(
                    path,
                    format!("NoteHeld: note out of range {} (0-127)", note),
                );
            }
            if *channel > 15 {
                ctx.error(
                    path,
                    format!("NoteHeld: channel out of range {} (0-15)", channel),
                );
            }
        }
        // ADR-025 Phase 2.C sugar: fixed bounds (64..=127 / 0..=63)
        // make min>max structurally impossible, so only cc/channel/
        // device need validation. Same shape as the CcValueInRange
        // arm minus the min/max checks.
        Condition::CcIsOn {
            cc,
            channel,
            device,
        }
        | Condition::CcIsOff {
            cc,
            channel,
            device,
        } => {
            let kind = if matches!(condition, Condition::CcIsOn { .. }) {
                "CcIsOn"
            } else {
                "CcIsOff"
            };
            if device.is_empty() {
                ctx.error(path, format!("{}: device is required", kind));
            } else if !ctx.device_known(device) {
                ctx.error(path, format!("{}: unknown device '{}'", kind, device));
            }
            if *cc > 127 {
                ctx.error(path, format!("{}: cc out of range {} (0-127)", kind, cc));
            }
            if *channel > 15 {
                ctx.error(
                    path,
                    format!("{}: channel out of range {} (0-15)", kind, channel),
                );
            }
        }
        Condition::And { conditions } | Condition::Or { conditions } => {
            for (i, c) in conditions.iter().enumerate() {
                validate_condition(c, &format!("{}[{}]", path, i), ctx);
            }
        }
        Condition::Not { condition } => {
            validate_condition(condition, &format!("{}.not", path), ctx);
        }
        // Pre-ADR-025 variants: no additional checks needed beyond what
        // serde already enforces. Listed explicitly so adding a new
        // variant in the future is a compile error here.
        Condition::Always
        | Condition::Never
        | Condition::TimeRange { .. }
        | Condition::DayOfWeek { .. }
        | Condition::AppRunning { .. }
        | Condition::AppFrontmost { .. }
        | Condition::ModeIs { .. } => {}
    }
}

// ────────────────────────────────────────────────────────────────
// Action validation
// ────────────────────────────────────────────────────────────────

fn validate_action(action: &ActionConfig, path: &str, ctx: &mut ValidationCtx, depth: usize) {
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

/// Validate shell command for security (prevents command injection)
///
/// Blocks dangerous patterns that could enable command injection attacks:
/// - Command chaining: `;`, `&&`, `||`
/// - Piping: `|`
/// - Command substitution: `` ` ``, `$(`, `${`
/// - Redirects: `>`, `>>`, `<`, `<<`
/// - Background execution: `&` (at end of command)
fn validate_shell_command(command: &str, path: &str, ctx: &mut ValidationCtx) {
    validate_shell_input(command, path, ctx, ShellInputKind::Command);
}

/// ADR-027 D3 §3.2: apply the
/// `allow_interpreters` policy.
///
/// Resolves the effective binary via the same wrapper-unwinding pass
/// `capabilities_for_action` uses, then emits a warning or error
/// (depending on `ctx.allow_interpreters`) when the resolved binary
/// is a known interpreter family. `Allow` is a no-op.
///
/// The diagnostic includes:
/// - the interpreter family (so users can see which class triggered
///   the policy)
/// - the resolved binary path (so they can spot wrapper bypasses like
///   `env python -c …` even when their config wrote `command =
///   "/usr/bin/env"`)
fn validate_interpreter_policy(
    command: &str,
    args: Option<&[String]>,
    path: &str,
    ctx: &mut ValidationCtx,
) {
    use crate::config::types::InterpreterPolicy;
    use crate::security::resolved_interpreter_family;

    let Some(family) = resolved_interpreter_family(command, args) else {
        return;
    };

    // Spelling the family lower-cased matches how users would write
    // the basename in TOML (`python`, `bash`) — and `python` matches
    // the test assertion that the warning names the resolved binary.
    // Using `{:?}` would produce `Python` / `Bash` which the test
    // also matches but reads worse in user-facing UI.
    let family_display = match family {
        crate::security::InterpreterFamily::Python => "python",
        crate::security::InterpreterFamily::Ruby => "ruby",
        crate::security::InterpreterFamily::Perl => "perl",
        crate::security::InterpreterFamily::Node => "node",
        crate::security::InterpreterFamily::Bash => "bash",
        crate::security::InterpreterFamily::Sh => "sh",
        crate::security::InterpreterFamily::Zsh => "zsh",
        crate::security::InterpreterFamily::Fish => "fish",
        crate::security::InterpreterFamily::AwkOrSed => "awk/sed",
        crate::security::InterpreterFamily::Lua => "lua",
        crate::security::InterpreterFamily::TclSh => "tclsh",
        crate::security::InterpreterFamily::Php => "php",
        crate::security::InterpreterFamily::Other => "interpreter",
    };

    let message = format!(
        "Shell action invokes an interpreter ({family_display}). \
         Interpreters can execute arbitrary code via `-c` / `-e` flags \
         and bypass argv-array protections. Set \
         `advanced_settings.allow_interpreters = \"allow\"` to opt in \
         deliberately, or change the action to invoke a non-interpreter \
         binary directly."
    );

    match ctx.allow_interpreters {
        InterpreterPolicy::Allow => {} // explicit opt-in — no diagnostic
        InterpreterPolicy::Warn => ctx.warning(path, message),
        InterpreterPolicy::Deny => ctx.error(path, message),
    }
}

/// Returns true if the trimmed legacy Shell command contains at least
/// one character that would survive `parse_command_line` tokenisation
/// as a non-quote argv part.
///
/// Anything composed entirely of whitespace and the `'`/`"` quote
/// characters (matched or unmatched) toggles parser state without ever
/// emitting a token, so the executor would log "Failed to parse shell
/// command" and abort. Pulling that check up to validation time gives
/// users a clear "Shell action requires command" diagnostic at config
/// load instead of a silent runtime no-op. Only meaningful for the
/// legacy single-string form; argv-form `command` is a binary path
/// that the metacharacter blocklist and OS-level spawn error already
/// cover.
fn command_has_runnable_token(command: &str) -> bool {
    command
        .chars()
        .any(|c| !c.is_whitespace() && c != '\'' && c != '"')
}

/// Validate an argv-form `args[i]` token (ADR-027 D3 §3.1). Same
/// blocklist as [`validate_shell_command`] — only the
/// error wording changes so the diagnostic reads "Shell argument
/// contains…" instead of "Shell command contains…", which is otherwise
/// confusing when the failure path is `…args[2]` rather than the
/// top-level `command`.
fn validate_shell_arg(arg: &str, path: &str, ctx: &mut ValidationCtx) {
    validate_shell_input(arg, path, ctx, ShellInputKind::Arg);
}

#[derive(Clone, Copy)]
enum ShellInputKind {
    Command,
    Arg,
}

impl ShellInputKind {
    fn label(self) -> &'static str {
        match self {
            ShellInputKind::Command => "Shell command",
            ShellInputKind::Arg => "Shell argument",
        }
    }
}

fn validate_shell_input(input: &str, path: &str, ctx: &mut ValidationCtx, kind: ShellInputKind) {
    let dangerous_patterns = [
        (";", "command chaining with semicolon"),
        ("&&", "command chaining with AND"),
        ("||", "command chaining with OR"),
        ("|", "piping"),
        ("`", "backtick command substitution"),
        ("$(", "dollar-paren command substitution"),
        ("${", "variable expansion"),
        (">>", "append redirection"),
        ("<<", "here-document"),
        (">", "output redirection"),
        ("<", "input redirection"),
        ("&\n", "background execution"),
        ("&\r", "background execution"),
    ];

    let label = kind.label();
    for (pattern, description) in &dangerous_patterns {
        if input.contains(pattern) {
            ctx.error(
                path,
                format!(
                    "{} contains dangerous pattern '{}' ({}). \
                     This could enable command injection attacks. \
                     Use safe alternatives or split into separate mappings.",
                    label, pattern, description
                ),
            );
            return; // fail-fast like the original
        }
    }

    if input.trim_end().ends_with('&') {
        ctx.error(
            path,
            format!(
                "{} ends with '&' (background execution). \
                 This could enable command injection attacks.",
                label
            ),
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Security: App name validation (from former loader.rs)
// ────────────────────────────────────────────────────────────────

/// Validate application name for security (prevents shell injection via Launch action)
fn validate_app_name(app: &str, path: &str, ctx: &mut ValidationCtx) {
    let allowed_pattern = regex::Regex::new(r"^[a-zA-Z0-9\s\-_./ ]+$").unwrap();

    if !allowed_pattern.is_match(app) {
        ctx.error(
            path,
            format!(
                "Launch action app name '{}' contains invalid characters. \
                 Only alphanumeric, spaces, hyphens, underscores, periods, and forward slashes are allowed.",
                app
            ),
        );
        return;
    }

    if app.contains("..") {
        ctx.error(
            path,
            "Launch action app name cannot contain '..' (path traversal)",
        );
    }
}

// ────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{
        ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind, Mapping, Mode,
        RouteConfig, SignalFilter, SignalTransform,
    };
    use crate::identity::DeviceMatcher;

    fn default_config() -> Config {
        Config::default_config()
    }

    /// Build an Input-direction `Matcher` endpoint carrying `matchers` (the
    /// ADR-035 replacement for the removed `DeviceIdentityConfig` matchers-only
    /// fixture — `lower_binding` mapped `(input=None, output=None)` with
    /// non-empty top-level `matchers` to an `Input` Matcher).
    fn ep_input(alias: &str, matchers: Vec<DeviceMatcher>) -> EndpointConfig {
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
                no_probe: false,
            },
        }
    }

    fn config_with_mapping(trigger: Trigger, action: ActionConfig) -> Config {
        Config {
            config_meta: Default::default(),
            endpoints: vec![],
            modes: vec![Mode {
                name: "Test".to_string(),
                color: None,
                mappings: vec![Mapping {
                    trigger,
                    action,
                    description: None,
                    let_through: false,
                }],
            }],
            ..default_config()
        }
    }

    fn config_with_action(action: ActionConfig) -> Config {
        config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: Some(1),
                channel: None,
                device: None,
            },
            action,
        )
    }

    // ── ADR-039-A: OSC route validation (D8 + filter) ──

    fn ep_osc_input(alias: &str) -> EndpointConfig {
        use crate::config::types::NetworkSecurityConfig;
        EndpointConfig {
            alias: alias.to_string(),
            direction: ConnectorDirection::Input,
            protocol: None, // inferred Osc from the OscEndpoint kind
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::OscEndpoint {
                host: "127.0.0.1".to_string(),
                port: 9000,
                security: NetworkSecurityConfig::default(),
            },
        }
    }

    fn ep_midi(
        alias: &str,
        dir: ConnectorDirection,
        matchers: Vec<DeviceMatcher>,
    ) -> EndpointConfig {
        EndpointConfig {
            alias: alias.to_string(),
            direction: dir,
            protocol: None, // Matcher kind defaults to Midi
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers,
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }
    }

    fn osc_to_midi_route(from: &str, to: &str, filter: Option<SignalFilter>) -> RouteConfig {
        RouteConfig {
            from: from.to_string(),
            to: to.to_string(),
            transform: Some(SignalTransform::OscToMidi {
                address_to_cc: Some("/eos/fader/{cc}".to_string()),
                address_to_note: None,
                channel: Some(0),
            }),
            filter,
            enabled: true,
            description: None,
            modes: vec![],
        }
    }

    fn config_with_osc_route(endpoints: Vec<EndpointConfig>, route: RouteConfig) -> Config {
        Config {
            config_meta: Default::default(),
            endpoints,
            routes: vec![route],
            ..default_config()
        }
    }

    // ── ADR-039-A: OscToArtNet route validation ──

    fn ep_artnet_output(alias: &str) -> EndpointConfig {
        use crate::config::types::NetworkSecurityConfig;
        EndpointConfig {
            alias: alias.to_string(),
            direction: ConnectorDirection::Output,
            protocol: None, // inferred ArtNet from the ArtNetEndpoint kind
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::ArtNetEndpoint {
                universe: 0,
                host: "127.0.0.1".to_string(),
                port: 6454,
                allow_broadcast: false,
                security: NetworkSecurityConfig::default(),
            },
        }
    }

    fn osc_to_artnet_route(from: &str, to: &str, template: &str) -> RouteConfig {
        RouteConfig {
            from: from.to_string(),
            to: to.to_string(),
            transform: Some(SignalTransform::OscToArtNet {
                address_to_dmx: template.to_string(),
            }),
            filter: None,
            enabled: true,
            description: None,
            modes: vec![],
        }
    }

    #[test]
    fn osc_to_artnet_route_with_valid_template_passes() {
        let cfg = config_with_osc_route(
            vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
            osc_to_artnet_route("osc-in", "dmx-out", "/dmx/{dmx}"),
        );
        let report = validate_config(&cfg);
        assert!(
            report.errors.is_empty(),
            "valid OscToArtNet route must pass: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_to_artnet_template_without_placeholder_rejected() {
        let cfg = config_with_osc_route(
            vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
            osc_to_artnet_route("osc-in", "dmx-out", "/dmx/static"),
        );
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| e.message.contains("{dmx}")),
            "missing placeholder must be a load error: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_to_artnet_template_without_leading_slash_rejected() {
        let cfg = config_with_osc_route(
            vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
            osc_to_artnet_route("osc-in", "dmx-out", "dmx/{dmx}"),
        );
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("OscToArtNet")),
            "missing leading slash must be a load error: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_to_artnet_template_with_two_placeholders_rejected() {
        let cfg = config_with_osc_route(
            vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
            osc_to_artnet_route("osc-in", "dmx-out", "/{dmx}/{dmx}"),
        );
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| e.message.contains("{dmx}")),
            "duplicate placeholder must be a load error: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_to_artnet_route_without_transform_requires_one() {
        // (Osc, ArtNet) is now Required("OscToArtNet") in the matrix — a
        // transform-less route must be rejected, not silently passed.
        let mut route = osc_to_artnet_route("osc-in", "dmx-out", "/dmx/{dmx}");
        route.transform = None;
        let cfg = config_with_osc_route(
            vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
            route,
        );
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("OscToArtNet")),
            "missing required transform must name OscToArtNet: {:?}",
            report.errors
        );
    }

    // ── ADR-039-A: OscForward action validation ──

    fn ep_osc_output(alias: &str) -> EndpointConfig {
        use crate::config::types::NetworkSecurityConfig;
        EndpointConfig {
            alias: alias.to_string(),
            direction: ConnectorDirection::Output,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::OscEndpoint {
                host: "127.0.0.1".to_string(),
                port: 9100,
                security: NetworkSecurityConfig::default(),
            },
        }
    }

    fn config_with_osc_forward(endpoints: Vec<EndpointConfig>, action: ActionConfig) -> Config {
        Config {
            config_meta: Default::default(),
            endpoints,
            modes: vec![Mode {
                name: "Test".to_string(),
                color: None,
                mappings: vec![Mapping {
                    trigger: Trigger::OscMessage {
                        address: "/eos/go".to_string(),
                        device: Some("osc-in".to_string()),
                    },
                    action,
                    description: None,
                    let_through: false,
                }],
            }],
            ..default_config()
        }
    }

    #[test]
    fn osc_forward_to_osc_output_passes() {
        let cfg = config_with_osc_forward(
            vec![ep_osc_input("osc-in"), ep_osc_output("eos-out")],
            ActionConfig::OscForward {
                target: "eos-out".to_string(),
                transform: None,
            },
        );
        let report = validate_config(&cfg);
        assert!(
            report.errors.is_empty(),
            "valid OscForward must pass: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_forward_with_transform_rejected_in_v1() {
        let cfg = config_with_osc_forward(
            vec![ep_osc_input("osc-in"), ep_osc_output("eos-out")],
            ActionConfig::OscForward {
                target: "eos-out".to_string(),
                transform: Some(SignalTransform::OscToMidi {
                    address_to_cc: Some("/f/{cc}".to_string()),
                    address_to_note: None,
                    channel: None,
                }),
            },
        );
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("OscForward transform is not supported")),
            "a transform must be rejected in V1: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_forward_to_non_osc_target_rejected() {
        let cfg = config_with_osc_forward(
            vec![ep_osc_input("osc-in"), ep_artnet_output("dmx-out")],
            ActionConfig::OscForward {
                target: "dmx-out".to_string(),
                transform: None,
            },
        );
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("requires an OSC output endpoint")),
            "an Art-Net target must be rejected: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_forward_to_unknown_target_rejected() {
        let cfg = config_with_osc_forward(
            vec![ep_osc_input("osc-in")],
            ActionConfig::OscForward {
                target: "nope".to_string(),
                transform: None,
            },
        );
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("does not match any [[endpoints]] alias")),
            "an unknown target alias must be rejected: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_forward_to_input_only_osc_target_rejected() {
        // An OSC endpoint that is Input-only can't be sent to — the daemon's
        // runtime map would never contain it, so load must reject.
        let cfg = config_with_osc_forward(
            vec![ep_osc_input("osc-in")],
            ActionConfig::OscForward {
                target: "osc-in".to_string(),
                transform: None,
            },
        );
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("Input-only OSC endpoint")),
            "an Input-only OSC target must be rejected: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_forward_to_disabled_osc_output_rejected() {
        // A disabled OSC output is excluded from the daemon's runtime map, so
        // load must reject it for clear UX rather than a silent no-op.
        let mut disabled = ep_osc_output("eos-out");
        disabled.enabled = false;
        let cfg = config_with_osc_forward(
            vec![ep_osc_input("osc-in"), disabled],
            ActionConfig::OscForward {
                target: "eos-out".to_string(),
                transform: None,
            },
        );
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("disabled endpoint")),
            "a disabled OSC output target must be rejected: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_forward_to_bidirectional_osc_output_passes() {
        // Bidirectional OSC is a valid send target (mirrors the runtime map).
        let mut bidir = ep_osc_output("eos-io");
        bidir.direction = ConnectorDirection::Bidirectional;
        let cfg = config_with_osc_forward(
            vec![ep_osc_input("osc-in"), bidir],
            ActionConfig::OscForward {
                target: "eos-io".to_string(),
                transform: None,
            },
        );
        let report = validate_config(&cfg);
        assert!(
            report.errors.is_empty(),
            "a Bidirectional OSC output target must pass: {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_route_to_bidirectional_midi_rejected() {
        // D8: a Bidirectional MIDI target is both output and input → self-loop.
        let cfg = config_with_osc_route(
            vec![
                ep_osc_input("console"),
                ep_midi(
                    "synth",
                    ConnectorDirection::Bidirectional,
                    vec![DeviceMatcher::NameContains {
                        value: "Synth".into(),
                    }],
                ),
            ],
            osc_to_midi_route("console", "synth", None),
        );
        let report = validate_config(&cfg);
        assert!(
            !report.is_valid(),
            "OSC→bidirectional-MIDI must be rejected"
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("feedback loop")),
            "error should name the feedback loop; got {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_route_to_self_ingested_midi_rejected() {
        // D8: distinct Output + Input MIDI endpoints with identical matchers =
        // same device wired in and out.
        let matchers = vec![DeviceMatcher::NameContains {
            value: "Mikro".into(),
        }];
        let cfg = config_with_osc_route(
            vec![
                ep_osc_input("console"),
                ep_midi("mikro_out", ConnectorDirection::Output, matchers.clone()),
                ep_midi("mikro_in", ConnectorDirection::Input, matchers),
            ],
            osc_to_midi_route("console", "mikro_out", None),
        );
        let report = validate_config(&cfg);
        assert!(
            !report.is_valid(),
            "OSC→self-ingested-MIDI must be rejected"
        );
    }

    #[test]
    fn osc_route_to_distinct_midi_output_ok() {
        // No MIDI input shares the output's device → no loop → valid.
        let cfg = config_with_osc_route(
            vec![
                ep_osc_input("console"),
                ep_midi(
                    "fx_out",
                    ConnectorDirection::Output,
                    vec![DeviceMatcher::NameContains { value: "FX".into() }],
                ),
                ep_midi(
                    "pad_in",
                    ConnectorDirection::Input,
                    vec![DeviceMatcher::NameContains {
                        value: "Pad".into(),
                    }],
                ),
            ],
            osc_to_midi_route("console", "fx_out", None),
        );
        let report = validate_config(&cfg);
        assert!(
            report.is_valid(),
            "OSC→distinct-MIDI-output should validate; got {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_route_with_filter_rejected() {
        // OSC routes are currently catch-all only.
        let filter = SignalFilter {
            message_types: vec![],
            channels: vec![5],
            cc_range: None,
            note_range: None,
            osc_address_prefix: None,
        };
        let cfg = config_with_osc_route(
            vec![
                ep_osc_input("console"),
                ep_midi(
                    "fx_out",
                    ConnectorDirection::Output,
                    vec![DeviceMatcher::NameContains { value: "FX".into() }],
                ),
            ],
            osc_to_midi_route("console", "fx_out", Some(filter)),
        );
        let report = validate_config(&cfg);
        assert!(
            !report.is_valid(),
            "OSC-source route with a filter must be rejected"
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("catch-all")),
            "error should mention catch-all; got {:?}",
            report.errors
        );
    }

    fn ep_virtual(alias: &str, dir: ConnectorDirection, port_name: &str) -> EndpointConfig {
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

    #[test]
    fn osc_route_to_self_ingested_virtual_port_rejected() {
        // D8: a MidiVirtualPort output has no DeviceMatchers, so
        // the matcher-signature path misses it — but a Conductor-created virtual
        // port that an input endpoint also names is the classic loopback.
        let cfg = config_with_osc_route(
            vec![
                ep_osc_input("console"),
                ep_virtual("bus_out", ConnectorDirection::Output, "Conductor Bus"),
                ep_midi(
                    "bus_in",
                    ConnectorDirection::Input,
                    vec![DeviceMatcher::ExactName {
                        value: "Conductor Bus".into(),
                    }],
                ),
            ],
            osc_to_midi_route("console", "bus_out", None),
        );
        let report = validate_config(&cfg);
        assert!(
            !report.is_valid(),
            "OSC→virtual-port also ingested as input must be rejected"
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("feedback loop")),
            "error should name the feedback loop; got {:?}",
            report.errors
        );
    }

    #[test]
    fn osc_route_to_virtual_port_input_twin_rejected() {
        // The input twin is itself a MidiVirtualPort of the same name.
        let cfg = config_with_osc_route(
            vec![
                ep_osc_input("console"),
                ep_virtual("bus_out", ConnectorDirection::Output, "Bus"),
                ep_virtual("bus_in", ConnectorDirection::Input, "Bus"),
            ],
            osc_to_midi_route("console", "bus_out", None),
        );
        assert!(!validate_config(&cfg).is_valid());
    }

    #[test]
    fn osc_route_to_unmonitored_virtual_port_ok() {
        // No input names this virtual port → no loop → valid.
        let cfg = config_with_osc_route(
            vec![
                ep_osc_input("console"),
                ep_virtual("bus_out", ConnectorDirection::Output, "Conductor Bus"),
                ep_midi(
                    "pad_in",
                    ConnectorDirection::Input,
                    vec![DeviceMatcher::NameContains {
                        value: "Pad".into(),
                    }],
                ),
            ],
            osc_to_midi_route("console", "bus_out", None),
        );
        let report = validate_config(&cfg);
        assert!(
            report.is_valid(),
            "OSC→unmonitored virtual port should validate; got {:?}",
            report.errors
        );
    }

    // ── Structural tests (from former loader.rs) ─────────────

    #[test]
    fn test_validate_valid_config() {
        let config = default_config();
        let report = validate_config(&config);
        assert!(report.is_valid());
    }

    #[test]
    fn test_trace_buffer_size_zero_rejected() {
        let mut config = default_config();
        config.advanced_settings.trace_buffer_size = 0;
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.path == "advanced_settings.trace_buffer_size"
                    && e.message.contains("at least 1")),
            "0 must be rejected with a clear message"
        );
    }

    #[test]
    fn test_trace_buffer_size_too_large_rejected() {
        use crate::config::types::MAX_TRACE_BUFFER_SIZE;
        let mut config = default_config();
        config.advanced_settings.trace_buffer_size = MAX_TRACE_BUFFER_SIZE + 1;
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.path == "advanced_settings.trace_buffer_size"
                    && e.message.contains("exceeds the maximum")),
            "values above the cap must be rejected"
        );
    }

    #[test]
    fn test_trace_buffer_size_in_range_accepted() {
        let mut config = default_config();
        config.advanced_settings.trace_buffer_size = 5000;
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "a sane in-range buffer size must validate: {:?}",
            report.errors
        );
    }

    #[test]
    fn test_validate_duplicate_mode_names() {
        let mut config = default_config();
        config.modes.push(Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![],
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("Duplicate mode name"))
        );
    }

    #[test]
    fn test_validate_invalid_note_number() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 128,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("out of range"))
        );
    }

    #[test]
    fn test_validate_invalid_modifier() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec!["invalid_mod".to_string()],
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("Unknown modifier"))
        );
    }

    #[test]
    fn test_validate_invalid_direction() {
        let config = config_with_mapping(
            Trigger::EncoderTurn {
                cc: 1,
                direction: Some("Invalid".to_string()),
                channel: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("Invalid direction"))
        );
    }

    #[test]
    fn test_validate_empty_keystroke_keys() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: String::new(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
    }

    #[test]
    fn test_validate_sequence_with_empty_actions() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Sequence { actions: vec![] },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
    }

    #[test]
    fn test_validate_encoder_direction_clockwise() {
        let config = config_with_mapping(
            Trigger::EncoderTurn {
                cc: 1,
                direction: Some("Clockwise".to_string()),
                channel: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        assert!(report.is_valid());
    }

    #[test]
    fn test_validate_encoder_direction_counter_clockwise() {
        let config = config_with_mapping(
            Trigger::EncoderTurn {
                cc: 1,
                direction: Some("CounterClockwise".to_string()),
                channel: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        assert!(report.is_valid());
    }

    #[test]
    fn test_validate_note_chord_with_empty_notes() {
        let config = config_with_mapping(
            Trigger::NoteChord {
                notes: vec![],
                timeout_ms: None,
                channel: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
    }

    #[test]
    fn test_validate_invalid_mouse_button() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::MouseClick {
                button: "invalid".to_string(),
                x: None,
                y: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
    }

    #[test]
    fn test_validate_volume_control_set_without_value() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::VolumeControl {
                operation: "Set".to_string(),
                value: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
    }

    // ── Security tests (from former loader.rs) ──────────────

    #[test]
    fn test_shell_injection_semicolon_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "echo test; rm -rf /".to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("command chaining with semicolon"))
        );
    }

    #[test]
    fn test_shell_injection_and_operator_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "ls && malicious_command".to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("command chaining with AND"))
        );
    }

    #[test]
    fn test_shell_injection_or_operator_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "false || evil_fallback".to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("command chaining with OR"))
        );
    }

    #[test]
    fn test_shell_injection_pipe_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "cat /etc/passwd | grep root".to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| e.message.contains("piping")));
    }

    #[test]
    fn test_shell_injection_backtick_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "echo `whoami`".to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("backtick command substitution"))
        );
    }

    #[test]
    fn test_shell_injection_dollar_paren_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "echo $(whoami)".to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("dollar-paren command substitution"))
        );
    }

    #[test]
    fn test_shell_injection_variable_expansion_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "echo ${DANGEROUS_VAR}".to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("variable expansion"))
        );
    }

    #[test]
    fn test_shell_injection_output_redirect_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "echo data > /etc/important_file".to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
    }

    #[test]
    fn test_shell_injection_background_execution_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "malicious_daemon &".to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("background execution"))
        );
    }

    #[test]
    fn test_shell_safe_commands_allowed() {
        let safe_commands = [
            "git status",
            "cargo build",
            "ls -la",
            "echo hello world",
            "pwd",
        ];
        for cmd in &safe_commands {
            let config = config_with_mapping(
                Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: None,
                    device: None,
                },
                ActionConfig::Shell {
                    sandbox: None,
                    command: cmd.to_string(),
                    args: None,
                    timeout_ms: None,
                },
            );
            let report = validate_config(&config);
            assert!(
                report.is_valid(),
                "Safe command '{}' should be allowed",
                cmd
            );
        }
    }

    // ───────────────────────────────────────────────────────────
    // ADR-027 D3 §3.1 — argv-form `args` also
    // get the metacharacter blocklist applied, so users can't
    // smuggle redirects / pipes / chains past the validator by
    // moving them into argv.
    // ───────────────────────────────────────────────────────────

    #[test]
    fn test_shell_argv_form_args_metacharacters_blocked() {
        // The exact bypass class — `>` redirect smuggled via argv-form args.
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "/bin/sh".to_string(),
                args: Some(vec!["-c".to_string(), "env > /tmp/leak".to_string()]),
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(
            !report.is_valid(),
            "argv-form args containing `>` must be rejected"
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("output redirection")),
            "error should attribute the rejection to the `>` redirect — got: {:?}",
            report.errors
        );
        // Wording: when the failure is in argv-form `.args[i]`, the
        // diagnostic must say "Shell argument" not "Shell command" —
        // otherwise users see a misleading "Shell command contains
        // '>'" error pointing at a path that ends in `.args[1]`.
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.starts_with("Shell argument")),
            "argv-form arg-blocklist error must use 'Shell argument' wording — got: {:?}",
            report.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_shell_argv_form_args_chain_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "/usr/bin/env".to_string(),
                args: Some(vec!["FOO=bar; rm -rf /".to_string()]),
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(
            !report.is_valid(),
            "argv-form args containing `;` must be rejected"
        );
    }

    #[test]
    fn test_shell_argv_form_safe_args_allowed() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "/usr/bin/osascript".to_string(),
                args: Some(vec![
                    "-e".to_string(),
                    "display notification \"MIDI triggered\"".to_string(),
                ]),
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "argv-form with safe args should be allowed — got errors: {:?}",
            report.errors
        );
    }

    #[test]
    fn test_shell_whitespace_only_command_rejected() {
        // A whitespace-only `command` used to pass validation and
        // become a runtime no-op (the executor trims and aborts
        // silently). The validator's `command.trim().is_empty()`
        // check now rejects it at load with the standard "Shell
        // action requires command" error.
        for whitespace in &[" ", "   ", "\t", "\n", " \t \n "] {
            let config = config_with_mapping(
                Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: None,
                    device: None,
                },
                ActionConfig::Shell {
                    sandbox: None,
                    command: whitespace.to_string(),
                    args: None,
                    timeout_ms: None,
                },
            );
            let report = validate_config(&config);
            assert!(
                !report.is_valid(),
                "whitespace-only command {:?} should be rejected",
                whitespace
            );
            assert!(
                report
                    .errors
                    .iter()
                    .any(|e| e.message.contains("requires command")),
                "error should explain the empty command — got: {:?}",
                report.errors
            );
        }
    }

    #[test]
    fn test_shell_quote_only_legacy_command_rejected() {
        // Legacy commands made up entirely of whitespace and the
        // `'`/`"` quote characters tokenise to zero argv parts, so
        // without this guard they'd pass validation only to no-op at
        // runtime (the executor logs "Failed to parse shell command"
        // and aborts). The `command_has_runnable_token` helper
        // rejects them at load instead.
        for cmd in &["'", "''", "\"", "\"\"", " ' ", "\t ' '", "'  \"  '"] {
            let config = config_with_mapping(
                Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: None,
                    device: None,
                },
                ActionConfig::Shell {
                    sandbox: None,
                    command: cmd.to_string(),
                    args: None,
                    timeout_ms: None,
                },
            );
            let report = validate_config(&config);
            assert!(
                !report.is_valid(),
                "quote-only legacy command {:?} should be rejected",
                cmd
            );
            assert!(
                report
                    .errors
                    .iter()
                    .any(|e| e.message.contains("requires command")),
                "error should diagnose as missing command — got: {:?}",
                report.errors
            );
        }
    }

    #[test]
    fn test_shell_argv_form_quote_only_command_with_args_allowed() {
        // A legacy quote-only `command` is rejected because the
        // tokeniser yields nothing — but an argv-form invocation
        // with `command = "'"` and explicit `args` would spawn
        // (and fail at the OS level with "no such file or
        // directory: '"). That's a clearer user-facing failure
        // than the legacy silent no-op, so the validator allows
        // it through (the metacharacter blocklist still applies
        // and would reject any actually-dangerous patterns).
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "'".to_string(),
                args: Some(vec!["arg".to_string()]),
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "argv-form lets weird `command` through — got: {:?}",
            report.errors
        );
    }

    #[test]
    fn test_shell_argv_form_args_path_includes_index() {
        // Error path should pinpoint WHICH arg failed, not just say
        // "Shell action somewhere broke". This helps users debug
        // multi-arg argv configs.
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "/usr/bin/env".to_string(),
                args: Some(vec!["SAFE=1".to_string(), "BAD$(rm -rf /)".to_string()]),
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report.errors.iter().any(|e| e.path.contains(".args[1]")),
            "error path should pinpoint args[1] — got: {:?}",
            report.errors.iter().map(|e| &e.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_launch_injection_special_chars_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Launch {
                app: "Terminal; rm -rf /".to_string(),
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("invalid characters"))
        );
    }

    #[test]
    fn test_launch_path_traversal_blocked() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Launch {
                app: "../../malicious".to_string(),
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("path traversal"))
        );
    }

    #[test]
    fn test_launch_safe_app_names_allowed() {
        let safe_apps = [
            "Terminal",
            "VS Code",
            "Google Chrome",
            "/Applications/Safari.app",
            "my-app_v2.0",
        ];
        for app in &safe_apps {
            let config = config_with_mapping(
                Trigger::Note {
                    note: 60,
                    velocity_min: None,
                    channel: None,
                    device: None,
                },
                ActionConfig::Launch {
                    app: app.to_string(),
                },
            );
            let report = validate_config(&config);
            assert!(
                report.is_valid(),
                "Safe app name '{}' should be allowed",
                app
            );
        }
    }

    // ── Protocol coverage tests (from former validator.rs) ───

    #[test]
    fn test_midi_note_range_valid() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "c".to_string(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        assert!(report.is_valid());
        assert!(report.coverage.midi.used.contains(&"Note".to_string()));
    }

    #[test]
    fn test_hid_button_range_valid() {
        let config = config_with_mapping(
            Trigger::GamepadButton {
                button: 128,
                velocity_min: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        assert!(report.is_valid());
        assert!(
            report
                .coverage
                .hid
                .used
                .contains(&"GamepadButton".to_string())
        );
    }

    #[test]
    fn test_hid_button_in_midi_range_errors() {
        let config = config_with_mapping(
            Trigger::GamepadButton {
                button: 50,
                velocity_min: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        // Unified: now an error (was warning in validator.rs, error in loader.rs)
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("MIDI conflicts"))
        );
    }

    #[test]
    fn test_shell_injection_warning_in_report() {
        // The unified system now treats shell injection as ERROR, not warning
        let config = config_with_mapping(
            Trigger::Note {
                note: 36,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::Shell {
                sandbox: None,
                command: "echo $USER | tee /tmp/out".to_string(),
                args: None,
                timeout_ms: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
    }

    #[test]
    fn test_coverage_calculation() {
        let config = Config {
            config_meta: Default::default(),
            endpoints: vec![],
            modes: vec![Mode {
                name: "Test".to_string(),
                color: None,
                mappings: vec![
                    Mapping {
                        trigger: Trigger::Note {
                            note: 36,
                            velocity_min: None,
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Keystroke {
                            keys: "c".to_string(),
                            modifiers: vec![],
                        },
                        description: None,
                        let_through: false,
                    },
                    Mapping {
                        trigger: Trigger::CC {
                            cc: 1,
                            value_min: None,
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Keystroke {
                            keys: "v".to_string(),
                            modifiers: vec![],
                        },
                        description: None,
                        let_through: false,
                    },
                ],
            }],
            ..default_config()
        };
        let report = validate_config(&config);
        assert_eq!(report.coverage.midi.used.len(), 2);
        // 2 used / 11 available = 18.18% (Raw removed from the available set,
        // ADR-036 Phase 2).
        assert!(report.coverage.midi.percentage > 18.0);
        assert!(report.coverage.midi.percentage < 19.0);
    }

    #[test]
    fn test_send_midi_channel_out_of_range() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 36,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::SendMidi {
                port: "Virtual Output".to_string(),
                channel: 16,
                note: Some(60),
                velocity: Some(100),
                message_type: "NoteOn".to_string(),
                controller: None,
                value: None,
                program: None,
                pitch: None,
                pressure: None,
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| e.message.contains("channel")));
    }

    // ── validate_for_loading adapter tests ───────────────────

    #[test]
    fn test_validate_for_loading_ok() {
        let config = default_config();
        assert!(validate_for_loading(&config).is_ok());
    }

    #[test]
    fn test_validate_for_loading_error() {
        let mut config = default_config();
        config.modes.push(Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![],
        });
        let err = validate_for_loading(&config).unwrap_err();
        assert!(err.to_string().contains("Duplicate mode name"));
    }

    // ── Cross-field validation tests (NEW) ───────────────────

    #[test]
    fn test_mode_change_references_existing_mode() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::ModeChange {
                mode: "Test".to_string(),
            },
        );
        let report = validate_config(&config);
        assert!(report.is_valid());
    }

    #[test]
    fn test_mode_change_references_nonexistent_mode() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: None,
            },
            ActionConfig::ModeChange {
                mode: "NonExistent".to_string(),
            },
        );
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("non-existent mode"))
        );
    }

    #[test]
    fn test_device_reference_undefined_alias() {
        let config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: Some("missing_device".to_string()),
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        // With no devices defined, device refs are allowed (backward compat)
        // But with devices defined and ref not matching, it's an error
        let mut config_with_devices = config;
        config_with_devices.endpoints = vec![ep_input(
            "my_device",
            vec![DeviceMatcher::NameContains {
                value: "Device".to_string(),
            }],
        )];
        let report = validate_config(&config_with_devices);
        // Undefined device alias is a warning (not error) since ListenMode::All
        // auto-discovers devices without needing [[devices]] entries
        assert!(report.is_valid());
        // ADR-035: warning must mention `[[endpoints]]` (the config
        // section that resolves the alias) and the alternative remediation
        // (remove the device filter). Both phrasings must be present so the
        // operator doesn't read the message as a connectivity hint.
        let warning = report
            .warnings
            .iter()
            .find(|w| w.message.contains("Trigger references device alias"))
            .expect("undefined-device-alias warning should fire");
        assert!(
            warning.message.contains("[[endpoints]]"),
            "warning must mention the [[endpoints]] section; got: {}",
            warning.message
        );
        assert!(
            warning.message.contains("remove the `device` filter"),
            "warning must mention the remove-filter remediation; got: {}",
            warning.message
        );
        assert!(
            !warning
                .message
                .contains("will only match if this device connects"),
            "warning must NOT imply this is a connectivity issue; got: {}",
            warning.message
        );
    }

    #[test]
    fn test_device_reference_valid_alias() {
        let mut config = config_with_mapping(
            Trigger::Note {
                note: 60,
                velocity_min: None,
                channel: None,
                device: Some("my_device".to_string()),
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        config.endpoints = vec![ep_input(
            "my_device",
            vec![DeviceMatcher::NameContains {
                value: "Device".to_string(),
            }],
        )];
        let report = validate_config(&config);
        assert!(report.is_valid());
    }

    // ── Velocity zone overlap warning (from former validator.rs) ──

    #[test]
    fn test_velocity_zone_overlap_warning() {
        let config = config_with_mapping(
            Trigger::VelocityRange {
                note: 60,
                soft_max: Some(100),
                medium_max: Some(80),
                channel: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        assert!(report.is_valid()); // warnings don't make it invalid
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.message.contains("velocity zones overlap"))
        );
    }

    #[test]
    fn test_note_chord_size_warning() {
        let config = config_with_mapping(
            Trigger::NoteChord {
                notes: vec![60],
                timeout_ms: None,
                channel: None,
                device: None,
            },
            ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec![],
            },
        );
        let report = validate_config(&config);
        assert!(report.is_valid()); // single-note chord is valid but warned
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.message.contains("fewer than 2 notes"))
        );
    }

    // ── LED config validation tests ─────────────

    #[test]
    fn test_led_config_valid() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            enabled: true,
            brightness: 100,
            scheme: "reactive".to_string(),
            idle_timeout_secs: 0,
            mode_colors: std::collections::BTreeMap::new(),
            midi: None,
            hid: None,
            velocity_colors: None,
            default_fade_ms: None,
        });
        let report = validate_config(&config);
        assert!(report.is_valid());
    }

    #[test]
    fn test_led_brightness_too_high() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            enabled: true,
            brightness: 200,
            scheme: "reactive".to_string(),
            idle_timeout_secs: 0,
            mode_colors: std::collections::BTreeMap::new(),
            midi: None,
            hid: None,
            velocity_colors: None,
            default_fade_ms: None,
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("brightness"))
        );
    }

    #[test]
    fn test_led_unknown_scheme() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            enabled: true,
            brightness: 100,
            scheme: "disco".to_string(),
            idle_timeout_secs: 0,
            mode_colors: std::collections::BTreeMap::new(),
            midi: None,
            hid: None,
            velocity_colors: None,
            default_fade_ms: None,
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("Unknown LED scheme"))
        );
    }

    #[test]
    fn test_led_mode_color_invalid_ref() {
        let mut config = default_config();
        let mut mode_colors = std::collections::BTreeMap::new();
        mode_colors.insert(
            "NonExistentMode".to_string(),
            crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
        );
        config.led = Some(crate::config::types::LedConfig {
            enabled: true,
            brightness: 100,
            scheme: "reactive".to_string(),
            idle_timeout_secs: 0,
            mode_colors,
            midi: None,
            hid: None,
            velocity_colors: None,
            default_fade_ms: None,
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("non-existent mode"))
        );
    }

    #[test]
    fn test_led_none_backward_compat() {
        let mut config = default_config();
        config.led = None;
        let report = validate_config(&config);
        assert!(report.is_valid());
    }

    #[test]
    fn test_plugin_action_valid() {
        let config = config_with_action(ActionConfig::Plugin {
            plugin: "my-plugin".to_string(),
            params: serde_json::json!({"key": "value"}),
        });
        let report = validate_config(&config);
        assert!(report.is_valid(), "Valid plugin should pass: {:?}", report);
    }

    #[test]
    fn test_plugin_action_empty_name() {
        let config = config_with_action(ActionConfig::Plugin {
            plugin: "".to_string(),
            params: serde_json::json!({}),
        });
        let report = validate_config(&config);
        assert!(
            !report.is_valid(),
            "Empty plugin name should fail validation"
        );
    }

    #[test]
    fn test_plugin_action_invalid_chars() {
        let config = config_with_action(ActionConfig::Plugin {
            plugin: "my plugin!".to_string(),
            params: serde_json::json!({}),
        });
        let report = validate_config(&config);
        assert!(
            !report.is_valid(),
            "Plugin name with spaces/special chars should fail"
        );
    }

    #[test]
    fn test_plugin_action_null_params_warns() {
        let config = config_with_action(ActionConfig::Plugin {
            plugin: "my-plugin".to_string(),
            params: serde_json::Value::Null,
        });
        let report = validate_config(&config);
        // Should be valid but have a warning
        assert!(report.is_valid());
        assert!(
            !report.warnings.is_empty(),
            "Null params should produce a warning"
        );
    }

    #[test]
    fn test_plugin_action_dot_namespaced() {
        let config = config_with_action(ActionConfig::Plugin {
            plugin: "com.example.my-plugin".to_string(),
            params: serde_json::json!({"key": "value"}),
        });
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "Dot-namespaced plugin names should be valid: {:?}",
            report
        );
    }

    // ── MIDI LED Config Validation Tests ────────

    #[test]
    fn test_midi_led_config_valid() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            midi: Some(crate::config::types::MidiLedConfig::default()),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(report.is_valid(), "Valid MIDI LED config: {:?}", report);
    }

    #[test]
    fn test_midi_led_channel_zero() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            midi: Some(crate::config::types::MidiLedConfig {
                channel: 0,
                ..Default::default()
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| e.message.contains("channel")));
    }

    #[test]
    fn test_midi_led_channel_too_high() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            midi: Some(crate::config::types::MidiLedConfig {
                channel: 17,
                ..Default::default()
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| e.message.contains("channel")));
    }

    #[test]
    fn test_midi_led_no_config_backward_compat() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            midi: None,
            hid: None,
            velocity_colors: None,
            default_fade_ms: None,
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(report.is_valid());
    }

    // ========== HID LED Validation Tests ==========

    #[test]
    fn test_hid_led_valid_profile() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            hid: Some(crate::config::types::HidLedConfig {
                hid_profile: Some("mikro-mk3".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(report.is_valid(), "errors: {:?}", report.errors);
    }

    #[test]
    fn test_hid_led_unknown_profile() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            hid: Some(crate::config::types::HidLedConfig {
                hid_profile: Some("unknown-device".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| e.message.contains("Unknown")));
    }

    #[test]
    fn test_hid_led_no_vendor_no_profile_fails() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            hid: Some(crate::config::types::HidLedConfig {
                product_id: Some(0x1234),
                ..Default::default()
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("vendor_id"))
        );
    }

    #[test]
    fn test_hid_led_explicit_valid() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            hid: Some(crate::config::types::HidLedConfig {
                vendor_id: Some(0x17CC),
                product_id: Some(0x1700),
                buffer_size: Some(80),
                pad_led_offset: Some(39),
                pad_count: Some(16),
                ..Default::default()
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(report.is_valid(), "errors: {:?}", report.errors);
    }

    #[test]
    fn test_hid_led_buffer_overflow_fails() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            hid: Some(crate::config::types::HidLedConfig {
                vendor_id: Some(0x1234),
                product_id: Some(0x5678),
                buffer_size: Some(10),
                pad_led_offset: Some(5),
                pad_count: Some(8),
                ..Default::default()
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
    }

    #[test]
    fn test_hid_led_pad_layout_mismatch() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            hid: Some(crate::config::types::HidLedConfig {
                hid_profile: Some("mikro-mk3".to_string()),
                pad_count: Some(16),
                pad_layout: Some(vec![0, 1, 2]), // 3 != 16
                ..Default::default()
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("pad_layout"))
        );
    }

    #[test]
    fn test_hid_led_duplicate_pad_layout() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            hid: Some(crate::config::types::HidLedConfig {
                vendor_id: Some(0x1234),
                product_id: Some(0x5678),
                buffer_size: Some(20),
                pad_led_offset: Some(0),
                pad_count: Some(4),
                pad_layout: Some(vec![0, 1, 1, 3]),
                ..Default::default()
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.to_lowercase().contains("duplicate"))
        );
    }

    #[test]
    fn test_hid_led_no_config_backward_compat() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            hid: None,
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(report.is_valid());
    }

    // ========== Velocity Color Map Validation ==========

    #[test]
    fn test_velocity_map_valid_default() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            velocity_colors: Some(crate::config::types::VelocityColorMap::default()),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(report.is_valid(), "errors: {:?}", report.errors);
    }

    #[test]
    fn test_velocity_map_overlapping_ranges() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            velocity_colors: Some(crate::config::types::VelocityColorMap {
                ranges: vec![
                    crate::config::types::VelocityRange {
                        min: 0,
                        max: 80,
                        color: crate::config::types::RgbColor { r: 0, g: 255, b: 0 },
                    },
                    crate::config::types::VelocityRange {
                        min: 60, // overlaps with 0-80
                        max: 127,
                        color: crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
                    },
                ],
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("Overlapping"))
        );
    }

    #[test]
    fn test_velocity_map_inverted_range() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            velocity_colors: Some(crate::config::types::VelocityColorMap {
                ranges: vec![crate::config::types::VelocityRange {
                    min: 80,
                    max: 40, // inverted
                    color: crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
                }],
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
    }

    #[test]
    fn test_velocity_map_gap_warns() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            velocity_colors: Some(crate::config::types::VelocityColorMap {
                ranges: vec![
                    crate::config::types::VelocityRange {
                        min: 0,
                        max: 30,
                        color: crate::config::types::RgbColor { r: 0, g: 255, b: 0 },
                    },
                    crate::config::types::VelocityRange {
                        min: 50, // gap: 31-49 unmapped
                        max: 127,
                        color: crate::config::types::RgbColor { r: 255, g: 0, b: 0 },
                    },
                ],
            }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(report.is_valid()); // gaps are warnings, not errors
        assert!(report.warnings.iter().any(|w| w.message.contains("Gap")));
    }

    #[test]
    fn test_velocity_map_empty_ranges_fails() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            velocity_colors: Some(crate::config::types::VelocityColorMap { ranges: vec![] }),
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(!report.is_valid());
    }

    #[test]
    fn test_velocity_map_no_config_backward_compat() {
        let mut config = default_config();
        config.led = Some(crate::config::types::LedConfig {
            velocity_colors: None,
            ..Default::default()
        });
        let report = validate_config(&config);
        assert!(report.is_valid());
    }

    // ── D10: Endpoint identity validation rules (ADR-035) ──
    //
    // Migrated from the removed `DeviceIdentityConfig`/`DevicePortBinding`
    // lowering fixtures. The legacy binding model (separate `input`/`output`
    // `DevicePortBinding`s + OSC host/port carried on a binding) no longer
    // exists, so the tests that solely exercised that lowering — empty
    // input/output `DevicePortBinding.matchers`, OSC-on-a-binding host/port
    // completeness, and the matchers↔input coexistence warning — were deleted.
    // The matchers-only / output-only / no-matchers invariants survive against
    // the unified `[[endpoints]]` schema.

    #[test]
    fn test_endpoint_no_matchers_is_error() {
        let mut config = default_config();
        config.endpoints = vec![EndpointConfig {
            alias: "empty".to_string(),
            direction: ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers: vec![],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }];
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("no matchers"))
        );
    }

    #[test]
    fn test_endpoint_with_only_matchers_is_valid() {
        let mut config = default_config();
        config.endpoints = vec![ep_input(
            "pads",
            vec![DeviceMatcher::NameContains {
                value: "Mikro".to_string(),
            }],
        )];
        let report = validate_config(&config);
        assert!(report.is_valid(), "Endpoint with matchers should be valid");
    }

    #[test]
    fn test_endpoint_with_only_output_is_valid() {
        let mut config = default_config();
        config.endpoints = vec![EndpointConfig {
            alias: "synth-out".to_string(),
            direction: ConnectorDirection::Output,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers: vec![],
                input_matchers: vec![],
                output_matchers: vec![DeviceMatcher::NameContains {
                    value: "IAC".to_string(),
                }],
                no_probe: false,
            },
        }];
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "Output-only endpoint should be valid: {:?}",
            report.errors
        );
    }

    /// Helper: a `[[endpoints]]` Matcher endpoint with the given direction and
    /// asymmetric matcher sets (ADR-035 direction↔matcher checks).
    fn endpoint_with_matchers(
        alias: &str,
        direction: crate::config::types::ConnectorDirection,
        input_matchers: Vec<DeviceMatcher>,
        output_matchers: Vec<DeviceMatcher>,
    ) -> crate::config::types::EndpointConfig {
        crate::config::types::EndpointConfig {
            alias: alias.to_string(),
            direction,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: crate::config::types::EndpointKind::Matcher {
                matchers: vec![],
                input_matchers,
                output_matchers,
                no_probe: false,
            },
        }
    }

    #[test]
    fn test_endpoint_input_direction_with_output_matchers_is_error() {
        use crate::config::types::ConnectorDirection;
        let mut config = default_config();
        config.endpoints = vec![endpoint_with_matchers(
            "mis-directed",
            ConnectorDirection::Input,
            vec![DeviceMatcher::name_contains("In")],
            vec![DeviceMatcher::name_contains("Out")], // ignored by effective_matchers(Input)
        )];
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(
            report.errors.iter().any(|e| e
                .message
                .contains("direction = Input but defines `output_matchers`")),
            "Input endpoint with output_matchers must be a hard error, not a silent no-op"
        );
    }

    #[test]
    fn test_endpoint_output_direction_with_input_matchers_is_error() {
        use crate::config::types::ConnectorDirection;
        let mut config = default_config();
        config.endpoints = vec![endpoint_with_matchers(
            "mis-directed",
            ConnectorDirection::Output,
            vec![DeviceMatcher::name_contains("In")], // ignored by effective_matchers(Output)
            vec![DeviceMatcher::name_contains("Out")],
        )];
        let report = validate_config(&config);
        assert!(!report.is_valid());
        assert!(report.errors.iter().any(|e| {
            e.message
                .contains("direction = Output but defines `input_matchers`")
        }));
    }

    #[test]
    fn test_endpoint_bidirectional_with_both_matchers_is_valid() {
        use crate::config::types::ConnectorDirection;
        let mut config = default_config();
        config.endpoints = vec![endpoint_with_matchers(
            "split",
            ConnectorDirection::Bidirectional,
            vec![DeviceMatcher::name_contains("In")],
            vec![DeviceMatcher::name_contains("Out")],
        )];
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "Bidirectional legitimately uses both input_matchers and output_matchers: {:?}",
            report.errors
        );
    }

    /// Build a HID `Matcher` endpoint with the given direction.
    fn hid_endpoint(
        alias: &str,
        direction: crate::config::types::ConnectorDirection,
    ) -> crate::config::types::EndpointConfig {
        crate::config::types::EndpointConfig {
            alias: alias.to_string(),
            direction,
            protocol: Some(crate::config::types::ConnectorProtocol::Hid),
            description: None,
            enabled: true,
            channels: vec![],
            kind: crate::config::types::EndpointKind::Matcher {
                matchers: vec![DeviceMatcher::name_contains("Xbox")],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }
    }

    #[test]
    fn test_endpoint_hid_non_input_is_error() {
        use crate::config::types::ConnectorDirection;
        for dir in [
            ConnectorDirection::Output,
            ConnectorDirection::Bidirectional,
        ] {
            let mut config = default_config();
            config.endpoints = vec![hid_endpoint("xbox", dir)];
            let report = validate_config(&config);
            assert!(!report.is_valid(), "HID {dir:?} must be rejected");
            assert!(
                report
                    .errors
                    .iter()
                    .any(|e| e.message.contains("HID is input-only")),
                "expected HID input-only error for {dir:?}, got {:?}",
                report.errors
            );
        }
    }

    #[test]
    fn test_endpoint_hid_input_is_valid() {
        use crate::config::types::ConnectorDirection;
        let mut config = default_config();
        config.endpoints = vec![hid_endpoint("xbox", ConnectorDirection::Input)];
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "HID Input endpoint should be valid: {:?}",
            report.errors
        );
    }

    /// A MIDI **output** endpoint for HID→MIDI route tests (mirrors
    /// `hid_endpoint`'s shape with `protocol = Midi`).
    fn midi_output_endpoint(alias: &str) -> crate::config::types::EndpointConfig {
        crate::config::types::EndpointConfig {
            alias: alias.to_string(),
            direction: crate::config::types::ConnectorDirection::Output,
            protocol: Some(crate::config::types::ConnectorProtocol::Midi),
            description: None,
            enabled: true,
            channels: vec![],
            kind: crate::config::types::EndpointKind::Matcher {
                matchers: vec![DeviceMatcher::name_contains("Synth")],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }
    }

    fn hid_to_midi_route(channel: u8, cc: u8) -> crate::config::types::RouteConfig {
        use std::collections::HashMap;
        let mut trigger_to_cc = HashMap::new();
        trigger_to_cc.insert("south".to_string(), cc);
        crate::config::types::RouteConfig {
            from: "xbox".to_string(),
            to: "synth".to_string(),
            transform: Some(crate::config::types::SignalTransform::HidToMidi {
                trigger_to_cc,
                channel,
            }),
            filter: None,
            enabled: true,
            description: None,
            modes: vec![],
        }
    }

    #[test]
    fn test_hid_to_midi_out_of_range_is_rejected() {
        // ADR-039-B: out-of-range HidToMidi channel/CC
        // must be REJECTED at config-load, not silently masked at runtime.
        use crate::config::types::ConnectorDirection;
        let mut config = default_config();
        config.endpoints = vec![
            hid_endpoint("xbox", ConnectorDirection::Input),
            midi_output_endpoint("synth"),
        ];
        config.routes = vec![hid_to_midi_route(20, 200)]; // channel > 15, cc > 127
        let report = validate_config(&config);
        assert!(
            !report.is_valid(),
            "out-of-range HidToMidi must be rejected"
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("channel 20 out of range")),
            "expected channel range error, got {:?}",
            report.errors
        );
        assert!(
            report.errors.iter().any(|e| e.message.contains("CC 200")),
            "expected CC range error, got {:?}",
            report.errors
        );
    }

    #[test]
    fn test_hid_to_midi_valid_ranges_accepted() {
        use crate::config::types::ConnectorDirection;
        let mut config = default_config();
        config.endpoints = vec![
            hid_endpoint("xbox", ConnectorDirection::Input),
            midi_output_endpoint("synth"),
        ];
        config.routes = vec![hid_to_midi_route(5, 20)]; // in range
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "in-range HidToMidi route should validate: {:?}",
            report.errors
        );
    }

    fn osc_output_endpoint(alias: &str) -> crate::config::types::EndpointConfig {
        crate::config::types::EndpointConfig {
            alias: alias.to_string(),
            direction: crate::config::types::ConnectorDirection::Output,
            protocol: None, // OscEndpoint kind ⇒ effective protocol = Osc
            description: None,
            enabled: true,
            channels: vec![],
            kind: crate::config::types::EndpointKind::OscEndpoint {
                host: "127.0.0.1".to_string(),
                port: 9000,
                security: Default::default(),
            },
        }
    }

    fn hid_to_osc_route(address: &str) -> crate::config::types::RouteConfig {
        use std::collections::HashMap;
        let mut trigger_to_address = HashMap::new();
        trigger_to_address.insert("south".to_string(), address.to_string());
        crate::config::types::RouteConfig {
            from: "xbox".to_string(),
            to: "osc_out".to_string(),
            transform: Some(crate::config::types::SignalTransform::HidToOsc {
                trigger_to_address,
                value_to_float: true,
            }),
            filter: None,
            enabled: true,
            description: None,
            modes: vec![],
        }
    }

    #[test]
    fn test_hid_to_osc_invalid_address_is_rejected() {
        // ADR-039-B: an OSC address not starting with '/' must be
        // rejected at config-load (else an invalid OSC packet is emitted).
        use crate::config::types::ConnectorDirection;
        let mut config = default_config();
        config.endpoints = vec![
            hid_endpoint("xbox", ConnectorDirection::Input),
            osc_output_endpoint("osc_out"),
        ];
        config.routes = vec![hid_to_osc_route("pad/a")]; // missing leading '/'
        let report = validate_config(&config);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("OSC addresses must start with '/'")),
            "expected OSC address error, got {:?}",
            report.errors
        );
    }

    #[test]
    fn test_hid_to_osc_valid_address_accepted() {
        use crate::config::types::ConnectorDirection;
        let mut config = default_config();
        config.endpoints = vec![
            hid_endpoint("xbox", ConnectorDirection::Input),
            osc_output_endpoint("osc_out"),
        ];
        config.routes = vec![hid_to_osc_route("/pad/a")]; // valid
        let report = validate_config(&config);
        assert!(
            !report
                .errors
                .iter()
                .any(|e| e.message.contains("OSC addresses must start with '/'")),
            "valid OSC address must not trip the address check: {:?}",
            report.errors
        );
    }

    #[test]
    fn test_hid_endpoint_let_through_is_error() {
        // ADR-035 Phase 2 regression: a non-gamepad trigger whose `device`
        // filter resolves to a HID *endpoint* must still trip the ADR-038 §4.3
        // let-through hard error. Pre-Phase-2 the protocol map was built only
        // from [[bindings]] (now empty for any loaded config), so an endpoint's
        // `protocol = "hid"` went unseen and the invalid `let_through` slipped
        // through. `device_protocols` now unions `[[endpoints]]`.
        use crate::config::types::ConnectorDirection;
        let mut config = default_config();
        config.endpoints = vec![hid_endpoint("xbox", ConnectorDirection::Input)];
        config.modes = vec![Mode {
            name: "Default".into(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 36,
                    velocity_min: None,
                    channel: None,
                    device: Some("xbox".into()),
                },
                action: ActionConfig::Shell {
                    sandbox: None,
                    command: "echo hi".into(),
                    args: None,
                    timeout_ms: None,
                },
                description: None,
                let_through: true,
            }],
        }];
        let report = validate_config(&config);
        assert!(
            !report.is_valid(),
            "let_through on a HID-endpoint source must be rejected"
        );
        assert!(
            report.errors.iter().any(|e| e
                .message
                .contains("let_through = true on a HID-only source")),
            "expected the HID let-through hard error, got {:?}",
            report.errors
        );
    }

    #[test]
    fn test_endpoint_channel_out_of_range_is_error() {
        let mut config = default_config();
        let mut ep = ep_input(
            "drums",
            vec![DeviceMatcher::NameContains {
                value: "Drums".to_string(),
            }],
        );
        ep.channels = vec![9, 16]; // 16 is out of range (valid: 0-15)
        config.endpoints = vec![ep];
        let report = validate_config(&config);
        assert!(
            !report.is_valid(),
            "Channel 16 should cause a validation error"
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("channel 16")
                    && e.message.contains("out of range")
                    && e.message.contains("endpoint"))
        );
    }

    #[test]
    fn test_endpoint_channel_valid_range_ok() {
        let mut config = default_config();
        let mut ep = ep_input(
            "drums",
            vec![DeviceMatcher::NameContains {
                value: "Drums".to_string(),
            }],
        );
        ep.channels = vec![0, 9, 15]; // All valid
        config.endpoints = vec![ep];
        let report = validate_config(&config);
        // No channel errors (there may be other warnings but channels should be fine)
        assert!(!report.errors.iter().any(|e| e.path.contains("channels")));
    }

    #[test]
    fn test_hid_protocol_with_channels_warns() {
        // HID devices don't have MIDI channels — channels field is meaningless
        let mut config = default_config();
        let mut ep = ep_input(
            "gamepad",
            vec![DeviceMatcher::NameContains {
                value: "Xbox".to_string(),
            }],
        );
        ep.protocol = Some(ConnectorProtocol::Hid);
        ep.channels = vec![9];
        config.endpoints = vec![ep];
        let report = validate_config(&config);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.message.contains("channels") && w.message.contains("Hid"))
        );
    }

    // ─── ADR-025 Phase 2: validate_condition ─────────

    fn make_config_with_condition(condition: crate::actions::Condition) -> Config {
        use crate::config::types::{ActionConfig, Mapping, Mode, Trigger};
        let mut cfg = default_config();
        // Declare the "keyboard" alias the existing tests use so the
        // device-known check (added in ADR-025 Phase 2.G) doesn't
        // mask the bounds-error assertions these tests actually care
        // about. Device-unknown behaviour has dedicated coverage in
        // the 2.G test block below.
        cfg.endpoints = vec![ep_input(
            "keyboard",
            vec![DeviceMatcher::NameContains {
                value: "keyboard".to_string(),
            }],
        )];
        cfg.modes = vec![Mode {
            name: "Default".into(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 36,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Conditional {
                    condition,
                    then_action: Box::new(ActionConfig::Shell {
                        sandbox: None,
                        command: "echo then".into(),
                        args: None,
                        timeout_ms: None,
                    }),
                    else_action: None,
                },
                description: None,
                let_through: false,
            }],
        }];
        cfg
    }

    #[test]
    fn validator_rejects_cc_value_in_range_with_min_greater_than_max() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::CcValueInRange {
            cc: 1,
            channel: 0,
            min: 80,
            max: 20,
            device: "keyboard".into(),
        });
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("min") && e.message.contains("max")),
            "expected min>max error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_cc_value_in_range_with_out_of_range_bounds() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::CcValueInRange {
            cc: 1,
            channel: 0,
            min: 0,
            max: 200,
            device: "keyboard".into(),
        });
        let report = validate_config(&cfg);
        assert!(report.errors.iter().any(|e| e.message.contains("max")));
    }

    #[test]
    fn validator_rejects_active_pc_is_missing_device() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::ActivePcIs {
            pc: 12,
            channel: 0,
            device: "".into(),
        });
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| e.message.contains("device")),
            "expected device-required error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_note_held_out_of_range() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::NoteHeld {
            note: 200,
            channel: 0,
            device: "keyboard".into(),
            ttl_override_ms: None,
        });
        let report = validate_config(&cfg);
        assert!(report.errors.iter().any(|e| e.message.contains("note")));
    }

    #[test]
    fn validator_recurses_into_and_or_not() {
        use crate::actions::Condition;
        // Nested broken condition inside And should still surface.
        let cfg = make_config_with_condition(Condition::And {
            conditions: vec![
                Condition::Always,
                Condition::CcValueInRange {
                    cc: 1,
                    channel: 0,
                    min: 80,
                    max: 20,
                    device: "keyboard".into(),
                },
            ],
        });
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("min") && e.message.contains("max")),
            "And should recurse into children; errors: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_accepts_well_formed_state_conditions() {
        use crate::actions::Condition;
        // "keyboard" is the alias seeded by `make_config_with_condition`.
        let cfg = make_config_with_condition(Condition::ActivePcIs {
            pc: 12,
            channel: 0,
            device: "keyboard".into(),
        });
        let report = validate_config(&cfg);
        assert!(
            report.errors.is_empty(),
            "well-formed condition must pass, got errors: {:?}",
            report.errors
        );
    }

    // ─── ADR-025 Phase 2.C: CcIsOn / CcIsOff sugar ──────────────────

    #[test]
    fn validator_accepts_well_formed_cc_is_on() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::CcIsOn {
            cc: 64,
            channel: 0,
            device: "keyboard".into(),
        });
        let report = validate_config(&cfg);
        assert!(
            report.errors.is_empty(),
            "CcIsOn must pass, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_accepts_well_formed_cc_is_off() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::CcIsOff {
            cc: 64,
            channel: 0,
            device: "keyboard".into(),
        });
        let report = validate_config(&cfg);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn validator_rejects_cc_is_on_missing_device() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::CcIsOn {
            cc: 64,
            channel: 0,
            device: "".into(),
        });
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("CcIsOn") && e.message.contains("device")),
            "expected CcIsOn device error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_cc_is_off_out_of_range() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::CcIsOff {
            cc: 200, // out of range
            channel: 0,
            device: "keyboard".into(),
        });
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("CcIsOff") && e.message.contains("cc")),
            "expected CcIsOff cc bounds error, got: {:?}",
            report.errors
        );
    }

    // ─── Symmetric coverage ──────────────────────────
    //
    // The shared validator arm uses a `matches!(condition, CcIsOn {..})`
    // check to pick which of the two error-message kinds to emit. These
    // tests pin both branches of that selection so a copy-paste mistake
    // — e.g. swapping `CcIsOn`/`CcIsOff` in the matches arm or in the
    // emitted message prefix — surfaces immediately instead of leaking
    // through unnoticed.

    #[test]
    fn validator_rejects_cc_is_off_missing_device() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::CcIsOff {
            cc: 64,
            channel: 0,
            device: "".into(),
        });
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("CcIsOff") && e.message.contains("device")),
            "expected CcIsOff device error (not CcIsOn), got: {:?}",
            report.errors
        );
        // Explicitly guard against the wrong kind being emitted.
        assert!(
            !report.errors.iter().any(|e| e.message.contains("CcIsOn")),
            "CcIsOff error should not mention CcIsOn; got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_cc_is_on_out_of_range() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::CcIsOn {
            cc: 200, // out of range
            channel: 0,
            device: "keyboard".into(),
        });
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("CcIsOn") && e.message.contains("cc")),
            "expected CcIsOn cc bounds error, got: {:?}",
            report.errors
        );
        assert!(
            !report.errors.iter().any(|e| e.message.contains("CcIsOff")),
            "CcIsOn error should not mention CcIsOff; got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_cc_is_on_bad_channel() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::CcIsOn {
            cc: 64,
            channel: 42, // out of range
            device: "keyboard".into(),
        });
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("CcIsOn") && e.message.contains("channel")),
            "expected CcIsOn channel error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_cc_is_off_bad_channel() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition(Condition::CcIsOff {
            cc: 64,
            channel: 42, // out of range
            device: "keyboard".into(),
        });
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("CcIsOff") && e.message.contains("channel")),
            "expected CcIsOff channel error, got: {:?}",
            report.errors
        );
    }

    // ─── ADR-025 Phase 2.G: context-switch validator ──────────
    //
    // Two new classes of check on top of the field-level bounds that
    // already shipped in 2.D:
    //
    //   1. CcContextSwitch range overlap detection — first-match-wins
    //      at runtime, so overlapping ranges silently mask later
    //      branches. Detect pairwise, order-independent.
    //   2. Device alias resolution — unknown device in a condition or
    //      context-switch is an ERROR (stronger than trigger's WARNING,
    //      because state conditions can't observe a device that isn't
    //      bound to the store).

    fn device_identity(alias: &str) -> EndpointConfig {
        ep_input(
            alias,
            vec![DeviceMatcher::NameContains {
                value: alias.to_string(),
            }],
        )
    }

    fn make_config_with_action_and_devices(
        action: ActionConfig,
        devices: Vec<EndpointConfig>,
    ) -> Config {
        let mut cfg = default_config();
        cfg.endpoints = devices;
        cfg.modes = vec![Mode {
            name: "Default".into(),
            color: None,
            mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 36,
                    velocity_min: Some(1),
                    channel: None,
                    device: None,
                },
                action,
                description: None,
                let_through: false,
            }],
        }];
        cfg
    }

    fn make_config_with_condition_and_devices(
        condition: crate::actions::Condition,
        devices: Vec<EndpointConfig>,
    ) -> Config {
        make_config_with_action_and_devices(
            ActionConfig::Conditional {
                condition,
                then_action: Box::new(ActionConfig::Shell {
                    sandbox: None,
                    command: "echo ok".into(),
                    args: None,
                    timeout_ms: None,
                }),
                else_action: None,
            },
            devices,
        )
    }

    // ── Device alias resolution: context-switch actions ───────────────

    #[test]
    fn validator_rejects_unknown_device_in_pc_context_switch() {
        use indexmap::IndexMap;
        let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
        mappings.insert(
            0,
            Box::new(ActionConfig::Shell {
                sandbox: None,
                command: "echo a".into(),
                args: None,
                timeout_ms: None,
            }),
        );
        let cfg = make_config_with_action_and_devices(
            ActionConfig::PcContextSwitch {
                channel: 0,
                device: "unknown_alias".into(),
                mappings,
                default: None,
            },
            vec![], // no devices declared
        );
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| {
                e.message.contains("PcContextSwitch")
                    && e.message.contains("unknown device")
                    && e.message.contains("unknown_alias")
            }),
            "expected unknown-device error, got errors: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_unknown_device_in_cc_context_switch() {
        use crate::config::types::CcRange;
        let cfg = make_config_with_action_and_devices(
            ActionConfig::CcContextSwitch {
                cc: 1,
                channel: 0,
                device: "ghost".into(),
                ranges: vec![CcRange {
                    min: 0,
                    max: 63,
                    action: Box::new(ActionConfig::Shell {
                        sandbox: None,
                        command: "echo a".into(),
                        args: None,
                        timeout_ms: None,
                    }),
                }],
                default: None,
            },
            vec![],
        );
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| {
                e.message.contains("CcContextSwitch")
                    && e.message.contains("unknown device")
                    && e.message.contains("ghost")
            }),
            "expected unknown-device error, got errors: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_accepts_known_device_in_context_switch_and_condition() {
        use crate::actions::Condition;
        use crate::config::types::CcRange;
        let cfg = make_config_with_action_and_devices(
            ActionConfig::Conditional {
                condition: Condition::CcIsOn {
                    cc: 64,
                    channel: 0,
                    device: "fcb1010".into(),
                },
                then_action: Box::new(ActionConfig::CcContextSwitch {
                    cc: 7,
                    channel: 0,
                    device: "fcb1010".into(),
                    ranges: vec![CcRange {
                        min: 0,
                        max: 127,
                        action: Box::new(ActionConfig::Shell {
                            sandbox: None,
                            command: "echo a".into(),
                            args: None,
                            timeout_ms: None,
                        }),
                    }],
                    default: None,
                }),
                else_action: None,
            },
            vec![device_identity("fcb1010")],
        );
        let report = validate_config(&cfg);
        assert!(
            !report
                .errors
                .iter()
                .any(|e| e.message.contains("unknown device")),
            "expected no unknown-device error, got: {:?}",
            report.errors
        );
    }

    // ── Device alias resolution: state conditions ─────────────────────

    #[test]
    fn validator_rejects_unknown_device_in_active_pc_is() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition_and_devices(
            Condition::ActivePcIs {
                pc: 0,
                channel: 0,
                device: "phantom".into(),
            },
            vec![],
        );
        let report = validate_config(&cfg);
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("ActivePcIs")
                    && e.message.contains("unknown device")
                    && e.message.contains("phantom")),
            "expected ActivePcIs unknown-device error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_unknown_device_in_cc_value_in_range() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition_and_devices(
            Condition::CcValueInRange {
                cc: 1,
                channel: 0,
                min: 0,
                max: 63,
                device: "phantom".into(),
            },
            vec![],
        );
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| {
                e.message.contains("CcValueInRange")
                    && e.message.contains("unknown device")
                    && e.message.contains("phantom")
            }),
            "expected CcValueInRange unknown-device error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_unknown_device_in_note_held() {
        use crate::actions::Condition;
        let cfg = make_config_with_condition_and_devices(
            Condition::NoteHeld {
                note: 60,
                channel: 0,
                device: "phantom".into(),
                ttl_override_ms: None,
            },
            vec![],
        );
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| e.message.contains("NoteHeld")
                && e.message.contains("unknown device")
                && e.message.contains("phantom")),
            "expected NoteHeld unknown-device error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_unknown_device_in_cc_is_on_off() {
        use crate::actions::Condition;
        for cond in [
            Condition::CcIsOn {
                cc: 64,
                channel: 0,
                device: "phantom".into(),
            },
            Condition::CcIsOff {
                cc: 64,
                channel: 0,
                device: "phantom".into(),
            },
        ] {
            let name = match cond {
                Condition::CcIsOn { .. } => "CcIsOn",
                Condition::CcIsOff { .. } => "CcIsOff",
                _ => unreachable!(),
            };
            let cfg = make_config_with_condition_and_devices(cond, vec![]);
            let report = validate_config(&cfg);
            assert!(
                report.errors.iter().any(|e| {
                    e.message.contains(name)
                        && e.message.contains("unknown device")
                        && e.message.contains("phantom")
                }),
                "expected {} unknown-device error, got: {:?}",
                name,
                report.errors
            );
        }
    }

    // ── CcContextSwitch range overlap detection ───────────────────────

    fn cc_switch_with_ranges(ranges: Vec<(u8, u8)>) -> Config {
        use crate::config::types::CcRange;
        make_config_with_action_and_devices(
            ActionConfig::CcContextSwitch {
                cc: 1,
                channel: 0,
                device: "fcb1010".into(),
                ranges: ranges
                    .into_iter()
                    .map(|(min, max)| CcRange {
                        min,
                        max,
                        action: Box::new(ActionConfig::Shell {
                            sandbox: None,
                            command: "echo a".into(),
                            args: None,
                            timeout_ms: None,
                        }),
                    })
                    .collect(),
                default: None,
            },
            vec![device_identity("fcb1010")],
        )
    }

    #[test]
    fn validator_rejects_overlapping_cc_ranges_contained() {
        // [0, 100] fully contains [20, 40] — overlap error.
        let cfg = cc_switch_with_ranges(vec![(0, 100), (20, 40)]);
        let report = validate_config(&cfg);
        let overlap = report
            .errors
            .iter()
            .find(|e| {
                e.message.contains("CcContextSwitch")
                    && e.message.contains("overlap")
                    && e.message.contains("[0]")
                    && e.message.contains("[1]")
            })
            .unwrap_or_else(|| {
                panic!(
                    "expected overlap error naming both range indices, got: {:?}",
                    report.errors
                )
            });
        // Anchored to the later (masked) range so UIs can point
        // directly at the branch that will never fire.
        assert!(
            overlap.path.ends_with(".ranges[1]"),
            "overlap error should anchor to the masked range path, got path: {:?}",
            overlap.path
        );
    }

    #[test]
    fn validator_rejects_overlapping_cc_ranges_partial() {
        // [0, 50] partially overlaps [40, 80] at 40-50.
        let cfg = cc_switch_with_ranges(vec![(0, 50), (40, 80)]);
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| e.message.contains("overlap")),
            "expected partial overlap error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_overlapping_cc_ranges_unsorted_pair() {
        // Non-adjacent overlap in an unsorted list:
        // [0,20], [60,127], [10,15] — (10,15) overlaps (0,20) even
        // though they aren't adjacent in the Vec. The spec's naive
        // "prev_max only" check would miss this; we detect pairwise.
        let cfg = cc_switch_with_ranges(vec![(0, 20), (60, 127), (10, 15)]);
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| e.message.contains("overlap")),
            "expected non-adjacent overlap error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_rejects_overlapping_cc_ranges_identical() {
        let cfg = cc_switch_with_ranges(vec![(0, 50), (0, 50)]);
        let report = validate_config(&cfg);
        assert!(
            report.errors.iter().any(|e| e.message.contains("overlap")),
            "expected identical-range overlap error, got: {:?}",
            report.errors
        );
    }

    #[test]
    fn validator_accepts_adjacent_non_overlapping_cc_ranges() {
        // [0,49] then [50,99] then [100,127] — touching at min-1/max
        // boundary but non-overlapping. Valid config.
        let cfg = cc_switch_with_ranges(vec![(0, 49), (50, 99), (100, 127)]);
        let report = validate_config(&cfg);
        assert!(
            !report.errors.iter().any(|e| e.message.contains("overlap")),
            "expected no overlap error for adjacent ranges, got: {:?}",
            report.errors
        );
    }

    // ═══════════════════════════════════════════════════════════════════
    // ADR-027 D3 §3.2 — `allow_interpreters` policy
    //
    // The validator runs `resolve_effective_binary` against every Shell
    // action; when the resolved binary is a known interpreter family
    // and `advanced_settings.allow_interpreters` is `Deny` or `Warn`,
    // emits a config-load diagnostic. `Allow` is the opt-in escape
    // hatch for power users who deliberately rely on shell scripting.
    // ═══════════════════════════════════════════════════════════════════

    use crate::config::types::InterpreterPolicy;

    fn config_with_action_and_policy(action: ActionConfig, policy: InterpreterPolicy) -> Config {
        let mut config = config_with_action(action);
        config.advanced_settings.allow_interpreters = policy;
        config
    }

    #[test]
    fn allow_interpreters_default_is_warn() {
        // Default for new configs and `..default_config()` builders —
        // backwards-compat without users opting in, but the warning
        // surfaces the new gate so they're aware of the policy.
        let settings = crate::config::types::AdvancedSettings::default();
        assert_eq!(settings.allow_interpreters, InterpreterPolicy::Warn);
    }

    #[test]
    fn allow_interpreters_warn_emits_warning_for_sh() {
        // Default policy (Warn): `/bin/sh -c …` should produce a
        // validation WARNING, not an error. Config still loads.
        let config = config_with_action_and_policy(
            ActionConfig::Shell {
                sandbox: None,
                command: "/bin/sh".to_string(),
                args: Some(vec!["-c".to_string(), "echo hi".to_string()]),
                timeout_ms: None,
            },
            InterpreterPolicy::Warn,
        );
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "Warn policy keeps the config valid — got errors: {:?}",
            report.errors
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.message.contains("interpreter")
                    && (w.message.contains("sh") || w.message.contains("Sh"))),
            "expected an interpreter-family warning — got warnings: {:?}",
            report
                .warnings
                .iter()
                .map(|w| &w.message)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn allow_interpreters_deny_emits_error_for_sh() {
        let config = config_with_action_and_policy(
            ActionConfig::Shell {
                sandbox: None,
                command: "/bin/sh".to_string(),
                args: Some(vec!["-c".to_string(), "echo hi".to_string()]),
                timeout_ms: None,
            },
            InterpreterPolicy::Deny,
        );
        let report = validate_config(&config);
        assert!(
            !report.is_valid(),
            "Deny policy must reject interpreter invocations"
        );
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("interpreter")),
            "expected an interpreter-family error — got: {:?}",
            report.errors
        );
    }

    #[test]
    fn allow_interpreters_allow_emits_no_finding_for_sh() {
        // Explicit opt-in: users who know they want shell semantics.
        // No warning, no error.
        let config = config_with_action_and_policy(
            ActionConfig::Shell {
                sandbox: None,
                command: "/bin/sh".to_string(),
                args: Some(vec!["-c".to_string(), "echo hi".to_string()]),
                timeout_ms: None,
            },
            InterpreterPolicy::Allow,
        );
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "Allow policy keeps the config valid — got errors: {:?}",
            report.errors
        );
        assert!(
            !report
                .warnings
                .iter()
                .any(|w| w.message.contains("interpreter")),
            "Allow policy must NOT emit an interpreter warning — got: {:?}",
            report
                .warnings
                .iter()
                .map(|w| &w.message)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn allow_interpreters_warn_fires_on_env_wrapper() {
        // Wrapper unwinding: `env python -c …` resolves to python →
        // warn. Closes the canonical D3 bypass class.
        let config = config_with_action_and_policy(
            ActionConfig::Shell {
                sandbox: None,
                command: "/usr/bin/env".to_string(),
                args: Some(vec![
                    "python".to_string(),
                    "-c".to_string(),
                    "print(1)".to_string(),
                ]),
                timeout_ms: None,
            },
            InterpreterPolicy::Warn,
        );
        let report = validate_config(&config);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.message.contains("interpreter")
                    && w.message.to_lowercase().contains("python")),
            "expected a Python interpreter warning — got: {:?}",
            report
                .warnings
                .iter()
                .map(|w| &w.message)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn allow_interpreters_does_not_fire_on_non_interpreter_binary() {
        // `/bin/ls` is not an interpreter — no finding regardless of
        // policy. Verifies the resolver classified correctly + the
        // policy is gated on `family.is_some()`.
        for policy in [
            InterpreterPolicy::Allow,
            InterpreterPolicy::Warn,
            InterpreterPolicy::Deny,
        ] {
            let config = config_with_action_and_policy(
                ActionConfig::Shell {
                    sandbox: None,
                    command: "/bin/ls -la".to_string(),
                    args: None,
                    timeout_ms: None,
                },
                policy,
            );
            let report = validate_config(&config);
            assert!(
                report.is_valid(),
                "non-interpreter binary should never trip the policy ({:?}) — got: {:?}",
                policy,
                report.errors
            );
            assert!(
                !report
                    .warnings
                    .iter()
                    .any(|w| w.message.contains("interpreter")),
                "non-interpreter binary should never trip the policy ({:?}) — got warnings: {:?}",
                policy,
                report
                    .warnings
                    .iter()
                    .map(|w| &w.message)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn allow_interpreters_warn_message_includes_resolved_binary_path() {
        // Diagnostic UX: the warning should name the resolved binary
        // so users can find it (Phase 3 GUI editor will key its chip
        // off this same data).
        let config = config_with_action_and_policy(
            ActionConfig::Shell {
                sandbox: None,
                command: "/usr/bin/env".to_string(),
                args: Some(vec![
                    "python".to_string(),
                    "-c".to_string(),
                    "1".to_string(),
                ]),
                timeout_ms: None,
            },
            InterpreterPolicy::Warn,
        );
        let report = validate_config(&config);
        let interpreter_warning = report
            .warnings
            .iter()
            .find(|w| w.message.contains("interpreter"))
            .expect("expected an interpreter warning");
        assert!(
            interpreter_warning.message.contains("python"),
            "warning should name `python` — got: {:?}",
            interpreter_warning.message
        );
    }

    // ── Shadowed-mapping detection ─────────────────────────────
    //
    // The rule engine matches first-match-wins. If two mappings in the
    // same mode have overlapping triggers and the broader one appears
    // first, the narrower one never fires. These tests pin the shadow
    // detection on the four trigger types covered in v1: Note, CC,
    // Aftertouch, PolyAftertouch. Cross-type pairs and uncovered
    // variants must not produce false positives.

    fn note_trigger(
        note: u8,
        velocity_min: Option<u8>,
        channel: Option<u8>,
        device: Option<&str>,
    ) -> Trigger {
        Trigger::Note {
            note,
            velocity_min,
            channel,
            device: device.map(String::from),
        }
    }

    fn config_with_mappings(triggers: Vec<Trigger>) -> Config {
        Config {
            config_meta: Default::default(),
            endpoints: vec![],
            modes: vec![Mode {
                name: "Test".to_string(),
                color: None,
                mappings: triggers
                    .into_iter()
                    .enumerate()
                    .map(|(i, t)| Mapping {
                        trigger: t,
                        action: ActionConfig::Keystroke {
                            keys: format!("k{i}"),
                            modifiers: vec![],
                        },
                        description: Some(format!("mapping-{i}")),
                        let_through: false,
                    })
                    .collect(),
            }],
            ..default_config()
        }
    }

    fn shadow_warnings(report: &ValidationReport) -> Vec<&str> {
        report
            .warnings
            .iter()
            .filter(|w| w.message.contains("shadowed by mapping #"))
            .map(|w| w.message.as_str())
            .collect()
    }

    #[test]
    fn test_shadow_exact_duplicate_note_triggers_warns() {
        // The issue's primary example: two mappings with identical Note
        // triggers — the second never fires.
        let cfg = config_with_mappings(vec![
            note_trigger(60, None, None, None),
            note_trigger(60, None, None, None),
        ]);
        let report = validate_config(&cfg);
        let warnings = shadow_warnings(&report);
        assert_eq!(
            warnings.len(),
            1,
            "exact duplicate must produce exactly one shadow warning, got: {warnings:?}"
        );
        assert!(
            warnings[0].contains("shadowed by mapping #0"),
            "warning must point at the earlier shadowing mapping; got: {warnings:?}"
        );
    }

    #[test]
    fn test_shadow_velocity_min_subset_warns() {
        // Note{velocity_min:None} accepts any velocity; Note{velocity_min:80}
        // only accepts ≥80 — strict subset, so the second is shadowed.
        let cfg = config_with_mappings(vec![
            note_trigger(60, None, None, None),
            note_trigger(60, Some(80), None, None),
        ]);
        let report = validate_config(&cfg);
        let warnings = shadow_warnings(&report);
        assert_eq!(
            warnings.len(),
            1,
            "velocity-min subset must shadow; got: {warnings:?}"
        );
    }

    #[test]
    fn test_shadow_device_filter_subset_warns() {
        // Note without device filter (matches any device) covers Note that
        // requires device "mpk".
        let cfg = config_with_mappings(vec![
            note_trigger(60, None, None, None),
            note_trigger(60, None, None, Some("mpk")),
        ]);
        let report = validate_config(&cfg);
        let warnings = shadow_warnings(&report);
        assert_eq!(
            warnings.len(),
            1,
            "no-device-filter must shadow specific-device-filter; got: {warnings:?}"
        );
    }

    #[test]
    fn test_shadow_disjoint_devices_no_warn() {
        // Two specific-device-filter triggers on different devices are
        // disjoint — neither shadows the other.
        let cfg = config_with_mappings(vec![
            note_trigger(60, None, None, Some("mpk")),
            note_trigger(60, None, None, Some("mikro")),
        ]);
        let report = validate_config(&cfg);
        let warnings = shadow_warnings(&report);
        assert!(
            warnings.is_empty(),
            "disjoint device filters must NOT shadow; got: {warnings:?}"
        );
    }

    #[test]
    fn test_shadow_different_notes_no_warn() {
        let cfg = config_with_mappings(vec![
            note_trigger(60, None, None, None),
            note_trigger(61, None, None, None),
        ]);
        let report = validate_config(&cfg);
        let warnings = shadow_warnings(&report);
        assert!(
            warnings.is_empty(),
            "different note numbers must NOT shadow; got: {warnings:?}"
        );
    }

    #[test]
    fn test_shadow_cross_type_no_warn() {
        // A Note trigger and a CC trigger never overlap — different
        // ProcessedEvent types.
        let cfg = config_with_mappings(vec![
            note_trigger(60, None, None, None),
            Trigger::CC {
                cc: 7,
                value_min: None,
                channel: None,
                device: None,
            },
        ]);
        let report = validate_config(&cfg);
        let warnings = shadow_warnings(&report);
        assert!(
            warnings.is_empty(),
            "cross-type triggers must NOT shadow; got: {warnings:?}"
        );
    }

    #[test]
    fn test_shadow_cc_value_min_subset_warns() {
        let cfg = config_with_mappings(vec![
            Trigger::CC {
                cc: 7,
                value_min: None,
                channel: None,
                device: None,
            },
            Trigger::CC {
                cc: 7,
                value_min: Some(64),
                channel: None,
                device: None,
            },
        ]);
        let report = validate_config(&cfg);
        let warnings = shadow_warnings(&report);
        assert_eq!(
            warnings.len(),
            1,
            "CC value_min subset must shadow; got: {warnings:?}"
        );
    }

    #[test]
    fn test_shadow_unanalyzed_variant_no_warn() {
        // LongPress is intentionally not analysed in v1 — the
        // conservative default returns false. Two identical LongPress
        // triggers must NOT raise a shadow warning until subset rules
        // for that variant are added.
        let cfg = config_with_mappings(vec![
            Trigger::LongPress {
                note: 60,
                duration_ms: Some(2000),
                channel: None,
                device: None,
            },
            Trigger::LongPress {
                note: 60,
                duration_ms: Some(2000),
                channel: None,
                device: None,
            },
        ]);
        let report = validate_config(&cfg);
        let warnings = shadow_warnings(&report);
        assert!(
            warnings.is_empty(),
            "unanalyzed variant must NOT shadow in v1; got: {warnings:?}"
        );
    }

    // ── MidiForward raw-port-name target lint ─────────────────
    //
    // A `MidiForward.target` that matches a `[[bindings]]` alias gets
    // hot-plug-aware output routing (the rescan loop refreshes the
    // device output map for aliased outputs). A raw port-name target
    // bypasses that map entirely — it still forwards, but receives no
    // hot-plug liveness, status pill, mute affordance, or future
    // ADR-031 connector treatment. The validator emits a non-blocking
    // warning so the operator can choose to define a binding.

    fn midi_forward(target: &str) -> ActionConfig {
        ActionConfig::MidiForward {
            target: target.to_string(),
            transform: None,
        }
    }

    // ── ADR-047 §D3a: frozen legacy gamepad sentinel (id 255) ──

    #[test]
    fn test_gamepad_analog_stick_accepts_dpad_axis_ids_d3b() {
        // ADR-047 §D3b: 147/148 (d-pad-as-axis) must validate, kept in sync with
        // the matcher in mapping.rs. Out-of-range still errors.
        for axis in [128u8, 131, 147, 148] {
            let cfg = config_with_mappings(vec![Trigger::GamepadAnalogStick {
                axis,
                direction: None,
                device: None,
            }]);
            let report = validate_config(&cfg);
            assert!(
                report.is_valid(),
                "axis {axis} should validate; errors: {:?}",
                report.errors
            );
        }
        let bad = config_with_mappings(vec![Trigger::GamepadAnalogStick {
            axis: 200,
            direction: None,
            device: None,
        }]);
        assert!(
            !validate_config(&bad).is_valid(),
            "axis 200 is out of range and must error"
        );
    }

    #[test]
    fn test_gamepad_button_255_warns() {
        let cfg = config_with_mappings(vec![Trigger::GamepadButton {
            button: 255,
            velocity_min: None,
            device: None,
        }]);
        let report = validate_config(&cfg);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.message.contains("255") && w.message.contains("ADR-047")),
            "button 255 must warn; got: {:?}",
            report.warnings
        );
    }

    #[test]
    fn test_gamepad_chord_containing_255_warns() {
        let cfg = config_with_mappings(vec![Trigger::GamepadButtonChord {
            buttons: vec![128, 255],
            timeout_ms: None,
            device: None,
        }]);
        let report = validate_config(&cfg);
        assert!(
            report.warnings.iter().any(|w| w.message.contains("255")),
            "chord containing 255 must warn; got: {:?}",
            report.warnings
        );
    }

    #[test]
    fn test_valid_gamepad_button_does_not_warn_255() {
        let cfg = config_with_mappings(vec![Trigger::GamepadButton {
            button: 128, // South — valid
            velocity_min: None,
            device: None,
        }]);
        let report = validate_config(&cfg);
        assert!(
            !report
                .warnings
                .iter()
                .any(|w| w.message.contains("legacy") && w.message.contains("255")),
            "valid button must not warn about 255; got: {:?}",
            report.warnings
        );
    }

    #[test]
    fn test_disable_legacy_sentinel_drops_only_255_binds() {
        let mut cfg = config_with_mappings(vec![
            Trigger::GamepadButton {
                button: 128,
                velocity_min: None,
                device: None,
            },
            Trigger::GamepadButton {
                button: 255,
                velocity_min: None,
                device: None,
            },
            Trigger::GamepadButtonChord {
                buttons: vec![129, 255],
                timeout_ms: None,
                device: None,
            },
        ]);
        let removed = disable_legacy_gamepad_sentinel_binds(&mut cfg);
        assert_eq!(
            removed, 2,
            "the 255 button and the 255-chord must be dropped"
        );
        let remaining: Vec<_> = cfg.modes[0].mappings.iter().map(|m| &m.trigger).collect();
        assert_eq!(remaining.len(), 1);
        assert!(
            matches!(remaining[0], Trigger::GamepadButton { button: 128, .. }),
            "only the valid button-128 mapping survives"
        );
    }

    #[test]
    fn test_midi_forward_raw_port_name_warns() {
        // Target doesn't match any [[bindings]] alias → raw port name.
        let config = config_with_action(midi_forward("Komplete Audio 6 MK2"));
        let report = validate_config(&config);
        // Non-blocking: the config is still valid.
        assert!(
            report.is_valid(),
            "raw-port-name target must NOT be a hard error"
        );
        let warning = report
            .warnings
            .iter()
            .find(|w| w.message.contains("MidiForward target"))
            .expect("expected a MidiForward raw-port-name warning");
        assert!(
            warning.message.contains("Komplete Audio 6 MK2"),
            "warning must name the target; got: {}",
            warning.message
        );
        assert!(
            warning.message.contains("[[endpoints]]"),
            "warning must point at the [[endpoints]] remediation; got: {}",
            warning.message
        );
    }

    #[test]
    fn test_midi_forward_aliased_target_no_warn() {
        // Target matches a defined [[bindings]] alias → no warning.
        let mut config = config_with_action(midi_forward("studio_out"));
        config.endpoints = vec![ep_input(
            "studio_out",
            vec![DeviceMatcher::NameContains {
                value: "Komplete".to_string(),
            }],
        )];
        let report = validate_config(&config);
        assert!(report.is_valid());
        assert!(
            !report
                .warnings
                .iter()
                .any(|w| w.message.contains("MidiForward target")),
            "aliased target must NOT produce the raw-port-name warning"
        );
    }

    #[test]
    fn test_midi_forward_empty_target_still_errors_not_warns() {
        // Empty target is a hard error (pre-existing behaviour); the
        // raw-port-name warning must not replace or suppress it.
        let config = config_with_action(midi_forward(""));
        let report = validate_config(&config);
        assert!(!report.is_valid(), "empty target must remain a hard error");
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.message.contains("MidiForward requires target")),
            "empty target must keep the 'requires target port name' error"
        );
        assert!(
            !report
                .warnings
                .iter()
                .any(|w| w.message.contains("does not match any [[endpoints]]")),
            "empty target must NOT also emit the raw-port-name warning"
        );
    }

    // ── Input-direction connectors are off-spec (ADR-031 §3.4) ──
    //
    // ADR-031 line 124/136/143 establishes that input identity is
    // configured via [[bindings]] (ADR-022); [[connectors]] carries
    // OUTPUT or BIDIRECTIONAL endpoints only. An input-direction
    // connector is structurally dead — `PortResolver` only walks
    // [[bindings]], so dispatched events never carry that alias and
    // any routes keyed on it silently never fire.

    fn matcher_endpoint(name_contains: &str) -> EndpointKind {
        EndpointKind::Matcher {
            input_matchers: Vec::new(),
            output_matchers: Vec::new(),
            matchers: vec![DeviceMatcher::NameContains {
                value: name_contains.to_string(),
            }],
            no_probe: false,
        }
    }

    fn connector(alias: &str, direction: ConnectorDirection) -> EndpointConfig {
        EndpointConfig {
            alias: alias.to_string(),
            direction,
            protocol: Some(ConnectorProtocol::Midi),
            description: None,
            enabled: true,
            channels: vec![],
            kind: matcher_endpoint(alias),
        }
    }

    #[test]
    fn test_validate_accepts_input_direction_connector_adr035() {
        // ADR-035 REMOVED the `direction = Input` endpoint rejection —
        // input endpoints are now first-class (unblocks ADR-039 input listeners).
        let mut config = default_config();
        config
            .endpoints
            .push(connector("mpk_input", ConnectorDirection::Input));
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "input-direction endpoint is now accepted (ADR-035): {:?}",
            report.errors
        );
    }

    #[test]
    fn test_validate_accepts_output_and_bidirectional_connectors() {
        let mut config = default_config();
        config
            .endpoints
            .push(connector("absynth_output", ConnectorDirection::Output));
        config
            .endpoints
            .push(connector("iac_bus", ConnectorDirection::Bidirectional));
        let report = validate_config(&config);
        assert!(
            report.is_valid(),
            "output + bidirectional must be accepted: {:?}",
            report.errors
        );
    }
}
