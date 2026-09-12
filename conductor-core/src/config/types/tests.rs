// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

/// ADR-035: the legacy `[device]` block is no longer a Config field, so
/// serde silently ignores it (no `deny_unknown_fields`). A config that
/// still carries one parses fine; only `[[endpoints]]`/modes/etc. are read.
#[test]
fn test_config_deserialize_ignores_legacy_device_block() {
    let toml_str = r#"
[device]
name = "Test Device"
auto_connect = true

[[modes]]
name = "Default"
color = "blue"

[[modes.mappings]]
description = "Test mapping"

[modes.mappings.trigger]
type = "Note"
note = 60
velocity_min = 1

[modes.mappings.action]
type = "Keystroke"
keys = "space"
modifiers = ["cmd"]
"#;

    let config: Config = toml::from_str(toml_str).expect("Failed to parse config");
    assert_eq!(config.modes.len(), 1);
    assert_eq!(config.modes[0].name, "Default");
}

/// ListenMode default is All — listen-first so all hardware is visible
#[test]
fn test_listen_mode_default_is_all() {
    assert_eq!(ListenMode::default(), ListenMode::All);
}

/// AdvancedSettings default uses All listen_mode
#[test]
fn test_advanced_settings_default_listen_mode() {
    let settings = AdvancedSettings::default();
    assert_eq!(settings.listen_mode, ListenMode::All);
}

/// spec §10 Open Item #3: trace_buffer_size defaults to 1000 and is
/// populated by serde when omitted from a config document.
#[test]
fn test_advanced_settings_chord_learn_timeout_ms() {
    // Learn-mode chord window is its own config field (default 150ms,
    // the historical hardcoded daemon value) so the daemon is the single
    // source of truth — not a UI `chord_timeout_ms × 3` fiction.
    assert_eq!(AdvancedSettings::default().chord_learn_timeout_ms, 150);
    // Omitted in TOML → serde default fills it in (independent of the
    // normal-mode chord window).
    let parsed: AdvancedSettings = toml::from_str("chord_timeout_ms = 500").unwrap();
    assert_eq!(parsed.chord_learn_timeout_ms, 150);
    // Present in TOML → honoured.
    let parsed: AdvancedSettings = toml::from_str("chord_learn_timeout_ms = 220").unwrap();
    assert_eq!(parsed.chord_learn_timeout_ms, 220);
}

#[test]
fn test_advanced_settings_default_trace_buffer_size() {
    assert_eq!(AdvancedSettings::default().trace_buffer_size, 1000);
    // Omitted in TOML → serde default fills it in.
    let parsed: AdvancedSettings = toml::from_str("chord_timeout_ms = 50").unwrap();
    assert_eq!(parsed.trace_buffer_size, 1000);
    // Present in TOML → honoured.
    let parsed: AdvancedSettings = toml::from_str("trace_buffer_size = 256").unwrap();
    assert_eq!(parsed.trace_buffer_size, 256);
}

/// Cascade-suppression defaults — `allow_cascade = false`
/// so cross-note feedback is blocked out of the box; users opt in
/// when they deliberately chain mappings via MIDI routing. The TTL
/// matches the existing per-message echo guard (100ms).
#[test]
fn test_advanced_settings_default_cascade_suppression() {
    let settings = AdvancedSettings::default();
    assert!(
        !settings.allow_cascade,
        "default must be `false` so cascades are blocked out of the box"
    );
    assert_eq!(settings.cascade_ttl_ms, 100);
}

/// `allow_cascade` and `cascade_ttl_ms` round-trip cleanly through
/// TOML serde — both fields can be omitted (defaults apply) or set
/// explicitly without affecting other settings.
#[test]
fn test_advanced_settings_cascade_serde_roundtrip() {
    // Omitted in TOML → defaults
    let toml_default: AdvancedSettings = toml::from_str("").unwrap();
    assert!(!toml_default.allow_cascade);
    assert_eq!(toml_default.cascade_ttl_ms, 100);

    // Explicitly set in TOML → values applied
    let toml_explicit: AdvancedSettings =
        toml::from_str("allow_cascade = true\ncascade_ttl_ms = 250").unwrap();
    assert!(toml_explicit.allow_cascade);
    assert_eq!(toml_explicit.cascade_ttl_ms, 250);
}

// ─────────────────────────────────────────────────────────────────
// ADR-026 Phase 3.C.1 — SysEx identity probing flags
// ─────────────────────────────────────────────────────────────────

/// Both flags default to `true` so probe-on-connect is enabled
/// out of the box per ADR-026 D6 ("default-on, settings-gated"). 3.C.2
/// will plug the actual probing logic into these gates.
#[test]
fn test_advanced_settings_default_sysex_identity_probing_is_on() {
    let settings = AdvancedSettings::default();
    assert!(settings.sysex_identity_probing);
    assert!(settings.probe_on_connect);
}

/// Configs that omit the flags inherit the on-by-default
/// behaviour. Existing user TOML files (which never had these
/// fields) MUST keep working unchanged.
#[test]
fn test_config_omitting_sysex_flags_uses_on_defaults() {
    let toml_str = r#"
[[modes]]
name = "Default"
"#;
    let config: Config = toml::from_str(toml_str).expect("parse");
    assert!(config.advanced_settings.sysex_identity_probing);
    assert!(config.advanced_settings.probe_on_connect);
}

/// Users can disable identity probing globally via
/// `sysex_identity_probing = false` (the kill-switch — Phase 4
/// surfaces this as a Settings UI toggle). 3.C.2's wiring will
/// short-circuit when this is off.
#[test]
fn test_config_can_disable_sysex_identity_probing_globally() {
    let toml_str = r#"
[advanced_settings]
sysex_identity_probing = false

[[modes]]
name = "Default"
"#;
    let config: Config = toml::from_str(toml_str).expect("parse");
    assert!(!config.advanced_settings.sysex_identity_probing);
    // Probe-on-connect default still on — users can disable
    // *just* the auto-on-bind flow without disabling identity
    // probing entirely (e.g. they want manual probes only).
    assert!(config.advanced_settings.probe_on_connect);
}

/// `probe_on_connect = false` keeps SysEx probing available but
/// stops the auto-on-bind background task from firing. Users
/// invoke probes manually via the GUI Identify button (Phase
/// 3.D) or the MCP tool.
#[test]
fn test_config_can_disable_only_probe_on_connect() {
    let toml_str = r#"
[advanced_settings]
probe_on_connect = false

[[modes]]
name = "Default"
"#;
    let config: Config = toml::from_str(toml_str).expect("parse");
    assert!(config.advanced_settings.sysex_identity_probing);
    assert!(!config.advanced_settings.probe_on_connect);
}

/// Roundtrip: serialise an AdvancedSettings with the flags
/// flipped, parse it back, ensure the flags survive. Pins the
/// serde field names so the migration to TOML doesn't silently
/// rename to `sysexIdentityProbing` etc. (which would break
/// every existing config the moment the field is added).
#[test]
fn test_advanced_settings_sysex_flags_serde_roundtrip() {
    let settings = AdvancedSettings {
        sysex_identity_probing: false,
        probe_on_connect: false,
        ..AdvancedSettings::default()
    };
    let toml_str = toml::to_string(&settings).expect("serialise");
    // Spot-check the serialised TOML uses snake_case names.
    assert!(
        toml_str.contains("sysex_identity_probing = false"),
        "expected snake_case field in serialised TOML; got:\n{}",
        toml_str
    );
    assert!(
        toml_str.contains("probe_on_connect = false"),
        "expected snake_case field in serialised TOML; got:\n{}",
        toml_str
    );
    let parsed: AdvancedSettings = toml::from_str(&toml_str).expect("re-parse");
    assert!(!parsed.sysex_identity_probing);
    assert!(!parsed.probe_on_connect);
}

/// Config with no listen_mode uses All default
#[test]
fn test_config_default_listen_mode() {
    let toml_str = r#"
[[modes]]
name = "Default"
"#;
    let config: Config = toml::from_str(toml_str).expect("parse");
    assert_eq!(config.advanced_settings.listen_mode, ListenMode::All);
}

/// Explicit listen_mode = "All" is accepted (matches default)
#[test]
fn test_config_explicit_listen_mode_all() {
    let toml_str = r#"
[advanced_settings]
listen_mode = "All"

[[modes]]
name = "Default"
"#;
    let config: Config = toml::from_str(toml_str).expect("parse");
    assert_eq!(config.advanced_settings.listen_mode, ListenMode::All);
}

/// Explicit listen_mode = "Configured" overrides All default
#[test]
fn test_config_explicit_listen_mode_configured() {
    let toml_str = r#"
[advanced_settings]
listen_mode = "Configured"

[[modes]]
name = "Default"
"#;
    let config: Config = toml::from_str(toml_str).expect("parse");
    assert_eq!(config.advanced_settings.listen_mode, ListenMode::Configured);
}

#[test]
fn test_trigger_note() {
    let trigger = Trigger::Note {
        note: 60,
        velocity_min: Some(1),
        channel: None,
        device: None,
    };
    assert!(matches!(trigger, Trigger::Note { note: 60, .. }));
}

#[test]
fn test_action_keystroke() {
    let action = ActionConfig::Keystroke {
        keys: "space".to_string(),
        modifiers: vec!["cmd".to_string()],
    };
    assert!(matches!(action, ActionConfig::Keystroke { .. }));
}

#[test]
fn test_action_plugin_toml_roundtrip() {
    let toml_str = r#"
type = "Plugin"
plugin = "spotify-control"

[params]
command = "play_pause"
"#;
    let action: ActionConfig = toml::from_str(toml_str).expect("parse Plugin action");
    match &action {
        ActionConfig::Plugin { plugin, params } => {
            assert_eq!(plugin, "spotify-control");
            assert_eq!(params["command"], "play_pause");
        }
        _ => panic!("Expected Plugin variant"),
    }

    // Roundtrip
    let serialized = toml::to_string(&action).expect("serialize Plugin action");
    let deserialized: ActionConfig = toml::from_str(&serialized).expect("re-parse");
    assert!(matches!(deserialized, ActionConfig::Plugin { .. }));
}

#[test]
fn test_action_plugin_no_params() {
    let toml_str = r#"
type = "Plugin"
plugin = "my-plugin"
"#;
    let action: ActionConfig = toml::from_str(toml_str).expect("parse Plugin without params");
    match &action {
        ActionConfig::Plugin { plugin, params } => {
            assert_eq!(plugin, "my-plugin");
            assert!(params.is_null());
        }
        _ => panic!("Expected Plugin variant"),
    }
}

// ========== MidiForward Config Tests (ADR-009 Gap 2) ==========

#[test]
fn test_midi_forward_config_parse() {
    let toml_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Forward CC to synth"

[modes.mappings.trigger]
type = "CC"
cc = 74

[modes.mappings.action]
type = "MidiForward"
target = "Synth Output"
"#;
    let config: Config = toml::from_str(toml_str).expect("Failed to parse MidiForward config");
    let action = &config.modes[0].mappings[0].action;
    match action {
        ActionConfig::MidiForward { target, transform } => {
            assert_eq!(target, "Synth Output");
            assert!(transform.is_none());
        }
        _ => panic!("Expected MidiForward action"),
    }
}

#[test]
fn test_midi_forward_config_with_transform() {
    let toml_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
description = "Forward with channel remap"

[modes.mappings.trigger]
type = "CC"
cc = 74

[modes.mappings.action]
type = "MidiForward"
target = "Synth Output"

[modes.mappings.action.transform]
channel = 5
cc = 1
velocity_scale = 1.5
invert_value = false
"#;
    let config: Config =
        toml::from_str(toml_str).expect("Failed to parse MidiForward with transform");
    let action = &config.modes[0].mappings[0].action;
    match action {
        ActionConfig::MidiForward { target, transform } => {
            assert_eq!(target, "Synth Output");
            let t = transform.as_ref().unwrap();
            assert_eq!(t.channel, Some(5));
            assert_eq!(t.cc, Some(1));
            assert_eq!(t.velocity_scale, Some(1.5));
            assert!(!t.invert_value);
        }
        _ => panic!("Expected MidiForward action"),
    }
}

#[test]
fn test_midi_forward_action_conversion() {
    use crate::actions::Action;

    let config = ActionConfig::MidiForward {
        target: "Synth".to_string(),
        transform: None,
    };
    let action: Action = config.into();
    match action {
        Action::MidiForward { target, transform } => {
            assert_eq!(target, "Synth");
            assert!(transform.is_none());
        }
        _ => panic!("Expected MidiForward action"),
    }
}

// ========== OscSend Config Tests (ADR-009 Gap H) ==========

#[test]
fn test_osc_send_config_parse() {
    let toml_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "OscSend"
host = "127.0.0.1"
port = 9000
address = "/track/1/volume"
args = [
  { type = "Float", value = 0.75 },
]
"#;
    let config: Config = toml::from_str(toml_str).expect("Failed to parse OscSend config");
    let action = &config.modes[0].mappings[0].action;
    match action {
        ActionConfig::OscSend {
            host,
            port,
            address,
            args,
        } => {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(*port, 9000);
            assert_eq!(address, "/track/1/volume");
            assert_eq!(args.len(), 1);
            assert_eq!(args[0], crate::actions::OscArg::Float(0.75));
        }
        _ => panic!("Expected OscSend action"),
    }
}

#[test]
fn test_osc_send_config_no_args() {
    let toml_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "OscSend"
host = "127.0.0.1"
port = 8000
address = "/heartbeat"
"#;
    let config: Config = toml::from_str(toml_str).expect("Failed to parse OscSend config");
    let action = &config.modes[0].mappings[0].action;
    match action {
        ActionConfig::OscSend { args, .. } => {
            assert!(args.is_empty());
        }
        _ => panic!("Expected OscSend action"),
    }
}

#[test]
fn test_osc_send_config_multiple_args() {
    let toml_str = r#"
[device]
name = "Test"
auto_connect = false

[[modes]]
name = "Default"

[[modes.mappings]]
[modes.mappings.trigger]
type = "Note"
note = 36

[modes.mappings.action]
type = "OscSend"
host = "10.0.0.1"
port = 7000
address = "/fx/param"
args = [
  { type = "Int", value = 1 },
  { type = "Float", value = 0.5 },
  { type = "String", value = "reverb" },
]
"#;
    let config: Config = toml::from_str(toml_str).expect("Failed to parse OscSend config");
    let action = &config.modes[0].mappings[0].action;
    match action {
        ActionConfig::OscSend { args, .. } => {
            assert_eq!(args.len(), 3);
            assert_eq!(args[0], crate::actions::OscArg::Int(1));
            assert_eq!(args[1], crate::actions::OscArg::Float(0.5));
            assert_eq!(
                args[2],
                crate::actions::OscArg::String("reverb".to_string())
            );
        }
        _ => panic!("Expected OscSend action"),
    }
}

// ========== LED Config Tests ==========

#[test]
fn test_led_config_parse() {
    let toml_str = r#"
[[modes]]
name = "Default"

[led]
enabled = true
brightness = 80
scheme = "rainbow"
idle_timeout_secs = 30

[led.mode_colors.Default]
r = 255
g = 0
b = 128
"#;
    let config: Config = toml::from_str(toml_str).expect("Failed to parse LED config");
    let led = config.led.unwrap();
    assert!(led.enabled);
    assert_eq!(led.brightness, 80);
    assert_eq!(led.scheme, "rainbow");
    assert_eq!(led.idle_timeout_secs, 30);
    let color = led.mode_colors.get("Default").unwrap();
    assert_eq!(color.r, 255);
    assert_eq!(color.g, 0);
    assert_eq!(color.b, 128);
}

#[test]
fn test_led_config_defaults() {
    let toml_str = r#"
[[modes]]
name = "Default"

[led]
"#;
    let config: Config = toml::from_str(toml_str).expect("Failed to parse LED config");
    let led = config.led.unwrap();
    assert!(led.enabled);
    assert_eq!(led.brightness, 100);
    assert_eq!(led.scheme, "reactive");
    assert_eq!(led.idle_timeout_secs, 0);
    assert!(led.mode_colors.is_empty());
}

#[test]
fn test_led_config_missing_backward_compat() {
    let toml_str = r#"
[[modes]]
name = "Default"
"#;
    let config: Config = toml::from_str(toml_str).expect("Failed to parse config");
    assert!(config.led.is_none());
}

#[test]
fn test_led_config_roundtrip() {
    let led = LedConfig {
        enabled: true,
        brightness: 50,
        scheme: "breathing".to_string(),
        idle_timeout_secs: 60,
        mode_colors: {
            let mut m = std::collections::BTreeMap::new();
            m.insert("Default".to_string(), RgbColor { r: 0, g: 255, b: 0 });
            m
        },
        midi: None,
        hid: None,
        velocity_colors: None,
        default_fade_ms: None,
    };
    let toml_str = toml::to_string(&led).expect("serialize");
    let parsed: LedConfig = toml::from_str(&toml_str).expect("deserialize");
    assert_eq!(parsed.brightness, 50);
    assert_eq!(parsed.scheme, "breathing");
    assert_eq!(parsed.idle_timeout_secs, 60);
    assert_eq!(parsed.mode_colors.get("Default").unwrap().g, 255);
}

#[test]
fn test_led_config_skip_serializing_none() {
    let config = Config {
        config_meta: Default::default(),
        endpoints: vec![],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![],
        }],
        led: None,
        ..Config::default_config()
    };
    let toml_str = toml::to_string(&config).expect("serialize");
    assert!(!toml_str.contains("[led]"));
}

#[test]
fn test_rgb_color_conversion() {
    let rgb_color = RgbColor {
        r: 10,
        g: 20,
        b: 30,
    };
    let mikro_rgb: crate::mikro_leds::RGB = rgb_color.clone().into();
    assert_eq!(mikro_rgb.r, 10);
    assert_eq!(mikro_rgb.g, 20);
    assert_eq!(mikro_rgb.b, 30);

    let back: RgbColor = mikro_rgb.into();
    assert_eq!(back.r, 10);
    assert_eq!(back.g, 20);
    assert_eq!(back.b, 30);
}

#[test]
fn test_rgb_to_velocity_colors() {
    let colors = MidiLedColors::default();

    // Pure colors
    assert_eq!(colors.rgb_to_velocity(255, 0, 0), colors.red);
    assert_eq!(colors.rgb_to_velocity(0, 255, 0), colors.green);
    assert_eq!(colors.rgb_to_velocity(0, 0, 255), colors.blue);
    assert_eq!(colors.rgb_to_velocity(0, 0, 0), colors.off);

    // Mixed colors
    assert_eq!(colors.rgb_to_velocity(200, 200, 0), colors.yellow);
    assert_eq!(colors.rgb_to_velocity(200, 180, 0), colors.yellow);
    assert_eq!(colors.rgb_to_velocity(100, 50, 50), colors.red);
    assert_eq!(colors.rgb_to_velocity(50, 100, 50), colors.green);
    assert_eq!(colors.rgb_to_velocity(50, 50, 100), colors.blue);

    // Edge cases — equal RGB is gray, not yellow (blue is not low enough)
    assert_eq!(colors.rgb_to_velocity(100, 100, 100), colors.amber);
    assert_eq!(colors.rgb_to_velocity(1, 0, 0), colors.red);
}

// ========== MIDI LED Config Tests ==========

#[test]
fn test_midi_led_config_defaults() {
    let config = MidiLedConfig::default();
    assert_eq!(config.channel, 1);
    assert_eq!(config.note_on_velocity, 127);
    assert_eq!(config.note_off_velocity, 0);
    assert_eq!(config.colors.red, 5);
    assert_eq!(config.colors.green, 21);
    assert_eq!(config.colors.yellow, 13);
    assert_eq!(config.colors.amber, 9);
    assert_eq!(config.colors.off, 0);
    assert!(config.custom_mappings.is_empty());
}

#[test]
fn test_midi_led_config_parse() {
    let toml_str = r#"
[[modes]]
name = "Default"

[led]
enabled = true

[led.midi]
channel = 2
note_on_velocity = 100
note_off_velocity = 10

[led.midi.colors]
red = 3
green = 17
"#;
    let config: Config = toml::from_str(toml_str).expect("Failed to parse MIDI LED config");
    let midi = config.led.unwrap().midi.unwrap();
    assert_eq!(midi.channel, 2);
    assert_eq!(midi.note_on_velocity, 100);
    assert_eq!(midi.note_off_velocity, 10);
    assert_eq!(midi.colors.red, 3);
    assert_eq!(midi.colors.green, 17);
    // Defaults for unset fields
    assert_eq!(midi.colors.yellow, 13);
    assert_eq!(midi.colors.amber, 9);
}

#[test]
fn test_midi_led_config_backward_compat() {
    let toml_str = r#"
[[modes]]
name = "Default"

[led]
brightness = 80
"#;
    let config: Config = toml::from_str(toml_str).expect("parse");
    assert!(config.led.unwrap().midi.is_none());
}

#[test]
fn test_midi_led_custom_mapping_parse() {
    let toml_str = r#"
[[modes]]
name = "Default"

[led.midi]
channel = 1

[[led.midi.custom_mappings]]
pad = 36
led_on = { type = "note_on", note = 36, velocity = 127 }
led_off = { type = "note_off", note = 36, velocity = 0 }

[[led.midi.custom_mappings]]
pad = 37
led_on = { type = "cc", cc = 37, value = 127 }
led_off = { type = "cc", cc = 37, value = 0 }
"#;
    let config: Config = toml::from_str(toml_str).expect("parse custom mappings");
    let midi = config.led.unwrap().midi.unwrap();
    assert_eq!(midi.custom_mappings.len(), 2);
    assert_eq!(midi.custom_mappings[0].pad, 36);
    assert!(matches!(
        midi.custom_mappings[0].led_on,
        MidiLedMessage::NoteOn {
            note: 36,
            velocity: 127
        }
    ));
    assert!(matches!(
        midi.custom_mappings[1].led_on,
        MidiLedMessage::Cc { cc: 37, value: 127 }
    ));
}

#[test]
fn test_midi_led_config_roundtrip() {
    let config = MidiLedConfig {
        channel: 3,
        note_on_velocity: 100,
        note_off_velocity: 5,
        colors: MidiLedColors::default(),
        custom_mappings: vec![MidiLedCustomMapping {
            pad: 40,
            led_on: MidiLedMessage::NoteOn {
                note: 40,
                velocity: 100,
            },
            led_off: MidiLedMessage::NoteOff {
                note: 40,
                velocity: 0,
            },
        }],
    };
    let json = serde_json::to_string(&config).expect("serialize");
    let parsed: MidiLedConfig = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed.channel, 3);
    assert_eq!(parsed.custom_mappings.len(), 1);
}

#[test]
fn test_event_console_config_defaults() {
    let config = EventConsoleConfig::default();
    assert_eq!(config.buffer_size, 1000);
    assert_eq!(config.max_events_per_second, 0);
    assert!(config.capture_midi);
    assert!(config.capture_processed);
    assert!(config.capture_actions);
    assert!(config.filters.is_empty());
}

#[test]
fn test_event_console_config_toml_roundtrip() {
    let toml_str = r#"
buffer_size = 5000
max_events_per_second = 30
capture_midi = true
capture_processed = false
capture_actions = true

[filters.pads_only]
description = "Only pad note events"
event_type = "note_on,note_off"
note_min = 36
note_max = 51
"#;
    let config: EventConsoleConfig = toml::from_str(toml_str).expect("parse");
    assert_eq!(config.buffer_size, 5000);
    assert_eq!(config.max_events_per_second, 30);
    assert!(config.capture_midi);
    assert!(!config.capture_processed);
    assert!(config.capture_actions);
    assert_eq!(config.filters.len(), 1);
    let pads = &config.filters["pads_only"];
    assert_eq!(pads.description.as_deref(), Some("Only pad note events"));
    assert_eq!(pads.event_type.as_deref(), Some("note_on,note_off"));
    assert_eq!(pads.note_min, Some(36));
    assert_eq!(pads.note_max, Some(51));
    assert!(pads.channel.is_none());
    assert!(pads.device_id.is_none());
}

#[test]
fn test_event_console_config_with_triggers() {
    let toml_str = r#"
buffer_size = 1000
enable_profiling = true
track_latency = true
track_memory = true

[triggers.high_errors]
condition = "error_rate > 5 per_minute"
cooldown_secs = 120

[triggers.high_errors.action]
type = "Notification"
message = "High error rate detected!"

[triggers.event_flood]
condition = "event_count > 100 per_second"

[triggers.event_flood.action]
type = "log"
message = "Event flood warning"
"#;
    let config: EventConsoleConfig = toml::from_str(toml_str).expect("parse");
    assert!(config.enable_profiling);
    assert!(config.track_latency);
    assert!(config.track_memory);
    assert_eq!(config.triggers.len(), 2);

    let errors = &config.triggers["high_errors"];
    assert_eq!(errors.condition, "error_rate > 5 per_minute");
    assert_eq!(errors.cooldown_secs, Some(120));
    assert!(matches!(errors.action, TriggerAction::Notification { .. }));

    let flood = &config.triggers["event_flood"];
    assert_eq!(flood.condition, "event_count > 100 per_second");
    assert!(matches!(flood.action, TriggerAction::Log { .. }));
}

#[test]
fn test_event_console_config_omitted_uses_defaults() {
    // When [event_console] is absent, Config should have None
    let toml_str = r#"
[[modes]]
name = "Default"
mappings = []
"#;
    let config: super::super::Config = toml::from_str(toml_str).expect("parse");
    assert!(config.event_console.is_none());
}

// ========== HID LED Config Tests ==========

#[test]
fn test_hid_led_config_all_fields_none_by_default() {
    let config = HidLedConfig::default();
    assert!(config.hid_profile.is_none());
    assert!(config.vendor_id.is_none());
    assert!(config.product_id.is_none());
    assert!(config.interface_number.is_none());
    assert!(config.led_report_id.is_none());
    assert!(config.buffer_size.is_none());
    assert!(config.pad_led_offset.is_none());
    assert!(config.pad_count.is_none());
    assert!(config.color_palette.is_none());
    assert!(config.pad_layout.is_none());
}

#[test]
fn test_hid_led_config_mikro_mk3_profile_values() {
    let mk3 = HidLedConfig::mikro_mk3();
    assert_eq!(mk3.vendor_id, Some(0x17CC));
    assert_eq!(mk3.product_id, Some(0x1700));
    assert_eq!(mk3.interface_number, Some(0));
    assert_eq!(mk3.led_report_id, Some(0x80));
    assert_eq!(mk3.buffer_size, Some(80));
    assert_eq!(mk3.pad_led_offset, Some(39));
    assert_eq!(mk3.pad_count, Some(16));
    assert!(mk3.color_palette.as_ref().unwrap().len() >= 18);
    assert_eq!(mk3.pad_layout.as_ref().unwrap().len(), 16);
}

#[test]
fn test_hid_led_config_from_profile_known() {
    assert!(HidLedConfig::from_profile("mikro-mk3").is_some());
}

#[test]
fn test_hid_led_config_from_profile_unknown() {
    assert!(HidLedConfig::from_profile("nonexistent").is_none());
}

#[test]
fn test_hid_led_config_resolve_profile_fills_defaults() {
    let config = HidLedConfig {
        hid_profile: Some("mikro-mk3".to_string()),
        ..Default::default()
    };
    let resolved = config.resolve_profile().unwrap();
    assert_eq!(resolved.vendor_id, 0x17CC);
    assert_eq!(resolved.product_id, 0x1700);
    assert_eq!(resolved.interface_number, 0);
    assert_eq!(resolved.led_report_id, 0x80);
    assert_eq!(resolved.buffer_size, 80);
    assert_eq!(resolved.pad_led_offset, 39);
    assert_eq!(resolved.pad_count, 16);
    assert!(!resolved.color_palette.is_empty());
    assert!(!resolved.pad_layout.is_empty());
}

#[test]
fn test_hid_led_config_resolve_user_overrides_kept() {
    let config = HidLedConfig {
        hid_profile: Some("mikro-mk3".to_string()),
        vendor_id: Some(0xBEEF),
        // Override pad_count AND clear pad_layout (since profile's 16-entry
        // layout wouldn't match 8 pads)
        pad_count: Some(8),
        pad_layout: Some(vec![7, 6, 5, 4, 3, 2, 1, 0]),
        ..Default::default()
    };
    let resolved = config.resolve_profile().unwrap();
    assert_eq!(resolved.vendor_id, 0xBEEF); // user override kept
    assert_eq!(resolved.product_id, 0x1700); // from profile
    assert_eq!(resolved.pad_count, 8); // user override kept
    assert_eq!(resolved.pad_layout.len(), 8); // user override kept
}

#[test]
fn test_hid_led_config_resolve_no_profile_requires_vendor_product() {
    let config = HidLedConfig::default(); // no profile, no vendor/product
    assert!(config.resolve_profile().is_err());
}

#[test]
fn test_hid_led_config_resolve_explicit_works_without_profile() {
    let config = HidLedConfig {
        vendor_id: Some(0x1234),
        product_id: Some(0x5678),
        buffer_size: Some(100),
        pad_led_offset: Some(10),
        pad_count: Some(8),
        ..Default::default()
    };
    let resolved = config.resolve_profile().unwrap();
    assert_eq!(resolved.vendor_id, 0x1234);
    assert_eq!(resolved.product_id, 0x5678);
    assert_eq!(resolved.buffer_size, 100);
}

#[test]
fn test_hid_led_config_resolve_unknown_profile_fails() {
    let config = HidLedConfig {
        hid_profile: Some("nonexistent".to_string()),
        ..Default::default()
    };
    assert!(config.resolve_profile().is_err());
}

#[test]
fn test_hid_led_config_resolve_buffer_overflow_fails() {
    let config = HidLedConfig {
        vendor_id: Some(0x1234),
        product_id: Some(0x5678),
        buffer_size: Some(10),
        pad_led_offset: Some(5),
        pad_count: Some(8), // 5 + 8 = 13 > 10
        ..Default::default()
    };
    assert!(config.resolve_profile().is_err());
}

#[test]
fn test_hid_led_config_resolve_pad_layout_duplicate_fails() {
    let config = HidLedConfig {
        vendor_id: Some(0x1234),
        product_id: Some(0x5678),
        buffer_size: Some(20),
        pad_led_offset: Some(0),
        pad_count: Some(4),
        pad_layout: Some(vec![0, 1, 1, 3]), // duplicate position 1
        ..Default::default()
    };
    assert!(config.resolve_profile().is_err());
}

#[test]
fn test_hid_led_config_resolve_pad_layout_out_of_bounds_fails() {
    let config = HidLedConfig {
        vendor_id: Some(0x1234),
        product_id: Some(0x5678),
        buffer_size: Some(20),
        pad_led_offset: Some(0),
        pad_count: Some(4),
        pad_layout: Some(vec![0, 1, 2, 99]), // 99 >= pad_count
        ..Default::default()
    };
    assert!(config.resolve_profile().is_err());
}

#[test]
fn test_hid_led_config_resolve_buffer_size_zero_fails() {
    // resolve_profile() itself rejects buffer_size=0, proving
    // any downstream check is redundant.
    let config = HidLedConfig {
        vendor_id: Some(0x1234),
        product_id: Some(0x5678),
        buffer_size: Some(0),
        pad_led_offset: Some(0),
        pad_count: Some(1),
        ..Default::default()
    };
    let err = config.resolve_profile().unwrap_err();
    assert!(
        err.contains("buffer_size must be > 0"),
        "expected buffer_size error, got: {}",
        err
    );
}

#[test]
fn test_hid_led_config_toml_parse_profile() {
    let toml_str = r#"
[[modes]]
name = "Default"

[led.hid]
hid_profile = "mikro-mk3"
"#;
    let config: super::super::Config = toml::from_str(toml_str).expect("parse");
    let hid = config.led.unwrap().hid.unwrap();
    assert_eq!(hid.hid_profile.as_deref(), Some("mikro-mk3"));
}

#[test]
fn test_hid_led_config_toml_parse_explicit() {
    let toml_str = r#"
[[modes]]
name = "Default"

[led.hid]
vendor_id = 6092
product_id = 5888
led_report_id = 128
buffer_size = 80
pad_led_offset = 39
pad_count = 16
"#;
    let config: super::super::Config = toml::from_str(toml_str).expect("parse");
    let hid = config.led.unwrap().hid.unwrap();
    assert_eq!(hid.vendor_id, Some(6092));
    assert_eq!(hid.product_id, Some(5888));
}

#[test]
fn test_hid_led_config_backward_compat_no_hid_section() {
    let toml_str = r#"
[[modes]]
name = "Default"

[led]
brightness = 80
"#;
    let config: super::super::Config = toml::from_str(toml_str).expect("parse");
    assert!(config.led.unwrap().hid.is_none());
}

#[test]
fn test_hid_led_config_skip_serializing_none() {
    let config = HidLedConfig::default();
    let json = serde_json::to_string(&config).unwrap();
    assert_eq!(json, "{}");
}

#[test]
fn test_mikro_mk3_pad_layout_vertical_flip() {
    let mk3 = HidLedConfig::mikro_mk3();
    let layout = mk3.pad_layout.unwrap();
    // MK3 pads are bottom-to-top logical, top-to-bottom physical
    // Pad 0 (bottom-left) → position 12 (4th row in physical)
    assert_eq!(layout[0], 12);
    // Pad 15 (top-right) → position 3 (1st row in physical)
    assert_eq!(layout[15], 3);
}

// ========== Velocity Color Map Tests ==========

#[test]
fn test_velocity_color_map_default_three_ranges() {
    let vcm = VelocityColorMap::default();
    assert_eq!(vcm.ranges.len(), 3);
    // soft = green (0-39)
    assert_eq!(vcm.ranges[0].min, 0);
    assert_eq!(vcm.ranges[0].max, 39);
    assert_eq!(vcm.ranges[0].color, RgbColor { r: 0, g: 255, b: 0 });
    // medium = yellow (40-79)
    assert_eq!(vcm.ranges[1].min, 40);
    assert_eq!(vcm.ranges[1].max, 79);
    assert_eq!(
        vcm.ranges[1].color,
        RgbColor {
            r: 255,
            g: 255,
            b: 0
        }
    );
    // hard = red (80-127)
    assert_eq!(vcm.ranges[2].min, 80);
    assert_eq!(vcm.ranges[2].max, 127);
    assert_eq!(vcm.ranges[2].color, RgbColor { r: 255, g: 0, b: 0 });
}

#[test]
fn test_velocity_color_map_lookup_each_range() {
    let vcm = VelocityColorMap::default();
    // Soft range
    let c = vcm.color_for_velocity(0).unwrap();
    assert_eq!(c.g, 255);
    assert_eq!(c.r, 0);
    // Medium range
    let c = vcm.color_for_velocity(60).unwrap();
    assert_eq!(c.r, 255);
    assert_eq!(c.g, 255);
    // Hard range
    let c = vcm.color_for_velocity(127).unwrap();
    assert_eq!(c.r, 255);
    assert_eq!(c.g, 0);
}

#[test]
fn test_velocity_color_map_lookup_no_match() {
    let vcm = VelocityColorMap {
        ranges: vec![VelocityRange {
            min: 10,
            max: 20,
            color: RgbColor { r: 255, g: 0, b: 0 },
        }],
    };
    assert!(vcm.color_for_velocity(5).is_none());
    assert!(vcm.color_for_velocity(25).is_none());
    assert!(vcm.color_for_velocity(15).is_some());
}

#[test]
fn test_velocity_color_map_toml_parse() {
    let toml_str = r#"
[[modes]]
name = "Default"

[[led.velocity_colors.ranges]]
min = 0
max = 63
color = { r = 0, g = 255, b = 0 }

[[led.velocity_colors.ranges]]
min = 64
max = 127
color = { r = 255, g = 0, b = 0 }
"#;
    let config: super::super::Config = toml::from_str(toml_str).expect("parse");
    let led = config.led.unwrap();
    let vcm = led.velocity_colors.unwrap();
    assert_eq!(vcm.ranges.len(), 2);
    assert_eq!(vcm.ranges[0].max, 63);
    assert_eq!(vcm.ranges[1].min, 64);
}

#[test]
fn test_led_config_default_fade_ms() {
    let toml_str = r#"
[[modes]]
name = "Default"

[led]
default_fade_ms = 500
"#;
    let config: super::super::Config = toml::from_str(toml_str).expect("parse");
    assert_eq!(config.led.unwrap().default_fade_ms, Some(500));
}

#[test]
fn test_led_config_default_fade_ms_absent() {
    let toml_str = r#"
[[modes]]
name = "Default"

[led]
brightness = 100
"#;
    let config: super::super::Config = toml::from_str(toml_str).expect("parse");
    assert!(config.led.unwrap().default_fade_ms.is_none());
}
