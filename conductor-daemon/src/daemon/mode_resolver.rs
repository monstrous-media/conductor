// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-040 D4 — pure mode-resolution precedence stack.
//!
//! Given a manual lock, a coherent [`ContextSnapshot`] (frontmost app +
//! focused window title), and the precompiled [`ResolverRules`], decide the
//! active mode and *which layer* decided it. Resolution is **pure**: no
//! platform calls, no I/O, no clock, and — because [`ResolverRules::compile`]
//! tokenises globs and compiles regexes ONCE up front — no per-call parsing or
//! regex compilation. The daemon's pollers build the snapshot (Slice 5) and own
//! the lock lifecycle (Slice 4); here we only compile + resolve.
//!
//! D4 precedence (highest first):
//!   1. Manual lock         — user explicitly pinned a mode
//!   2. Window-title rule   — `[per_app_modes.window_rules]`
//!   3. App-foreground rule — `[per_app_modes].rules` (app name → mode)
//!   4. Global default      — `[per_app_modes].default`
//!
//! Window-rule ranking (§4.3, most specific first):
//!   constraint depth (App+Title > App-only) → matcher type (Exact > Glob >
//!   Regex) → longer literal prefix → declaration order (ties keep the first
//!   declared). "Title-only" is unreachable: `WindowRule.app` is mandatory.

use conductor_core::config::types::PerAppModes;
use std::collections::HashMap;

/// Which precedence layer decided the active mode. Returned alongside the mode
/// so status surfaces (`conductor_mode_status`, GUI) can show *why*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionLayer {
    /// A mode was manually pinned (lock held).
    ManualLock,
    /// A `[[per_app_modes.window_rules]]` entry matched.
    WindowTitle,
    /// A `[per_app_modes].rules` app-name entry matched.
    AppForeground,
    /// Fell through to `[per_app_modes].default`.
    Default,
}

/// A coherent snapshot of the foreground context (§4.5).
///
/// The resolver consumes this single struct — never the app and title pollers'
/// caches independently — so it can't resolve a stale app/title pair. When an
/// app change invalidates the cached title, the caller sets `window_title`
/// to `None` (Unknown); the resolver then treats it as "no window match" and
/// falls to app-name rules rather than matching a stale title.
#[derive(Debug, Clone, Default)]
pub struct ContextSnapshot {
    /// Frontmost application name. `None` = unknown (no app/window rules apply).
    pub app: Option<String>,
    /// Focused window title. `None` = unknown → window rules requiring a title
    /// can't match (app-only window rules still can).
    pub window_title: Option<String>,
}

/// Precompiled title matcher for one window rule — built once by
/// [`ResolverRules::compile`] so resolution never parses or compiles.
enum TitleMatcher {
    /// No title constraint (app-only fallback rule).
    AppOnly,
    /// Literal pattern (no glob metacharacters) — compared by equality.
    Exact(String),
    /// Glob pattern, pre-tokenised, with its leading-literal prefix length.
    Glob { toks: Vec<Tok>, prefix: usize },
    /// Pre-compiled regex.
    Regex(regex::Regex),
}

struct CompiledWindowRule {
    app: String,
    matcher: TitleMatcher,
    mode: String,
}

/// `[per_app_modes]` compiled into a form the resolver can evaluate with no
/// per-call allocation/parsing: app-name rules as-is, window rules with their
/// globs tokenised and regexes compiled.
pub struct ResolverRules {
    default: Option<String>,
    app_rules: HashMap<String, String>,
    window_rules: Vec<CompiledWindowRule>,
}

impl ResolverRules {
    /// Compile `[per_app_modes]` once. Tokenises each glob and compiles each
    /// `title_regex`, so [`resolve_mode`] does zero parsing/compilation per
    /// call (resolution runs on every context change, not once).
    ///
    /// Returns `Err` if a `title_regex` fails to compile. The config validator
    /// already rejects bad regexes at load (ADR-040 Slice 2); this is the
    /// defensive boundary for any rules that reach the resolver unvalidated.
    pub fn compile(pam: &PerAppModes) -> Result<Self, regex::Error> {
        let mut window_rules = Vec::with_capacity(pam.window_rules.len());
        for wr in &pam.window_rules {
            let matcher = if let Some(p) = wr.title_pattern.as_deref() {
                if is_exact(p) {
                    TitleMatcher::Exact(p.to_string())
                } else {
                    TitleMatcher::Glob {
                        toks: tokenize(p),
                        prefix: literal_prefix_len(p),
                    }
                }
            } else if let Some(re) = wr.title_regex.as_deref() {
                TitleMatcher::Regex(regex::Regex::new(re)?)
            } else {
                TitleMatcher::AppOnly
            };
            window_rules.push(CompiledWindowRule {
                app: wr.app.clone(),
                matcher,
                mode: wr.mode.clone(),
            });
        }
        Ok(ResolverRules {
            default: pam.default.clone(),
            app_rules: pam.rules.clone(),
            window_rules,
        })
    }
}

/// Resolve the active mode from the D4 precedence stack.
///
/// `locked` is the manually-pinned mode, if any (Slice 4 owns its lifecycle;
/// here it is just the top precedence input). Returns the chosen mode and the
/// layer that decided it, or `None` when nothing matches and there is no
/// `default` (the caller then applies its own first-mode fallback).
pub fn resolve_mode(
    locked: Option<&str>,
    snapshot: &ContextSnapshot,
    rules: &ResolverRules,
) -> Option<(String, ResolutionLayer)> {
    // 1. Manual lock dominates outright.
    if let Some(mode) = locked {
        return Some((mode.to_string(), ResolutionLayer::ManualLock));
    }

    if let Some(app) = snapshot.app.as_deref() {
        // 2. Window-title rules (most specific). Pick the best-ranked match.
        if let Some(wr) =
            best_window_rule(app, snapshot.window_title.as_deref(), &rules.window_rules)
        {
            return Some((wr.mode.clone(), ResolutionLayer::WindowTitle));
        }
        // 3. App-foreground rule.
        if let Some(mode) = rules.app_rules.get(app) {
            return Some((mode.clone(), ResolutionLayer::AppForeground));
        }
    }

    // 4. Global default.
    rules.default.clone().map(|m| (m, ResolutionLayer::Default))
}

/// Specificity key for a matching window rule (higher tuple = more specific).
/// Compared lexicographically: depth → matcher rank → literal-prefix length.
/// Declaration order is the final tiebreak, handled by the caller keeping the
/// first-declared rule on an equal key.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct RuleRank {
    /// App+Title (1) beats App-only (0).
    depth: u8,
    /// Exact (2) > Glob (1) > Regex (0).
    matcher: u8,
    /// Length of the leading literal run (Exact/Glob only; 0 for Regex/app-only).
    prefix: usize,
}

/// Find the highest-ranked window rule that matches `(app, title)`.
fn best_window_rule<'a>(
    app: &str,
    title: Option<&str>,
    window_rules: &'a [CompiledWindowRule],
) -> Option<&'a CompiledWindowRule> {
    let mut best: Option<(RuleRank, &CompiledWindowRule)> = None;
    for wr in window_rules {
        if wr.app != app {
            continue;
        }
        let Some(rank) = match_rank(wr, title) else {
            continue; // didn't match this title
        };
        // Strictly-greater replacement keeps the first-declared rule on ties
        // (declaration order is the final §4.3 tiebreak).
        if best.as_ref().is_none_or(|(b, _)| rank > *b) {
            best = Some((rank, wr));
        }
    }
    best.map(|(_, wr)| wr)
}

/// If `wr` matches `title`, return its specificity rank; else `None`.
fn match_rank(wr: &CompiledWindowRule, title: Option<&str>) -> Option<RuleRank> {
    match &wr.matcher {
        // App-only fallback: matches any title (including Unknown/None).
        TitleMatcher::AppOnly => Some(RuleRank {
            depth: 0,
            matcher: 0,
            prefix: 0,
        }),
        TitleMatcher::Exact(p) => {
            let t = title?;
            (p == t).then(|| RuleRank {
                depth: 1,
                matcher: 2,
                prefix: p.chars().count(),
            })
        }
        TitleMatcher::Glob { toks, prefix } => {
            let t = title?;
            glob_match(toks, t).then_some(RuleRank {
                depth: 1,
                matcher: 1,
                prefix: *prefix,
            })
        }
        TitleMatcher::Regex(r) => {
            let t = title?;
            r.is_match(t).then_some(RuleRank {
                depth: 1,
                matcher: 0, // Regex
                prefix: 0,  // regex-vs-regex falls to declaration order
            })
        }
    }
}

/// True when a glob pattern contains no metacharacters (so it is an exact match).
fn is_exact(pattern: &str) -> bool {
    !pattern.contains(['*', '?', '['])
}

/// Length (in chars) of the leading literal run before the first glob
/// metacharacter (`*`, `?`, `[`).
fn literal_prefix_len(pattern: &str) -> usize {
    pattern
        .chars()
        .take_while(|c| !matches!(c, '*' | '?' | '['))
        .count()
}

/// Glob match over pre-tokenised `toks`. Supports `*` (any run), `?` (one
/// char), and `[...]` character classes. Linear-ish with single-`*`
/// backtracking.
fn glob_match(toks: &[Tok], text: &str) -> bool {
    let t: Vec<char> = text.chars().collect();

    let (mut pi, mut ti) = (0usize, 0usize);
    // Last `*` we can backtrack to: (token index after the star, text index).
    let mut star: Option<(usize, usize)> = None;

    while ti < t.len() {
        match toks.get(pi) {
            Some(Tok::Star) => {
                star = Some((pi + 1, ti));
                pi += 1;
            }
            Some(tok) if tok.matches(t[ti]) => {
                pi += 1;
                ti += 1;
            }
            _ => {
                // Mismatch (or pattern exhausted): extend the last `*` by one char.
                if let Some((spi, sti)) = star {
                    pi = spi;
                    ti = sti + 1;
                    star = Some((spi, sti + 1));
                } else {
                    return false;
                }
            }
        }
    }
    // Text consumed; remaining pattern must be only `*`.
    while matches!(toks.get(pi), Some(Tok::Star)) {
        pi += 1;
    }
    pi == toks.len()
}

enum Tok {
    Star,
    Any,
    Lit(char),
    /// Character class: inclusive ranges + negation flag.
    Class {
        ranges: Vec<(char, char)>,
        neg: bool,
    },
}

impl Tok {
    /// Does this token consume the single literal char `c`? `Star` is handled
    /// structurally in [`glob_match`] and never reaches here; it returns
    /// `false` (fail-safe — never a spurious match) rather than `true`.
    fn matches(&self, c: char) -> bool {
        match self {
            Tok::Star => false,
            Tok::Any => true,
            Tok::Lit(l) => *l == c,
            Tok::Class { ranges, neg } => {
                let hit = ranges.iter().any(|(lo, hi)| *lo <= c && c <= *hi);
                hit != *neg
            }
        }
    }
}

/// Parse a glob pattern into tokens. A malformed `[` (no closing `]`) is
/// treated as a literal `[`.
fn tokenize(pattern: &str) -> Vec<Tok> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut toks = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                toks.push(Tok::Star);
                i += 1;
            }
            '?' => {
                toks.push(Tok::Any);
                i += 1;
            }
            '[' => {
                if let Some((tok, next)) = parse_class(&chars, i) {
                    toks.push(tok);
                    i = next;
                } else {
                    toks.push(Tok::Lit('['));
                    i += 1;
                }
            }
            c => {
                toks.push(Tok::Lit(c));
                i += 1;
            }
        }
    }
    toks
}

/// Parse a `[...]` class starting at `chars[start] == '['`. Returns the class
/// token and the index just past the closing `]`, or `None` if unterminated.
/// An inverted range (`z-a`) is normalised to `a-z` rather than silently
/// matching nothing.
fn parse_class(chars: &[char], start: usize) -> Option<(Tok, usize)> {
    let mut i = start + 1;
    let neg = matches!(chars.get(i), Some('!') | Some('^'));
    if neg {
        i += 1;
    }
    let mut ranges: Vec<(char, char)> = Vec::new();
    let class_start = i;
    while i < chars.len() {
        let c = chars[i];
        // A `]` as the very first class char is a literal `]`, not a terminator.
        if c == ']' && i > class_start {
            return Some((Tok::Class { ranges, neg }, i + 1));
        }
        // Range `a-z`: current char, a `-`, and a following non-`]` char.
        if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i + 2] != ']' {
            let (a, b) = (c, chars[i + 2]);
            // Normalise inverted ranges so `[z-a]` behaves as `[a-z]` instead of
            // silently matching nothing.
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            ranges.push((lo, hi));
            i += 3;
        } else {
            ranges.push((c, c));
            i += 1;
        }
    }
    None // no closing ']'
}
