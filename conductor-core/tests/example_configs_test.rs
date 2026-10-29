// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Every shipped example config (`config/examples/**/*.toml`) must load, validate, and
//! keep every key it sets.
//!
//! The use-case templates under `config/examples/use-cases/` are published for people to
//! download from the website, so a schema change that breaks one should fail here, not on
//! a user's machine. `Config` has no `deny_unknown_fields`, so a misspelt or renamed key
//! would be dropped silently: the round-trip check below catches that.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use conductor_core::Config;
use conductor_core::actions::parse_keys;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn toml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read config/examples") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            toml_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "toml") {
            out.push(path);
        }
    }
}

/// Dotted key paths for every leaf; array elements collapse to `[]`.
fn leaf_paths(v: &toml::Value, prefix: &str, out: &mut BTreeSet<String>) {
    match v {
        toml::Value::Table(t) => {
            for (k, v) in t {
                leaf_paths(v, &format!("{prefix}.{k}"), out);
            }
        }
        toml::Value::Array(a) => {
            for v in a {
                leaf_paths(v, &format!("{prefix}[]"), out);
            }
        }
        _ => {
            out.insert(prefix.to_string());
        }
    }
}

/// Every `type = "Keystroke"` table: each `+`-separated key must be one `parse_keys` knows.
fn check_keystrokes(v: &toml::Value, path: &str, errs: &mut Vec<String>) {
    match v {
        toml::Value::Table(t) => {
            let is_keystroke = t.get("type").and_then(|v| v.as_str()) == Some("Keystroke");
            let bad_keys = t
                .get("keys")
                .and_then(|v| v.as_str())
                .filter(|k| is_keystroke && parse_keys(k).len() != k.split('+').count());
            if let Some(keys) = bad_keys {
                errs.push(format!("{path}: unrecognised key in {keys:?}"));
            }
            for (k, v) in t {
                check_keystrokes(v, &format!("{path}.{k}"), errs);
            }
        }
        toml::Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                check_keystrokes(v, &format!("{path}[{i}]"), errs);
            }
        }
        _ => {}
    }
}

#[test]
fn example_configs_load_validate_and_round_trip() {
    // `Config::load` only accepts paths under the config dir, the current dir or /tmp, so run
    // from the workspace root and pass repo-relative paths (this file holds a single test, so
    // changing the process cwd can't race another test).
    std::env::set_current_dir(repo_root()).expect("cd to workspace root");
    let mut files = Vec::new();
    toml_files(Path::new("config/examples"), &mut files);
    files.sort();
    assert!(
        files
            .iter()
            .any(|p| p.to_string_lossy().contains("use-cases")),
        "expected the use-case templates under config/examples/use-cases"
    );

    let mut failures = Vec::new();
    for path in &files {
        let name = path.display().to_string();
        let config = match Config::load(path.to_str().expect("utf-8 path")) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{name}: load failed: {e}"));
                continue;
            }
        };
        if let Err(e) = config.validate() {
            failures.push(format!("{name}: invalid: {e}"));
            continue;
        }

        let source: toml::Value =
            toml::from_str(&std::fs::read_to_string(path).expect("read")).expect("toml");
        let round_trip: toml::Value =
            toml::Value::try_from(&config).expect("serialise Config back to TOML");
        let (mut want, mut got) = (BTreeSet::new(), BTreeSet::new());
        leaf_paths(&source, "", &mut want);
        leaf_paths(&round_trip, "", &mut got);
        for dropped in want.difference(&got) {
            failures.push(format!("{name}: key ignored by the parser: {dropped}"));
        }

        check_keystrokes(&source, &name, &mut failures);
    }

    assert!(
        failures.is_empty(),
        "example config problems:\n{}",
        failures.join("\n")
    );
}
