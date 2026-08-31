// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-040 Slice 2 — `[per_app_modes]` + `WindowRule` schema + validation.
//!
//! Schema-layer half of mode auto-switching (D3) and window-title matching
//! (D5). This slice only proves the config deserializes, defaults correctly,
//! and that the validator rejects: a `default`/`rules`/`window_rules` mode
//! reference that doesn't name a declared `[[modes]]` block; a `title_regex`
//! that doesn't compile; and a `WindowRule` that sets *both* `title_pattern`
//! and `title_regex` (mutually exclusive per spec §4.1). The daemon
//! auto-switch path, the precedence resolver, and the title poller land in
//! later slices.
//!
//! Spec: `docs/context-consolidation/ADR-040-implementation-spec.md`
//! §4.1, §5 Slice 2. Closes #1765.

use conductor_core::Config;
use conductor_core::config::validation::{ValidationReport, validate_config};

fn parse(toml: &str) -> Config {
    toml::from_str(toml).expect("config parses")
}

/// Assert an error exists at the *exact* `path` whose message contains
/// `fragment` (case-insensitive). Exact-match (not a prefix) so a test can't
/// pass on an error from a sibling/child path — e.g. an assertion targeting
/// `per_app_modes.window_rules[0]` (mutual-exclusivity) won't be satisfied by
/// an error at `per_app_modes.window_rules[0].mode` (Council review on #2274).
fn assert_error_at(report: &ValidationReport, path: &str, fragment: &str) {
    let hit = report
        .errors
        .iter()
        .any(|e| e.path == path && e.message.to_lowercase().contains(&fragment.to_lowercase()));
    assert!(
        hit,
        "expected an error at exact path '{path}' mentioning '{fragment}'; got: {:#?}",
        report.errors
    );
}

fn assert_no_errors(report: &ValidationReport) {
    assert!(
        report.errors.is_empty(),
        "expected no errors, got: {:#?}",
        report.errors
    );
}

const VALID: &str = r#"
[[modes]]
name = "Default"

[[modes]]
name = "Production"

[[modes]]
name = "Streaming"

[[modes]]
name = "RustDev"

[per_app_modes]
default = "Default"

[per_app_modes.rules]
"Logic Pro" = "Production"
"OBS" = "Streaming"

[[per_app_modes.window_rules]]
app = "Visual Studio Code"
title_pattern = "*.rs - *"
mode = "RustDev"

[[per_app_modes.window_rules]]
app = "Visual Studio Code"
mode = "Production"
"#;

#[test]
fn parses_per_app_modes_and_window_rules() {
    let cfg = parse(VALID);
    let pam = cfg.per_app_modes.as_ref().expect("per_app_modes present");
    assert_eq!(pam.default.as_deref(), Some("Default"));
    assert_eq!(
        pam.rules.get("Logic Pro").map(String::as_str),
        Some("Production")
    );
    assert_eq!(pam.rules.get("OBS").map(String::as_str), Some("Streaming"));
    assert_eq!(pam.window_rules.len(), 2);
    assert_eq!(pam.window_rules[0].app, "Visual Studio Code");
    assert_eq!(
        pam.window_rules[0].title_pattern.as_deref(),
        Some("*.rs - *")
    );
    assert_eq!(pam.window_rules[0].mode, "RustDev");
    // App-only fallback rule: neither title field set.
    assert!(pam.window_rules[1].title_pattern.is_none());
    assert!(pam.window_rules[1].title_regex.is_none());
    assert_no_errors(&validate_config(&cfg));
}

#[test]
fn log_titles_defaults_false() {
    let cfg = parse(VALID);
    assert!(
        !cfg.per_app_modes.unwrap().log_titles,
        "log_titles defaults false (privacy)"
    );
}

#[test]
fn omitted_per_app_modes_is_none() {
    let cfg = parse("[[modes]]\nname = \"Default\"\n");
    assert!(cfg.per_app_modes.is_none());
    assert_no_errors(&validate_config(&cfg));
}

#[test]
fn unknown_default_mode_errors() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[per_app_modes]
default = "Ghost"
"#,
    );
    assert_error_at(&validate_config(&cfg), "per_app_modes.default", "Ghost");
}

#[test]
fn unknown_rule_mode_errors() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[per_app_modes.rules]
"OBS" = "Ghost"
"#,
    );
    assert_error_at(
        &validate_config(&cfg),
        "per_app_modes.rules.\"OBS\"",
        "Ghost",
    );
}

#[test]
fn unknown_window_rule_mode_errors() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[per_app_modes.window_rules]]
app = "Safari"
title_pattern = "*Jira*"
mode = "Ghost"
"#,
    );
    assert_error_at(
        &validate_config(&cfg),
        "per_app_modes.window_rules[0].mode",
        "Ghost",
    );
}

#[test]
fn bad_title_regex_errors() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[per_app_modes.window_rules]]
app = "Safari"
title_regex = "^(unclosed"
mode = "Default"
"#,
    );
    assert_error_at(
        &validate_config(&cfg),
        "per_app_modes.window_rules[0].title_regex",
        "regex",
    );
}

#[test]
fn title_pattern_and_regex_mutually_exclusive_errors() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[per_app_modes.window_rules]]
app = "Safari"
title_pattern = "*Jira*"
title_regex = "^Jira"
mode = "Default"
"#,
    );
    assert_error_at(
        &validate_config(&cfg),
        "per_app_modes.window_rules[0]",
        "mutually exclusive",
    );
}

#[test]
fn app_only_window_rule_is_valid() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[per_app_modes.window_rules]]
app = "Visual Studio Code"
mode = "Default"
"#,
    );
    assert_no_errors(&validate_config(&cfg));
}

/// Happy path: a *valid* `title_regex` (with a declared target mode) compiles
/// and passes validation (Council review on #2274 — the suite previously only
/// covered the bad-regex error path).
#[test]
fn valid_title_regex_is_accepted() {
    let cfg = parse(
        r#"
[[modes]]
name = "Default"

[[per_app_modes.window_rules]]
app = "Safari"
title_regex = "^(Jira|Confluence).*"
mode = "Default"
"#,
    );
    assert_no_errors(&validate_config(&cfg));
}
