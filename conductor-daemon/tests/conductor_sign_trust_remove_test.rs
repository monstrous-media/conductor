// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! #1315: `conductor-sign trust remove <key>` corrupts the
//! metadata of all retained `TrustedKey` entries.
//!
//! Bug shape: `trust_remove` calls `load_trusted_keys()` which only
//! reads the public-key strings, retains the ones to keep, then
//! calls `save_trusted_keys(&[String])` which reconstructs
//! `TrustedKey` records with `name=""`, `email=""`, and
//! `added_at=now()`. So removing ONE key wipes the name/email/
//! added_at of EVERY retained key — silent metadata corruption.
//!
//! Fix: full-record load/save (`load_trusted_keys_full` /
//! `save_trusted_keys_full`) used by `trust_remove`, so metadata
//! survives mutation.
//!
//! Black-box subprocess test isolates HOME/XDG_CONFIG_HOME to a
//! tempdir, drives the binary through the `trust add` / `trust
//! remove` flow, then inspects the on-disk `trusted_keys.toml` to
//! verify retained-key metadata survived.

#![cfg(all(unix, feature = "plugin-signing"))]

use std::path::PathBuf;
use std::process::Command;

fn conductor_sign_path() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_conductor-sign") {
        return PathBuf::from(p);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("workspace root")
        .join("target")
        .join("debug")
        .join("conductor-sign")
}

#[derive(serde::Deserialize)]
struct TrustedKeysFile {
    keys: Vec<TrustedKey>,
}

#[derive(serde::Deserialize, Debug, Clone, PartialEq)]
struct TrustedKey {
    name: String,
    email: String,
    public_key: String,
    added_at: String,
}

/// Resolve the on-disk trusted_keys.toml path under our tempdir.
/// macOS: `dirs::config_dir()` returns `~/Library/Application Support`
/// and does NOT honour `XDG_CONFIG_HOME`. We try both candidates.
fn locate_trusted_keys(tempdir: &std::path::Path) -> PathBuf {
    let xdg = tempdir
        .join("config")
        .join("conductor")
        .join("trusted_keys.toml");
    if xdg.exists() {
        return xdg;
    }
    let macos = tempdir
        .join("Library")
        .join("Application Support")
        .join("conductor")
        .join("trusted_keys.toml");
    if macos.exists() {
        return macos;
    }
    panic!(
        "trusted_keys.toml not found at either:\n  {}\n  {}",
        xdg.display(),
        macos.display()
    );
}

fn parse(toml_text: &str) -> TrustedKeysFile {
    toml::from_str(toml_text)
        .unwrap_or_else(|e| panic!("invalid trusted_keys.toml: {e}\n{toml_text}"))
}

/// THE regression test for #1315. Pre-write a `trusted_keys.toml`
/// with TWO entries that have rich, distinct metadata (name AND
/// email AND a fixed added_at timestamp). Run `trust remove` on
/// one entry. Assert the retained entry's ALL THREE metadata
/// fields survive — pre-fix the bug wiped all of them.
///
/// Pre-writing the TOML (rather than going through `trust add`)
/// is the load-bearing change: `trust add` only takes a name
/// (email is always empty going through CLI), so it can't
/// exercise the email-preservation case. Council R2 surfaced this.
#[test]
fn trust_remove_preserves_retained_key_metadata() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let bin = conductor_sign_path();

    let key_alice = "a".repeat(64);
    let key_bob = "b".repeat(64);
    let alice_added_at = "2025-01-15T10:30:00Z".to_string();
    let bob_added_at = "2025-02-20T14:45:00Z".to_string();

    // Pre-write the TOML directly at the path the CLI will use.
    // On Linux `dirs::config_dir()` honours XDG_CONFIG_HOME; on
    // macOS it returns `$HOME/Library/Application Support` and
    // ignores XDG_CONFIG_HOME. Seeding both paths breaks the test
    // (the CLI only updates one) so we pick the right one
    // per-platform.
    let initial_toml = format!(
        r#"[[keys]]
name = "Alice Developer"
email = "alice@example.com"
public_key = "{key_alice}"
added_at = "{alice_added_at}"

[[keys]]
name = "Bob Maintainer"
email = "bob@example.com"
public_key = "{key_bob}"
added_at = "{bob_added_at}"
"#
    );
    let cli_config_path = if cfg!(target_os = "macos") {
        tempdir
            .path()
            .join("Library")
            .join("Application Support")
            .join("conductor")
    } else {
        tempdir.path().join("config").join("conductor")
    };
    std::fs::create_dir_all(&cli_config_path).expect("create config dir");
    std::fs::write(cli_config_path.join("trusted_keys.toml"), &initial_toml).expect("seed");

    // Sanity: re-read the seed from disk (not the in-memory
    // literal — Council R3: validate what's actually on disk in
    // case the path/write went wrong silently) and confirm both
    // records present with full metadata.
    let seed_on_disk = std::fs::read_to_string(cli_config_path.join("trusted_keys.toml"))
        .expect("re-read seed from disk");
    let before = parse(&seed_on_disk);
    assert_eq!(
        before.keys.len(),
        2,
        "seed should have 2 entries on disk; got:\n{seed_on_disk}"
    );
    let alice_before = before
        .keys
        .iter()
        .find(|k| k.public_key == key_alice)
        .expect("Alice in seed");
    assert_eq!(alice_before.name, "Alice Developer");
    assert_eq!(alice_before.email, "alice@example.com");
    assert_eq!(alice_before.added_at, alice_added_at);

    // Now remove Bob via the CLI.
    let out = Command::new(&bin)
        .args(["trust", "remove", &key_bob])
        .env("HOME", tempdir.path())
        .env("XDG_CONFIG_HOME", tempdir.path().join("config"))
        .output()
        .expect("trust remove Bob");
    assert!(
        out.status.success(),
        "trust remove Bob must succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Parse the post-removal TOML from whichever path the CLI
    // wrote to.
    let after_path = locate_trusted_keys(tempdir.path());
    let after_text = std::fs::read_to_string(&after_path).expect("read after");
    let after = parse(&after_text);

    // Bob is gone, Alice survives.
    assert_eq!(
        after.keys.len(),
        1,
        "exactly one key should remain post-removal; got:\n{after_text}"
    );
    // Council R3: don't index — look up by public_key. Index 0 would
    // accidentally pass if ordering changed and we kept Bob (we wouldn't
    // be checking it's actually Alice). find() makes the check
    // identity-based.
    let alice_after = after
        .keys
        .iter()
        .find(|k| k.public_key == key_alice)
        .unwrap_or_else(|| {
            panic!("Alice's public key must survive trust remove Bob; got:\n{after_text}")
        });

    // THE security/data-loss assertions — all THREE metadata
    // fields MUST survive. Pre-fix, save_trusted_keys reconstructed
    // every record with empty name + empty email + fresh added_at.
    assert_eq!(
        alice_after.name, "Alice Developer",
        "Alice's NAME must survive trust remove Bob (#1315)"
    );
    assert_eq!(
        alice_after.email, "alice@example.com",
        "Alice's EMAIL must survive trust remove Bob (#1315 — \
         Council R2 specifically called out this field as the \
         most likely silent failure point in the bug)"
    );
    assert_eq!(
        alice_after.added_at, alice_added_at,
        "Alice's added_at must survive trust remove Bob (#1315 \
         regenerated it to now())"
    );
}
