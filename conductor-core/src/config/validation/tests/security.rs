// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

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
