// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Pure-function tests for [`derive_shell_argv`] (ADR-027 D3 §3.1,
//! issue #1037).
//!
//! Pins the argv-derivation logic the Shell executor uses before
//! `Command::spawn`. Lightweight by design — no spawn, no async, no
//! daemon state — so it's fast to run in isolation and won't pile on
//! the heavy tokio-based integration tests when the workspace runs
//! under memory pressure.
//!
//! Some payloads here contain shell metacharacters (`>`, `;`, etc.) on
//! purpose — they exist to prove the **derivation layer** passes
//! argv-form args through verbatim without re-tokenising. They do NOT
//! imply such payloads would survive **config validation**: the
//! conductor-core validator extends its metacharacter blocklist to
//! argv-form `args` (see
//! `conductor-core/src/config/validation.rs::validate_shell_arg`), so a
//! real config of the same shape would be rejected at load time.
//! These derivation tests sit below the validator and are deliberately
//! pure-function — the layering is `validate → derive → spawn`, and
//! this file pins only the middle step.

use conductor_daemon::derive_shell_argv;

// ───────────────────────────────────────────────────────────────────────
// Argv form (`explicit_args = Some(_)`) — no tokenisation
// ───────────────────────────────────────────────────────────────────────

#[test]
fn argv_form_passes_args_through_verbatim() {
    let args = vec!["-c".to_string(), "env > /tmp/out.txt".to_string()];
    let result = derive_shell_argv("/bin/sh", Some(&args));
    assert_eq!(
        result,
        Some(("/bin/sh".to_string(), args)),
        "argv-form must NOT tokenise — the embedded space + redirect \
         have to land in argv[2] exactly as supplied, otherwise the \
         shell interpreter receives a different script than the user \
         wrote"
    );
}

#[test]
fn argv_form_preserves_empty_args_array() {
    // `args = Some(vec![])` is a meaningful distinct shape from
    // `args = None` — it asserts "argv-form invocation with zero
    // arguments" rather than "legacy whitespace-split form". The
    // derivation must keep them distinguishable so D3 §3.2's
    // wrapper-classification can rely on it.
    assert_eq!(
        derive_shell_argv("/bin/ls", Some(&[])),
        Some(("/bin/ls".to_string(), vec![])),
    );
}

#[test]
fn argv_form_does_not_split_command_on_whitespace() {
    // The whole point of argv form: the executor must NOT whitespace-
    // split `command`. If the user wrote `command = "/bin/sh -c …"`
    // in argv form (probably a mistake, but legal), we hand
    // `/bin/sh -c …` to execve as a single argv[0] and let the OS
    // surface the resulting ENOENT. We don't silently rescue it by
    // re-tokenising, because that would defeat the point of giving
    // users an unambiguous argv shape.
    let args = vec!["arg1".into()];
    assert_eq!(
        derive_shell_argv("/bin/sh -c 'evil'", Some(&args)),
        Some(("/bin/sh -c 'evil'".to_string(), vec!["arg1".into()])),
    );
}

// ───────────────────────────────────────────────────────────────────────
// Legacy form (`explicit_args = None`) — whitespace-split
// ───────────────────────────────────────────────────────────────────────

#[test]
fn legacy_form_whitespace_splits_command() {
    assert_eq!(
        derive_shell_argv("echo hello world", None),
        Some(("echo".to_string(), vec!["hello".into(), "world".into()])),
    );
}

#[test]
fn legacy_form_respects_single_quotes() {
    // Legacy parser must keep treating `'…'` as a quoted single token —
    // we cannot regress the existing config-file behaviour.
    assert_eq!(
        derive_shell_argv("echo 'hello world'", None),
        Some(("echo".to_string(), vec!["hello world".into()])),
    );
}

#[test]
fn legacy_form_returns_none_for_whitespace_only_command() {
    // Empty / whitespace-only string in legacy form yields no parts —
    // executor short-circuits before spawn.
    assert_eq!(derive_shell_argv("   \t  ", None), None);
}

#[test]
fn empty_command_returns_none_for_both_forms() {
    // Both shapes agree: empty command = nothing runnable.
    assert_eq!(derive_shell_argv("", None), None);
    assert_eq!(derive_shell_argv("", Some(&["-c".into()])), None);
}

#[test]
fn legacy_form_returns_none_for_quote_only_inputs() {
    // #1711 update: `parse_command_line` was rewritten to preserve
    // empty quoted args (so mid-stream `foo "" bar` correctly yields
    // three tokens). Quote-only inputs now tokenise to a single empty
    // token instead of zero tokens — but the user contract for
    // `derive_shell_argv` is unchanged: "nothing runnable" returns
    // None. The guard in `derive_shell_argv` (return None when
    // argv[0] is empty) preserves that contract because an empty
    // program path is never spawnable.
    assert_eq!(derive_shell_argv("'", None), None);
    assert_eq!(derive_shell_argv("''", None), None);
    assert_eq!(derive_shell_argv("\"\"", None), None);
    // Leading quote-only also returns None (empty argv[0]) even
    // though tail tokens exist — can't spawn an empty binary.
    assert_eq!(derive_shell_argv("\"\" foo", None), None);
}

#[test]
fn legacy_form_returns_none_for_unbalanced_quotes() {
    // #1717: an unbalanced quote (parser exits mid-quoted-segment)
    // means the user's intent is ambiguous — silently swallowing the
    // rest of the line as one argument is the wrong default. Treat
    // it the same as quote-only / empty input: "nothing runnable",
    // surfaced as None so the caller can raise a parse error.
    assert_eq!(derive_shell_argv("cmd \"open arg", None), None);
    assert_eq!(derive_shell_argv("cmd 'open arg", None), None);
    // Tail-only unbalanced too.
    assert_eq!(derive_shell_argv("'", None), None);
    assert_eq!(derive_shell_argv("\"", None), None);
}

#[test]
fn legacy_form_preserves_mid_stream_empty_quoted_args() {
    // #1711: the actual reported bug — an intentional empty argument
    // between other tokens must reach the spawned process as a real
    // argv entry. Pre-fix, `parse_command_line` dropped `""` between
    // tokens, so `cmd "" foo` arrived as `argc==2` instead of 3.
    let Some((program, args)) = derive_shell_argv(r#"cmd "" foo"#, None) else {
        panic!("mid-stream empty quoted arg must NOT cause None — there's a real program to run");
    };
    assert_eq!(program, "cmd");
    assert_eq!(args, vec!["".to_string(), "foo".to_string()]);

    // Single-quoted form same story.
    let Some((program, args)) = derive_shell_argv("cmd '' foo", None) else {
        panic!("single-quoted empty arg must NOT cause None");
    };
    assert_eq!(program, "cmd");
    assert_eq!(args, vec!["".to_string(), "foo".to_string()]);
}
