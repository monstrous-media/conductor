// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Integration tests for `ActionConfig::Shell`'s ADR-027 D3 argv-form
//! schema (issue #1037).
//!
//! The legacy schema is a single `command: String` whitespace-split at
//! execution time. The argv-form schema additionally accepts
//! `args: Vec<String>`, passed directly to `Command::args` without any
//! whitespace tokenisation. The argv form is what foreshadows D3 §3.1:
//! capabilities live in the config (eventually), the executor honours
//! argv exactly, and quote/space handling stops being parser-defined.
//!
//! These tests pin the **deserialisation** half of Phase 1. The executor
//! half — that `args.is_some()` skips `parse_command_line` — is exercised
//! in `conductor-daemon/tests/`.
//!
//! Some test payloads contain shell metacharacters (`>`, `;`, etc.) on
//! purpose — they mirror the exact bypass class issue #1037 calls out.
//! These tests exercise the **schema layer only** (`toml::from_str`);
//! they do NOT run config validation, so the metacharacter blocklist
//! Phase 1 also extends to argv-form `args` (see
//! `conductor-core/src/config/validation.rs::validate_shell_arg`) does
//! not fire here. Validation-layer rejection is pinned separately in
//! `config::validation::tests::test_shell_argv_form_args_metacharacters_blocked`
//! and friends — so deserialising successfully here does **not** imply
//! the config would load successfully.

use conductor_core::config::ActionConfig;

// ───────────────────────────────────────────────────────────────────────
// Argv-form deserialisation
// ───────────────────────────────────────────────────────────────────────

#[test]
fn shell_argv_form_with_args_array_deserialises() {
    // The exact TOML shape reported in issue #1037 — `command` is the
    // resolved binary path, `args` is the argv array. Equivalent to
    // execve("/bin/sh", &["-c", "env > /tmp/out.txt"]).
    let toml_src = r#"
type = "Shell"
command = "/bin/sh"
args = ["-c", "env > /tmp/out.txt"]
"#;

    let action: ActionConfig =
        toml::from_str(toml_src).expect("argv-form Shell action should deserialise");

    match action {
        ActionConfig::Shell { command, args, .. } => {
            assert_eq!(command, "/bin/sh");
            assert_eq!(
                args,
                Some(vec!["-c".to_string(), "env > /tmp/out.txt".to_string()])
            );
        }
        other => panic!("expected ActionConfig::Shell, got {:?}", other),
    }
}

#[test]
fn shell_legacy_form_without_args_still_deserialises() {
    // The legacy single-string schema must keep working — every existing
    // config in the wild uses this shape and we cannot break them.
    let toml_src = r#"
type = "Shell"
command = "echo hello world"
"#;

    let action: ActionConfig =
        toml::from_str(toml_src).expect("legacy Shell action should still deserialise");

    match action {
        ActionConfig::Shell { command, args, .. } => {
            assert_eq!(command, "echo hello world");
            assert_eq!(args, None, "legacy form omits args -> None");
        }
        other => panic!("expected ActionConfig::Shell, got {:?}", other),
    }
}

#[test]
fn shell_argv_form_with_empty_args_array_deserialises() {
    // `args = []` is meaningful — it asserts "this is an argv-form
    // invocation with zero arguments" (equivalent to execve("/bin/ls"))
    // and disables the legacy whitespace-splitting at the executor.
    // Distinct from `args` being omitted.
    let toml_src = r#"
type = "Shell"
command = "/bin/ls"
args = []
"#;

    let action: ActionConfig =
        toml::from_str(toml_src).expect("argv-form with empty args should deserialise");

    match action {
        ActionConfig::Shell { command, args, .. } => {
            assert_eq!(command, "/bin/ls");
            assert_eq!(args, Some(vec![]), "empty argv-form keeps the Some marker");
        }
        other => panic!("expected ActionConfig::Shell, got {:?}", other),
    }
}

#[test]
fn shell_argv_form_round_trips_through_serde() {
    // Round-trip pins both halves of the schema — the Some(_) variant
    // serialises back to a TOML shape that re-parses to the same
    // command/args; the None variant omits the `args` key entirely (no
    // `args = null` leak that would confuse downstream tooling /
    // diff-rendering).
    let argv = ActionConfig::Shell {
        sandbox: None,
        command: "/usr/bin/osascript".into(),
        args: Some(vec!["-e".into(), "tell app \"Finder\" to activate".into()]),
        timeout_ms: None,
    };
    let toml_str = toml::to_string(&argv).expect("serialise argv form");
    let parsed: ActionConfig = toml::from_str(&toml_str).expect("re-parse argv form");
    match parsed {
        ActionConfig::Shell { command, args, .. } => {
            assert_eq!(command, "/usr/bin/osascript");
            assert_eq!(
                args,
                Some(vec![
                    "-e".to_string(),
                    "tell app \"Finder\" to activate".to_string(),
                ])
            );
        }
        other => panic!(
            "expected ActionConfig::Shell on round-trip, got {:?}",
            other
        ),
    }

    let legacy = ActionConfig::Shell {
        sandbox: None,
        command: "echo hi".into(),
        args: None,
        timeout_ms: None,
    };
    let toml_str = toml::to_string(&legacy).expect("serialise legacy form");
    assert!(
        !toml_str.contains("args"),
        "legacy form must omit `args` entirely, not write `args = null` — got:\n{}",
        toml_str
    );
}
