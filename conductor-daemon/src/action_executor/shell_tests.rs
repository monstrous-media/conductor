// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Unit tests for the `shell` module (#1719 extraction from `shell.rs`).
//!
//! Declared from `shell.rs` as `#[cfg(test)] #[path = "shell_tests.rs"]
//! mod tests;` — still a child module of `shell`, so `use super::*;`
//! continues to reach the parent's `pub(crate)` items
//! (`parse_command_line_with_status`, `sanitised_shell_env`,
//! `execute_shell`) without exposing them more broadly. The split keeps
//! production-only `shell.rs` under the LLM Council `verify` 50K-char
//! ceiling — see PR #1718's audit comment for the motivating numbers.

use super::*;
use crate::action_executor::test_support::test_executor;
use conductor_core::Action;

// ========== Command Line Parser Tests ==========

#[test]
fn test_parse_simple_command() {
    assert_eq!(parse_command_line("git status"), vec!["git", "status"]);
}

#[test]
fn test_parse_command_with_args() {
    assert_eq!(parse_command_line("ls -la /tmp"), vec!["ls", "-la", "/tmp"]);
}

#[test]
fn test_parse_single_quoted_string() {
    assert_eq!(
        parse_command_line("echo 'hello world'"),
        vec!["echo", "hello world"]
    );
}

#[test]
fn test_parse_double_quoted_string() {
    assert_eq!(
        parse_command_line("echo \"hello world\""),
        vec!["echo", "hello world"]
    );
}

#[test]
fn test_parse_osascript_command() {
    assert_eq!(
        parse_command_line("osascript -e 'set volume 50'"),
        vec!["osascript", "-e", "set volume 50"]
    );
}

#[test]
fn test_parse_escaped_quotes_in_double_quotes() {
    assert_eq!(
        parse_command_line("echo \"hello \\\"world\\\"\""),
        vec!["echo", "hello \"world\""]
    );
}

#[test]
fn test_parse_mixed_quotes() {
    assert_eq!(
        parse_command_line("cmd 'single quoted' \"double quoted\" unquoted"),
        vec!["cmd", "single quoted", "double quoted", "unquoted"]
    );
}

#[test]
fn test_parse_empty_command() {
    assert_eq!(parse_command_line(""), Vec::<String>::new());
}

#[test]
fn test_parse_with_status_detects_unbalanced_double_quote() {
    // #1717: parse_command_line stays lenient (extracts tokens it can
    // find), but the internal *_with_status variant exposes whether
    // the parser ended mid-quoted-segment so callers like
    // derive_shell_argv / execute_shell can reject the input.
    let (parts, unbalanced) = parse_command_line_with_status("cmd \"open arg");
    assert!(unbalanced, "unbalanced double quote must set the flag");
    assert_eq!(
        parts,
        vec!["cmd".to_string(), "open arg".to_string()],
        "tokens still extracted leniently for inspection"
    );
}

#[test]
fn test_parse_with_status_detects_unbalanced_single_quote() {
    let (parts, unbalanced) = parse_command_line_with_status("cmd 'open arg");
    assert!(unbalanced, "unbalanced single quote must set the flag");
    assert_eq!(parts, vec!["cmd".to_string(), "open arg".to_string()]);
}

#[test]
fn test_parse_with_status_balanced_quotes_unflagged() {
    // Properly balanced input must NOT set unbalanced=true.
    let (_parts, unbalanced) = parse_command_line_with_status(r#"cmd "ok" 'ok' bare"#);
    assert!(
        !unbalanced,
        "balanced quoted segments must not flag unbalanced"
    );
    // The empty-quoted-arg case from #1711 is also balanced.
    let (_parts, unbalanced) = parse_command_line_with_status(r#"cmd "" foo"#);
    assert!(!unbalanced, "explicit empty quoted arg is still balanced");
}

#[test]
fn test_parse_with_status_quote_only_inputs_unbalanced_when_odd() {
    // Single solitary quote = unbalanced.
    let (_parts, unbalanced) = parse_command_line_with_status("'");
    assert!(unbalanced);
    let (_parts, unbalanced) = parse_command_line_with_status("\"");
    assert!(unbalanced);
    // Pair of quotes = balanced empty arg.
    let (_parts, unbalanced) = parse_command_line_with_status("''");
    assert!(!unbalanced);
    let (_parts, unbalanced) = parse_command_line_with_status("\"\"");
    assert!(!unbalanced);
}

#[test]
fn test_parse_preserves_empty_quoted_args() {
    // #1711: an empty `""` or `''` between other tokens is a real,
    // intentional positional argument in POSIX shell semantics
    // (`argc` for `cmd "" foo` is 3, not 2). The naive parser
    // dropped it silently because the inter-quote `current`
    // remained empty and the whitespace-separator's
    // `!current.is_empty()` guard skipped the push. Track whether
    // the current segment ever entered a quoted region and push
    // empty segments in that case.
    assert_eq!(
        parse_command_line(r#"foo "" bar"#),
        vec!["foo".to_string(), "".to_string(), "bar".to_string()],
        "empty double-quoted arg must be preserved"
    );
    assert_eq!(
        parse_command_line("foo '' bar"),
        vec!["foo".to_string(), "".to_string(), "bar".to_string()],
        "empty single-quoted arg must be preserved"
    );
    // Empty quoted arg at end of input.
    assert_eq!(
        parse_command_line(r#"foo """#),
        vec!["foo".to_string(), "".to_string()],
        "trailing empty quoted arg must be preserved"
    );
    // Two consecutive empty quoted args.
    assert_eq!(
        parse_command_line(r#"foo "" "" bar"#),
        vec![
            "foo".to_string(),
            "".to_string(),
            "".to_string(),
            "bar".to_string()
        ],
        "consecutive empty quoted args must each be preserved"
    );
}

#[test]
fn test_parse_whitespace_only() {
    assert_eq!(parse_command_line("   \t\n  "), Vec::<String>::new());
}

#[test]
fn test_parse_multiple_spaces() {
    assert_eq!(
        parse_command_line("git    status    --short"),
        vec!["git", "status", "--short"]
    );
}

#[test]
fn test_parse_trailing_spaces() {
    assert_eq!(parse_command_line("git status  "), vec!["git", "status"]);
}

#[test]
fn test_parse_leading_spaces() {
    assert_eq!(parse_command_line("  git status"), vec!["git", "status"]);
}

#[test]
fn test_parse_notification_command() {
    assert_eq!(
        parse_command_line("osascript -e 'display notification \"MIDI triggered!\"'"),
        vec![
            "osascript",
            "-e",
            "display notification \"MIDI triggered!\""
        ]
    );
}

#[test]
fn test_parse_file_path_with_tilde() {
    assert_eq!(
        parse_command_line("open ~/Downloads"),
        vec!["open", "~/Downloads"]
    );
}

#[test]
fn test_parse_complex_apfs_command() {
    assert_eq!(
        parse_command_line("system_profiler SPUSBDataType"),
        vec!["system_profiler", "SPUSBDataType"]
    );
}

// ========== Security: No Shell Expansion ==========

#[test]
fn test_parse_does_not_expand_variables() {
    // Variables should be passed as literals, not expanded
    assert_eq!(parse_command_line("echo $HOME"), vec!["echo", "$HOME"]);
}

#[test]
fn test_parse_does_not_expand_globs() {
    // Globs should be passed as literals, not expanded
    assert_eq!(parse_command_line("ls *.txt"), vec!["ls", "*.txt"]);
}

#[test]
fn shell_returns_err_for_nonexistent_binary_argv_form() {
    // #1479: a shell action whose program can't be spawned (ENOENT)
    // must surface as Err — pre-fix `execute_shell` swallowed the
    // spawn error and `execute` returned Ok(Completed), so the
    // monitor/metrics pipeline saw a "completed" action that never
    // ran. Argv form: explicit args present.
    let mut executor = test_executor();
    let action = Action::Shell {
        sandbox: None,
        command: "/definitely/not/a/real/binary_xyzzy_98765".to_string(),
        args: Some(vec![]),
        timeout_ms: None,
    };
    let result = executor.execute(action, None);
    assert!(
        result.is_err(),
        "shell spawn failure must propagate; got {:?}",
        result
    );
    match result.unwrap_err() {
        DispatchError::Shell(msg) => assert!(
            msg.contains("binary_xyzzy_98765"),
            "error must name the failing command for diagnosability: {msg}"
        ),
        other => panic!("Expected DispatchError::Shell, got {:?}", other),
    }
}

#[test]
fn shell_returns_err_for_nonexistent_binary_legacy_form() {
    // Legacy form: no explicit args, command tokenised via
    // parse_command_line. Same ENOENT spawn failure must propagate.
    let mut executor = test_executor();
    let action = Action::Shell {
        sandbox: None,
        command: "/definitely/not/a/real/binary_xyzzy_98765".to_string(),
        args: None,
        timeout_ms: None,
    };
    let result = executor.execute(action, None);
    assert!(
        result.is_err(),
        "legacy-form shell spawn failure must propagate; got {:?}",
        result
    );
    assert!(matches!(result.unwrap_err(), DispatchError::Shell(_)));
}

#[test]
fn shell_returns_err_for_empty_command() {
    // An empty command starts no process; reporting Completed would
    // be the same false-success #1479 fixes.
    let mut executor = test_executor();
    let action = Action::Shell {
        sandbox: None,
        command: "   ".to_string(),
        args: None,
        timeout_ms: None,
    };
    let result = executor.execute(action, None);
    assert!(
        matches!(result, Err(DispatchError::Shell(_))),
        "empty shell command must be an error, got {:?}",
        result
    );
}

#[test]
fn shell_returns_err_for_unbalanced_quotes() {
    // #1717: an unbalanced quote means the user's intent is
    // ambiguous — surface as a parse error rather than silently
    // swallowing the rest of the line as one argument.
    let mut executor = test_executor();
    for input in [r#"cmd "open arg"#, "cmd 'open arg"] {
        let action = Action::Shell {
            sandbox: None,
            command: input.to_string(),
            args: None,
            timeout_ms: None,
        };
        let result = executor.execute(action, None);
        assert!(
            matches!(result, Err(DispatchError::Shell(_))),
            "unbalanced quote `{input}` must surface as DispatchError::Shell; got {result:?}"
        );
    }
}

// ========== ADR-027 D7 — Shell env sanitisation ==========
//
// Audit Finding F-07 (in part): the shell-action child process
// inherits the daemon's full environment, so a same-user attacker
// who has set `LD_PRELOAD=/tmp/evil.so` (or `DYLD_INSERT_LIBRARIES`,
// `PYTHONPATH`, `NODE_OPTIONS`, a malicious `PATH` prefix, etc.)
// in the daemon's env can turn ANY benign shell action into
// arbitrary code execution.
//
// `sanitised_shell_env()` strips the well-known dynamic-loader
// and interpreter-injection variables before each spawn, drops
// everything outside the explicit pass-through allowlist (HOME,
// LANG, LC_ALL — matches ADR-027 §D7), and ALWAYS overrides
// PATH with a fixed safe value (`/usr/bin:/bin`) so a
// hijacked-PATH inheritance can't turn a relative `argv[0]`
// into an attacker-controlled program.

/// Helper: build an OsString-keyed HashMap from string pairs
/// for tests. Production callers feed `std::env::vars_os()`
/// directly so non-UTF8 keys don't panic.
fn os_env(pairs: &[(&str, &str)]) -> std::collections::HashMap<OsString, OsString> {
    pairs
        .iter()
        .map(|(k, v)| (OsString::from(*k), OsString::from(*v)))
        .collect()
}

#[test]
fn sanitised_env_strips_dynamic_loader_variables() {
    // Names taken from ADR-027 §D7. The dynamic loader trusts
    // these in non-setuid processes and they're the textbook
    // env-hijack vectors on Unix-likes.
    let dangerous = [
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "LD_AUDIT",
        "DYLD_INSERT_LIBRARIES",
        "DYLD_LIBRARY_PATH",
        "DYLD_FRAMEWORK_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "DYLD_FALLBACK_FRAMEWORK_PATH",
    ];
    let pairs: Vec<(&str, &str)> = dangerous
        .iter()
        .map(|k| (*k, "/tmp/evil.so"))
        .chain(std::iter::once(("PATH", "/usr/bin:/bin")))
        .collect();
    let source = os_env(&pairs);

    let cleaned = sanitised_shell_env(source);

    for var in dangerous {
        assert!(
            !cleaned.contains_key(OsStr::new(var)),
            "ADR-027 D7: env-sanitisation must strip `{var}`; \
             leaving it in the child env lets a same-user \
             attacker hijack any shell action via the dynamic \
             loader. Cleaned env: {cleaned:?}",
        );
    }
}

#[test]
fn sanitised_env_strips_interpreter_injection_variables() {
    // Per-language interpreter injection vectors. Each is the
    // language's documented "load this code automatically"
    // mechanism; an attacker controls them by setting the
    // daemon's env and weaponising the next shell action.
    let dangerous = [
        "PYTHONPATH",
        "PYTHONSTARTUP",
        "NODE_OPTIONS",
        "RUBYLIB",
        "RUBYOPT",
        "PERL5LIB",
        "PERL5OPT",
    ];
    let pairs: Vec<(&str, &str)> = dangerous.iter().map(|k| (*k, "/tmp/inject")).collect();
    let source = os_env(&pairs);

    let cleaned = sanitised_shell_env(source);

    for var in dangerous {
        assert!(
            !cleaned.contains_key(OsStr::new(var)),
            "ADR-027 D7: env-sanitisation must strip `{var}`. \
             Cleaned env: {cleaned:?}",
        );
    }
}

#[cfg(unix)]
#[test]
fn sanitised_env_passes_through_allowlist() {
    // Legitimate vars the Unix child process needs to function.
    // ADR-027 §D7's exact set: HOME for user data, LANG/LC_ALL
    // for i18n. PATH is intentionally NOT here — see the
    // dedicated PATH test below for why.
    let pairs = [
        ("HOME", "/Users/alice"),
        ("LANG", "en_GB.UTF-8"),
        ("LC_ALL", "en_GB.UTF-8"),
    ];
    let source = os_env(&pairs);

    let cleaned = sanitised_shell_env(source);

    for (k, v) in &pairs {
        let got = cleaned
            .get(OsStr::new(*k))
            .map(|os| os.to_string_lossy().into_owned());
        assert_eq!(
            got.as_deref(),
            Some(*v),
            "ADR-027 D7: pass-through allowlist must preserve `{k}` \
             unchanged. Cleaned env: {cleaned:?}",
        );
    }
}

#[cfg(windows)]
#[test]
fn sanitised_env_passes_through_windows_essentials() {
    // PR #1026 review (2026-05-02): Windows child processes
    // need a richer baseline because of how the OS itself
    // works. Stripping `SystemRoot`/`WINDIR`/`PATHEXT`/etc.
    // would make almost every shell action fail (even
    // `cmd.exe /C echo hi` depends on `SystemRoot` for DLL
    // loading). They aren't credential-bearing or
    // code-injection vectors, so passing them through is
    // consistent with the threat model.
    //
    // `Path` is also passed through on Windows (the Unix
    // fixed-PATH approach doesn't translate — typical
    // Windows shell actions need program-resolution paths
    // that aren't OS-fixed).
    let pairs = [
        ("SystemRoot", "C:\\Windows"),
        ("WINDIR", "C:\\Windows"),
        ("ComSpec", "C:\\Windows\\System32\\cmd.exe"),
        ("PATHEXT", ".COM;.EXE;.BAT;.CMD"),
        ("TEMP", "C:\\Users\\alice\\AppData\\Local\\Temp"),
        ("TMP", "C:\\Users\\alice\\AppData\\Local\\Temp"),
        ("USERPROFILE", "C:\\Users\\alice"),
        ("LANG", "en_GB.UTF-8"),
        ("LC_ALL", "en_GB.UTF-8"),
        ("Path", "C:\\Windows\\System32;C:\\Program Files\\Git\\cmd"),
    ];
    let source = os_env(&pairs);

    let cleaned = sanitised_shell_env(source);

    for (k, v) in &pairs {
        let got = cleaned
            .get(OsStr::new(*k))
            .map(|os| os.to_string_lossy().into_owned());
        assert_eq!(
            got.as_deref(),
            Some(*v),
            "ADR-027 D7 (Windows): allowlist must preserve `{k}` \
             unchanged. Cleaned env: {cleaned:?}",
        );
    }
}

#[cfg(windows)]
#[test]
fn sanitised_env_collapses_case_variant_duplicates_on_windows() {
    // #1717: Windows env names are case-insensitive at the OS
    // level, but `vars_os()` can yield both `PATH=...` and
    // `Path=...` if a parent process set them with different
    // case. Pre-fix, `sanitised_shell_env` inserted under each
    // ORIGINAL key, so the cleaned env contained two distinct
    // HashMap entries; Command::envs() on Windows then merged
    // them at the env block with implementation-defined
    // precedence. Post-fix, we canonicalize to uppercase on
    // Windows, so duplicates collapse to one HashMap entry
    // before spawn (last write wins, deterministically).
    let source = vec![
        (OsString::from("PATH"), OsString::from("first")),
        (OsString::from("Path"), OsString::from("second")),
    ];
    let cleaned = sanitised_shell_env(source);
    let path_entries: Vec<&OsString> = cleaned
        .keys()
        .filter(|k| k.to_string_lossy().eq_ignore_ascii_case("PATH"))
        .collect();
    assert_eq!(
        path_entries.len(),
        1,
        "case-variant PATH entries must collapse to one HashMap key; got {cleaned:?}"
    );
}

#[cfg(windows)]
#[test]
fn sanitised_env_windows_allowlist_is_case_insensitive() {
    // Regression test for PR #1026 round-3 review (Copilot,
    // 2026-05-02 12:47): pre-fix the Windows allowlist
    // matcher used exact-case `OsStr::new(allowed)`
    // comparisons, which would silently drop
    // parent-supplied `SYSTEMROOT`, `WINDIR`, `path`, etc.
    // because Windows env keys are case-insensitive but
    // the matcher wasn't.
    //
    // Now matches via `to_string_lossy().eq_ignore_ascii_case`
    // for every allowlist entry. Verify by passing each
    // allowed key in a non-canonical case spelling.
    let pairs = [
        ("SYSTEMROOT", "C:\\Windows"),                 // all upper
        ("windir", "C:\\Windows"),                     // all lower
        ("comspec", "C:\\Windows\\System32\\cmd.exe"), // all lower
        ("PathExt", ".COM;.EXE;.BAT;.CMD"),            // mixed
        ("temp", "C:\\Users\\alice\\AppData\\Local\\Temp"),
        ("TMP", "C:\\Users\\alice\\AppData\\Local\\Temp"),
        ("UserProfile", "C:\\Users\\alice"),
        ("PATH", "C:\\Windows\\System32;C:\\Program Files\\Git\\cmd"), // upper
    ];
    let source = os_env(&pairs);
    let cleaned = sanitised_shell_env(source);

    for (k, _) in &pairs {
        // #1717 canonicalisation: cleaned env now stores keys
        // uppercased on Windows (so case-variant duplicates
        // collapse before spawn — Windows env lookups are
        // case-insensitive, so this is behaviour-preserving).
        // Use case-insensitive lookup here so the test still
        // pins the original guarantee (non-canonical-case input
        // key survives sanitisation).
        let survived = cleaned
            .keys()
            .any(|ck| ck.to_string_lossy().eq_ignore_ascii_case(k));
        assert!(
            survived,
            "ADR-027 D7 (Windows): allowlist match must be \
             case-insensitive — non-canonical-case key `{k}` \
             must survive sanitisation. Cleaned env keys: {:?}",
            cleaned.keys().collect::<Vec<_>>(),
        );
    }
}

#[test]
fn sanitised_env_drops_unknown_variables_including_user_and_logname() {
    // Default-deny: anything outside the pass-through allowlist
    // is dropped — including USER/LOGNAME (which earlier drafts
    // included but ADR-027 §D7 deliberately excludes; an
    // attacker can influence them via runuser/sudo -E and most
    // shell actions don't need them).
    let pairs = [
        ("MY_CUSTOM_VAR", "leaked"),
        ("AWS_SECRET_ACCESS_KEY", "leaked"),
        ("USER", "alice"),
        ("LOGNAME", "alice"),
    ];
    let source = os_env(&pairs);

    let cleaned = sanitised_shell_env(source);

    for (k, _) in &pairs {
        assert!(
            !cleaned.contains_key(OsStr::new(*k)),
            "ADR-027 D7: `{k}` must be dropped (default-deny / \
             not on pass-through allowlist). Cleaned: {cleaned:?}",
        );
    }
}

#[cfg(unix)]
#[test]
fn sanitised_env_always_overrides_path_with_fixed_safe_value() {
    // Even if the daemon's env had a (potentially attacker-
    // influenced) PATH, the cleaned env always carries the
    // fixed `/usr/bin:/bin` value. This closes the PATH-hijack
    // arm of F-07 — a relative `argv[0]` like `git` will only
    // resolve against trusted system directories regardless of
    // what the daemon's env said.
    let source = os_env(&[("PATH", "/tmp/evil:/usr/bin")]);
    let cleaned = sanitised_shell_env(source);

    let path = cleaned
        .get(OsStr::new("PATH"))
        .map(|os| os.to_string_lossy().into_owned());
    assert_eq!(
        path.as_deref(),
        Some("/usr/bin:/bin"),
        "ADR-027 D7: PATH must be the fixed safe value, NEVER \
         inherited from the daemon's env. Got: {path:?}",
    );
    // Sanity: no `/tmp/evil` substring leaks through.
    assert!(
        !path.as_deref().unwrap_or("").contains("/tmp/evil"),
        "Hostile PATH prefix must not survive sanitisation. \
         Got: {path:?}",
    );
}

#[cfg(unix)]
#[test]
fn sanitised_env_sets_path_even_when_source_had_none() {
    // Empty source env (rare under some launchd configs / CI)
    // still yields a workable PATH so commands can resolve.
    let source: std::collections::HashMap<OsString, OsString> = std::collections::HashMap::new();

    let cleaned = sanitised_shell_env(source);

    let path = cleaned
        .get(OsStr::new("PATH"))
        .map(|os| os.to_string_lossy().into_owned());
    assert_eq!(path.as_deref(), Some("/usr/bin:/bin"));
}

#[test]
fn sanitised_env_handles_non_utf8_source_without_panic() {
    // Production callers pass `std::env::vars_os()`, which can
    // legally contain non-UTF8 keys/values on Unix. We must not
    // panic in that case (regression guard for the previous
    // `std::env::vars()` impl which did panic). Build an
    // OsString from raw bytes that aren't valid UTF-8 to
    // exercise this directly.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let bad_key = OsString::from_vec(vec![0xFF, 0xFE, b'B', b'A', b'D']);
        let bad_val = OsString::from_vec(vec![b'v', 0xC0, 0xC1, b'l']);
        let source: std::collections::HashMap<OsString, OsString> =
            [(bad_key, bad_val)].into_iter().collect();

        // Just calling it must not panic. Result should not
        // contain the bad key (it's not on the allowlist) but
        // PATH must still be set.
        let cleaned = sanitised_shell_env(source);
        assert!(cleaned.contains_key(OsStr::new("PATH")));
    }
}

// ========== ADR-027 D10a — persistence write veto (boundary) ==========
//
// These exercise the veto through the full `execute` → `execute_shell`
// path. They rely on the REAL `$HOME` (always set in the test env) so
// `~/.conductor/...` resolves to a protected root — but the veto fires
// *before* spawn, so no process runs and nothing is written.

#[test]
fn shell_veto_blocks_tee_into_protected_argv_form() {
    let mut executor = test_executor();
    let action = Action::Shell {
        sandbox: None,
        command: "tee".to_string(),
        args: Some(vec!["~/.conductor/config.toml".to_string()]),
        timeout_ms: None,
    };
    match executor.execute(action, None).unwrap_err() {
        DispatchError::ShellVetoedByPersistenceCheck { matched_pattern } => {
            assert_eq!(matched_pattern, "tee");
        }
        other => panic!("expected ShellVetoedByPersistenceCheck, got {other:?}"),
    }
}

#[test]
fn shell_veto_blocks_sh_c_redirect_into_protected() {
    // The interpreter re-introduces a redirect inside a single argv token,
    // which the raw command-string validation does not see.
    let mut executor = test_executor();
    let action = Action::Shell {
        sandbox: None,
        command: "sh".to_string(),
        args: Some(vec![
            "-c".to_string(),
            "echo pwned > ~/.conductor/config.toml".to_string(),
        ]),
        timeout_ms: None,
    };
    assert!(matches!(
        executor.execute(action, None).unwrap_err(),
        DispatchError::ShellVetoedByPersistenceCheck { .. }
    ));
}

#[test]
fn shell_veto_does_not_fire_for_safe_read() {
    // Reading a protected file (no write) must NOT be vetoed. `cat` of a
    // (likely absent) file may fail to spawn or exit non-zero, but the
    // error must never be the persistence veto.
    let mut executor = test_executor();
    let action = Action::Shell {
        sandbox: None,
        command: "cat".to_string(),
        args: Some(vec!["~/.conductor/config.toml".to_string()]),
        timeout_ms: None,
    };
    if let Err(e) = executor.execute(action, None) {
        assert!(
            !matches!(e, DispatchError::ShellVetoedByPersistenceCheck { .. }),
            "reading a protected file must not trip the write veto: {e:?}"
        );
    }
}
