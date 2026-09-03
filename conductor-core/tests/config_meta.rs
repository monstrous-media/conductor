// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

// ADR-034 §D7/§D9 — `[config]` metadata block tests.
//
// Covers the `source` (managed/file) and `user_file_policy`
// (notify/ignore) keys that drive the ConfigWatcher demotion (§D9), and
// — critically — that a DEFAULT `[config]` block is omitted from the
// canonical form so existing configs keep byte-identical
// `ConfigRevision`s.

use conductor_core::Config;
use conductor_core::config::{ConfigMeta, ConfigRevision, ConfigSource, UserFilePolicy, canonical};

const BASE: &str = r#"
[[modes]]
name = "Default"
color = "blue"

[[modes.mappings]]
trigger = { type = "Note", note = 36 }
action = { type = "Keystroke", keys = "space", modifiers = ["cmd"] }
"#;

fn parse(s: &str) -> Config {
    toml::from_str(s).expect("fixture parses")
}

fn revision(c: &Config) -> ConfigRevision {
    ConfigRevision::from_canonical_bytes(&canonical::serialise(c).expect("canonical"))
}

#[test]
fn default_config_meta_is_managed_notify() {
    // A config with no `[config]` block defaults to the ADR-034
    // production posture: daemon-managed source, notify-only watcher.
    let c = parse(BASE);
    assert_eq!(c.config_meta.source, ConfigSource::Managed);
    assert_eq!(c.config_meta.user_file_policy, UserFilePolicy::Notify);
    assert_eq!(c.config_meta, ConfigMeta::default());
}

#[test]
fn parses_user_file_policy_ignore() {
    let c = parse(&format!(
        "{BASE}\n[config]\nuser_file_policy = \"ignore\"\n"
    ));
    assert_eq!(c.config_meta.user_file_policy, UserFilePolicy::Ignore);
    // source still defaults
    assert_eq!(c.config_meta.source, ConfigSource::Managed);
}

#[test]
fn parses_config_source_file_legacy() {
    let c = parse(&format!("{BASE}\n[config]\nsource = \"file\"\n"));
    assert_eq!(c.config_meta.source, ConfigSource::File);
    assert_eq!(c.config_meta.user_file_policy, UserFilePolicy::Notify);
}

#[test]
fn unknown_config_key_is_ignored() {
    // `schema_version` (ADR-034 §D7) is deferred — a config carrying it
    // must still parse (no `deny_unknown_fields`), not error.
    let c = parse(&format!(
        "{BASE}\n[config]\nuser_file_policy = \"notify\"\nschema_version = 2\n"
    ));
    assert_eq!(c.config_meta.user_file_policy, UserFilePolicy::Notify);
}

#[test]
fn default_config_meta_is_omitted_from_canonical_form() {
    // Hash stability: a default `[config]` block must not change the
    // canonical bytes — otherwise every pre-ADR-034 config's revision
    // shifts. The skip-if-default keeps the golden hash intact.
    let without = parse(BASE);
    let bytes = canonical::serialise(&without).expect("canonical");
    let text = String::from_utf8(bytes).expect("utf8");
    assert!(
        !text.contains("[config]") && !text.contains("user_file_policy"),
        "default config_meta leaked into canonical form:\n{text}"
    );

    // Explicitly writing the default block yields the SAME revision as
    // omitting it entirely.
    let with_default = parse(&format!(
        "{BASE}\n[config]\nsource = \"managed\"\nuser_file_policy = \"notify\"\n"
    ));
    assert_eq!(revision(&without), revision(&with_default));
}

#[test]
fn non_default_policy_appears_in_canonical_form() {
    // A non-default policy IS material: it serialises, so it changes the
    // revision (operators editing it get a distinct config identity).
    let ignore = parse(&format!(
        "{BASE}\n[config]\nuser_file_policy = \"ignore\"\n"
    ));
    let bytes = canonical::serialise(&ignore).expect("canonical");
    let text = String::from_utf8(bytes).expect("utf8");
    assert!(text.contains("user_file_policy"), "policy missing:\n{text}");
    assert!(text.contains("ignore"), "ignore value missing:\n{text}");

    assert_ne!(revision(&parse(BASE)), revision(&ignore));
}
