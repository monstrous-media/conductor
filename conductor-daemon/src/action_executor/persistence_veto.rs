// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-027 §D10a — persistence write veto (pre-flight argv check).
//!
//! Before the daemon spawns a `Shell` action it inspects the resolved argv
//! for an attempt to *write* one of its own persistent-state directories
//! (`~/.conductor/`, the macOS Application-Support dir, or the XDG data dir).
//! A match is **vetoed**: the process is never spawned, so a vetoed action
//! can never reach `execve` (acceptance: "Vetoed actions never reach
//! `execve`").
//!
//! # Threat model & scope (Council R1)
//!
//! Council reviewed this as a **best-effort deterrent, NOT a hard security
//! boundary** — the hard boundary is D10b's OS-level sandbox
//! (`sandbox-exec` / landlock). Two properties shape the implementation:
//!
//! 1. **Shell actions run *without* an interpreter.** `execute_shell` spawns
//!    `Command::new(program).args(args)` directly (no implicit `sh -c`), and
//!    the config-validation layer already rejects raw `>`/`|`/`&&` in the
//!    command string. So the live write vectors are:
//!      * a **direct write program** (`tee`, `cp`, `mv`, `install`, `ln`,
//!        `rm`, `unlink`, `rmdir`, `shred`, `dd of=`, `truncate`, `sed -i`,
//!        `awk … > file`) whose target argument lands inside a protected
//!        directory; and
//!      * an **explicit interpreter** (`sh -c "…"`, `bash -c "…"`, …) that
//!        re-introduces redirects inside a single argv token, which the raw
//!        command-string validation does not see.
//!
//! 2. **Reads are not writes.** Per Council, detection MUST differentiate
//!    `<` (read — *not* vetoed, e.g. `cat ~/.conductor/config > /tmp/out`)
//!    from `>` / `>>` (write — vetoed). This module lexes interpreter
//!    scripts with a shell-aware tokenizer rather than a naive regex so the
//!    read/write distinction (and quoting) is respected.
//!
//! # Environment-injection (D7 inheritance)
//!
//! Council flagged `LD_PRELOAD` / `DYLD_INSERT_LIBRARIES` / `PYTHONPATH`
//! style hijacks as a P0 class. D10a inherits the mitigation *by
//! construction*: the veto runs inside `execute_shell`, which already spawns
//! with [`sanitised_shell_env`](super::shell::sanitised_shell_env) (ADR-027
//! §D7) — the child never sees the daemon's unsanitised environment, so the
//! veto does not need to re-strip it.
//!
//! # Known gaps (deferred to D10b, the hard boundary)
//!
//! * **Process substitution** (`> >(cmd)`) and **here-doc bodies**
//!   (`<<EOF … EOF`) are not fully modelled — only a redirect whose literal
//!   target resolves into a protected path is caught.
//! * **Relative paths** cannot be resolved (the daemon's cwd is not known to
//!   this pure check) and are therefore not vetoed.
//!
//! These are acceptable for a v1 deterrent; the OS sandbox in D10b is the
//! authoritative control.

use std::path::{Component, Path, PathBuf};

/// A vetoed write attempt: which heuristic fired and the protected path it
/// resolved to. Surfaced in the `DispatchError` message and the
/// `ShellVetoedByPersistenceCheck` audit event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VetoMatch {
    /// Human-readable pattern label, e.g. `tee` or `redirect >`.
    pub matched_pattern: String,
    /// The protected path (as resolved) the action attempted to write.
    pub protected_path: String,
}

/// The daemon's protected persistent-state roots. Absolute, best-effort
/// canonicalised so a symlinked alias of a protected dir is still matched.
#[derive(Debug, Clone)]
pub(crate) struct ProtectedPaths {
    roots: Vec<PathBuf>,
}

impl ProtectedPaths {
    /// Build the protected-root set from the live environment.
    ///
    /// * `~/.conductor/`
    /// * `~/Library/Application Support/conductor/` (macOS)
    /// * `$XDG_DATA_HOME/conductor/` (or `~/.local/share/conductor/`)
    pub(crate) fn from_env() -> Self {
        let home = home_dir();
        let xdg = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        Self::build(home.as_deref(), xdg.as_deref())
    }

    /// Deterministic constructor for tests: explicit `home` and optional
    /// `XDG_DATA_HOME`. No environment is read.
    #[cfg(test)]
    pub(crate) fn for_test(home: &Path, xdg_data_home: Option<&Path>) -> Self {
        Self::build(Some(home), xdg_data_home)
    }

    fn build(home: Option<&Path>, xdg: Option<&Path>) -> Self {
        let mut roots = Vec::new();
        if let Some(home) = home {
            roots.push(home.join(".conductor"));
            roots.push(
                home.join("Library")
                    .join("Application Support")
                    .join("conductor"),
            );
            match xdg {
                Some(x) => roots.push(x.join("conductor")),
                None => roots.push(home.join(".local").join("share").join("conductor")),
            }
        } else if let Some(x) = xdg {
            // No HOME but XDG set — still protect the data dir.
            roots.push(x.join("conductor"));
        }
        let roots = roots.iter().map(|r| best_effort_canonical(r)).collect();
        Self { roots }
    }

    /// If `candidate` is, or is nested under, a protected root, return that
    /// root (canonicalised). Both sides are best-effort canonicalised so a
    /// symlinked ancestor (`~/alias -> ~/.conductor`) still matches.
    fn matched(&self, candidate: &Path) -> Option<&Path> {
        let canon = best_effort_canonical(candidate);
        self.roots
            .iter()
            .find(|root| canon == **root || canon.starts_with(root))
            .map(|p| p.as_path())
    }
}

/// Interpreters whose `-c <script>` argument is lexed for redirects and
/// nested write-commands.
const SHELL_INTERPRETERS: &[&str] = &[
    "sh", "bash", "dash", "zsh", "ksh", "ksh93", "mksh", "ash", "fish",
];

/// `awk`-family programs that write via in-language `print > "file"`.
const AWK_FAMILY: &[&str] = &["awk", "gawk", "mawk", "nawk"];

/// Production entry point: build the protected-root set and `~`-expansion
/// home from the live environment, then run the veto. Used by
/// `execute_shell`; tests use [`persistence_write_veto`] with explicit paths.
pub(crate) fn persistence_write_veto_env(program: &str, args: &[String]) -> Option<VetoMatch> {
    let protected = ProtectedPaths::from_env();
    let home = home_dir();
    persistence_write_veto(program, args, &protected, home.as_deref())
}

/// Pre-flight veto check. Returns `Some(VetoMatch)` when the argv attempts to
/// write a protected path; `None` otherwise.
///
/// `program` is argv[0]; `args` is argv[1..]; `home` is used to expand a
/// leading `~`.
pub(crate) fn persistence_write_veto(
    program: &str,
    args: &[String],
    protected: &ProtectedPaths,
    home: Option<&Path>,
) -> Option<VetoMatch> {
    let base = program_basename(program);

    // (a) Explicit interpreter: lex the WHOLE arg tail as a script. Joining
    //     all args (rather than only the operand after a separate `-c`) is the
    //     robust form — it catches glued/embedded flags (`-cSCRIPT`, `-lc
    //     SCRIPT`), scripts split across args, and positional `$0 $1` that
    //     happen to be write-commands. `sh script.sh` joins to a bare path
    //     (no redirect / write-command token) so it is correctly not vetoed.
    if SHELL_INTERPRETERS.contains(&base) {
        let joined = args.join(" ");
        return script_write_veto(&joined, protected, home);
    }

    // (b) awk-family: redirects live inside the program-text arguments.
    if AWK_FAMILY.contains(&base) {
        for arg in args {
            if let Some(m) = redirect_targets_in(arg, protected, home) {
                return Some(m);
            }
        }
        return None;
    }

    // (c) Direct write command (tee/cp/mv/rm/sed -i/dd/...).
    if let Some(m) = command_write_veto(base, args, protected, home) {
        return Some(m);
    }

    // (d) Defensive: argv may already be tokenised with a stray redirect
    //     operator (`>`/`>>`) and a protected target, even though the
    //     validation layer normally rejects raw redirects in the command
    //     string. Treat argv as pre-split words.
    let words: Vec<String> = std::iter::once(program.to_string())
        .chain(args.iter().cloned())
        .collect();
    redirect_veto_over_words(&words, protected, home)
}

// ───────────────────────── interpreter scripts ─────────────────────────

/// Lex an interpreter script and veto if any write-redirect target or nested
/// write-command target lands in a protected path.
fn script_write_veto(
    script: &str,
    protected: &ProtectedPaths,
    home: Option<&Path>,
) -> Option<VetoMatch> {
    // Command substitutions (`$(…)`, `` `…` ``) run their own commands; a
    // write hidden inside one (`echo $(tee ~/.conductor/x)`) would otherwise
    // be invisible to the top-level command/redirect walk. Recurse into each
    // substitution body first (bodies are strictly shorter, so this
    // terminates).
    for body in extract_substitutions(script) {
        if let Some(m) = script_write_veto(&body, protected, home) {
            return Some(m);
        }
    }

    let toks = tokenize(script);

    // Walk tokens: handle redirects inline, accumulate simple-command words.
    let mut cmd_words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Redir { write: true } => {
                // Next word is the redirect target.
                if let Some(Tok::Word(target)) = toks.get(i + 1) {
                    if let Some(m) = check_target(target, "redirect >", protected, home) {
                        return Some(m);
                    }
                    i += 2;
                    continue;
                }
            }
            Tok::Redir { write: false } => {
                // Read redirect (`<`, `<<`, `<<<`): skip its source operand —
                // reading a protected file is explicitly allowed.
                i += 2;
                continue;
            }
            Tok::Sep => {
                if let Some(m) = command_write_veto_words(&cmd_words, protected, home) {
                    return Some(m);
                }
                cmd_words.clear();
            }
            Tok::Word(w) => cmd_words.push(w.clone()),
        }
        i += 1;
    }
    command_write_veto_words(&cmd_words, protected, home)
}

/// Extract the bodies of command substitutions — `$(…)` (with nesting) and
/// `` `…` `` — from a script. Quoting is deliberately not tracked: a
/// literal substitution inside single quotes is extracted too, which only
/// over-vetoes (safe for a deterrent). Returns the inner text of each.
fn extract_substitutions(script: &str) -> Vec<String> {
    let chars: Vec<char> = script.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'(') {
            // Find the matching ')', honouring nested `$( … )`.
            let mut depth = 1;
            let mut j = i + 2;
            let start = j;
            while j < chars.len() && depth > 0 {
                match chars[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    break;
                }
                j += 1;
            }
            if depth == 0 {
                out.push(chars[start..j].iter().collect());
            }
            i = j + 1;
        } else if chars[i] == '`' {
            // Backtick substitution: body runs to the next backtick.
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != '`' {
                j += 1;
            }
            if j < chars.len() {
                out.push(chars[start..j].iter().collect());
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Scan a single string (e.g. an `awk` program) for a write-redirect to a
/// protected path.
fn redirect_targets_in(
    text: &str,
    protected: &ProtectedPaths,
    home: Option<&Path>,
) -> Option<VetoMatch> {
    let toks = tokenize(text);
    let mut i = 0;
    while i < toks.len() {
        if let Tok::Redir { write: true } = toks[i]
            && let Some(Tok::Word(target)) = toks.get(i + 1)
            && let Some(m) = check_target(target, "redirect >", protected, home)
        {
            return Some(m);
        }
        i += 1;
    }
    None
}

// ───────────────────────── write commands ─────────────────────────

/// Veto for a *direct* (non-interpreter) write command `base args…`.
fn command_write_veto(
    base: &str,
    args: &[String],
    protected: &ProtectedPaths,
    home: Option<&Path>,
) -> Option<VetoMatch> {
    let mut words = Vec::with_capacity(args.len() + 1);
    words.push(base.to_string());
    words.extend(args.iter().cloned());
    command_write_veto_words(&words, protected, home)
}

/// Veto over an already-split simple command (`words[0]` is the program).
fn command_write_veto_words(
    words: &[String],
    protected: &ProtectedPaths,
    home: Option<&Path>,
) -> Option<VetoMatch> {
    let (prog, args) = words.split_first()?;
    let base = program_basename(prog);
    for (target, label) in write_targets(base, args) {
        if let Some(m) = check_target(&target, label, protected, home) {
            return Some(m);
        }
    }
    None
}

/// Return `(target, pattern-label)` pairs that a known write command would
/// write. Empty for non-write commands.
fn write_targets(base: &str, args: &[String]) -> Vec<(String, &'static str)> {
    let operands = non_flag_operands(args);
    match base {
        // Every file operand is written.
        "tee" => operands.into_iter().map(|t| (t, "tee")).collect(),
        "rm" | "unlink" | "rmdir" => operands.into_iter().map(|t| (t, "rm")).collect::<Vec<_>>(),
        "shred" => operands.into_iter().map(|t| (t, "shred")).collect(),
        "truncate" => truncate_targets(args),
        // Destination is the final operand — UNLESS `-t`/`--target-directory`
        // moves it into a flag value (handled by `copy_move_targets`).
        "cp" | "mv" | "install" | "ln" => copy_move_targets(base, args),
        // `dd of=PATH`.
        "dd" => args
            .iter()
            .filter_map(|a| a.strip_prefix("of=").map(|p| (p.to_string(), "dd of=")))
            .collect(),
        // `sed -i` rewrites its file operands in place.
        "sed" => sed_inplace_targets(args),
        _ => Vec::new(),
    }
}

/// `cp`/`mv`/`install`/`ln` destination resolution. The destination is the
/// final positional operand, except when `-t DIR` / `-tDIR` /
/// `--target-directory[=]DIR` redirects it into a directory flag value (which
/// the naive last-operand check misses — Council R1 round-1 bypass).
fn copy_move_targets(base: &str, args: &[String]) -> Vec<(String, &'static str)> {
    let label = command_label(base);
    let mut tdirs: Vec<String> = Vec::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut end_flags = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !end_flags && a == "--" {
            end_flags = true;
            i += 1;
            continue;
        }
        if !end_flags {
            if a == "-t" || a == "--target-directory" {
                if let Some(v) = args.get(i + 1) {
                    tdirs.push(v.clone());
                }
                i += 2;
                continue;
            }
            if let Some(v) = a.strip_prefix("--target-directory=") {
                tdirs.push(v.to_string());
                i += 1;
                continue;
            }
            if let Some(v) = a.strip_prefix("-t")
                && !v.is_empty()
                && !a.starts_with("--")
            {
                tdirs.push(v.to_string());
                i += 1;
                continue;
            }
            if a.starts_with('-') && a.len() > 1 {
                i += 1;
                continue;
            }
        }
        positionals.push(a.clone());
        i += 1;
    }
    if !tdirs.is_empty() {
        return tdirs.into_iter().map(|t| (t, label)).collect();
    }
    match positionals.last() {
        Some(dest) if positionals.len() >= 2 || base == "ln" => vec![(dest.clone(), label)],
        _ => Vec::new(),
    }
}

fn command_label(base: &str) -> &'static str {
    match base {
        "cp" => "cp",
        "mv" => "mv",
        "install" => "install",
        "ln" => "ln",
        _ => "write",
    }
}

/// `truncate [-s SIZE] FILE…` — every FILE operand is written; the `-s`
/// value is consumed, not a target.
fn truncate_targets(args: &[String]) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut end_flags = false;
    while i < args.len() {
        let a = &args[i];
        if !end_flags && a == "--" {
            end_flags = true;
            i += 1;
            continue;
        }
        if !end_flags && (a == "-s" || a == "--size") {
            i += 2; // skip flag + its value
            continue;
        }
        if !end_flags && a.starts_with('-') {
            i += 1;
            continue;
        }
        out.push((a.clone(), "truncate"));
        i += 1;
    }
    out
}

/// `sed` writes only with an in-place flag (`-i`, `-i.bak`, `--in-place`).
/// When present, the file operands (everything that is not the script or a
/// flag) are rewritten.
fn sed_inplace_targets(args: &[String]) -> Vec<(String, &'static str)> {
    let in_place = args.iter().any(|a| {
        a == "-i" || a.starts_with("-i") && !a.starts_with("--") || a.starts_with("--in-place")
    });
    if !in_place {
        return Vec::new();
    }
    // Heuristic: an explicit script source (`-e`/`-f`) means *all* operands
    // are files; otherwise the first operand is the script and the rest are
    // files.
    let has_explicit_script = args
        .iter()
        .any(|a| a == "-e" || a == "-f" || a.starts_with("-e") || a.starts_with("-f"));
    let operands = non_flag_operands_skipping_values(args);
    let files: Vec<String> = if has_explicit_script {
        operands
    } else {
        operands.into_iter().skip(1).collect()
    };
    files.into_iter().map(|t| (t, "sed -i")).collect()
}

/// Collect non-flag operands, honouring a `--` end-of-flags marker.
fn non_flag_operands(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut end_flags = false;
    for a in args {
        if !end_flags && a == "--" {
            end_flags = true;
            continue;
        }
        if !end_flags && a.starts_with('-') && a.len() > 1 {
            continue;
        }
        out.push(a.clone());
    }
    out
}

/// Like [`non_flag_operands`] but also skips the *value* of value-taking sed
/// flags (`-e SCRIPT`, `-f FILE`) so those values aren't mistaken for write
/// targets.
fn non_flag_operands_skipping_values(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut end_flags = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !end_flags && a == "--" {
            end_flags = true;
            i += 1;
            continue;
        }
        if !end_flags && (a == "-e" || a == "-f") {
            i += 2; // skip flag and its separate value
            continue;
        }
        if !end_flags && a.starts_with('-') && a.len() > 1 {
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

/// Run the argv-as-words redirect scan (case (d) in `persistence_write_veto`).
fn redirect_veto_over_words(
    words: &[String],
    protected: &ProtectedPaths,
    home: Option<&Path>,
) -> Option<VetoMatch> {
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        if (w == ">" || w == ">>" || w == ">|")
            && let Some(target) = words.get(i + 1)
            && let Some(m) = check_target(target, "redirect >", protected, home)
        {
            return Some(m);
        }
        i += 1;
    }
    None
}

// ───────────────────────── target resolution ─────────────────────────

fn check_target(
    token: &str,
    label: &'static str,
    protected: &ProtectedPaths,
    home: Option<&Path>,
) -> Option<VetoMatch> {
    let candidate = resolve_candidate(token, home)?;
    let root = protected.matched(&candidate)?;
    Some(VetoMatch {
        matched_pattern: label.to_string(),
        protected_path: root.display().to_string(),
    })
}

/// Resolve a token to an absolute path for protected-prefix matching.
/// Returns `None` for relative paths (cwd unknown — see module gaps).
fn resolve_candidate(token: &str, home: Option<&Path>) -> Option<PathBuf> {
    if token.is_empty() {
        return None;
    }
    let expanded = if token == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = token.strip_prefix("~/") {
        home?.join(rest)
    } else if Path::new(token).is_absolute() {
        PathBuf::from(token)
    } else {
        return None;
    };
    Some(lexical_clean(&expanded))
}

/// Lexically normalise `.` / `..` / redundant separators without touching the
/// filesystem.
fn lexical_clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalise the longest existing ancestor of `p` and re-append the
/// non-existent remainder. Resolves symlinked protected-dir aliases while
/// still working for not-yet-created write targets.
fn best_effort_canonical(p: &Path) -> PathBuf {
    let p = lexical_clean(p);
    let mut existing = p.as_path();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if existing.exists() {
            break;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                existing = parent;
            }
            _ => return p.clone(),
        }
    }
    let mut base = existing
        .canonicalize()
        .unwrap_or_else(|_| existing.to_path_buf());
    for name in tail.iter().rev() {
        base.push(name);
    }
    base
}

fn program_basename(program: &str) -> &str {
    program
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(program)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

// ───────────────────────── shell tokenizer ─────────────────────────

/// A lexed script token: a word, a redirect operator (write vs read), or a
/// simple-command separator.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Word(String),
    /// A redirect operator. `write` is `true` for `>`/`>>`/`>|`/`&>`/`<>`,
    /// `false` for read-only `<`/`<<`/`<<<`.
    Redir {
        write: bool,
    },
    /// `;`, `&&`, `||`, `|`, `&`, or a newline — a simple-command boundary.
    Sep,
}

/// Shell-aware tokenizer. Splits on whitespace and operator runs, respects
/// single/double quoting and backslash escapes, and classifies redirect
/// operators by direction. Quotes are stripped from emitted words (a leading
/// `~` inside quotes is still expanded later — conservative for a deterrent).
// The `flush_word!` macro's trailing `has_word = false` is a dead store on its
// final (end-of-input) expansion; the reset is load-bearing on every earlier
// call, so the dead store is inherent to the single-macro design.
#[allow(unused_assignments)]
fn tokenize(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut has_word = false;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    macro_rules! flush_word {
        () => {
            if has_word {
                out.push(Tok::Word(std::mem::take(&mut word)));
                has_word = false;
            }
        };
    }

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' => {
                has_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    word.push(chars[i]);
                    i += 1;
                }
                i += 1; // consume closing quote (if any)
            }
            '"' => {
                has_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\'
                        && i + 1 < chars.len()
                        && matches!(chars[i + 1], '"' | '\\' | '$' | '`')
                    {
                        word.push(chars[i + 1]);
                        i += 2;
                    } else {
                        word.push(chars[i]);
                        i += 1;
                    }
                }
                i += 1; // consume closing quote (if any)
            }
            '\\' => {
                has_word = true;
                if i + 1 < chars.len() {
                    word.push(chars[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            c if c.is_whitespace() => {
                flush_word!();
                i += 1;
            }
            ';' | '\n' => {
                flush_word!();
                out.push(Tok::Sep);
                i += 1;
            }
            '&' => {
                // `&>` / `&>>` write-redirect, else `&&` / `&` separator.
                if chars.get(i + 1) == Some(&'>') {
                    flush_word!();
                    out.push(Tok::Redir { write: true });
                    i += if chars.get(i + 2) == Some(&'>') { 3 } else { 2 };
                } else {
                    flush_word!();
                    out.push(Tok::Sep);
                    i += if chars.get(i + 1) == Some(&'&') { 2 } else { 1 };
                }
            }
            '|' => {
                flush_word!();
                out.push(Tok::Sep);
                i += if chars.get(i + 1) == Some(&'|') { 2 } else { 1 };
            }
            '>' => {
                // Drop an immediately-preceding fd-number word (`2>`): it is
                // a descriptor, not a separate operand.
                if has_word && word.chars().all(|c| c.is_ascii_digit()) {
                    word.clear();
                    has_word = false;
                } else {
                    flush_word!();
                }
                // `>>` append, `>|` clobber, `>` truncate — all writes.
                let next = chars.get(i + 1);
                if next == Some(&'>') || next == Some(&'|') {
                    i += 2;
                } else {
                    i += 1;
                }
                out.push(Tok::Redir { write: true });
            }
            '<' => {
                if has_word && word.chars().all(|c| c.is_ascii_digit()) {
                    word.clear();
                    has_word = false;
                } else {
                    flush_word!();
                }
                // `<>` is read-write (treat as write); `<`, `<<`, `<<<` read.
                if chars.get(i + 1) == Some(&'>') {
                    out.push(Tok::Redir { write: true });
                    i += 2;
                } else {
                    let mut j = i + 1;
                    while chars.get(j) == Some(&'<') {
                        j += 1;
                    }
                    out.push(Tok::Redir { write: false });
                    i = j;
                }
            }
            other => {
                has_word = true;
                word.push(other);
                i += 1;
            }
        }
    }
    flush_word!();
    out
}

#[cfg(test)]
#[path = "persistence_veto_tests.rs"]
mod persistence_veto_tests;
