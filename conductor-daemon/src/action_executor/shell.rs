// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Shell command execution and argv parsing (#1684 split from
//! `action_executor.rs`). Includes the `execute_shell` method plus the
//! pure-function command-line helpers (`derive_shell_argv`,
//! `parse_command_line`) and the ADR-027 D7 environment sanitiser
//! (`sanitised_shell_env`).

use super::ActionExecutor;
use conductor_core::dispatch::DispatchError;
use std::ffi::{OsStr, OsString};
use std::process::Command;
use std::time::Duration;

impl ActionExecutor {
    /// Execute a shell command WITHOUT implicitly wrapping it in an
    /// interpreter (no auto `sh -c` / `cmd /C` at the spawn boundary).
    ///
    /// # Security Design
    /// The executor does **not** itself invoke `sh`, `bash`, `cmd`, or
    /// `powershell` — commands are parsed into program + arguments and
    /// spawned via `Command::new(program).args(args)`, so metacharacters
    /// in user-supplied strings are passed verbatim as argv tokens
    /// rather than interpreted by a shell.
    ///
    /// This is **not** the same as "the running child can never be a
    /// shell interpreter" — with ADR-027 D3 §3.1 argv form (issue
    /// #1037), users can explicitly set `command = "/bin/sh"`, `args =
    /// ["-c", "..."]` and get full interpreter semantics by deliberate
    /// configuration. Phase 2's `allow_interpreters` policy applies
    /// the appropriate guard (warn / deny) for that case; this layer
    /// guarantees only that we don't *implicitly* introduce one.
    ///
    /// Defence-in-depth alongside `validate_shell_command()` (config
    /// loader), which blocks shell metacharacters in both `command` and
    /// argv-form `args` at load time.
    ///
    /// # Command Parsing
    /// Commands are split on whitespace into program + arguments:
    /// - "git status" → Command::new("git").args(&["status"])
    /// - "ls -la /tmp" → Command::new("ls").args(&["-la", "/tmp"])
    /// - "osascript -e 'display notification \"MIDI\"'" → Parsed with proper quote handling
    ///
    /// # Limitations
    /// The following shell features are NOT supported (by design):
    /// - Piping (|), redirection (>, <), command substitution ($(), ``)
    /// - Environment variable expansion ($VAR, ${VAR})
    /// - Globbing (*.txt, [a-z].sh)
    /// - Command chaining (;, &&, ||)
    ///
    /// Users must use the Launch action for apps or break complex operations
    /// into separate mappings.
    ///
    /// # Examples
    /// Supported:
    /// - "git status" → Direct execution
    /// - "open ~/Downloads" → Direct execution
    /// - "osascript -e 'set volume 50'" → Direct execution
    ///
    /// Blocked (by validation layer):
    /// - "git add . && git commit" → Contains &&
    /// - "ls | grep txt" → Contains |
    /// - "cat file.txt > output.txt" → Contains >
    pub(crate) fn execute_shell(
        &self,
        cmd: &str,
        explicit_args: Option<&[String]>,
        timeout_ms: Option<u64>,
        sandbox: Option<&conductor_core::config::types::ShellSandboxConfig>,
    ) -> Result<(), DispatchError> {
        let cmd = cmd.trim();

        // Handle empty command — no process starts, so this is a failed
        // dispatch (#1479), not a silent success.
        if cmd.is_empty() {
            tracing::warn!("Attempted to execute empty shell command");
            return Err(DispatchError::Shell("empty shell command".to_string()));
        }

        // ADR-027 D3 §3.1 (issue #1037) — derive argv. Two paths:
        //
        //   - **Argv form** (`explicit_args = Some(_)`): borrow `cmd`
        //     and the slice directly. Zero allocation — going through
        //     `derive_shell_argv` would clone every argv-form arg per
        //     Shell execution for no benefit on the hot path.
        //   - **Legacy form** (`explicit_args = None`): tokenise via
        //     `parse_command_line` — allocation is unavoidable; we
        //     need the parsed `Vec<String>` to outlive the spawn so
        //     `program` + `args` can borrow into it.
        //
        // `derive_shell_argv` is still the documented spec + pure-
        // function test surface for the decision logic; this site
        // branches explicitly only to skip its argv-form clone.
        let parsed_legacy: Vec<String>;
        let (program, args): (&str, &[String]) = if let Some(args) = explicit_args {
            (cmd, args)
        } else {
            let (legacy_parts, unbalanced) = parse_command_line_with_status(cmd);
            // #1717: unbalanced quote means the user's intent is
            // ambiguous — surface as a parse error rather than
            // silently swallowing the rest of the line.
            if unbalanced {
                tracing::warn!("Shell command has unbalanced quote: {}", cmd);
                return Err(DispatchError::Shell(format!(
                    "failed to parse shell command: {cmd}"
                )));
            }
            parsed_legacy = legacy_parts;
            let Some((head, tail)) = parsed_legacy.split_first() else {
                tracing::warn!("Failed to parse shell command: {}", cmd);
                return Err(DispatchError::Shell(format!(
                    "failed to parse shell command: {cmd}"
                )));
            };
            // #1711: parser now preserves empty quoted args, so a
            // quote-only input produces a single empty token instead
            // of zero tokens. Treat empty argv[0] as "nothing
            // runnable" — same surface as the pre-fix behaviour.
            if head.is_empty() {
                tracing::warn!("Shell command has empty program name: {}", cmd);
                return Err(DispatchError::Shell(format!(
                    "failed to parse shell command: {cmd}"
                )));
            }
            (head.as_str(), tail)
        };

        // ADR-027 D10a — persistence write veto. Refuse to spawn a shell
        // action whose resolved argv would write one of the daemon's own
        // protected state directories (`~/.conductor/`, the macOS
        // Application-Support dir, or the XDG data dir). This runs BEFORE the
        // env-sanitisation + spawn below, so a vetoed action never reaches
        // `execve` (best-effort deterrent; D10b's OS sandbox is the hard
        // boundary). The veto inherits D7's env strip by construction — the
        // spawn it guards already uses `sanitised_shell_env`.
        if let Some(veto) = super::persistence_veto::persistence_write_veto_env(program, args) {
            tracing::warn!(
                action = %cmd,
                matched_pattern = %veto.matched_pattern,
                protected_path = %veto.protected_path,
                "ShellVetoedByPersistenceCheck: refusing to write protected daemon state"
            );
            return Err(DispatchError::ShellVetoedByPersistenceCheck {
                matched_pattern: veto.matched_pattern,
            });
        }

        // ADR-027 D7 — sanitise the child's environment before
        // spawn. The daemon's full env is NEVER inherited:
        //
        //   - On Unix, only HOME / LANG / LC_ALL survive
        //     (`SHELL_ENV_PASSTHROUGH_ALLOWLIST`); `PATH` is
        //     ALWAYS force-set to `/usr/bin:/bin`
        //     (`SHELL_ENV_FIXED_PATH`), never inherited.
        //   - On Windows, the allowlist is broader (SystemRoot,
        //     WINDIR, ComSpec, PATHEXT, TEMP, TMP, USERPROFILE,
        //     LANG, LC_ALL — all needed for typical shell-action
        //     children) and `Path` is ALLOWLISTED rather than
        //     force-set (Windows program-resolution paths aren't
        //     OS-fixed). The PATH-hijack arm of D7 doesn't
        //     cleanly translate to Windows; tracked under D10b
        //     (sandboxing).
        //
        // Closes audit Finding F-07's env-hijack arm: a
        // same-user attacker who has set `LD_PRELOAD`,
        // `DYLD_INSERT_LIBRARIES`, `PYTHONPATH`, `NODE_OPTIONS`,
        // or (on Unix) a malicious `PATH` prefix in the daemon's
        // env can no longer turn a benign shell action into
        // arbitrary code execution.
        //
        // We use `vars_os()` rather than `vars()` because `vars()`
        // panics if any key/value contains non-UTF8 bytes (legal
        // on Unix). On a long-running daemon process that crash
        // would be triggerable just by having a non-UTF8 env var
        // present at the moment a shell action runs.
        let clean_env = sanitised_shell_env(std::env::vars_os());

        // Execute command WITHOUT shell interpreter
        // This is the critical security improvement: no sh -c, no cmd /C
        //
        // ADR-027 D7 (#1166): wrap the spawn in a SIGTERM→grace→SIGKILL
        // watchdog (process-group-scoped on Unix). The watcher
        // `JoinHandle` is intentionally dropped — the executor stays
        // fire-and-forget; the watcher detaches and reaps on its own.
        let mut command = Command::new(program);
        command.args(args).env_clear().envs(&clean_env);

        // ADR-027 §D10b — OS-sandbox the child before spawn. Installs a
        // `pre_exec` confinement hook (macOS Seatbelt / Linux Landlock).
        // When the platform can't sandbox and `allow_unsandboxed = false`,
        // fail closed rather than spawn an unconfined shell action.
        let policy = super::sandbox::SandboxPolicy::from_config(sandbox);
        match super::sandbox::apply_to_command(&mut command, &policy, self.shell_allow_unsandboxed)
        {
            Ok(super::sandbox::SandboxOutcome::Sandboxed) => {}
            Ok(super::sandbox::SandboxOutcome::Unsandboxed { reason }) => {
                tracing::warn!(
                    action = %cmd,
                    %reason,
                    "ADR-027 D10b: spawning shell action UNSANDBOXED (security.shell.allow_unsandboxed = true)"
                );
            }
            Err(super::sandbox::SandboxRefused { reason }) => {
                tracing::warn!(
                    action = %cmd,
                    %reason,
                    "ShellSandboxUnavailable: refusing to spawn an unsandboxable shell action"
                );
                return Err(DispatchError::ShellSandboxUnavailable { reason });
            }
        }

        let timeout = Duration::from_millis(
            timeout_ms
                .map(|ms| ms.clamp(1_000, 300_000))
                .unwrap_or(crate::shell_timeout::DEFAULT_SHELL_TIMEOUT_MS),
        );
        let grace = Duration::from_millis(crate::shell_timeout::SHELL_TIMEOUT_GRACE_MS);

        match crate::shell_timeout::spawn_with_timeout(command, timeout, grace) {
            Ok(_) => {
                // Spawned successfully — watcher thread enforces timeout.
                Ok(())
            }
            Err(e) => {
                // #1479: a spawn failure (missing binary, permission
                // denied, resource exhaustion) means no process started.
                // Propagate it so the caller returns a failed dispatch
                // instead of Completed.
                tracing::error!("Failed to execute command '{}': {}", cmd, e);
                Err(DispatchError::Shell(format!(
                    "failed to execute command '{cmd}': {e}"
                )))
            }
        }
    }
}

/// ADR-027 D7 — pass-through allowlist for shell-action child
/// processes (PATH is handled separately, see below).
///
/// The daemon's full environment is NEVER inherited by a shell
/// action. Only the variables on this list pass through from the
/// daemon's env unchanged, so a same-user attacker can't smuggle
/// code execution into a benign shell action by setting
/// `LD_PRELOAD=/tmp/evil.so` (or any of the documented dynamic-
/// loader / interpreter-injection vectors) in the daemon's env.
///
/// Unix set matches ADR-027 §D7 exactly. `USER`/`LOGNAME` etc.
/// are deliberately excluded — they're metadata an attacker
/// could influence (via `runuser`, `sudo -E`, or just process
/// inheritance) and most shell actions don't need them. Add
/// only with explicit review.
///
/// **Windows** (PR #1026 review, 2026-05-02): Windows child
/// processes need a richer baseline because of how the OS itself
/// works — `SystemRoot`/`WINDIR` resolve API DLLs, `PATHEXT`
/// drives executable-extension lookup, `ComSpec` points to
/// `cmd.exe`, `TEMP`/`TMP` are required by most file-creating
/// programs, and `USERPROFILE` is Windows's `HOME` equivalent.
/// Stripping these would make almost every shell action fail.
/// They're not credential-bearing or code-injection vectors, so
/// passing them through is consistent with the threat model.
#[cfg(unix)]
const SHELL_ENV_PASSTHROUGH_ALLOWLIST: &[&str] = &["HOME", "LANG", "LC_ALL"];

#[cfg(windows)]
const SHELL_ENV_PASSTHROUGH_ALLOWLIST: &[&str] = &[
    // OS-essential — programs link against DLLs in System32.
    "SystemRoot",
    "WINDIR",
    // Default shell location; some programs probe it.
    "ComSpec",
    // Executable-extension lookup; without this, `git status` in
    // cmd-style spawn finds nothing.
    "PATHEXT",
    // Temp dirs — required by most file-creating programs.
    "TEMP",
    "TMP",
    // Windows's HOME equivalent; some apps (PowerShell profiles,
    // Git's gitconfig probe, npm) depend on it.
    "USERPROFILE",
    // Locale (parity with Unix).
    "LANG",
    "LC_ALL",
];

/// ADR-027 D7 — fixed `PATH` for shell-action child processes
/// (Unix only).
///
/// On Unix, `PATH` is NOT inherited from the daemon's env. An
/// attacker who can influence the daemon's env (systemd unit
/// override, launchd plist, parent shell) could otherwise
/// prepend a malicious directory and hijack any shell action
/// that uses a relative program name (`git`, `ls`, …). A fixed
/// system-only PATH closes that vector regardless of how
/// `argv[0]` is named.
///
/// Per ADR-027 §D7 this is `/usr/bin:/bin` — the canonical POSIX
/// system locations. `/usr/local/bin`, `/opt/*`, and
/// `~/.cargo/bin` are deliberately excluded: those are
/// user-writable on common installs (Homebrew puts
/// `/usr/local/bin` in user space on Intel macOS;
/// `/opt/homebrew/bin` is user-owned on Apple Silicon).
///
/// **Windows** (PR #1026 review, 2026-05-02): the PATH-hijack
/// mitigation does NOT cleanly translate to Windows, where
/// typical shell actions need program-resolution paths that
/// aren't OS-fixed (`C:\Program Files\Git\cmd`,
/// `C:\Program Files\nodejs\`, etc.). A fixed
/// `C:\Windows\System32;C:\Windows` would make most shell
/// actions fail. Windows's `Path` is therefore allowlisted
/// (passes through) — the dynamic-loader / interpreter-
/// injection arm of D7 still applies (those vars aren't in the
/// allowlist), but the PATH-hijack arm is a documented gap on
/// Windows. Tracked for follow-up under D10b (sandboxing).
#[cfg(unix)]
const SHELL_ENV_FIXED_PATH: &str = "/usr/bin:/bin";

/// Build the sanitised environment for a shell-action child
/// process. Drops anything outside
/// [`SHELL_ENV_PASSTHROUGH_ALLOWLIST`] from the source env.
///
/// On Unix, `PATH` is also force-set to [`SHELL_ENV_FIXED_PATH`]
/// regardless of whether the source env contained one. On
/// Windows, `Path` is allowlisted (added to the passthrough set)
/// since the PATH-hijack mitigation doesn't cleanly apply to
/// Windows shell actions — see the [`SHELL_ENV_FIXED_PATH`]
/// docstring for the rationale.
///
/// Uses [`OsString`] throughout so callers can pass
/// `std::env::vars_os()` (which never panics on non-UTF8 keys or
/// values) without lossy conversion. Tests can pass synthetic
/// fixtures via [`HashMap::from_iter`].
///
/// The function is pure: no `std::env::set_var` side effects, no
/// global state.
pub(crate) fn sanitised_shell_env<I>(source: I) -> std::collections::HashMap<OsString, OsString>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut env: std::collections::HashMap<OsString, OsString> = std::collections::HashMap::new();
    for (k, v) in source {
        // Unix: env-var keys are case-sensitive (`HOME` and
        // `home` are distinct), so an exact-string match is
        // correct. Windows: keys are case-insensitive, so
        // `SystemRoot`, `SYSTEMROOT`, and `Systemroot` all
        // refer to the same variable; we compare via
        // `to_string_lossy().eq_ignore_ascii_case` to match
        // any spelling. Same `lossy` rationale as elsewhere —
        // env-var KEYS are guaranteed UTF-16 by Windows, so
        // the conversion is lossless in practice.
        #[cfg(unix)]
        let in_allowlist = SHELL_ENV_PASSTHROUGH_ALLOWLIST
            .iter()
            .any(|&allowed| k == OsStr::new(allowed));

        // Windows: case-insensitive match across the entire
        // allowlist plus `Path` (which Windows callers spell as
        // `Path`, `PATH`, or rarely `path`). Pre-fix, hardcoded
        // exact-case strings would silently drop e.g.
        // `SYSTEMROOT` from a parent process that uppercased
        // the key (PR #1026 review, 2026-05-02).
        #[cfg(windows)]
        let in_allowlist = {
            let key_str = k.to_string_lossy();
            SHELL_ENV_PASSTHROUGH_ALLOWLIST
                .iter()
                .any(|&allowed| key_str.eq_ignore_ascii_case(allowed))
                || key_str.eq_ignore_ascii_case("PATH")
        };

        if in_allowlist {
            // #1717: on Windows, env names are case-insensitive at
            // the OS level. `vars_os()` can yield both `PATH=x` and
            // `Path=y` (parent process set them with different
            // case); pre-fix we inserted under each ORIGINAL key,
            // so the cleaned env held two distinct HashMap entries
            // and Command::envs() merged them at the env block with
            // implementation-defined precedence. Canonicalize to
            // uppercase so duplicates collapse to one HashMap entry
            // before spawn — last write wins, deterministically,
            // and behaviourally equivalent because Windows resolves
            // env-name lookups case-insensitively post-spawn either
            // way.
            #[cfg(windows)]
            let k = OsString::from(k.to_string_lossy().to_uppercase());
            env.insert(k, v);
        }
    }
    // Unix: ALWAYS override PATH with the fixed safe value —
    // never inherited. This closes the PATH-hijack vector even
    // when `argv[0]` is a relative program name.
    #[cfg(unix)]
    env.insert(OsString::from("PATH"), OsString::from(SHELL_ENV_FIXED_PATH));
    env
}

/// Decision-logic spec for argv derivation under the two ADR-027 D3
/// §3.1 schema shapes (issue #1037). **Pure, owned, testable** — returns
/// owned `String`s so callers can exercise the branching without
/// providing storage. `execute_shell` itself takes the equivalent
/// branch inline with borrowed inputs to avoid the per-execution
/// `Vec<String>` clone on the argv-form hot path.
///
/// - **Argv form** (`explicit_args = Some(_)`): `cmd` is taken verbatim
///   as argv[0]; `explicit_args` is argv[1..]. The eventual spawn site
///   uses `Command::new(cmd).args(args)`, which produces an OS-level
///   argv of `[cmd] ++ args` — i.e. `cmd` IS argv[0]; the caller does
///   NOT need to repeat it inside `explicit_args`. No whitespace
///   tokenisation happens on either input; the quoting semantics live
///   entirely with the caller. Returns `Some((cmd, explicit_args))`
///   unless `cmd` is empty.
/// - **Legacy form** (`explicit_args = None`): `cmd` is a full command
///   line; [`parse_command_line`] splits it into argv0+rest, respecting
///   single/double quotes. Returns `None` whenever the parser yields
///   zero tokens — the most common case is whitespace-only input, but
///   inputs consisting solely of unbalanced or empty quote pairs
///   (`"'"`, `"''"`, `"\"\""`) can also tokenise to zero parts; the
///   caller should treat any `None` as "nothing runnable" without
///   assuming why. Kept for back-compat with every existing config in
///   the wild.
///
/// Pure function — no I/O, no spawning. Callable from tests without
/// touching `Command`.
///
/// # Examples
///
/// ```
/// # use conductor_daemon::derive_shell_argv;
/// // Argv form — `cmd` is argv[0], explicit args verbatim.
/// assert_eq!(
///     derive_shell_argv("/bin/sh", Some(&["-c".into(), "env > /tmp/out".into()])),
///     Some(("/bin/sh".into(), vec!["-c".into(), "env > /tmp/out".into()])),
/// );
///
/// // Legacy form — whitespace-split.
/// assert_eq!(
///     derive_shell_argv("echo hello world", None),
///     Some(("echo".into(), vec!["hello".into(), "world".into()])),
/// );
///
/// // Empty / unparseable legacy command — no argv to spawn.
/// assert_eq!(derive_shell_argv("   ", None), None);
/// ```
pub fn derive_shell_argv(
    cmd: &str,
    explicit_args: Option<&[String]>,
) -> Option<(String, Vec<String>)> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return None;
    }
    if let Some(args) = explicit_args {
        // Argv form: cmd is argv[0], args is argv[1..]. No tokenisation.
        // An empty `args` slice is still valid (Some(vec![]) means
        // "argv-form invocation with zero arguments").
        return Some((cmd.to_string(), args.to_vec()));
    }
    // Legacy form: whitespace-split the full command line. Consume
    // `parts` via `into_iter` so the head + tail move out without
    // cloning (the previous `head.clone()` / `tail.to_vec()` pair
    // allocated twice per Shell execution for no benefit).
    //
    // #1717: use the strict variant so unbalanced quotes (parser
    // exits mid-quoted-segment) surface as "nothing runnable" —
    // same treatment as empty/quote-only inputs, rather than
    // silently swallowing the rest of the line as one argument.
    let (parts, unbalanced) = parse_command_line_with_status(cmd);
    if unbalanced {
        return None;
    }
    let mut iter = parts.into_iter();
    let head = iter.next()?;
    // #1711: the parser now preserves explicit empty quoted args (so
    // mid-stream `foo "" bar` correctly yields three tokens). When the
    // *first* token is empty (i.e. the user typed quote-only or led
    // with `""`), there's no program to spawn — return None so the
    // caller treats it as "nothing runnable", matching the pre-#1711
    // contract that callers depend on. The mid-stream case still
    // benefits: `derive_shell_argv("a \"\" b", None)` returns
    // `Some(("a", ["", "b"]))`.
    if head.is_empty() {
        return None;
    }
    let tail: Vec<String> = iter.collect();
    Some((head, tail))
}

/// Parse a command line into program + arguments, respecting quoted strings
///
/// This is a simple whitespace-based parser that handles:
/// - Single quotes: 'text with spaces'
/// - Double quotes: "text with spaces"
/// - Escaped quotes: \"text\" within quotes
/// - Unquoted arguments: split on whitespace
///
/// # Examples
/// ```
/// # use conductor_daemon::parse_command_line;
/// assert_eq!(parse_command_line("git status"), vec!["git", "status"]);
/// assert_eq!(parse_command_line("ls -la /tmp"), vec!["ls", "-la", "/tmp"]);
/// assert_eq!(parse_command_line("echo 'hello world'"), vec!["echo", "hello world"]);
/// assert_eq!(parse_command_line("osascript -e 'code'"), vec!["osascript", "-e", "code"]);
/// ```
///
/// # Security Note
/// This parser does NOT perform shell expansion (variables, globs, etc.).
/// This is intentional for security - we want literal arguments only.
pub fn parse_command_line(cmd: &str) -> Vec<String> {
    // Lenient public API — discards the unbalanced-quote signal.
    // Callers that need strictness should use
    // [`parse_command_line_with_status`] and check the flag (#1717).
    parse_command_line_with_status(cmd).0
}

/// Parse a command line and report whether the input ended
/// mid-quoted-segment (#1717).
///
/// Tokenisation is identical to [`parse_command_line`] — same lenient
/// behaviour (extract whatever tokens we can) — but the second return
/// value is `true` when the parser exits with an open `'` or `"`.
/// Strict callers like [`derive_shell_argv`] and `execute_shell` use
/// this to reject ambiguous inputs (the user almost certainly didn't
/// mean to swallow the rest of the line as one argument).
///
/// Kept module-private — the public [`parse_command_line`] is the
/// stable surface; callers that need the flag are inside the
/// `action_executor` module.
pub(crate) fn parse_command_line_with_status(cmd: &str) -> (Vec<String>, bool) {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = cmd.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    // #1711: track whether the current segment has *ever* entered a
    // quoted region (open `"` or `'`). POSIX shells preserve empty
    // quoted args (`cmd "" foo` is 3 argv entries); pre-fix the
    // whitespace separator only pushed when `current` was non-empty,
    // so `""` between tokens was silently dropped. With this flag, an
    // empty segment that came from explicit quoting still gets pushed.
    // Reset alongside `current` on every push.
    let mut had_quote = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_double_quote => {
                // Toggle single quote mode (unless inside double quotes)
                in_single_quote = !in_single_quote;
                had_quote = true;
            }
            '"' if !in_single_quote => {
                // Toggle double quote mode (unless inside single quotes)
                in_double_quote = !in_double_quote;
                had_quote = true;
            }
            '\\' if in_double_quote => {
                // Handle escape sequences in double quotes
                if let Some(&next_ch) = chars.peek() {
                    if next_ch == '"' || next_ch == '\\' {
                        current.push(chars.next().unwrap());
                    } else {
                        current.push(ch);
                    }
                } else {
                    current.push(ch);
                }
            }
            ' ' | '\t' | '\n' | '\r' if !in_single_quote && !in_double_quote => {
                // Whitespace outside quotes: end of argument. Push when
                // the segment had content OR was an explicit quoted
                // region (#1711 — preserves `""`/`''`).
                if !current.is_empty() || had_quote {
                    parts.push(current.clone());
                    current.clear();
                    had_quote = false;
                }
            }
            _ => {
                // Regular character: add to current argument
                current.push(ch);
            }
        }
    }

    // Add final argument if any (or an empty trailing quoted segment).
    if !current.is_empty() || had_quote {
        parts.push(current);
    }

    // #1717: parser exited with an unclosed quote — user input is
    // malformed (unbalanced `"` or `'`). Lenient callers ignore this;
    // strict callers (derive_shell_argv, execute_shell) reject.
    let unbalanced = in_single_quote || in_double_quote;
    (parts, unbalanced)
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
