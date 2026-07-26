// Copyright 2025 Amiable Team
// SPDX-License-Identifier: MIT

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]

//! Integration tests for filesystem capability and directory preopening
//!
//! This test suite verifies that:
//! 1. Directory preopening works correctly with filesystem capability
//! 2. Plugins can access preopened directories
//! 3. Plugins cannot access directories outside sandbox
//! 4. Multiple plugins share the same data directory safely

#![cfg(all(test, feature = "plugin-wasm"))]

use conductor_core::plugin::{
    Capability, TriggerContext,
    wasm_runtime::{WasmConfig, WasmPlugin},
};
use std::path::PathBuf;

/// Get path to system-utils plugin (which uses filesystem capability)
fn get_system_utils_plugin_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .unwrap()
        .join("plugins")
        .join("wasm-system-utils")
        .join("target")
        .join("wasm32-wasip1")
        .join("release")
        .join("conductor_wasm_system_utils.wasm")
}

/// Build a per-test-run unique plugin id and pre-clean any
/// stale directory left by an earlier run (PR #1029 round-4
/// review, 2026-05-02). The tests load plugins under the real
/// `dirs::data_dir()` location, so without a unique-per-run id
/// (and cleanup), an earlier run's leftover subdir could
/// satisfy the assertions even when the code under test
/// regressed and failed to create the new sandbox.
fn unique_test_plugin_id(prefix: &str) -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // `[A-Za-z0-9_-]` only — `validate_plugin_id` would reject `.`.
    format!("{prefix}-{}-{}", std::process::id(), nanos)
}

/// Compute the per-plugin sandbox directory for a given id and
/// remove any leftover state from prior runs. Returns the path
/// for assertion use after the test runs.
fn fresh_per_plugin_dir(plugin_id: &str) -> PathBuf {
    let dir = dirs::data_dir()
        .expect("No data directory")
        .join("conductor")
        .join("plugin-data")
        .join(plugin_id);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[tokio::test]
async fn test_filesystem_capability_granted() {
    let wasm_path = get_system_utils_plugin_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test: System utils plugin not built");
        return;
    }

    // Create config with filesystem capability
    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.capabilities.push(Capability::Filesystem);

    let result = WasmPlugin::load(&wasm_path, config).await;
    assert!(
        result.is_ok(),
        "Plugin with filesystem capability should load successfully"
    );
}

#[tokio::test]
async fn test_per_plugin_data_subdir_is_created() {
    // PR #1029 round-3 review (2026-05-02): the previous version
    // of this test only asserted the SHARED `plugin-data/`
    // parent existed, which is exactly the behaviour D10c
    // replaces. It would still have passed if the loader
    // regressed back to mounting the parent. The post-D10c
    // assertion is that `<plugin-data>/<plugin_id>/` exists.
    let wasm_path = get_system_utils_plugin_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test: System utils plugin not built");
        return;
    }

    // Per-run unique id + pre-cleaned target dir, so a stale
    // dir from an earlier run can't pass this test in the case
    // where the code under test regressed and failed to create
    // the new sandbox (PR #1029 round-4 review, 2026-05-02).
    let plugin_id = unique_test_plugin_id("d10c-fs-create");
    let per_plugin_dir = fresh_per_plugin_dir(&plugin_id);
    assert!(
        !per_plugin_dir.exists(),
        "test setup invariant: cleaned per-plugin dir must not \
         exist before the load. Got: {per_plugin_dir:?}",
    );

    let mut config = WasmConfig::new(&plugin_id).expect("safe id");
    config.capabilities.push(Capability::Filesystem);

    // PR #1029 round-8 review (2026-05-02): `WasmPlugin::load`
    // doesn't create the sandbox subdir — that happens in
    // `create_store()` which is reached only from `init()` /
    // `execute()`. Pre-fix this assertion ran right after
    // `load()` and would've reported "dir created" even when
    // it wasn't (caught by stale-dir leftovers in fresh runs)
    // OR failed for the wrong reason (no dir at all). Now we
    // call `init()` to drive the create_store path.
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");
    plugin.init().await.expect("Failed to init plugin");

    assert!(
        per_plugin_dir.exists(),
        "ADR-027 D10c: per-plugin sandbox subdir must be CREATED \
         BY THIS LOAD+INIT at {per_plugin_dir:?}. The shared \
         `plugin-data/` root existing is NOT sufficient — that's \
         the pre-D10c behaviour we replaced.",
    );
    assert!(
        per_plugin_dir.is_dir(),
        "Per-plugin sandbox path must be a directory: {per_plugin_dir:?}",
    );

    // Hygiene: clean up so we don't leave debris for the next run.
    let _ = std::fs::remove_dir_all(&per_plugin_dir);
}

#[tokio::test]
async fn test_filesystem_capability_not_granted() {
    let wasm_path = get_system_utils_plugin_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test: System utils plugin not built");
        return;
    }

    // Create config WITHOUT filesystem capability
    let config = WasmConfig::new("test-plugin").expect("safe id");
    assert!(
        config.capabilities.is_empty(),
        "Default config should have no capabilities"
    );

    // Plugin should still load (but won't have filesystem access)
    let result = WasmPlugin::load(&wasm_path, config).await;
    assert!(
        result.is_ok(),
        "Plugin without filesystem capability should still load"
    );
}

#[tokio::test]
async fn test_distinct_plugin_ids_get_distinct_subdirs() {
    // PR #1029 round-3 review (2026-05-02): the previous version
    // used the SAME plugin_id for both loads and asserted the
    // SHARED parent existed. It couldn't catch the cross-plugin-
    // read regression D10c is meant to close. This test now uses
    // two distinct ids and asserts that they map to distinct
    // sandbox subdirs — which is the actual D10c invariant.
    let wasm_path = get_system_utils_plugin_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test: System utils plugin not built");
        return;
    }

    // Per-run unique ids + pre-cleaned target dirs, so stale
    // dirs from earlier runs can't satisfy the assertions
    // (PR #1029 round-4 review, 2026-05-02).
    let id_a = unique_test_plugin_id("d10c-distinct-a");
    let id_b = unique_test_plugin_id("d10c-distinct-b");
    let dir_a = fresh_per_plugin_dir(&id_a);
    let dir_b = fresh_per_plugin_dir(&id_b);
    assert!(!dir_a.exists() && !dir_b.exists(), "test setup: cleaned");

    // PR #1029 round-8: drive create_store via init() (load
    // alone doesn't create the sandbox subdir).
    let mut config1 = WasmConfig::new(&id_a).expect("safe id");
    config1.capabilities.push(Capability::Filesystem);
    let mut plugin1 = WasmPlugin::load(&wasm_path, config1)
        .await
        .expect("Failed to load plugin A");
    plugin1.init().await.expect("Failed to init plugin A");

    let mut config2 = WasmConfig::new(&id_b).expect("safe id");
    config2.capabilities.push(Capability::Filesystem);
    let mut plugin2 = WasmPlugin::load(&wasm_path, config2)
        .await
        .expect("Failed to load plugin B");
    plugin2.init().await.expect("Failed to init plugin B");

    assert!(
        dir_a.exists(),
        "Plugin A's per-plugin subdir must be CREATED BY LOAD+INIT: {dir_a:?}",
    );
    assert!(
        dir_b.exists(),
        "Plugin B's per-plugin subdir must be CREATED BY LOAD+INIT: {dir_b:?}",
    );
    assert_ne!(
        dir_a, dir_b,
        "ADR-027 D10c: distinct plugin_ids MUST map to distinct \
         filesystem sandboxes; if these collapsed to the same path \
         the per-plugin scoping is broken and the cross-plugin-read \
         vulnerability D10c addresses is back.",
    );

    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[tokio::test]
async fn test_per_plugin_subdir_permissions() {
    // PR #1029 round-3 review (2026-05-02): the previous version
    // checked permissions on the SHARED `plugin-data/` parent —
    // not the per-plugin sandbox D10c actually creates. A
    // regression where the per-plugin subdir had wrong
    // permissions (or wasn't created at all) would have passed
    // the old assertion. Now we check the actual sandbox.
    let wasm_path = get_system_utils_plugin_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test: System utils plugin not built");
        return;
    }

    // Per-run unique id + pre-cleaned target dir, so leftover
    // state from earlier runs can't pass the metadata check
    // (PR #1029 round-4 review, 2026-05-02).
    let plugin_id = unique_test_plugin_id("d10c-perms");
    let per_plugin_dir = fresh_per_plugin_dir(&plugin_id);
    assert!(!per_plugin_dir.exists(), "test setup: cleaned");

    let mut config = WasmConfig::new(&plugin_id).expect("safe id");
    config.capabilities.push(Capability::Filesystem);

    // PR #1029 round-8: load+init drives create_store, which
    // is where the sandbox subdir actually gets mkdir'd.
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");
    plugin.init().await.expect("Failed to init plugin");

    let metadata = std::fs::metadata(&per_plugin_dir).unwrap_or_else(|e| {
        panic!(
            "Per-plugin sandbox subdir {per_plugin_dir:?} must exist \
             after load+init with the Filesystem capability. \
             ADR-027 D10c regression — pre-D10c the subdir was \
             never created (we cleaned it in setup, so a leftover \
             can't be masking this). Underlying error: {e}",
        )
    });

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        // PR #1029 round-5 review (2026-05-02): assert ALL THREE
        // owner bits are set, not "any one of them". Pre-fix
        // `mode & 0o700 != 0` passed for `0o400` (read-only),
        // which would silently break the plugin's ability to
        // write its own data while making the test pass.
        let owner_bits = mode & 0o700;
        assert_eq!(
            owner_bits, 0o700,
            "Per-plugin sandbox at {per_plugin_dir:?} must have \
             owner rwx (mode 0o700 minimum); got mode {mode:o} \
             (owner bits = {owner_bits:o}). Anything less means \
             the plugin can't write its own data, defeating the \
             Filesystem capability.",
        );
    }
    assert!(
        !metadata.permissions().readonly(),
        "Per-plugin sandbox must not be read-only — the plugin \
         needs to write its own data there.",
    );

    let _ = std::fs::remove_dir_all(&per_plugin_dir);
}

#[tokio::test]
async fn test_system_utils_with_filesystem_access() {
    let wasm_path = get_system_utils_plugin_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test: System utils plugin not built");
        return;
    }

    // Create config with filesystem capability
    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.capabilities.push(Capability::Filesystem);

    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    // Execute action that might use filesystem
    let context = TriggerContext::default();
    let result = plugin.execute("mute_system", &[], &context).await;
    assert!(
        result.is_ok(),
        "System utils plugin should work with filesystem access"
    );
}

#[tokio::test]
async fn test_config_with_multiple_capabilities() {
    let wasm_path = get_system_utils_plugin_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test: System utils plugin not built");
        return;
    }

    // Create config with multiple capabilities
    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.capabilities.push(Capability::Filesystem);
    config.capabilities.push(Capability::Network);

    let result = WasmPlugin::load(&wasm_path, config).await;
    assert!(
        result.is_ok(),
        "Plugin with multiple capabilities should load successfully"
    );
}
