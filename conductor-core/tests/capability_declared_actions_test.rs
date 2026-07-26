// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! Integration tests for [`capabilities_for_action`] (ADR-027 D3 spec §3.1).
//!
//! Pins the per-`ActionConfig`-variant capability mapping. The function is
//! exhaustive (no `_` match arm) so adding a new `ActionConfig` variant
//! fails compilation at the function until capabilities are declared; this
//! test file pins the *content* of the declarations row by row, plus the
//! recursive composition for `Sequence` / `Conditional` / `Repeat` /
//! `PcContextSwitch` / `CcContextSwitch`.
//!
//! [`capabilities_for_action`]: conductor_core::security::capabilities_for_action

use conductor_core::config::ActionConfig;
use conductor_core::config::types::CcRange;
use conductor_core::security::{Capability, CapabilitySet, capabilities_for_action};
use conductor_core::{Condition, OscArg};
use indexmap::IndexMap;

fn caps(items: impl IntoIterator<Item = Capability>) -> CapabilitySet {
    items.into_iter().collect()
}

// ───────────────────────────────────────────────────────────────────────
// Leaf variants — direct mapping
// ───────────────────────────────────────────────────────────────────────

#[test]
fn keystroke_without_modifiers_requires_keystroke_send_only() {
    let action = ActionConfig::Keystroke {
        keys: "space".into(),
        modifiers: vec![],
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::KeystrokeSend])
    );
}

#[test]
fn keystroke_with_modifiers_also_requires_modifier_combo() {
    let action = ActionConfig::Keystroke {
        keys: "q".into(),
        modifiers: vec!["cmd".into()],
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([
            Capability::KeystrokeSend,
            Capability::KeystrokeModifierCombo
        ])
    );
}

#[test]
fn text_requires_keystroke_send() {
    let action = ActionConfig::Text {
        text: "hello".into(),
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::KeystrokeSend])
    );
}

#[test]
fn launch_requires_launch_app() {
    let action = ActionConfig::Launch {
        app: "Logic Pro".into(),
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::LaunchApp])
    );
}

#[test]
fn shell_non_interpreter_binary_requires_shell_exec_only() {
    // `/bin/ls` is a plain binary — no interpreter semantics. D3 3/N
    // (issue #1037 Phase 2) resolves the effective binary at
    // capability-derivation time but classifies this as non-interpreter,
    // so the declared set stays at `ShellExec` only.
    let action = ActionConfig::Shell {
        sandbox: None,
        command: "/bin/ls -la".into(),
        args: None,
        timeout_ms: None,
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::ShellExec])
    );
}

#[test]
fn shell_argv_form_sh_adds_interpreter_exec() {
    // D3 3/N (issue #1037 Phase 2): the schema-time declaration now
    // includes `InterpreterExec(Sh)` because the resolved binary is
    // `/bin/sh`. This is what gates the `/bin/sh -c "rm -rf /"` class —
    // the gate can refuse `InterpreterExec(Sh)` while still allowing
    // plain `ShellExec` for legitimate non-interpreter binaries.
    use conductor_core::security::InterpreterFamily;
    let action = ActionConfig::Shell {
        sandbox: None,
        command: "/bin/sh".into(),
        args: Some(vec!["-c".into(), "echo hi".into()]),
        timeout_ms: None,
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([
            Capability::ShellExec,
            Capability::InterpreterExec(InterpreterFamily::Sh),
        ])
    );
}

#[test]
fn shell_env_python_wrapper_adds_interpreter_exec() {
    // D3 3/N — wrapper resolution: `env python -c …` strips `env` and
    // classifies the wrapped program as Python. This is the canonical
    // bypass class the schema-time declaration must defeat.
    use conductor_core::security::InterpreterFamily;
    let action = ActionConfig::Shell {
        sandbox: None,
        command: "/usr/bin/env".into(),
        args: Some(vec!["python".into(), "-c".into(), "print(1)".into()]),
        timeout_ms: None,
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([
            Capability::ShellExec,
            Capability::InterpreterExec(InterpreterFamily::Python),
        ])
    );
}

#[test]
fn shell_legacy_form_sh_command_adds_interpreter_exec() {
    // Legacy single-string form (`args = None`): the resolver
    // whitespace-splits to derive argv[0]. `/bin/sh -c '…'` written as
    // one string still classifies as Sh.
    use conductor_core::security::InterpreterFamily;
    let action = ActionConfig::Shell {
        sandbox: None,
        command: "/bin/sh -c 'echo hi'".into(),
        args: None,
        timeout_ms: None,
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([
            Capability::ShellExec,
            Capability::InterpreterExec(InterpreterFamily::Sh),
        ])
    );
}

#[test]
fn delay_is_a_control_flow_primitive_with_no_capability_requirement() {
    let action = ActionConfig::Delay { ms: 100 };
    assert_eq!(capabilities_for_action(&action), CapabilitySet::new());
}

#[test]
fn mode_change_requires_config_write_because_it_persists_to_disk() {
    // ModeChange persists `last_selected_mode` to the config TOML via
    // `EngineManager::persist_mode_change` (`conductor-daemon/src/daemon/
    // engine_manager.rs:5877`). The on-disk write is the security-relevant
    // side effect — the in-memory ArcSwap<ModeState> flip is bookkeeping
    // for the same persisted change.
    let action = ActionConfig::ModeChange {
        mode: "Studio".into(),
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::ConfigWrite])
    );
}

#[test]
fn mouse_click_requires_mouse_send() {
    let action = ActionConfig::MouseClick {
        button: "left".into(),
        x: None,
        y: None,
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::MouseSend])
    );
}

#[test]
fn volume_control_requires_shell_exec_because_it_spawns_processes() {
    // macOS spawns `osascript -e`, Linux spawns `pactl`. Both are process
    // spawns from the daemon, so the cross-platform minimum is ShellExec.
    // (`osascript` would additionally classify as InterpreterExec(Other)
    // post D3 3/N when wrapper resolution / interpreter classification
    // lands; this PR maps only what the schema declares.)
    let action = ActionConfig::VolumeControl {
        operation: "Up".into(),
        value: None,
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::ShellExec])
    );
}

#[test]
fn send_midi_requires_midi_out() {
    let action = ActionConfig::SendMidi {
        port: "iac-bus-1".into(),
        message_type: "NoteOn".into(),
        channel: 0,
        note: Some(60),
        velocity: Some(100),
        controller: None,
        value: None,
        program: None,
        pitch: None,
        pressure: None,
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::MidiOut])
    );
}

#[test]
fn midi_forward_requires_midi_out() {
    let action = ActionConfig::MidiForward {
        target: "absynth".into(),
        transform: None,
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::MidiOut])
    );
}

#[test]
fn osc_send_requires_osc_out() {
    let action = ActionConfig::OscSend {
        host: "127.0.0.1".into(),
        port: 9000,
        address: "/track/1/volume".into(),
        args: vec![OscArg::Float(0.75)],
    };
    assert_eq!(capabilities_for_action(&action), caps([Capability::OscOut]));
}

#[test]
fn plugin_requires_plugin_exec() {
    let action = ActionConfig::Plugin {
        plugin: "spotify-control".into(),
        params: serde_json::json!({"command": "play_pause"}),
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::PluginExec])
    );
}

#[test]
fn tap_emits_only_an_event_message_so_requires_no_capability() {
    // Tap (ADR-038) only emits a templated message onto the internal event
    // bus — no OS-visible side effect — so it declares no capability. This
    // is the last leaf `ActionConfig` variant; pinning it keeps the
    // row-by-row leaf coverage exhaustive (matches `capabilities.rs`:
    // `Tap { .. } => CapabilitySet::new()`).
    let action = ActionConfig::Tap {
        message: "scene/next".into(),
    };
    assert_eq!(capabilities_for_action(&action), CapabilitySet::new());
}

// ───────────────────────────────────────────────────────────────────────
// Recursive composition — union of inner action capabilities
// ───────────────────────────────────────────────────────────────────────

#[test]
fn sequence_unions_inner_action_capabilities() {
    let action = ActionConfig::Sequence {
        actions: vec![
            ActionConfig::Keystroke {
                keys: "a".into(),
                modifiers: vec![],
            },
            ActionConfig::Delay { ms: 50 },
            ActionConfig::SendMidi {
                port: "out".into(),
                message_type: "CC".into(),
                channel: 0,
                note: None,
                velocity: None,
                controller: Some(7),
                value: Some(64),
                program: None,
                pitch: None,
                pressure: None,
            },
        ],
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::KeystrokeSend, Capability::MidiOut])
    );
}

#[test]
fn empty_sequence_has_no_capability_requirement() {
    // A `Sequence { actions: [] }` is a no-op; same logic as `Delay`.
    let action = ActionConfig::Sequence { actions: vec![] };
    assert_eq!(capabilities_for_action(&action), CapabilitySet::new());
}

#[test]
fn repeat_propagates_inner_action_capabilities() {
    let action = ActionConfig::Repeat {
        action: Box::new(ActionConfig::Launch {
            app: "Calculator".into(),
        }),
        count: 3,
        delay_ms: Some(100),
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::LaunchApp])
    );
}

#[test]
fn conditional_unions_then_and_else_branches() {
    let action = ActionConfig::Conditional {
        condition: Condition::Always,
        then_action: Box::new(ActionConfig::Keystroke {
            keys: "tab".into(),
            modifiers: vec![],
        }),
        else_action: Some(Box::new(ActionConfig::Shell {
            sandbox: None,
            command: "/usr/bin/say boop".into(),
            args: None,
            timeout_ms: None,
        })),
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::KeystrokeSend, Capability::ShellExec])
    );
}

#[test]
fn conditional_without_else_just_uses_then() {
    let action = ActionConfig::Conditional {
        condition: Condition::Always,
        then_action: Box::new(ActionConfig::Text { text: "x".into() }),
        else_action: None,
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::KeystrokeSend])
    );
}

#[test]
fn pc_context_switch_unions_branches_and_default() {
    let mut mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    mappings.insert(
        0,
        Box::new(ActionConfig::SendMidi {
            port: "p".into(),
            message_type: "NoteOn".into(),
            channel: 0,
            note: Some(60),
            velocity: Some(100),
            controller: None,
            value: None,
            program: None,
            pitch: None,
            pressure: None,
        }),
    );
    mappings.insert(
        1,
        Box::new(ActionConfig::Launch {
            app: "Ableton".into(),
        }),
    );
    let action = ActionConfig::PcContextSwitch {
        channel: 0,
        device: "fcb1010".into(),
        mappings,
        default: Some(Box::new(ActionConfig::Text {
            text: "fallback".into(),
        })),
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([
            Capability::MidiOut,
            Capability::LaunchApp,
            Capability::KeystrokeSend,
        ])
    );
}

#[test]
fn cc_context_switch_unions_ranges_and_default() {
    let action = ActionConfig::CcContextSwitch {
        cc: 7,
        channel: 0,
        device: "modwheel".into(),
        ranges: vec![
            CcRange {
                min: 0,
                max: 63,
                action: Box::new(ActionConfig::Keystroke {
                    keys: "down".into(),
                    modifiers: vec![],
                }),
            },
            CcRange {
                min: 64,
                max: 127,
                action: Box::new(ActionConfig::SendMidi {
                    port: "out".into(),
                    message_type: "CC".into(),
                    channel: 0,
                    note: None,
                    velocity: None,
                    controller: Some(74),
                    value: Some(127),
                    program: None,
                    pitch: None,
                    pressure: None,
                }),
            },
        ],
        default: None,
    };
    assert_eq!(
        capabilities_for_action(&action),
        caps([Capability::KeystrokeSend, Capability::MidiOut])
    );
}

// ───────────────────────────────────────────────────────────────────────
// Cross-cutting invariants
// ───────────────────────────────────────────────────────────────────────

#[test]
fn no_capability_set_contains_unrelated_grants() {
    // Sanity check: a Keystroke action does not pull in MidiOut /
    // LaunchApp / etc. Catches accidental over-grants in the mapping.
    let action = ActionConfig::Keystroke {
        keys: "x".into(),
        modifiers: vec![],
    };
    let granted = capabilities_for_action(&action);
    for cap in [
        Capability::MidiOut,
        Capability::LaunchApp,
        Capability::ShellExec,
        Capability::OscOut,
        Capability::PluginExec,
        Capability::MouseSend,
        Capability::ConfigRead,
        Capability::ConfigWrite,
    ] {
        assert!(
            !granted.contains(&cap),
            "Keystroke should not require {:?}",
            cap
        );
    }
    // `InterpreterExec(_)` is parameterized over `InterpreterFamily`, so it
    // can't sit in the flat list above — assert no family variant leaks in.
    // This is the highest-privilege grant (the `/bin/sh -c …` gate); a
    // keystroke must never pull it in.
    assert!(
        !granted
            .iter()
            .any(|c| matches!(c, Capability::InterpreterExec(_))),
        "Keystroke should not require any InterpreterExec family, got {:?}",
        granted
    );
}

#[test]
fn fs_read_and_fs_write_are_not_yet_used_by_any_action() {
    // FsRead / FsWrite enter the action vocabulary when D3 3/N's
    // wrapper-resolution / interpreter classification step lands and
    // begins tracking files referenced by Shell argv. Until then NO
    // ActionConfig variant declares them. Sample EVERY variant (leaf +
    // recursive) — not just Shell/Plugin — so a regression that adds
    // FsRead/FsWrite to any arm (including a nested action inside a
    // Sequence/Repeat/Conditional/PcContextSwitch/CcContextSwitch) is
    // caught (#1529).
    let mut pc_mappings: IndexMap<u8, Box<ActionConfig>> = IndexMap::new();
    pc_mappings.insert(5, Box::new(ActionConfig::Text { text: "pc".into() }));

    let actions = [
        // Leaf variants.
        ActionConfig::Keystroke {
            keys: "a".into(),
            modifiers: vec!["cmd".into()],
        },
        ActionConfig::Text { text: "hi".into() },
        ActionConfig::Launch {
            app: "Safari".into(),
        },
        ActionConfig::Shell {
            sandbox: None,
            command: "/bin/ls".into(),
            args: None,
            timeout_ms: None,
        },
        ActionConfig::Delay { ms: 10 },
        ActionConfig::MouseClick {
            button: "left".into(),
            x: None,
            y: None,
        },
        ActionConfig::VolumeControl {
            operation: "up".into(),
            value: None,
        },
        ActionConfig::ModeChange {
            mode: "Edit".into(),
        },
        ActionConfig::SendMidi {
            port: "out".into(),
            message_type: "NoteOn".into(),
            channel: 0,
            note: Some(60),
            velocity: Some(100),
            controller: None,
            value: None,
            program: None,
            pitch: None,
            pressure: None,
        },
        ActionConfig::MidiForward {
            target: "synth".into(),
            transform: None,
        },
        ActionConfig::OscSend {
            host: "127.0.0.1".into(),
            port: 9000,
            address: "/x".into(),
            args: vec![],
        },
        ActionConfig::Plugin {
            plugin: "foo".into(),
            params: serde_json::Value::Null,
        },
        ActionConfig::Tap {
            message: "tap".into(),
        },
        // Recursive variants (capabilities_for_action recurses into children).
        ActionConfig::Sequence {
            actions: vec![ActionConfig::Text { text: "x".into() }],
        },
        ActionConfig::Repeat {
            action: Box::new(ActionConfig::Text { text: "x".into() }),
            count: 2,
            delay_ms: None,
        },
        ActionConfig::Conditional {
            condition: Condition::Always,
            then_action: Box::new(ActionConfig::Text { text: "t".into() }),
            else_action: Some(Box::new(ActionConfig::Text { text: "e".into() })),
        },
        ActionConfig::PcContextSwitch {
            channel: 0,
            device: "fcb1010".into(),
            mappings: pc_mappings,
            default: Some(Box::new(ActionConfig::Text { text: "d".into() })),
        },
        ActionConfig::CcContextSwitch {
            cc: 7,
            channel: 0,
            device: "modwheel".into(),
            ranges: vec![CcRange {
                min: 0,
                max: 127,
                action: Box::new(ActionConfig::Text { text: "r".into() }),
            }],
            default: None,
        },
    ];

    for action in actions {
        for cap in capabilities_for_action(&action) {
            assert!(
                !matches!(cap, Capability::FsRead(_) | Capability::FsWrite(_)),
                "no ActionConfig variant should declare {cap:?} yet, but {action:?} did"
            );
        }
    }
}
