// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Guard that every example config shipped in the repo loads cleanly.
//!
//! Restores the #2066 guard for `config/test-plugins.toml`, which was lost
//! when the GUI (and its unit test) moved out of this workspace (ADR-046),
//! and extends it to `config/examples/*.toml`. Each file is copied into a
//! tempdir before loading because `Config::load` only accepts paths under
//! the user config dir, the temp dir, or the current working directory —
//! and loading in place would skip that boundary.

use std::path::PathBuf;

use conductor_core::config::Config;

/// Workspace-root `config/` directory (repo checkout only; these files are
/// deliberately not packaged into the published crate).
fn repo_config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("conductor-core has a workspace parent")
        .join("config")
}

/// All example configs the repo promises will parse: `config/test-plugins.toml`
/// plus every TOML under `config/examples/`.
fn example_config_paths() -> Vec<PathBuf> {
    let config_dir = repo_config_dir();
    let mut paths = vec![config_dir.join("test-plugins.toml")];
    let examples = config_dir.join("examples");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&examples)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", examples.display()))
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    entries.sort();
    paths.extend(entries);
    paths
}

#[test]
fn every_example_config_loads() {
    let paths = example_config_paths();
    // Guard the guard: if the files move again, fail loudly instead of
    // silently testing nothing.
    assert!(
        paths.len() >= 4,
        "expected test-plugins.toml plus at least 3 configs in config/examples/, found {}: {paths:?}",
        paths.len()
    );

    let tempdir = tempfile::tempdir().expect("create tempdir");
    let mut failures = Vec::new();
    for source in &paths {
        assert!(
            source.exists(),
            "missing example config: {}",
            source.display()
        );
        let staged = tempdir.path().join(source.file_name().expect("file name"));
        std::fs::copy(source, &staged)
            .unwrap_or_else(|e| panic!("copy {} to tempdir: {e}", source.display()));
        if let Err(e) = Config::load(staged.to_str().expect("utf-8 temp path")) {
            failures.push(format!("{}: {e}", source.display()));
        }
    }

    assert!(
        failures.is_empty(),
        "example config(s) failed Config::load:\n{}",
        failures.join("\n")
    );
}
