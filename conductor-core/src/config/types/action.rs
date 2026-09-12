// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! The ActionConfig enum and CC ranges.

use super::*;

/// Action configuration types
///
/// Defines different actions that can be executed when a trigger is detected.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ActionConfig {
    /// Simulate keyboard keystroke(s) with optional modifiers
    ///
    /// # Examples
    /// ```toml
    /// [action]
    /// type = "Keystroke"
    /// keys = "space"
    /// modifiers = ["cmd"]
    /// ```
    Keystroke {
        /// Key name or sequence (e.g., "space", "Return", "Escape")
        keys: String,
        /// Modifier keys (e.g., "cmd", "shift", "alt", "ctrl")
        #[serde(default)]
        modifiers: Vec<String>,
    },

    /// Type a text string
    ///
    /// Simulates typing the provided text character by character.
    Text {
        /// Text to type
        text: String,
    },

    /// Launch an application
    ///
    /// Attempts to open the specified application by name or path.
    Launch {
        /// Application name or path to executable
        app: String,
    },

    /// Execute a shell command
    ///
    /// Runs an arbitrary shell command. Be cautious with untrusted config files.
    ///
    /// Two schema shapes (ADR-027 D3 §3.1):
    ///
    /// - **Legacy single-string** (`command = "echo hello world"`, `args` omitted):
    ///   the executor whitespace-splits `command` into argv at run time.
    ///   Kept for backward compatibility — every existing config in the
    ///   wild uses this shape.
    /// - **Argv form** (`command = "/bin/sh"`, `args = ["-c", "..."]`):
    ///   `command` is the resolved binary; `args` is argv[1..], passed
    ///   straight to `Command::args` (the spawn produces an OS argv of
    ///   `[command] ++ args` — i.e. `command` is itself argv[0]; the
    ///   caller does NOT repeat it inside `args`). No whitespace
    ///   tokenisation, no parser-defined quote handling, no
    ///   `parse_command_line`.
    ///
    /// `args = Some(vec![])` is distinct from `args = None` — `Some([])`
    /// means "argv-form invocation with zero arguments", `None` means
    /// "legacy whitespace-split form". Round-trips through serde
    /// preserve the distinction; the serialiser omits `args` entirely
    /// for `None` (no `args = null` leak that would confuse diff
    /// rendering or downstream tooling).
    ///
    /// **Validator behaviour.** Config validation extends the same
    /// shell-metacharacter blocklist that `command` already gets to
    /// every entry of `args` — so an argv form like
    /// `command = "/bin/sh", args = ["-c", "env > /tmp/leak"]`
    /// deserialises successfully (the schema accepts the shape) but
    /// will be rejected at config load with a `Shell argument contains
    /// dangerous pattern '>'` error. Argv form does **not** unlock
    /// redirects, pipes, command chaining, or substitution — Phase 2's
    /// `allow_interpreters` policy adds the additional guard against
    /// explicit interpreter invocation. See
    /// `conductor-core/src/config/validation.rs::validate_shell_arg`
    /// for the exact blocklist applied to args.
    Shell {
        /// Shell command — legacy form: full command line including
        /// args; argv form: resolved binary path (use `args` for the
        /// actual arguments).
        command: String,
        /// Argv array (argv form only). When `Some`, the executor passes
        /// these directly to `Command::args` and skips
        /// `parse_command_line`. When `None`, the legacy whitespace-split
        /// path runs against `command`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<Vec<String>>,
        /// Per-action timeout in milliseconds (ADR-027 D7).
        /// `None` falls back to `DEFAULT_SHELL_TIMEOUT_MS` (30s). The
        /// validator clamps to [1000, 300000] to keep the watchdog
        /// useful (sub-second timeouts kill kid-script shells, multi-
        /// minute timeouts defeat the purpose).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Per-action OS-sandbox profile override (ADR-027 §D10b). When
        /// present, widens the default deny-write / deny-network confinement
        /// (macOS Seatbelt / Linux Landlock) for this action only. `None`
        /// uses the default profile.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox: Option<ShellSandboxConfig>,
    },

    /// Execute a sequence of actions in order
    ///
    /// Executes multiple actions sequentially (useful for complex behaviors).
    Sequence {
        /// List of actions to execute in order
        actions: Vec<ActionConfig>,
    },

    /// Delay for a specified duration (in milliseconds)
    ///
    /// Pauses execution for the given duration. Useful in sequences.
    Delay {
        /// Delay duration in milliseconds
        ms: u64,
    },

    /// Simulate mouse click
    ///
    /// Clicks at the current or specified location with the specified button.
    MouseClick {
        /// Mouse button: "left", "right", "middle"
        button: String,
        /// X coordinate (optional, uses current mouse position if not specified)
        x: Option<i32>,
        /// Y coordinate (optional, uses current mouse position if not specified)
        y: Option<i32>,
    },

    /// Control system volume
    ///
    /// Adjusts or sets the system volume.
    VolumeControl {
        /// Operation: "Up", "Down", "Mute", "Unmute", "Set"
        operation: String,
        /// Volume level (0-100) for "Set" operation
        #[serde(default)]
        value: Option<u8>,
    },

    /// Switch to a different mode
    ///
    /// Changes the active mapping mode by name.
    ModeChange {
        /// Name of the mode to switch to
        mode: String,
    },

    /// Repeat an action multiple times
    ///
    /// Executes the specified action the given number of times.
    Repeat {
        /// Action to repeat
        action: Box<ActionConfig>,
        /// Number of times to repeat
        count: usize,
        /// Optional delay in milliseconds between repetitions
        #[serde(default)]
        delay_ms: Option<u64>,
    },

    /// Conditional action execution
    ///
    /// Executes different actions based on a condition.
    /// Supports time-based, app-based, mode-based conditions and logical operators.
    Conditional {
        /// Condition to evaluate at runtime
        condition: Condition,
        /// Action to execute if condition is true
        then_action: Box<ActionConfig>,
        /// Optional action to execute if condition is false
        #[serde(default)]
        else_action: Option<Box<ActionConfig>>,
    },

    // `PcContextSwitch.mappings` below uses a helper to round-trip
    // `IndexMap<u8, ...>` through TOML string keys. See
    // [`string_keyed_pc_map`] at the bottom of this file.
    /// Program-change context switch (ADR-025 Phase 2.D).
    ///
    /// Dispatches to one of several inner actions based on the most-
    /// recently-observed Program Change on the given `(device, channel)`
    /// tuple. Config-layer sugar — lowers at compile time (task #24) to
    /// a nested `Action::Conditional` chain keyed by `ActivePcIs`, or
    /// to a specialised `Action::ContextSwitchTable` when the branch
    /// count exceeds `MAX_LINEAR_BRANCHES` (task #25).
    ///
    /// Intended use: one-pedal-many-functions routing for MIDI foot
    /// controllers like the Behringer FCB1010 — a single expression-
    /// pedal CC drives different `SendMidi` actions per preset stomp.
    PcContextSwitch {
        /// MIDI channel of the target device to watch for PC.
        channel: u8,
        /// Device alias or binding ref whose physical state is read.
        device: String,
        /// Per-PC branches. `IndexMap` preserves TOML authoring order
        /// so earlier branches take priority after lowering.
        #[serde(with = "string_keyed_pc_map")]
        mappings: indexmap::IndexMap<u8, Box<ActionConfig>>,
        /// Fallback action when the active PC matches no branch and
        /// no PC has been observed yet. Omit for no-op fallback.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Box<ActionConfig>>,
    },

    /// CC-value-range context switch (ADR-025 Phase 2.D).
    ///
    /// Dispatches to one of several inner actions based on the most-
    /// recently-observed value of a given CC. Each range is an
    /// inclusive `[min, max]` window; the first matching range wins
    /// after lowering. Intended use: zoned controllers (modwheel,
    /// expression pedal soft/hard zones, ribbon controllers).
    CcContextSwitch {
        /// CC number whose value is consulted (0-127).
        cc: u8,
        /// MIDI channel of the target device to watch for the CC.
        channel: u8,
        /// Device alias or binding ref whose physical state is read.
        device: String,
        /// Ordered list of value ranges and their actions. Validator
        /// (task #26) will flag overlapping ranges and min > max.
        ranges: Vec<CcRange>,
        /// Fallback action when no range matches and no CC observed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Box<ActionConfig>>,
    },

    /// Send MIDI message
    ///
    /// Sends a MIDI message to a virtual or physical output port.
    /// Supports Note, CC, Program Change, Pitch Bend, and Aftertouch messages.
    SendMidi {
        /// Target MIDI output port name
        port: String,
        /// MIDI message type: "NoteOn", "NoteOff", "CC", "ProgramChange", "PitchBend", "Aftertouch"
        message_type: String,
        /// MIDI channel (0-15)
        channel: u8,
        /// Note number (0-127) for Note messages
        #[serde(default)]
        note: Option<u8>,
        /// Velocity (0-127) for Note messages
        #[serde(default)]
        velocity: Option<u8>,
        /// Controller number (0-127) for CC messages
        #[serde(default)]
        controller: Option<u8>,
        /// Controller value (0-127) for CC messages
        #[serde(default)]
        value: Option<u8>,
        /// Program number (0-127) for Program Change messages
        #[serde(default)]
        program: Option<u8>,
        /// Pitch bend value (-8192 to +8191) for Pitch Bend messages
        #[serde(default)]
        pitch: Option<i16>,
        /// Aftertouch pressure (0-127) for Aftertouch messages
        #[serde(default)]
        pressure: Option<u8>,
    },

    /// Forward MIDI data to an output port with optional transform (ADR-009 Gap 2)
    ///
    /// Passes raw MIDI bytes from the triggering event through an optional
    /// transform and sends them to the named output port.
    MidiForward {
        /// Target MIDI output port name
        target: String,
        /// Optional transform to apply before forwarding
        #[serde(default)]
        transform: Option<MidiTransform>,
    },

    /// Forward a gamepad (HID) event to a cross-protocol output endpoint
    /// (ADR-039-B).
    ///
    /// The mapping-triggered analogue of a HID route: where a route forwards
    /// a gamepad input endpoint unconditionally, `HidForward` fires only when
    /// its mapping's trigger condition is met (e.g. a long-press or chord on a
    /// gamepad button), then translates the *structured* triggering event to
    /// the target's protocol and sends it.
    ///
    /// `transform` is REQUIRED (unlike `MidiForward`'s optional MIDI→MIDI
    /// passthrough): a HID event cannot exist on a MIDI wire without
    /// translation, and the gamepad→MIDI byte serialization is lossy
    /// (button 128 → note 0), so an explicit structured transform is
    /// mandatory. **V1 forwards to a MIDI output only** — the transform must
    /// be `HidToMidi` and `target` must resolve to a MIDI output endpoint,
    /// validated at config-load. `HidToOsc`/`HidToArtNet` via an *action* are
    /// rejected at load: HID→OSC/Art-Net is route-only for now (OSC-by-alias
    /// needs output-endpoint resolution the action executor does not carry,
    /// and there is no Art-Net output capability yet). V1 is strictly
    /// per-event.
    HidForward {
        /// Target MIDI output endpoint alias.
        target: String,
        /// Structured HID→MIDI transform (`HidToMidi`). Required; other
        /// variants are rejected at config-load in V1.
        transform: SignalTransform,
    },

    /// Forward an inbound OSC message to an OSC **output** endpoint
    /// (ADR-039-A).
    ///
    /// The mapping-triggered analogue of an OSC route: fires only when its
    /// mapping's typed OSC trigger matches, then re-sends the *triggering*
    /// OSC message (address + args) to the `target` OSC output endpoint by
    /// alias. **Gated at dispatch, not load**: the executor needs the inbound
    /// `OscInbound` from the trigger context, so a MIDI/HID-triggered mapping
    /// (which has none) is a benign runtime no-op rather than a load error.
    /// `target` must resolve to an OSC **output** endpoint (checked at load).
    ///
    /// V1 is **pass-through**: the message is forwarded verbatim. `transform`
    /// is reserved for a future OSC→OSC address/arg remap and MUST be `None`
    /// in V1 (a non-`None` value is rejected at config-load) — mirroring how
    /// `HidForward` V1 restricts its transform.
    ///
    /// Not a sensitive action class (ADR-042 D17): it emits an OSC packet,
    /// not a host effect. It still rides the network-origin taint —
    /// an OSC-origin `OscForward` is fine; the taint gates sensitive actions,
    /// not packet forwarding.
    OscForward {
        /// Target OSC output endpoint alias.
        target: String,
        /// Reserved OSC→OSC transform. Must be `None` in V1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transform: Option<SignalTransform>,
    },

    /// Send an OSC message over UDP (ADR-009 Gap H)
    ///
    /// # Examples
    /// ```toml
    /// [action]
    /// type = "OscSend"
    /// host = "127.0.0.1"
    /// port = 9000
    /// address = "/track/1/volume"
    /// args = [
    ///   { type = "Float", value = 0.75 },
    /// ]
    /// ```
    OscSend {
        /// Target host (e.g. "127.0.0.1")
        host: String,
        /// Target UDP port (e.g. 9000)
        port: u16,
        /// OSC address pattern (e.g. "/track/1/volume")
        address: String,
        /// OSC arguments
        #[serde(default)]
        args: Vec<crate::actions::OscArg>,
    },

    /// Execute a plugin action
    ///
    /// Runs a WASM plugin by name with optional parameters.
    /// The plugin must be installed and enabled in the plugin manager.
    ///
    /// # Examples
    /// ```toml
    /// [action]
    /// type = "Plugin"
    /// plugin = "spotify-control"
    /// params = { command = "play_pause" }
    /// ```
    Plugin {
        /// Plugin identifier (must match installed plugin name)
        plugin: String,
        /// Plugin-specific parameters
        #[serde(default)]
        params: serde_json::Value,
    },

    /// Observation sugar (ADR-038 §4.1).
    ///
    /// Carries a `message` template (with `{value}`/`{note}`/`{cc}`/`{velocity}`
    /// substitution) and completes with no signal side-effect. Pair with
    /// `let_through = true` to observe an event without consuming it.
    ///
    /// **Current behaviour:** the daemon executor only debug-logs the raw
    /// template. The substitution and event-stream / trace emission described
    /// above are not yet implemented — until then, `Tap` is side-effect-free
    /// beyond the debug log.
    ///
    /// # Examples
    /// ```toml
    /// [action]
    /// type = "Tap"
    /// message = "note {note} velocity {velocity}"
    /// ```
    Tap {
        /// Template emitted on each match. Supports `{value}`, `{note}`, `{cc}`,
        /// and `{velocity}` substitution (not yet resolved by the Tap executor).
        message: String,
    },
}

/// A single `[min, max]` inclusive range in a [`ActionConfig::CcContextSwitch`]
/// action, together with the action to dispatch when the watched CC value
/// falls in that window.
///
/// Ordering matters: the first matching range wins after lowering (task #24).
/// The validator (task #26) will flag overlapping ranges and `min > max`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CcRange {
    /// Inclusive lower bound (0-127).
    pub min: u8,
    /// Inclusive upper bound (0-127).
    pub max: u8,
    /// Action to dispatch when the watched CC value is in `[min, max]`.
    pub action: Box<ActionConfig>,
}
