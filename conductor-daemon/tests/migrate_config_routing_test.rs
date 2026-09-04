// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Tests for `conductorctl migrate-config --routing` (ADR-036 Slice 8).
//!
//! These exercise the library functions directly on a
//! `toml_edit::DocumentMut` — no need to shell out to the binary.

use conductor_daemon::migration::migrate_raw_to_routes;
use toml_edit::DocumentMut;

fn parse(toml: &str) -> DocumentMut {
    toml.parse::<DocumentMut>().expect("fixture parses")
}

fn count_array_of_tables(doc: &DocumentMut, key: &str) -> usize {
    doc.get(key)
        .and_then(|i| i.as_array_of_tables())
        .map(|a| a.len())
        .unwrap_or(0)
}

fn count_mode_mappings(doc: &DocumentMut, mode_name: &str) -> usize {
    let modes = doc
        .get("modes")
        .and_then(|i| i.as_array_of_tables())
        .expect("modes present");
    for mode in modes.iter() {
        if mode.get("name").and_then(|i| i.as_str()) == Some(mode_name) {
            return mode
                .get("mappings")
                .and_then(|i| i.as_array_of_tables())
                .map(|a| a.len())
                .unwrap_or(0);
        }
    }
    0
}

#[test]
fn migrates_three_raw_forward_mappings_across_two_modes() {
    let toml = r#"
[[modes]]
name = "Drums"
[[modes.mappings]]
trigger = { type = "Raw", device = "pads" }
action = { type = "MidiForward", target = "absynth" }
[[modes.mappings]]
trigger = { type = "Raw", device = "pads", channel = 9 }
action = { type = "MidiForward", target = "kontakt" }

[[modes]]
name = "Keys"
[[modes.mappings]]
trigger = { type = "Raw" }
action = { type = "MidiForward", target = "piano" }
"#;

    let mut doc = parse(toml);
    let report = migrate_raw_to_routes(&mut doc).expect("forward migration succeeds");

    assert_eq!(report.rewrites.len(), 3, "three rewrites reported");
    assert_eq!(
        count_array_of_tables(&doc, "routes"),
        3,
        "three routes added"
    );
    assert_eq!(
        count_mode_mappings(&doc, "Drums"),
        0,
        "Drums mappings removed"
    );
    assert_eq!(
        count_mode_mappings(&doc, "Keys"),
        0,
        "Keys mappings removed"
    );

    // Routes re-parse and carry the expected shape.
    let cfg: conductor_core::Config =
        toml::from_str(&doc.to_string()).expect("migrated TOML re-parses");
    assert_eq!(cfg.routes.len(), 3);
    // First route: from = "pads", to = "absynth", mode scope. All routes
    // are post-mapping after ADR-036 Phase 3 (forward migration no longer
    // emits a `phase` field).
    let r0 = &cfg.routes[0];
    assert_eq!(r0.from, "pads");
    assert_eq!(r0.to, "absynth");
    assert_eq!(r0.modes, vec!["Drums".to_string()]);
    assert!(r0.enabled);
    assert!(r0.filter.is_none(), "no channel/message_types → no filter");

    // Second route had channel = 9 → filter.channels = [9].
    let r1 = &cfg.routes[1];
    let filter = r1.filter.as_ref().expect("channel produces a filter");
    assert_eq!(filter.channels, vec![9]);

    // Third route had no device → from = "*".
    assert_eq!(cfg.routes[2].from, "*");
}

#[test]
fn no_raw_triggers_is_a_noop() {
    let toml = r#"
[[modes]]
name = "Default"
[[modes.mappings]]
trigger = { type = "Note", note = 36 }
action = { type = "Keystroke", keys = ["cmd", "space"] }
"#;
    let mut doc = parse(toml);
    let before = doc.to_string();
    let report = migrate_raw_to_routes(&mut doc).expect("no-op succeeds");
    assert!(report.rewrites.is_empty(), "no rewrites");
    assert_eq!(count_array_of_tables(&doc, "routes"), 0, "no routes added");
    assert_eq!(doc.to_string(), before, "document unchanged");
}

#[test]
fn raw_with_non_midiforward_action_aborts() {
    let toml = r#"
[[modes]]
name = "Edit"
[[modes.mappings]]
trigger = { type = "Raw", device = "pads" }
action = { type = "Keystroke", keys = ["cmd", "c"] }
"#;
    let mut doc = parse(toml);
    let err = migrate_raw_to_routes(&mut doc).expect_err("must abort");
    assert!(err.contains("Edit"), "error names the mode: {err}");
    assert!(
        err.contains("MidiForward"),
        "error mentions MidiForward: {err}"
    );
}

#[test]
fn preserves_leading_comment() {
    let toml = r#"
[[modes]]
name = "Drums"

# forward the pads to absynth for layering
[[modes.mappings]]
trigger = { type = "Raw", device = "pads" }
action = { type = "MidiForward", target = "absynth" }
"#;
    let mut doc = parse(toml);
    migrate_raw_to_routes(&mut doc).expect("migration succeeds");
    let rendered = doc.to_string();
    assert!(
        rendered.contains("# forward the pads to absynth for layering"),
        "leading comment must survive migration:\n{rendered}"
    );
}

#[test]
fn forward_migration_is_idempotent() {
    let toml = r#"
[[modes]]
name = "Drums"
[[modes.mappings]]
trigger = { type = "Raw", device = "pads" }
action = { type = "MidiForward", target = "absynth" }
"#;
    let mut doc = parse(toml);
    let first = migrate_raw_to_routes(&mut doc).expect("first run");
    assert_eq!(first.rewrites.len(), 1);
    let after_first = doc.to_string();

    let second = migrate_raw_to_routes(&mut doc).expect("second run");
    assert!(second.rewrites.is_empty(), "second run adds nothing");
    assert_eq!(count_array_of_tables(&doc, "routes"), 1, "still one route");
    assert_eq!(
        doc.to_string(),
        after_first,
        "second run leaves the document byte-identical"
    );
}

// ADR-036 Phase 3 removed the reverse (`routes → Raw`) migration direction
// — `reverse_routes_to_raw` and the `--reverse` flag no longer exist, so the
// round-trip and reverse-error tests were deleted with them. The forward
// migration (`migrate-config --routing`) is retained: it's what Phase 2's
// Raw-rejection error points users to, and it now emits post-mapping routes
// with no `phase` field.

#[test]
fn migrated_routes_validate_without_errors() {
    // Fixture includes matching [[endpoints]] for the route source (from) and
    // target (to) so the lowered routes pass endpoint validation. (ADR-035:
    // authored I/O is `[[endpoints]]`; legacy [[bindings]]/[[connectors]] are
    // gone.)
    let toml = r#"
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
name = "Drums"
[[modes.mappings]]
trigger = { type = "Raw", device = "pads" }
action = { type = "MidiForward", target = "absynth" }
"#;
    let mut doc = parse(toml);
    migrate_raw_to_routes(&mut doc).expect("migration succeeds");

    let cfg: conductor_core::Config =
        toml::from_str(&doc.to_string()).expect("migrated TOML re-parses");

    let report = conductor_core::config::validation::validate_config(&cfg);
    let route_errors: Vec<_> = report
        .errors
        .iter()
        .filter(|e| e.path.starts_with("routes"))
        .collect();
    assert!(
        route_errors.is_empty(),
        "migrated routes must validate without errors, got: {route_errors:?}"
    );
}
