// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! ADR-031 Phase 2A § 4.3 — `[[routes]]` overlap-warning rules.
//!
//! Two non-fatal warnings (ADR-036 Phase 2 removed the former
//! route-shadowed-by-Raw rule along with `Trigger::Raw`):
//!   - **Route shadowed by specific**: a route with `from = X` where
//!     a specific trigger (Note, CC, …) with `device = X | None`
//!     already handles events on that device. The specific fires
//!     first; route only sees what doesn't match.
//!   - **Exact-duplicate route**: two routes with same `from`, `to`,
//!     AND same filter shape. Wasted CPU + same event sent twice.
//!
//! Both are WARNINGS, not errors — config still validates.
//!
//! ADR-035: route sources/targets resolve against the unified
//! `[[endpoints]]` set. A route source must be input-capable
//! (`direction = "Input"`/`"Bidirectional"`) for the trigger-shadow
//! scan to consider it; a `Matcher` endpoint must carry a matcher.

use conductor_core::Config;
use conductor_core::config::validation::validate_config;

fn parse_or_panic(toml: &str) -> Config {
    toml::from_str(toml).expect("config parses")
}

fn assert_warning_about(
    report: &conductor_core::config::validation::ValidationReport,
    fragment: &str,
) {
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.to_lowercase().contains(&fragment.to_lowercase())),
        "expected a warning mentioning '{}'; got: {:#?}",
        fragment,
        report.warnings
    );
}

// ── Rule 2: route shadowed by specific trigger ──

#[test]
fn route_with_source_matching_specific_trigger_emits_warning() {
    // A Note trigger on device "pads" already handles note events
    // from pads — the route at stage 9 won't see them.
    let cfg = parse_or_panic(
        r#"
[[endpoints]]
alias = "pads"
direction = "Input"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Mikro" }]

[[endpoints]]
alias = "absynth"
direction = "Output"
type = "Matcher"
matchers = [{ type = "NameContains", value = "Absynth" }]

[[modes]]
name = "Default"
[[modes.mappings]]
trigger = { type = "Note", note = 60, device = "pads" }
action = { type = "Keystroke", keys = "space" }

[[routes]]
from = "pads"
to = "absynth"
"#,
    );
    let report = validate_config(&cfg);
    assert_warning_about(&report, "specific");
    assert_warning_about(&report, "pads");
}

// ── Rule 3: exact-duplicate route ──

#[test]
fn exact_duplicate_route_emits_warning() {
    // Two routes with same from/to/filter (here both no-filter) waste
    // CPU + emit each event twice.
    let cfg = parse_or_panic(
        r#"
[[endpoints]]
alias = "a"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceA" }]

[[endpoints]]
alias = "b"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceB" }]

[[modes]]
name = "Default"

[[routes]]
from = "a"
to = "b"

[[routes]]
from = "a"
to = "b"
"#,
    );
    let report = validate_config(&cfg);
    assert_warning_about(&report, "duplicate");
}

#[test]
fn distinct_transforms_on_same_source_dest_do_not_warn_as_duplicate() {
    // Copilot review on PR #1161 finding #2: my slice-3 commit message
    // said "different transforms on same source/dest is legitimate
    // fan-out" — but the implementation EXCLUDED transform from the
    // shape check, which inverted the logic and made differently-
    // transformed routes (which SHOULD be allowed as fan-out) emit
    // a false duplicate warning.
    //
    // Fix: include transform in route_shapes_equal. Then routes with
    // same from/to/filter but DIFFERENT transforms are correctly
    // recognized as distinct (legitimate fan-out).
    let cfg = parse_or_panic(
        r#"
[[endpoints]]
alias = "a"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceA" }]

[[endpoints]]
alias = "b"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceB" }]

[[modes]]
name = "Default"

[[routes]]
from = "a"
to = "b"
[routes.transform]
type = "Midi"
channel = 1

[[routes]]
from = "a"
to = "b"
[routes.transform]
type = "Midi"
channel = 9
"#,
    );
    let report = validate_config(&cfg);
    let dup_warnings: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| w.message.to_lowercase().contains("duplicate"))
        .collect();
    assert!(
        dup_warnings.is_empty(),
        "different transforms on same source/dest is legitimate fan-out; got: {:#?}",
        dup_warnings
    );
}

#[test]
fn identical_transforms_on_same_source_dest_warn_as_duplicate() {
    // Counter-test: with the fix, same from/to/filter AND same
    // transform IS a true duplicate, must still warn.
    let cfg = parse_or_panic(
        r#"
[[endpoints]]
alias = "a"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceA" }]

[[endpoints]]
alias = "b"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceB" }]

[[modes]]
name = "Default"

[[routes]]
from = "a"
to = "b"
[routes.transform]
type = "Midi"
channel = 5

[[routes]]
from = "a"
to = "b"
[routes.transform]
type = "Midi"
channel = 5
"#,
    );
    let report = validate_config(&cfg);
    assert_warning_about(&report, "duplicate");
}

#[test]
fn distinct_routes_with_different_filters_do_not_warn_as_duplicate() {
    // Same from/to but different filter shape — legitimate fan-out
    // by message-type. No duplicate warning.
    let cfg = parse_or_panic(
        r#"
[[endpoints]]
alias = "a"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceA" }]

[[endpoints]]
alias = "b"
direction = "Bidirectional"
type = "Matcher"
matchers = [{ type = "NameContains", value = "DeviceB" }]

[[modes]]
name = "Default"

[[routes]]
from = "a"
to = "b"
[routes.filter]
message_types = ["NoteOn"]

[[routes]]
from = "a"
to = "b"
[routes.filter]
message_types = ["CC"]
"#,
    );
    let report = validate_config(&cfg);
    let dup_warnings: Vec<_> = report
        .warnings
        .iter()
        .filter(|w| w.message.to_lowercase().contains("duplicate"))
        .collect();
    assert!(
        dup_warnings.is_empty(),
        "distinct filters must not duplicate-warn; got: {:#?}",
        dup_warnings
    );
}
