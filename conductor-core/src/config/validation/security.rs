// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Shell-command, interpreter-policy, and app-name security validators.

use super::*;

/// Validate shell command for security (prevents command injection)
///
/// Blocks dangerous patterns that could enable command injection attacks:
/// - Command chaining: `;`, `&&`, `||`
/// - Piping: `|`
/// - Command substitution: `` ` ``, `$(`, `${`
/// - Redirects: `>`, `>>`, `<`, `<<`
/// - Background execution: `&` (at end of command)
pub(super) fn validate_shell_command(command: &str, path: &str, ctx: &mut ValidationCtx) {
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
pub(super) fn validate_interpreter_policy(
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
pub(super) fn command_has_runnable_token(command: &str) -> bool {
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
pub(super) fn validate_shell_arg(arg: &str, path: &str, ctx: &mut ValidationCtx) {
    validate_shell_input(arg, path, ctx, ShellInputKind::Arg);
}

#[derive(Clone, Copy)]
pub(super) enum ShellInputKind {
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

pub(super) fn validate_shell_input(
    input: &str,
    path: &str,
    ctx: &mut ValidationCtx,
    kind: ShellInputKind,
) {
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
pub(super) fn validate_app_name(app: &str, path: &str, ctx: &mut ValidationCtx) {
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
