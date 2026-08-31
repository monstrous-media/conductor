// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Wrapper-chain resolution and interpreter classification (ADR-027 D3 3/N,
//! spec §3.2).
//!
//! ## Why this exists
//!
//! The single biggest bypass of v1.0's `Shell` action gating was running an
//! interpreter via a wrapper binary. `command = "/usr/bin/env python3 -c
//! '…'"` walks straight past an interpreter blocklist that only checks
//! `argv[0]`, because `argv[0]` is `/usr/bin/env` — not on the list — while
//! the *effective* binary that runs user code is `python3`. The same trick
//! applies to `xargs`, `sudo`, `nice`, `timeout`, `nohup`, `setsid`, and
//! about a dozen other wrappers.
//!
//! Spec §3.2 closes the bypass class by:
//!
//! 1. Recursively resolving the wrapper chain to find the **effective**
//!    binary that will actually execute user code, with a hard depth limit
//!    to defeat pathological loops.
//! 2. Classifying the effective binary's basename as either a known
//!    interpreter family or `NotInterpreter`.
//! 3. Adding [`Capability::InterpreterExec(family)`] on top of
//!    [`Capability::ShellExec`] at runtime, so the gate's caller-side
//!    grant for raw `ShellExec` does **not** transitively grant the
//!    ability to run a Python script.
//!
//! ## Scope of (3/N)
//!
//! Pure functions — [`resolve_effective_executable`] and
//! [`classify_interpreter`]. No call sites yet. The runtime path that
//! actually augments the gate's capability check with the resolved family
//! lands together with D1 (peer-credential IPC auth) and the wiring
//! sub-piece, to keep the Phase 1A atomic-bundle invariant.
//!
//! ## Caller-side failure handling
//!
//! The resolver returns `Result<ResolvedExecutable, ResolveError>`. Any
//! error — depth exceeded, malformed argv, an unregistered wrapper at
//! `argv[0]`, a wrapper with no following program — must be treated by
//! the caller as
//! [`InterpreterClassification::Interpreter(InterpreterFamily::Other)`],
//! the strictest classification. This caller-side convention is what
//! gives the gate its fail-closed posture for the failure paths the
//! resolver explicitly reports.
//!
//! ## Coverage and the unknown-wrapper / unknown-option caveats
//!
//! All 16 wrappers from spec §3.2 have per-wrapper handlers covering the
//! options seen in real bypass attempts on Linux + macOS — the catalogue
//! of `env -i`, `env KEY=VAL`, `sudo -u user`, `sudo --user=value`,
//! `timeout 5s`, `nice -n 10`, `xargs -I {}`, `nohup`, `setsid`,
//! `unshare -r`, `stdbuf -oL`, `taskset 0x1`, `time`, `schedtool -e`.
//!
//! **Known limitation — wrappers absent from `WRAPPERS`.** [`is_wrapper`]
//! checks the basename against the closed [`WRAPPERS`] slice. A real
//! wrapper binary on the system that hasn't been added to that slice
//! (or a future / vendor-specific wrapper not yet known to us) will
//! be classified as a non-wrapper and returned as the
//! [`ResolvedExecutable::effective_path`] — the resolver does not
//! fail closed for this case because it does not know it is a wrapper.
//! The dispatch `_` arm in [`find_wrapped_program`] only protects
//! against the *internal* invariant of registering a wrapper in
//! `WRAPPERS` without giving it a handler arm; it cannot defend against
//! unknown wrappers in the wild. The mitigation is keeping `WRAPPERS`
//! comprehensive — adding a wrapper here is the explicit human review
//! point. The `every_wrapper_has_a_handler` test pins the in-tree
//! invariant.
//!
//! **Known limitation — unknown options within a recognised wrapper.**
//! The current per-wrapper handlers maintain an explicit table of
//! options that *take* a value (consume the next argv element). Any
//! flag not in that table is treated as **standalone** and skipped
//! singly. If a real wrapper grows a new value-taking option that the
//! handler hasn't been updated for, the resolver will skip the option
//! itself, see the option's *value* as the next argv element, and may
//! return that value as the wrapped program. The downstream classifier
//! then operates on the wrong basename. This is a fail-open path for
//! exotic invocations and is tracked as a follow-up: tighten each
//! per-wrapper handler to a closed allowlist so unknown options return
//! [`ResolveError::CannotResolveWrapper`] (caller fails closed). See
//! the issue tracker for the tightening work; until it lands, audit
//! reviewers should rely on [`ResolvedExecutable::wrapper_chain`] (D13a)
//! to surface unexpected resolutions for human inspection, and on
//! [`classify_interpreter`]'s closed allowlist of interpreter basenames
//! as a defence in depth (a misclassified "program" that isn't on the
//! interpreter list still requires `InterpreterExec` only if it is
//! itself an interpreter).

use crate::security::InterpreterFamily;
use std::path::{Path, PathBuf};

/// Wrapper binaries that delegate execution to a subsequent argv element.
/// When these appear as `argv[0]`, the *effective* binary is determined by
/// resolving forward through their arguments per the per-wrapper rules
/// in [`find_wrapped_program`].
///
/// Sourced from spec §3.2. Adding a wrapper here without a matching
/// per-wrapper handler in [`find_wrapped_program`] **fails closed**:
/// the dispatch match's `_` arm returns
/// [`ResolveError::CannotResolveWrapper`], so a wrapper appearing as
/// `argv[0]` with no handler can never be confused for a non-wrapper
/// binary. The reverse — a handler with no registered wrapper — is
/// dead code: [`is_wrapper`] consults this slice. The
/// `every_wrapper_has_a_handler` test pins the first half of the
/// contract at test time.
const WRAPPERS: &[&str] = &[
    "env",
    "nice",
    "ionice",
    "timeout",
    "nohup",
    "sudo",
    "doas",
    "setsid",
    "chpst",
    "runuser",
    "xargs",
    "unshare",
    "stdbuf",
    "time",
    "schedtool",
    "taskset",
];

/// Default recursion limit for [`resolve_effective_executable`]. Eight
/// levels comfortably covers real wrapper chains (`sudo env nohup …` is
/// already three) while shutting down pathological loops. Override via
/// the `max_depth` parameter when callers need a different limit.
pub const DEFAULT_MAX_WRAPPER_DEPTH: usize = 8;

/// The result of walking the wrapper chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExecutable {
    /// The final binary that will actually run user code, after wrapper
    /// unwinding. The gate uses this — and **not** `argv[0]` — when
    /// classifying via [`classify_interpreter`].
    pub effective_path: PathBuf,

    /// The chain of wrappers traversed, in walk order. Recorded for
    /// audit logging — D13a's denial / approval stream renders this
    /// chain so reviewers can see *how* a request reached the
    /// effective binary.
    pub wrapper_chain: Vec<PathBuf>,
}

/// Why wrapper resolution failed. **Callers should fail closed**: treat
/// every variant as [`InterpreterClassification::Interpreter`] with
/// [`InterpreterFamily::Other`], the strictest classification. Otherwise
/// an exotic invocation could bypass the interpreter capability check.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolveError {
    /// The wrapper chain exceeded `max_depth`. Either pathological
    /// nesting (a wrapper recursively wrapping itself) or a chain
    /// genuinely longer than the configured limit.
    DepthExceeded { limit: usize },

    /// A wrapper appeared as `argv[0]` (or as the resolved binary at
    /// some intermediate step) but had no following program argument
    /// — e.g. a bare `/usr/bin/env` with no args, or `/usr/bin/sudo
    /// -u nobody` with no command after the option.
    MissingProgram { wrapper: String },

    /// The argv could not be parsed for the named wrapper — e.g. an
    /// unrecognised long option combined with an unrecognised short
    /// option in a position that looks neither flag-like nor
    /// positional. Caller must fail closed.
    CannotResolveWrapper { wrapper: String },

    /// The path is malformed (no UTF-8 basename, empty, etc.).
    MalformedPath,
}

/// Classification of a resolved binary's basename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpreterClassification {
    /// Not on the interpreter list. `ShellExec` alone is sufficient.
    NotInterpreter,
    /// On the interpreter list. The gate must additionally check
    /// [`Capability::InterpreterExec`] for the named family.
    Interpreter(InterpreterFamily),
}

/// Walk the argv chain to determine the effective executable.
///
/// Starts with `command` as `argv[0]`. If `command`'s basename is on
/// [`WRAPPERS`], records it in the wrapper chain and resolves the
/// wrapped program by per-wrapper rules; the wrapped program becomes
/// the new "current" and the loop continues. Bottoms out either when
/// the current basename is **not** a wrapper (success) or when
/// `max_depth` iterations have completed (`DepthExceeded` error).
///
/// The function is pure: no filesystem access, no PATH lookup, no
/// symlink resolution. It works strictly on the argv strings the
/// caller provides.
///
/// **Per-wrapper option tables enumerate value-taking options only.**
/// Any flag the table doesn't list is treated as a standalone flag —
/// safe for genuinely-standalone options, fail-open if a real
/// value-taking option has been omitted (the option's value can be
/// returned as the "wrapped program"). See the module-level
/// "unknown-option caveat" docs and the tightening follow-up. Callers
/// must always fail closed on `Err` (treat as
/// [`InterpreterFamily::Other`]). The per-wrapper handlers cover the
/// options seen in real bypass attempts — see the tests at the bottom
/// of this file for the catalogue.
pub fn resolve_effective_executable(
    command: &Path,
    args: &[String],
    max_depth: usize,
) -> Result<ResolvedExecutable, ResolveError> {
    let mut wrapper_chain: Vec<PathBuf> = Vec::new();
    let mut current_path: PathBuf = command.to_path_buf();
    let mut current_args: Vec<String> = args.to_vec();
    let mut depth: usize = 0;

    loop {
        let basename = current_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(ResolveError::MalformedPath)?;

        // Non-wrapper (or anything we don't recognise as a wrapper):
        // bottom out and return as the effective binary. Done before
        // the depth check so a non-wrapper command with `max_depth =
        // 0` still resolves cleanly to itself rather than tripping
        // `DepthExceeded`.
        if !is_wrapper(basename) {
            return Ok(ResolvedExecutable {
                effective_path: current_path,
                wrapper_chain,
            });
        }

        // Wrapper detected. If we've already used our depth budget,
        // fail closed before unwinding further.
        if depth >= max_depth {
            return Err(ResolveError::DepthExceeded { limit: max_depth });
        }

        // Record this wrapper and resolve forward.
        wrapper_chain.push(current_path.clone());

        let next_program_idx = find_wrapped_program(basename, &current_args)?;
        if next_program_idx >= current_args.len() {
            return Err(ResolveError::MissingProgram {
                wrapper: basename.to_string(),
            });
        }

        let next_path = PathBuf::from(&current_args[next_program_idx]);
        current_args = current_args[next_program_idx + 1..].to_vec();
        current_path = next_path;
        depth += 1;
    }
}

/// Classify the resolved binary's basename. Match is on basename only;
/// absolute path is checked elsewhere by the gate.
///
/// Names not on the list classify as [`InterpreterClassification::NotInterpreter`].
/// The gate's caller can still treat unresolvable / suspicious paths as
/// [`InterpreterFamily::Other`] via the fail-closed convention on
/// [`ResolveError`].
pub fn classify_interpreter(path: &Path) -> InterpreterClassification {
    // Non-UTF8 / missing basename: classify as Interpreter(Other) for
    // fail-closed symmetry with `resolve_effective_executable`'s
    // `MalformedPath` error. Returning `NotInterpreter` here would
    // mean a path like `b"/usr/bin/\xff\xfepython3"` (bytes that
    // happen to round-trip back to a python interpreter under some
    // exotic locale) classifies as not-an-interpreter and only
    // requires `ShellExec` — over-permissive for a path we can't
    // even decode.
    let Some(basename) = path.file_name().and_then(|n| n.to_str()) else {
        return InterpreterClassification::Interpreter(InterpreterFamily::Other);
    };

    // Strip a single trailing version suffix so `python3`, `python3.12`,
    // `bash5`, `node20` all classify identically. The version regex is
    // strict — only digit-and-dot sequences trail.
    let stem = strip_version_suffix(basename);

    // Distribution / build aliases that don't strip cleanly to `python`
    // / `node` via the digit-suffix rule:
    //
    // - `pythonw` — windowed-mode Python (macOS / Windows). Strip
    //   doesn't apply (`w` isn't a digit), so an explicit arm is
    //   required.
    // - `python3m` — Python 3 with `--with-pymalloc` (M-suffix
    //   builds). Strip doesn't apply (trailing `m` after digits).
    // - `python3dm` — debug + pymalloc combination. Same reason.
    // - `nodejs` — Debian / older Ubuntu name for `node`. Strip
    //   doesn't apply (`s` isn't a digit).
    //
    // Names that *do* reduce via `strip_version_suffix` to one of
    // the above stems are deliberately **not** in this match (they
    // would be unreachable arms): e.g. `python2` → `python` already
    // matches the `"python"` arm; `pythonw3` → `pythonw` already
    // matches the `"pythonw"` arm. Every entry below is one whose
    // stripped stem is not a sibling.
    //
    // Without these aliases the gate would over-permit: `nodejs
    // script.js` would classify as `NotInterpreter` and only
    // require `ShellExec`, never `InterpreterExec(Node)`.
    let family = match stem {
        "python" | "pypy" | "micropython" | "pythonw" | "python3m" | "python3dm" => {
            InterpreterFamily::Python
        }
        "ruby" | "jruby" | "truffleruby" | "irb" => InterpreterFamily::Ruby,
        "perl" | "raku" => InterpreterFamily::Perl,
        "node" | "nodejs" | "deno" | "bun" => InterpreterFamily::Node,
        "bash" => InterpreterFamily::Bash,
        "sh" | "dash" | "ash" => InterpreterFamily::Sh,
        "zsh" => InterpreterFamily::Zsh,
        "fish" => InterpreterFamily::Fish,
        "awk" | "gawk" | "mawk" | "nawk" | "sed" => InterpreterFamily::AwkOrSed,
        "lua" | "luajit" => InterpreterFamily::Lua,
        "tclsh" | "wish" => InterpreterFamily::TclSh,
        "php" => InterpreterFamily::Php,
        // osascript runs AppleScript via -e and is exactly the bypass
        // class InterpreterExec exists to gate. There is no native
        // AppleScript family in the enum yet — file under Other so the
        // gate can grant per-target without committing to a new variant
        // until a use case demands it.
        "osascript" => InterpreterFamily::Other,
        _ => return InterpreterClassification::NotInterpreter,
    };
    InterpreterClassification::Interpreter(family)
}

// ─── helpers ──────────────────────────────────────────────────────────

fn is_wrapper(basename: &str) -> bool {
    WRAPPERS.contains(&basename)
}

/// Strip a trailing version suffix from an interpreter basename.
/// `python3` → `python`, `python3.12` → `python`, `bash5` → `bash`,
/// `node20.10.0` → `node`. Names without a version suffix are returned
/// unchanged.
fn strip_version_suffix(name: &str) -> &str {
    let trim_end = name
        .rfind(|c: char| !c.is_ascii_digit() && c != '.')
        .map_or(0, |i| i + 1);
    if trim_end == 0 {
        // All-numeric basename — give up, return as-is.
        return name;
    }
    &name[..trim_end]
}

/// Per-wrapper rule lookup. Returns the index in `args` where the
/// wrapped program lives — i.e. `args[idx]` is the program path /
/// command name, and `args[idx + 1 ..]` are its own args. On
/// unrecognised invocations returns `Err(CannotResolveWrapper)` so
/// the caller fails closed.
fn find_wrapped_program(wrapper: &str, args: &[String]) -> Result<usize, ResolveError> {
    match wrapper {
        "env" => find_env_program(args),
        "sudo" | "doas" => find_sudo_program(args, wrapper),
        "nice" | "ionice" => find_option_then_program(args, wrapper, &['n', 'c'], &[]),
        "timeout" => find_timeout_program(args),
        "xargs" => find_xargs_program(args),
        // "nohup", "setsid", "chpst", "runuser", "unshare", "stdbuf",
        // "time", "schedtool", "taskset" — all share the "first
        // non-option argv element is the wrapped program" shape, with
        // a per-wrapper short-option-with-value table.
        //
        // nohup accepts only `--help` / `--version` (which cause nohup
        // to print and exit without execing anything — benign) plus
        // the standard `--` end-of-options separator. We route through
        // `find_option_then_program` with empty option tables so the
        // `--` handling is shared. Without this, `nohup -- python3`
        // resolved to `--` as the effective binary — bypass once
        // the resolver is wired in.
        "nohup" => find_option_then_program(args, wrapper, &[], &[]),
        // /usr/bin/time accepts options: -p / --portability, -v /
        // --verbose, -q / --quiet, -a / --append (standalone) and
        // -f FORMAT / --format / -o FILE / --output (value-taking).
        // Without parsing them, `time -p python3` would resolve to
        // `-p` as the wrapped program — bypass.
        "time" => find_option_then_program(args, wrapper, &['f', 'o'], &["format", "output"]),
        "setsid" => find_option_then_program(args, wrapper, &[], &[]),
        "chpst" => find_option_then_program(
            args,
            wrapper,
            &[
                'u', 'U', 'b', 'e', '/', 'C', 'o', 'p', 'f', 'm', 'l', 'L', 'P',
            ],
            &[],
        ),
        "runuser" => find_option_then_program(args, wrapper, &['c', 'g', 'G', 's', 'u'], &[]),
        // unshare: all short options are standalone namespace flags
        // (`-r`/--map-root-user, `-U`/--user, `-i`/--ipc, `-m`/--mount,
        // `-n`/--net, `-p`/--pid, `-u`/--uts, `-c`/--map-current-user,
        // `-T`/--time, `-C`/--cgroup). Only long options that name a
        // value (--map-user, --setgid, …) consume the next argv element.
        "unshare" => find_option_then_program(
            args,
            wrapper,
            &[],
            &[
                "map-user",
                "map-group",
                "setuid",
                "setgid",
                "map-auto",
                "boottime",
                "monotonic",
            ],
        ),
        "stdbuf" => find_option_then_program(
            args,
            wrapper,
            &['i', 'o', 'e'],
            &["input", "output", "error"],
        ),
        // schedtool: `-D` (debug) and `-F` (FIFO scheduler) are
        // standalone flags; only `-p / -a / -M / -P / -n` take values.
        "schedtool" => find_option_then_program(args, wrapper, &['p', 'a', 'M', 'P', 'n'], &[]),
        // taskset: `taskset MASK COMMAND` is the canonical form (mask
        // first positional, command second). The general `find_option_then_program`
        // would return the MASK as the wrapped program — bypass.
        // Custom handler skips the MASK positional and rejects `-p`
        // mode (which manipulates an existing PID and spawns no
        // command).
        "taskset" => find_taskset_program(args),
        _ => Err(ResolveError::CannotResolveWrapper {
            wrapper: wrapper.to_string(),
        }),
    }
}

/// `env [-i] [-u VAR] [--] [KEY=VAL...] PROGRAM [args...]`
fn find_env_program(args: &[String]) -> Result<usize, ResolveError> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];

        // Common short / long flags that don't take values.
        //
        // Note on `--block-signal` / `--default-signal` / `--ignore-signal`:
        // these have **optional** SIG arguments per GNU coreutils. The
        // unambiguous `--block-signal=SIG` form is handled by the long-
        // flag-with-`=value` arm below. The bare `--block-signal` form
        // (which env actually treats as "block all signals" — no value
        // consumed) is intentionally **not** in this standalone list:
        // we let it fall through to the unrecognised-flag arm so the
        // resolver fails closed for `env --block-signal SIG PROG` style
        // invocations where it's ambiguous whether the user expected
        // SIG or PROG to be the program. Symmetric treatment of all
        // three GNU signal options. The cost is that legitimate bare
        // `env --block-signal PROG` invocations resolve as
        // `InterpreterFamily::Other` instead of cleanly classifying
        // PROG; the bypass risk of the alternative is not worth that
        // ergonomic gain.
        if a == "-i" || a == "-0" || a == "-v" || a == "--ignore-environment" || a == "--null" {
            i += 1;
            continue;
        }

        // -u VAR / --unset VAR — consume next arg.
        if a == "-u" || a == "--unset" {
            i += 2;
            continue;
        }

        // -uVAR concatenated short form (matches sudo's -uvalue
        // handling). Single arg, no follow-up consumed.
        if a.starts_with("-u") && a.len() > 2 && !a.starts_with("--") {
            i += 1;
            continue;
        }

        // Long flag with `=value`.
        if a.starts_with("--") && a.contains('=') {
            i += 1;
            continue;
        }

        // -- separator.
        if a == "--" {
            i += 1;
            return Ok(i);
        }

        // KEY=VAL variable assignment (env's signature feature).
        if !a.starts_with('-') && a.contains('=') {
            i += 1;
            continue;
        }

        // First plain positional arg is the wrapped program.
        if !a.starts_with('-') {
            return Ok(i);
        }

        // Unrecognised flag — bail out fail-closed.
        return Err(ResolveError::CannotResolveWrapper {
            wrapper: "env".into(),
        });
    }
    Err(ResolveError::MissingProgram {
        wrapper: "env".into(),
    })
}

/// `sudo` and `doas`. sudo has many short options taking values; treat
/// any unrecognised short option as standalone.
fn find_sudo_program(args: &[String], wrapper: &str) -> Result<usize, ResolveError> {
    // Short options that consume the next argv element. Conservative:
    // covers everything that's reasonably common; unknown options
    // without `=` fall through to the unrecognised-flag arm below.
    const SUDO_SHORT_WITH_VALUE: &[char] =
        &['u', 'g', 'p', 'r', 't', 'T', 'U', 'C', 'D', 'h', 'R', 'a'];
    const SUDO_LONG_WITH_VALUE: &[&str] = &[
        "user",
        "group",
        "prompt",
        "role",
        "type",
        "host",
        "command-timeout",
        "askpass",
        "close-from",
        "chroot",
        "cwd",
    ];

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];

        if a == "--" {
            i += 1;
            return Ok(i);
        }

        if let Some(name) = a.strip_prefix("--") {
            if name.contains('=') {
                // --opt=value — single arg.
                i += 1;
                continue;
            }
            if SUDO_LONG_WITH_VALUE.contains(&name) {
                i += 2;
                continue;
            }
            // Standalone long option.
            i += 1;
            continue;
        }

        // Single dash (not `--`) — short option or cluster.
        if let Some(rest) = a.strip_prefix('-')
            && !rest.is_empty()
        {
            // Concatenated value form: `-uvalue` (length > 2, first
            // char is value-taking). Single arg, no follow-up consumed.
            let first_char = rest.chars().next().unwrap();
            if SUDO_SHORT_WITH_VALUE.contains(&first_char) && rest.len() > 1 {
                i += 1;
                continue;
            }

            // Bare `-u` (single value-taking char). Consume next arg
            // as the value.
            if SUDO_SHORT_WITH_VALUE.contains(&first_char) {
                i += 2;
                continue;
            }

            // Cluster of short flags like `-iu`. Walk it to find
            // value-taking chars: at most one is allowed, and it must
            // be the LAST char of the cluster (so the next argv
            // element is the value). Anything else is ambiguous and
            // fails closed — sudo's option grammar makes
            // mid-cluster value-takers unparseable here without
            // making assumptions about what the user meant.
            let value_taking_at: Option<usize> = {
                let mut found: Option<usize> = None;
                for (idx, c) in rest.chars().enumerate() {
                    if SUDO_SHORT_WITH_VALUE.contains(&c) {
                        if found.is_some() {
                            // Two value-taking chars in one cluster.
                            return Err(ResolveError::CannotResolveWrapper {
                                wrapper: wrapper.to_string(),
                            });
                        }
                        found = Some(idx);
                    }
                }
                found
            };

            match value_taking_at {
                None => {
                    // Pure standalone cluster (e.g. `-iE`). Single arg.
                    i += 1;
                }
                Some(pos) if pos == rest.chars().count() - 1 => {
                    // Value-taker is at the END of the cluster: the
                    // next argv element is the value. (e.g. `-iu user`)
                    i += 2;
                }
                Some(_) => {
                    // Value-taker is in the MIDDLE of the cluster.
                    // Can't tell where the value lives without parsing
                    // sudo's specific concatenation rules — fail closed.
                    return Err(ResolveError::CannotResolveWrapper {
                        wrapper: wrapper.to_string(),
                    });
                }
            }
            continue;
        }

        // First non-flag positional arg is the wrapped program.
        return Ok(i);
    }
    Err(ResolveError::MissingProgram {
        wrapper: wrapper.to_string(),
    })
}

/// `timeout [OPTIONS] DURATION PROGRAM [args]` — the first non-option
/// positional argument is the duration; the second is the program.
fn find_timeout_program(args: &[String]) -> Result<usize, ResolveError> {
    let mut positional_seen = 0;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            i += 1;
            // Now first positional after `--` is duration, second is program.
            while i < args.len() && positional_seen < 1 {
                i += 1;
                positional_seen += 1;
            }
            if i >= args.len() {
                return Err(ResolveError::MissingProgram {
                    wrapper: "timeout".into(),
                });
            }
            return Ok(i);
        }

        if a == "-s" || a == "--signal" || a == "-k" || a == "--kill-after" {
            i += 2;
            continue;
        }
        if a.starts_with("--") && a.contains('=') {
            i += 1;
            continue;
        }
        if a.starts_with('-') && a.len() > 1 {
            // Standalone short or long option — `--preserve-status`, `--foreground`, etc.
            i += 1;
            continue;
        }

        // Positional. First is duration, second is program.
        if positional_seen == 0 {
            positional_seen = 1;
            i += 1;
            continue;
        }
        return Ok(i);
    }
    Err(ResolveError::MissingProgram {
        wrapper: "timeout".into(),
    })
}

/// `xargs [OPTIONS] [COMMAND]` — the first non-option positional argv
/// element is the command. Many xargs short options consume the next
/// argv element.
fn find_xargs_program(args: &[String]) -> Result<usize, ResolveError> {
    const XARGS_SHORT_WITH_VALUE: &[char] =
        &['a', 'd', 'E', 'I', 'i', 'L', 'l', 'n', 'P', 's', 'x'];
    const XARGS_LONG_WITH_VALUE: &[&str] = &[
        "arg-file",
        "delimiter",
        "eof",
        "replace",
        "max-lines",
        "max-args",
        "max-procs",
        "max-chars",
    ];
    find_option_then_program(args, "xargs", XARGS_SHORT_WITH_VALUE, XARGS_LONG_WITH_VALUE)
}

/// Generic resolver for wrappers whose options follow the
/// "short flag may consume next arg, long flag may take `=value` or
/// next arg, first non-option is the wrapped program" shape.
///
/// `short_with_value` is the set of short option chars (after a single
/// `-`) that consume the following argv element. `long_with_value` is
/// the set of long option names (after `--`) that consume the
/// following argv element.
///
/// **Unknown-option semantics.** Any flag not in `short_with_value` /
/// `long_with_value` is treated as a **standalone** flag (skipped
/// singly). This is the documented limitation in the module-level
/// "unknown-option caveat": if a real value-taking option is missing
/// from the table, the value can be misclassified as the wrapped
/// program. Defended in depth by [`classify_interpreter`]'s closed
/// allowlist of interpreter basenames and by [`ResolvedExecutable::wrapper_chain`]
/// audit logging. Tightening this to a closed per-wrapper allowlist
/// (unknown → `CannotResolveWrapper`) is tracked as follow-up work.
fn find_option_then_program(
    args: &[String],
    wrapper: &str,
    short_with_value: &[char],
    long_with_value: &[&str],
) -> Result<usize, ResolveError> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];

        if a == "--" {
            i += 1;
            return Ok(i);
        }

        if let Some(name) = a.strip_prefix("--") {
            if name.contains('=') {
                i += 1;
                continue;
            }
            if long_with_value.contains(&name) {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        if let Some(c) = a.strip_prefix('-').and_then(|s| s.chars().next()) {
            // Single dash with no chars (`-`) is unusual; treat as positional.
            if a == "-" {
                return Ok(i);
            }
            if short_with_value.contains(&c) {
                if a.len() > 2 {
                    i += 1;
                } else {
                    i += 2;
                }
                continue;
            }
            i += 1;
            continue;
        }

        return Ok(i);
    }
    Err(ResolveError::MissingProgram {
        wrapper: wrapper.to_string(),
    })
}

/// `taskset` has three forms:
///
/// - `taskset MASK COMMAND [args...]` — the canonical "set affinity
///   then exec" form. MASK is the first positional; COMMAND is the
///   second positional.
/// - `taskset -c CPULIST COMMAND [args...]` (or `--cpu-list`) — the
///   CPU set is specified via flag, so there is **no** MASK
///   positional; the first positional IS the COMMAND.
/// - `taskset -p [MASK] PID` (or `--pid`) — operate on an existing
///   PID, no command spawned. We return
///   [`ResolveError::CannotResolveWrapper`] for this case so the
///   caller fails closed; the daemon shouldn't reach this mode
///   through the action pipeline, and the resolver mustn't silently
///   return PID as a "program".
fn find_taskset_program(args: &[String]) -> Result<usize, ResolveError> {
    let mut i = 0;
    // Tracks whether `-c` / `--cpu-list` (or `--cpu-list=...`) was
    // seen. If so, the first positional is the COMMAND directly —
    // no MASK precedes it.
    let mut cpu_list_specified = false;

    while i < args.len() {
        let a = &args[i];

        if a == "--" {
            i += 1;
            break;
        }

        // -p / --pid: PID-mode, no command spawned. Fail closed.
        if a == "-p" || a == "--pid" {
            return Err(ResolveError::CannotResolveWrapper {
                wrapper: "taskset".into(),
            });
        }

        // -c CPULIST / --cpu-list LIST consumes the next argv element.
        if a == "-c" || a == "--cpu-list" {
            cpu_list_specified = true;
            i += 2;
            continue;
        }

        // --cpu-list=LIST single-arg form.
        if let Some(rest) = a.strip_prefix("--cpu-list=") {
            let _ = rest;
            cpu_list_specified = true;
            i += 1;
            continue;
        }

        if a.starts_with("--") && a.contains('=') {
            i += 1;
            continue;
        }
        if a.starts_with('-') && a.len() > 1 {
            // Standalone short / long option.
            i += 1;
            continue;
        }

        // First positional. If -c/--cpu-list was used, this IS the
        // program. Otherwise, this is the MASK; program is next.
        if cpu_list_specified {
            return Ok(i);
        }
        let cmd_idx = i + 1;
        if cmd_idx >= args.len() {
            return Err(ResolveError::MissingProgram {
                wrapper: "taskset".into(),
            });
        }
        return Ok(cmd_idx);
    }

    // After `--`: same disambiguation.
    if i >= args.len() {
        return Err(ResolveError::MissingProgram {
            wrapper: "taskset".into(),
        });
    }
    if cpu_list_specified {
        return Ok(i);
    }
    let cmd_idx = i + 1;
    if cmd_idx >= args.len() {
        return Err(ResolveError::MissingProgram {
            wrapper: "taskset".into(),
        });
    }
    Ok(cmd_idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn resolve(command: &str, args: &[&str]) -> Result<ResolvedExecutable, ResolveError> {
        resolve_effective_executable(Path::new(command), &argv(args), DEFAULT_MAX_WRAPPER_DEPTH)
    }

    // ─── classify_interpreter ─────────────────────────────────────────

    #[test]
    fn classify_recognises_python_with_version_suffix() {
        assert_eq!(
            classify_interpreter(Path::new("/usr/bin/python3")),
            InterpreterClassification::Interpreter(InterpreterFamily::Python)
        );
        assert_eq!(
            classify_interpreter(Path::new("/usr/local/bin/python3.12")),
            InterpreterClassification::Interpreter(InterpreterFamily::Python)
        );
    }

    #[test]
    fn classify_recognises_python_distribution_aliases() {
        // Two distinct paths:
        //
        // 1. Names where the suffix is pure digits / dots and reduces
        //    via `strip_version_suffix` to a stem already in the
        //    match table — these reach `Python` via the existing
        //    `"python"` / `"pythonw"` arms (no explicit alias arm
        //    needed). Examples: `python2`, `pythonw3`.
        //
        // 2. Names with non-digit suffix characters that strip can't
        //    reduce — these need explicit alias arms. Examples:
        //    `pythonw`, `python3m`, `python3dm`.
        //
        // Both paths must end at `Interpreter(Python)`; a regression
        // in either silently downgrades these binaries to `NotInterpreter`
        // and over-permits the gate.
        for bin in &[
            // path-2 (explicit alias arms)
            "/usr/bin/pythonw",   // windowed-mode (macOS / Windows)
            "/usr/bin/python3m",  // pymalloc build
            "/usr/bin/python3dm", // debug + pymalloc
            // path-1 (resolved via strip_version_suffix)
            "/usr/bin/python2",  // strips to "python"
            "/usr/bin/pythonw3", // strips to "pythonw"
        ] {
            assert_eq!(
                classify_interpreter(Path::new(bin)),
                InterpreterClassification::Interpreter(InterpreterFamily::Python),
                "expected Python for {}",
                bin
            );
        }
    }

    #[test]
    fn classify_recognises_bash_zsh_sh_separately() {
        assert_eq!(
            classify_interpreter(Path::new("/bin/bash")),
            InterpreterClassification::Interpreter(InterpreterFamily::Bash)
        );
        assert_eq!(
            classify_interpreter(Path::new("/bin/zsh")),
            InterpreterClassification::Interpreter(InterpreterFamily::Zsh)
        );
        assert_eq!(
            classify_interpreter(Path::new("/bin/sh")),
            InterpreterClassification::Interpreter(InterpreterFamily::Sh)
        );
        assert_eq!(
            classify_interpreter(Path::new("/usr/bin/dash")),
            InterpreterClassification::Interpreter(InterpreterFamily::Sh)
        );
    }

    #[test]
    fn classify_rejects_non_interpreter() {
        for path in &["/usr/bin/say", "/bin/ls", "/usr/bin/curl", "/usr/bin/open"] {
            assert_eq!(
                classify_interpreter(Path::new(path)),
                InterpreterClassification::NotInterpreter,
                "expected NotInterpreter for {}",
                path
            );
        }
    }

    #[test]
    fn classify_non_utf8_basename_fails_closed_as_other() {
        // A path whose basename can't be decoded as UTF-8 must
        // classify as `Interpreter(Other)`, not `NotInterpreter`.
        // Returning `NotInterpreter` would only require `ShellExec`
        // for an undecodable path, which is over-permissive — a
        // path we can't even read might encode a real interpreter
        // basename under some exotic locale.
        //
        // Symmetric with `resolve_effective_executable`'s
        // `MalformedPath` error for the same condition.
        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let invalid = OsStr::from_bytes(b"/usr/bin/\xff\xfe");
            let path = Path::new(invalid);
            assert_eq!(
                classify_interpreter(path),
                InterpreterClassification::Interpreter(InterpreterFamily::Other),
            );
        }
    }

    #[test]
    fn classify_recognises_node_runtimes() {
        // `nodejs` is the Debian / older-Ubuntu name for `node`.
        // Without the alias this would silently classify as
        // NotInterpreter and bypass the interpreter capability check
        // for every Debian Node script.
        for bin in &[
            "/usr/bin/node",
            "/usr/bin/nodejs",
            "/usr/bin/deno",
            "/usr/local/bin/bun",
        ] {
            let c = classify_interpreter(Path::new(bin));
            assert_eq!(
                c,
                InterpreterClassification::Interpreter(InterpreterFamily::Node),
                "expected Node for {}",
                bin
            );
        }
    }

    #[test]
    fn classify_osascript_as_other() {
        // osascript executes AppleScript via -e — exactly the bypass class
        // InterpreterExec exists to gate. No native AppleScript family
        // exists in the InterpreterFamily enum yet, so file under Other.
        assert_eq!(
            classify_interpreter(Path::new("/usr/bin/osascript")),
            InterpreterClassification::Interpreter(InterpreterFamily::Other)
        );
    }

    #[test]
    fn classify_recognises_awk_and_sed_as_one_family() {
        for bin in &["/usr/bin/awk", "/usr/bin/sed", "/usr/bin/gawk"] {
            assert_eq!(
                classify_interpreter(Path::new(bin)),
                InterpreterClassification::Interpreter(InterpreterFamily::AwkOrSed),
                "expected AwkOrSed for {}",
                bin
            );
        }
    }

    // ─── resolve_effective_executable: trivial cases ─────────────────

    #[test]
    fn non_wrapper_resolves_to_itself() {
        let r = resolve("/usr/bin/python3", &["-c", "print(1)"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
        assert!(r.wrapper_chain.is_empty());
    }

    #[test]
    fn say_resolves_to_itself_with_no_chain() {
        let r = resolve("/usr/bin/say", &["hello"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/say"));
        assert!(r.wrapper_chain.is_empty());
    }

    // ─── env wrapper ─────────────────────────────────────────────────

    #[test]
    fn env_python_resolves_to_python() {
        let r = resolve("/usr/bin/env", &["python3", "-c", "1"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("python3"));
        assert_eq!(r.wrapper_chain, vec![PathBuf::from("/usr/bin/env")]);
    }

    #[test]
    fn env_with_ignore_environment_resolves_to_program() {
        let r = resolve("/usr/bin/env", &["-i", "python3", "-c", "1"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("python3"));
    }

    #[test]
    fn env_with_unset_arg_resolves_correctly() {
        let r = resolve("/usr/bin/env", &["-u", "PYTHONPATH", "python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("python3"));
    }

    #[test]
    fn env_with_concatenated_unset_form_resolves() {
        // `-uVAR` (no space) — symmetric with the sudo `-uvalue`
        // concatenated form. A real bypass tries this when the
        // separated form is detected and rejected.
        let r = resolve("/usr/bin/env", &["-uPYTHONPATH", "python3", "-c", "1"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("python3"));
    }

    #[test]
    fn env_with_kv_pairs_resolves_past_them() {
        let r = resolve(
            "/usr/bin/env",
            &["FOO=bar", "BAZ=qux", "python3", "-c", "1"],
        )
        .unwrap();
        assert_eq!(r.effective_path, PathBuf::from("python3"));
    }

    #[test]
    fn env_double_dash_separator() {
        let r = resolve("/usr/bin/env", &["--", "python3", "-c", "1"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("python3"));
    }

    #[test]
    fn env_block_signal_with_equals_resolves() {
        // `--block-signal=SIG` is unambiguous: the SIG is part of the
        // arg via `=`. The PROG is the next argv element.
        let r = resolve(
            "/usr/bin/env",
            &["--block-signal=INT", "python3", "-c", "1"],
        )
        .unwrap();
        assert_eq!(r.effective_path, PathBuf::from("python3"));
    }

    #[test]
    fn env_bare_block_signal_fails_closed() {
        // Bare `--block-signal` (no `=`) followed by something that
        // could be either a SIG or a PROG is ambiguous. Resolver
        // fails closed via the unrecognised-flag arm so the caller
        // treats as InterpreterFamily::Other. Same for the sibling
        // `--default-signal` / `--ignore-signal` options.
        let err = resolve("/usr/bin/env", &["--block-signal", "INT", "python3"]).unwrap_err();
        assert!(matches!(
            err,
            ResolveError::CannotResolveWrapper { ref wrapper } if wrapper == "env"
        ));
    }

    #[test]
    fn env_bare_default_signal_fails_closed() {
        let err = resolve("/usr/bin/env", &["--default-signal", "TERM", "python3"]).unwrap_err();
        assert!(matches!(
            err,
            ResolveError::CannotResolveWrapper { ref wrapper } if wrapper == "env"
        ));
    }

    #[test]
    fn env_with_no_program_is_missing_program_error() {
        let err = resolve("/usr/bin/env", &["FOO=bar"]).unwrap_err();
        assert!(matches!(err, ResolveError::MissingProgram { ref wrapper } if wrapper == "env"));
    }

    // ─── sudo / doas ─────────────────────────────────────────────────

    #[test]
    fn sudo_python_resolves_to_python() {
        let r = resolve("/usr/bin/sudo", &["/usr/bin/python3", "-c", "1"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn sudo_with_user_option_resolves_correctly() {
        let r = resolve("/usr/bin/sudo", &["-u", "nobody", "/usr/bin/python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn sudo_with_concatenated_short_option_resolves() {
        // `-unobody` is `-u nobody` concatenated.
        let r = resolve("/usr/bin/sudo", &["-unobody", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn sudo_with_long_option_equals_value_resolves() {
        let r = resolve("/usr/bin/sudo", &["--user=nobody", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn sudo_with_long_option_separate_value_resolves() {
        let r = resolve("/usr/bin/sudo", &["--user", "nobody", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn sudo_with_combined_short_cluster_value_at_end_resolves() {
        // `sudo -iu nobody PROG` — `-i` standalone + `-u nobody`.
        // The value-taking `u` is at the end of the cluster, so the
        // next argv element is the value. Without cluster handling
        // this returned `nobody` as the wrapped program — bypass.
        let r = resolve(
            "/usr/bin/sudo",
            &["-iu", "nobody", "/usr/bin/python3", "-c", "1"],
        )
        .unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn sudo_with_combined_short_cluster_all_standalone_resolves() {
        // `-iE PROG` — both `-i` and `-E` are standalone (no values).
        // Cluster collapses to a single skip.
        let r = resolve("/usr/bin/sudo", &["-iE", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn sudo_with_value_taker_mid_cluster_fails_closed() {
        // `-AuB` — `A` standalone, value-taker `u` is in the
        // **middle** (idx 1 of 3), `B` standalone. Cannot tell where
        // the value lives — `B` could be part of u's concat value, or
        // standalone, or u could take the next argv. Fail closed so
        // caller treats as InterpreterFamily::Other.
        //
        // Distinct from `-uA` (value-taker at position 0 — concat
        // form, `A` is the value) and `-Au` (value-taker at end of
        // cluster — next argv is the value), both of which resolve
        // unambiguously.
        let err = resolve("/usr/bin/sudo", &["-AuB", "value", "/usr/bin/bash"]).unwrap_err();
        assert!(matches!(
            err,
            ResolveError::CannotResolveWrapper { ref wrapper } if wrapper == "sudo"
        ));
    }

    #[test]
    fn sudo_with_value_taker_at_start_of_cluster_concats() {
        // `-uA` — value-taker `u` at position 0 with more chars
        // following: legitimate concat-value form, `A` IS the value.
        // Resolver returns the next arg as the program (sudo runs as
        // user A on PROG).
        let r = resolve("/usr/bin/sudo", &["-uA", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn doas_resolves_like_sudo() {
        let r = resolve("/usr/bin/doas", &["-u", "nobody", "/usr/bin/python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    // ─── nice / ionice ───────────────────────────────────────────────

    #[test]
    fn nice_with_n_option_resolves() {
        let r = resolve("/usr/bin/nice", &["-n", "10", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn ionice_with_c_option_resolves() {
        let r = resolve("/usr/bin/ionice", &["-c", "3", "/usr/bin/python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    // ─── timeout ─────────────────────────────────────────────────────

    #[test]
    fn timeout_with_duration_resolves_to_program() {
        let r = resolve("/usr/bin/timeout", &["5s", "/usr/bin/python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn timeout_with_signal_option_and_duration_resolves() {
        let r = resolve(
            "/usr/bin/timeout",
            &["--signal", "TERM", "5s", "/usr/bin/python3"],
        )
        .unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn timeout_with_kill_after_resolves() {
        let r = resolve("/usr/bin/timeout", &["-k", "2s", "5s", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    // ─── xargs ───────────────────────────────────────────────────────

    #[test]
    fn xargs_with_command_resolves() {
        let r = resolve("/usr/bin/xargs", &["/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/bin/bash"));
    }

    #[test]
    fn xargs_with_replacement_option_resolves() {
        let r = resolve("/usr/bin/xargs", &["-I", "{}", "/usr/bin/python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    // ─── nohup, setsid, time ────────────────────────────────────────

    #[test]
    fn nohup_program_resolves() {
        let r = resolve("/usr/bin/nohup", &["/usr/bin/python3", "-c", "1"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn nohup_with_double_dash_separator_resolves() {
        // `nohup -- PROG` is a legitimate end-of-options invocation;
        // GNU/BSD nohup consumes the `--` and execs PROG. Earlier
        // resolver versions (find_simple_program) returned `--` as the
        // effective binary — bypass once wired in.
        let r = resolve("/usr/bin/nohup", &["--", "/usr/bin/python3", "-c", "1"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn setsid_program_resolves() {
        let r = resolve("/usr/bin/setsid", &["/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn time_program_resolves() {
        let r = resolve("/usr/bin/time", &["/usr/bin/python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn time_with_portability_flag_resolves() {
        // `/usr/bin/time -p PROG` — `-p` is the standalone POSIX
        // format flag, NOT a PID flag. Without option parsing the
        // resolver returned `-p` as the wrapped program — bypass.
        let r = resolve("/usr/bin/time", &["-p", "/usr/bin/python3", "-c", "1"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn time_with_format_value_resolves() {
        // `-f FORMAT` consumes the format string.
        let r = resolve("/usr/bin/time", &["-f", "%e %M", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn time_with_output_file_resolves() {
        // `--output FILE` consumes the file path.
        let r = resolve(
            "/usr/bin/time",
            &["--output", "/tmp/timing.log", "/usr/bin/python3"],
        )
        .unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    // ─── unshare, stdbuf ────────────────────────────────────────────

    #[test]
    fn unshare_with_user_namespace_resolves() {
        let r = resolve("/usr/bin/unshare", &["-r", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn stdbuf_with_output_buffering_resolves() {
        let r = resolve("/usr/bin/stdbuf", &["-oL", "/usr/bin/python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    // ─── chpst, runuser, schedtool, taskset ─────────────────────────
    //
    // These wrappers were dispatch-table residents without test coverage
    // until #1058's review caught the gap. Each test exercises a
    // value-taking option (the security-critical option-parsing path —
    // wrong argv-counting here would shift the wrapped program offset
    // and let a value masquerade as the program).

    #[test]
    fn chpst_with_user_option_resolves() {
        // `chpst -u nobody PROG` — `-u` consumes the user name.
        let r = resolve("/usr/bin/chpst", &["-u", "nobody", "/usr/bin/python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn chpst_with_chroot_option_resolves() {
        // `chpst -/ /var/jail PROG` — `-/` consumes the directory.
        let r = resolve("/usr/bin/chpst", &["-/", "/var/jail", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn runuser_with_user_option_resolves() {
        // `runuser -u nobody -- /usr/bin/python3`.
        let r = resolve(
            "/usr/bin/runuser",
            &["-u", "nobody", "--", "/usr/bin/python3"],
        )
        .unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn runuser_with_command_option_resolves() {
        // `-c CMD` — exotic but real; the command after -c is the
        // interpreter argument (passed to the runuser-spawned shell),
        // and the wrapped program in this resolver's view is the
        // remaining positional. We don't try to be smart about
        // whether `-c "python -e ..."` is itself an interpreter
        // invocation; that's downstream classifier territory.
        let r = resolve(
            "/usr/bin/runuser",
            &["-u", "nobody", "-s", "/bin/bash", "-l", "/usr/bin/python3"],
        )
        .unwrap();
        // -u consumed "nobody"; -s consumed "/bin/bash"; -l is standalone.
        // First positional after options: /usr/bin/python3.
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn schedtool_with_priority_then_program_resolves() {
        // `schedtool -p 99 PROG` — `-p` consumes the priority value.
        let r = resolve("/usr/bin/schedtool", &["-p", "99", "/usr/bin/python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn schedtool_with_standalone_debug_then_value_option_resolves() {
        // `-D` is standalone debug; `-n NICE` consumes value. Tests
        // that a standalone short option does NOT consume the next
        // arg as a value when followed by a value-taking option.
        let r = resolve("/usr/bin/schedtool", &["-D", "-n", "10", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn taskset_with_cpu_list_then_program_resolves() {
        // `taskset -c 0,1 PROG` — `-c` consumes the cpu-list value.
        let r = resolve("/usr/bin/taskset", &["-c", "0,1", "/usr/bin/python3"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn taskset_with_long_cpu_list_then_program_resolves() {
        // `--cpu-list 0,1` long form.
        let r = resolve("/usr/bin/taskset", &["--cpu-list", "0,1", "/usr/bin/bash"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/bash"));
    }

    #[test]
    fn taskset_mask_then_program_resolves() {
        // `taskset MASK COMMAND [args]` — the canonical form. MASK
        // (0x1) is the first positional; COMMAND (python3) is the
        // second. Without taskset-specific handling this returned
        // `0x1` as the wrapped program — bypass.
        let r = resolve("/usr/bin/taskset", &["0x1", "/usr/bin/python3", "-c", "1"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
    }

    #[test]
    fn taskset_pid_mode_fails_closed() {
        // `taskset -p MASK PID` operates on an existing PID and
        // spawns no command. The resolver must not silently return
        // PID as a "program". Caller must fail closed.
        let err = resolve("/usr/bin/taskset", &["-p", "0x1", "1234"]).unwrap_err();
        assert!(matches!(
            err,
            ResolveError::CannotResolveWrapper { ref wrapper } if wrapper == "taskset"
        ));
    }

    // ─── nested wrappers ─────────────────────────────────────────────

    #[test]
    fn env_nohup_python_resolves_through_chain() {
        // /usr/bin/env nohup python3 -c "..."
        let r = resolve("/usr/bin/env", &["nohup", "python3", "-c", "1"]).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("python3"));
        assert_eq!(r.wrapper_chain.len(), 2);
        assert_eq!(r.wrapper_chain[0], PathBuf::from("/usr/bin/env"));
        assert_eq!(r.wrapper_chain[1], PathBuf::from("nohup"));
    }

    #[test]
    fn sudo_env_python_resolves_through_chain() {
        let r = resolve(
            "/usr/bin/sudo",
            &["-u", "nobody", "/usr/bin/env", "python3", "-c", "1"],
        )
        .unwrap();
        assert_eq!(r.effective_path, PathBuf::from("python3"));
        assert_eq!(r.wrapper_chain.len(), 2);
    }

    // ─── depth limit ─────────────────────────────────────────────────

    #[test]
    fn depth_exceeded_returns_error() {
        // Construct a chain of nested env wrappers exceeding max_depth=3.
        let args = argv(&["env", "env", "env", "env", "env", "python3"]);
        let err = resolve_effective_executable(Path::new("/usr/bin/env"), &args, 3).unwrap_err();
        assert!(matches!(err, ResolveError::DepthExceeded { limit: 3 }));
    }

    #[test]
    fn depth_exact_limit_resolves() {
        // Depth-4 chain (env env env python3) at max_depth=4 succeeds.
        let args = argv(&["env", "env", "python3"]);
        let r = resolve_effective_executable(Path::new("/usr/bin/env"), &args, 4).unwrap();
        assert_eq!(r.effective_path, PathBuf::from("python3"));
    }

    #[test]
    fn max_depth_zero_resolves_non_wrapper_immediately() {
        // `max_depth = 0` with a non-wrapper command must NOT trip
        // `DepthExceeded` — it must return the command as the
        // effective binary because no unwinding is needed. Earlier
        // resolver versions returned `Err(DepthExceeded { limit: 0 })`
        // for any input when max_depth was 0, including non-wrappers.
        let r = resolve_effective_executable(Path::new("/usr/bin/python3"), &argv(&["-c", "1"]), 0)
            .unwrap();
        assert_eq!(r.effective_path, PathBuf::from("/usr/bin/python3"));
        assert!(r.wrapper_chain.is_empty());
    }

    #[test]
    fn max_depth_zero_with_wrapper_fails_closed() {
        // `max_depth = 0` AND argv[0] is a wrapper: caller asked for
        // zero unwinding, so we cannot resolve. Fail closed via
        // DepthExceeded.
        let err = resolve_effective_executable(
            Path::new("/usr/bin/env"),
            &argv(&["python3", "-c", "1"]),
            0,
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::DepthExceeded { limit: 0 }));
    }

    // ─── error paths ─────────────────────────────────────────────────

    #[test]
    fn env_with_only_kv_pair_no_program_errors() {
        let err = resolve("/usr/bin/env", &["KEY=VAL"]).unwrap_err();
        assert!(matches!(err, ResolveError::MissingProgram { .. }));
    }

    #[test]
    fn malformed_path_with_no_basename_errors() {
        // `/` has no filename component.
        let err = resolve_effective_executable(Path::new("/"), &[], DEFAULT_MAX_WRAPPER_DEPTH)
            .unwrap_err();
        assert!(matches!(err, ResolveError::MalformedPath));
    }

    // ─── compile-time invariants ─────────────────────────────────────

    #[test]
    fn resolved_executable_is_send_sync() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<ResolvedExecutable>();
        assert_send_sync::<ResolveError>();
        assert_send_sync::<InterpreterClassification>();
    }

    #[test]
    fn every_wrapper_has_a_handler() {
        // For each declared WRAPPER, find_wrapped_program(WRAPPER, &[]) must
        // not return CannotResolveWrapper — proving every wrapper has a
        // handler in the dispatch match. (Returning MissingProgram is
        // expected and fine; it just means the handler ran.)
        for w in WRAPPERS {
            if let Err(ResolveError::CannotResolveWrapper { wrapper }) =
                find_wrapped_program(w, &[])
            {
                panic!(
                    "WRAPPER `{}` is registered but find_wrapped_program has no handler — \
                     it would silently fail closed for every invocation. Add a handler arm.",
                    wrapper
                );
            }
        }
    }

    // ─── strip_version_suffix unit tests ─────────────────────────────

    #[test]
    fn strip_version_suffix_handles_common_cases() {
        assert_eq!(strip_version_suffix("python3"), "python");
        assert_eq!(strip_version_suffix("python3.12"), "python");
        assert_eq!(strip_version_suffix("bash"), "bash");
        assert_eq!(strip_version_suffix("node20.10.0"), "node");
        assert_eq!(strip_version_suffix("ruby"), "ruby");
        // All-numeric should be returned as-is rather than reduced to empty.
        assert_eq!(strip_version_suffix("3"), "3");
    }
}
