// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-040 Slice 3 — `mode_resolver` precedence stack (pure logic).
//!
//! The resolver answers: given a manual lock, a coherent `ContextSnapshot`
//! (frontmost app + focused window title), and the `[per_app_modes]` rules,
//! which mode is active and *which layer decided it*? It is pure — no platform
//! calls — so the full D4 ladder and the §4.3 deterministic window-rule
//! ranking are unit-testable in isolation. The daemon poller/snapshot
//! reconciler (Slice 5) and the lock lifecycle (Slice 4) wire it up later.
//!
//! D4 precedence (highest first): Manual lock > Window-title rule >
//! App-foreground rule > Global default.
//!
//! §4.3 window-rule ranking (most specific first): constraint depth
//! (App+Title > App-only) → matcher type (Exact > Glob > Regex) → longer
//! literal prefix → declaration order.
//!
//! Spec: `docs/context-consolidation/ADR-040-implementation-spec.md`
//! §4.3, §4.5, §5 Slice 3. Closes #1766.

use conductor_core::config::types::{PerAppModes, WindowRule};
use conductor_daemon::daemon::mode_resolver::{
    ContextSnapshot, ResolutionLayer, ResolverRules, resolve_mode,
};
use std::collections::HashMap;

fn wr(app: &str, pattern: Option<&str>, regex: Option<&str>, mode: &str) -> WindowRule {
    WindowRule {
        app: app.to_string(),
        title_pattern: pattern.map(String::from),
        title_regex: regex.map(String::from),
        mode: mode.to_string(),
    }
}

fn raw(
    default: Option<&str>,
    rules: &[(&str, &str)],
    window_rules: Vec<WindowRule>,
) -> PerAppModes {
    PerAppModes {
        default: default.map(String::from),
        rules: rules
            .iter()
            .map(|(a, m)| (a.to_string(), m.to_string()))
            .collect::<HashMap<_, _>>(),
        window_rules,
        log_titles: false,
    }
}

/// Build and precompile resolver rules (regexes/globs compiled once).
fn pam(
    default: Option<&str>,
    rules: &[(&str, &str)],
    window_rules: Vec<WindowRule>,
) -> ResolverRules {
    ResolverRules::compile(&raw(default, rules, window_rules)).expect("rules compile")
}

fn snap(app: Option<&str>, title: Option<&str>) -> ContextSnapshot {
    ContextSnapshot {
        app: app.map(String::from),
        window_title: title.map(String::from),
    }
}

// ── D4 precedence ladder ───────────────────────────────────────────

#[test]
fn manual_lock_wins_over_everything() {
    let rules = pam(
        Some("Default"),
        &[("Code", "Edit")],
        vec![wr("Code", Some("*.rs - *"), None, "Rust")],
    );
    let s = snap(Some("Code"), Some("main.rs - project"));
    // Even though the window rule and app rule both match, the lock dominates.
    let (mode, layer) = resolve_mode(Some("Locked"), &s, &rules).expect("a mode resolves");
    assert_eq!(mode, "Locked");
    assert_eq!(layer, ResolutionLayer::ManualLock);
}

#[test]
fn window_rule_beats_app_rule() {
    let rules = pam(
        Some("Default"),
        &[("Code", "Edit")],
        vec![wr("Code", Some("*.rs - *"), None, "Rust")],
    );
    let s = snap(Some("Code"), Some("main.rs - project"));
    let (mode, layer) = resolve_mode(None, &s, &rules).expect("a mode resolves");
    assert_eq!(mode, "Rust");
    assert_eq!(layer, ResolutionLayer::WindowTitle);
}

#[test]
fn app_rule_when_no_window_match() {
    let rules = pam(
        Some("Default"),
        &[("Code", "Edit")],
        vec![wr("Code", Some("*.rs - *"), None, "Rust")],
    );
    // Title doesn't match the *.rs pattern → falls to the app-name rule.
    let s = snap(Some("Code"), Some("notes.md - project"));
    let (mode, layer) = resolve_mode(None, &s, &rules).expect("a mode resolves");
    assert_eq!(mode, "Edit");
    assert_eq!(layer, ResolutionLayer::AppForeground);
}

#[test]
fn default_when_no_rule_matches() {
    let rules = pam(Some("Default"), &[("Code", "Edit")], vec![]);
    let s = snap(Some("Safari"), Some("anything"));
    let (mode, layer) = resolve_mode(None, &s, &rules).expect("default resolves");
    assert_eq!(mode, "Default");
    assert_eq!(layer, ResolutionLayer::Default);
}

#[test]
fn none_when_no_default_and_no_match() {
    let rules = pam(None, &[("Code", "Edit")], vec![]);
    let s = snap(Some("Safari"), None);
    assert!(resolve_mode(None, &s, &rules).is_none());
}

#[test]
fn unknown_title_falls_to_app_rule() {
    // §4.5: an app-change invalidates the cached title; the resolver treats an
    // Unknown (None) title as "no window match" rather than matching a stale one.
    let rules = pam(
        Some("Default"),
        &[("Code", "Edit")],
        vec![wr("Code", Some("*.rs - *"), None, "Rust")],
    );
    let s = snap(Some("Code"), None);
    let (mode, layer) = resolve_mode(None, &s, &rules).expect("a mode resolves");
    assert_eq!(mode, "Edit");
    assert_eq!(layer, ResolutionLayer::AppForeground);
}

// ── §4.3 window-rule ranking ───────────────────────────────────────

#[test]
fn app_plus_title_beats_app_only() {
    // Two window rules for the same app: one constrains the title (App+Title),
    // one is an app-only fallback. The titled rule is more specific.
    let rules = pam(
        None,
        &[],
        vec![
            wr("Code", None, None, "AppOnly"),            // app-only fallback
            wr("Code", Some("*.rs - *"), None, "Titled"), // App+Title
        ],
    );
    let s = snap(Some("Code"), Some("main.rs - project"));
    let (mode, _) = resolve_mode(None, &s, &rules).expect("resolves");
    assert_eq!(mode, "Titled");
}

#[test]
fn exact_beats_glob() {
    // Both match; the literal (no-metachar) pattern is Exact and outranks Glob.
    let rules = pam(
        None,
        &[],
        vec![
            wr("Code", Some("*.rs - project"), None, "Glob"),
            wr("Code", Some("main.rs - project"), None, "Exact"),
        ],
    );
    let s = snap(Some("Code"), Some("main.rs - project"));
    let (mode, _) = resolve_mode(None, &s, &rules).expect("resolves");
    assert_eq!(mode, "Exact");
}

#[test]
fn glob_beats_regex() {
    let rules = pam(
        None,
        &[],
        vec![
            wr("Code", None, Some("^main\\.rs"), "Regex"),
            wr("Code", Some("main.rs*"), None, "Glob"),
        ],
    );
    let s = snap(Some("Code"), Some("main.rs - project"));
    let (mode, _) = resolve_mode(None, &s, &rules).expect("resolves");
    assert_eq!(mode, "Glob");
}

#[test]
fn longer_literal_prefix_wins_among_globs() {
    let rules = pam(
        None,
        &[],
        vec![
            wr("Code", Some("ma*"), None, "Short"),
            wr("Code", Some("main.r*"), None, "Long"),
        ],
    );
    let s = snap(Some("Code"), Some("main.rs - project"));
    let (mode, _) = resolve_mode(None, &s, &rules).expect("resolves");
    assert_eq!(mode, "Long");
}

#[test]
fn declaration_order_breaks_ties() {
    // Two identically-specific globs both match; the first declared wins.
    let rules = pam(
        None,
        &[],
        vec![
            wr("Code", Some("*.rs - *"), None, "First"),
            wr("Code", Some("*.rs - *"), None, "Second"),
        ],
    );
    let s = snap(Some("Code"), Some("main.rs - project"));
    let (mode, _) = resolve_mode(None, &s, &rules).expect("resolves");
    assert_eq!(mode, "First");
}

// ── glob/regex matching semantics ──────────────────────────────────

#[test]
fn glob_question_and_charclass_match() {
    let rules = pam(None, &[], vec![wr("Term", Some("file?.[ch]"), None, "Hit")]);
    assert_eq!(
        resolve_mode(None, &snap(Some("Term"), Some("file1.c")), &rules).map(|(m, _)| m),
        Some("Hit".to_string())
    );
    assert_eq!(
        resolve_mode(None, &snap(Some("Term"), Some("file2.h")), &rules).map(|(m, _)| m),
        Some("Hit".to_string())
    );
    // `?` matches exactly one char; "file10.c" has two digits → no match.
    assert!(resolve_mode(None, &snap(Some("Term"), Some("file10.c")), &rules).is_none());
}

#[test]
fn glob_charclass_range_and_negation() {
    // Range `[0-9]` (Copilot review on #2275: cover ranges, not just plain sets).
    let range = pam(None, &[], vec![wr("App", Some("v[0-9].log"), None, "Num")]);
    assert_eq!(
        resolve_mode(None, &snap(Some("App"), Some("v3.log")), &range).map(|(m, _)| m),
        Some("Num".to_string())
    );
    assert!(resolve_mode(None, &snap(Some("App"), Some("vX.log")), &range).is_none());

    // Negation `[!.]`: the first char must NOT be a dot.
    let neg = pam(None, &[], vec![wr("App", Some("[!.]*"), None, "Visible")]);
    assert_eq!(
        resolve_mode(None, &snap(Some("App"), Some("notes")), &neg).map(|(m, _)| m),
        Some("Visible".to_string())
    );
    assert!(resolve_mode(None, &snap(Some("App"), Some(".hidden")), &neg).is_none());
}

#[test]
fn inverted_range_is_normalised() {
    // Council review on #2275: `[z-a]` must not silently match nothing — it is
    // normalised to `[a-z]`.
    let rules = pam(None, &[], vec![wr("App", Some("[z-a]"), None, "Hit")]);
    assert_eq!(
        resolve_mode(None, &snap(Some("App"), Some("m")), &rules).map(|(m, _)| m),
        Some("Hit".to_string())
    );
    assert!(resolve_mode(None, &snap(Some("App"), Some("5")), &rules).is_none());
}

#[test]
fn compile_rejects_bad_regex() {
    // The resolver's defensive boundary: an uncompilable title_regex fails to
    // compile rather than panicking at resolve time (config validation rejects
    // these at load — ADR-040 Slice 2 — but the resolver guards regardless).
    let bad = raw(None, &[], vec![wr("App", None, Some("^(unclosed"), "X")]);
    assert!(ResolverRules::compile(&bad).is_err());
}

#[test]
fn regex_rule_matches_title() {
    let rules = pam(
        None,
        &[],
        vec![wr("Safari", None, Some("^(Jira|Confluence)"), "PM")],
    );
    assert_eq!(
        resolve_mode(None, &snap(Some("Safari"), Some("Jira - board")), &rules).map(|(m, _)| m),
        Some("PM".to_string())
    );
    assert!(resolve_mode(None, &snap(Some("Safari"), Some("News")), &rules).is_none());
}
