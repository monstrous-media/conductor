// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]

//! Integration tests for WASM resource limiting — POSITIVE cases.
//!
//! This suite validates that real plugins run successfully within adequate
//! limits. Note that every test skips when the corresponding plugin isn't
//! built, so these are within-limits smoke checks, not enforcement proofs.
//!
//! #1557: the ENFORCEMENT (negative) cases — over-limit memory growth and table
//! growth are DENIED, and a runaway loop TRAPS once its fuel budget is spent —
//! live as deterministic unit tests in `conductor_core::plugin::wasm_runtime`
//! (`resource_limiter_denies_memory_growth_beyond_limit`,
//! `resource_limiter_denies_table_growth_beyond_cap`,
//! `fuel_metering_traps_runaway_loop`). They exercise the private
//! `PluginResourceLimiter` directly and use an inline WAT module, so they need
//! no built fixtures and always run in CI under `--features plugin-wasm` —
//! unlike the plugin-dependent positive tests below.

#![cfg(all(test, feature = "plugin-wasm"))]

use conductor_core::plugin::{
    TriggerContext,
    wasm_runtime::{WasmConfig, WasmPlugin},
};
use std::path::PathBuf;
use std::time::Duration;

/// Get path to a test plugin's built WASM artifact.
///
/// The WASM plugin crates live under `plugins/wasm-<name>/` (e.g.
/// `plugins/wasm-spotify`), each building `conductor_wasm_<name>.wasm` into its
/// own `target/wasm32-wasip1/release/` (they're excluded from the workspace).
/// #1558: the directory was previously `plugins/<name>/`, which never exists —
/// so these tests skipped unconditionally even when the plugins were built.
fn get_plugin_path(plugin_name: &str) -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .unwrap()
        .join("plugins")
        .join(format!("wasm-{plugin_name}"))
        .join("target")
        .join("wasm32-wasip1")
        .join("release")
        .join(format!(
            "conductor_wasm_{}.wasm",
            plugin_name.replace("-", "_")
        ))
}

/// Plugins whose built WASM artifacts the plugin-dependent tests below require.
const REQUIRED_PLUGIN_FIXTURES: &[&str] = &["spotify", "obs-control", "system-utils"];

/// #1558: whether a missing WASM fixture must be a HARD ERROR rather than a
/// silent skip. Set `CONDUCTOR_REQUIRE_WASM_FIXTURES=1` in any environment that
/// prebuilds the plugin `.wasm` artifacts (e.g. a dedicated CI job) so these
/// plugin-dependent tests actually run there — without it, a missing artifact
/// silently no-ops and `cargo test --workspace` reports success while exercising
/// none of these behavioural checks.
fn require_fixtures() -> bool {
    matches!(
        std::env::var("CONDUCTOR_REQUIRE_WASM_FIXTURES").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Resolve a plugin's WASM artifact, enforcing presence per [`require_fixtures`].
///
/// Returns `Some(path)` when the artifact exists. When it's absent: returns
/// `None` (an explicit skip) if fixtures aren't required, or PANICS with an
/// actionable message if `CONDUCTOR_REQUIRE_WASM_FIXTURES` is set — so a job that
/// was supposed to prepare fixtures but didn't fails loudly instead of passing
/// as a no-op.
fn fixture_or_skip(plugin_name: &str) -> Option<PathBuf> {
    let path = get_plugin_path(plugin_name);
    if path.exists() {
        return Some(path);
    }
    assert!(
        !require_fixtures(),
        "Required WASM fixture for plugin '{plugin_name}' is missing at {}.\n\
         CONDUCTOR_REQUIRE_WASM_FIXTURES is set, so this is a hard failure — the \
         resource-limiting suite must not silently no-op. Build the plugin's WASM \
         artifact (cd plugins/wasm-{plugin_name} && cargo build --target \
         wasm32-wasip1 --release) before running, or unset the variable to allow \
         skipping locally.",
        path.display(),
    );
    eprintln!(
        "Skipping: '{plugin_name}' plugin WASM not built — set \
         CONDUCTOR_REQUIRE_WASM_FIXTURES=1 to make this a hard failure instead."
    );
    None
}

/// #1558 CI-facing setup assertion: when fixtures are REQUIRED, every plugin
/// artifact the suite depends on must be present. A CI job that prebuilds the
/// WASM plugins and sets `CONDUCTOR_REQUIRE_WASM_FIXTURES=1` fails HERE — loudly
/// and in one place — if fixture preparation was skipped or failed, rather than
/// letting each plugin-dependent test quietly no-op. No-op when not enforced.
#[test]
fn required_wasm_fixtures_present_when_enforced() {
    if !require_fixtures() {
        eprintln!(
            "CONDUCTOR_REQUIRE_WASM_FIXTURES not set — WASM fixture presence is not \
             enforced; plugin-dependent resource-limiting tests may skip."
        );
        return;
    }
    let missing: Vec<_> = REQUIRED_PLUGIN_FIXTURES
        .iter()
        .filter(|p| !get_plugin_path(p).exists())
        .collect();
    assert!(
        missing.is_empty(),
        "CONDUCTOR_REQUIRE_WASM_FIXTURES is set but these plugin WASM fixtures are \
         missing: {missing:?}. Build them before running the resource-limiting \
         suite (cargo build --target wasm32-wasip1 --release in each \
         plugins/wasm-<name>)."
    );
}

#[tokio::test]
async fn test_default_resource_limits() {
    let Some(wasm_path) = fixture_or_skip("spotify") else {
        return;
    };

    // Use default limits
    let config = WasmConfig::new("test-plugin").expect("safe id");
    assert_eq!(config.max_memory_bytes, 128 * 1024 * 1024); // 128 MB
    assert_eq!(config.max_fuel, 100_000_000); // 100M instructions
    assert_eq!(config.max_execution_time, Duration::from_secs(5));

    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin with default limits");

    plugin.init().await.expect("Failed to initialize plugin");

    // Execute action - should succeed within limits
    let context = TriggerContext::default();
    let result = plugin.execute("play", &[], &context).await;
    assert!(
        result.is_ok(),
        "Action should succeed within default limits"
    );
}

#[tokio::test]
async fn test_custom_fuel_limit() {
    let Some(wasm_path) = fixture_or_skip("spotify") else {
        return;
    };

    // Set very high fuel limit
    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.max_fuel = 500_000_000; // 500M instructions

    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin with custom fuel limit");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::default();
    let result = plugin.execute("play", &[], &context).await;
    assert!(result.is_ok(), "Action should succeed with high fuel limit");
}

#[tokio::test]
async fn test_custom_memory_limit() {
    let Some(wasm_path) = fixture_or_skip("spotify") else {
        return;
    };

    // Set higher memory limit
    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.max_memory_bytes = 256 * 1024 * 1024; // 256 MB

    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin with custom memory limit");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::default();
    let result = plugin.execute("play", &[], &context).await;
    assert!(
        result.is_ok(),
        "Action should succeed with higher memory limit"
    );
}

#[tokio::test]
async fn test_obs_plugin_within_limits() {
    let Some(wasm_path) = fixture_or_skip("obs-control") else {
        return;
    };

    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load OBS plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    // OBS plugin should execute within default limits
    let context = TriggerContext::default();
    let result = plugin.execute("start_recording", &[], &context).await;
    assert!(
        result.is_ok(),
        "OBS plugin should work within default limits"
    );
}

#[tokio::test]
async fn test_multiple_executions_with_fuel_reset() {
    let Some(wasm_path) = fixture_or_skip("spotify") else {
        return;
    };

    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::default();

    // Execute multiple times - fuel should reset between executions
    for i in 0..5 {
        let result = plugin.execute("play", &[], &context).await;
        assert!(
            result.is_ok(),
            "Execution {} should succeed (fuel resets)",
            i + 1
        );
    }
}

#[tokio::test]
async fn test_low_fuel_limit_still_works() {
    let Some(wasm_path) = fixture_or_skip("spotify") else {
        return;
    };

    // Set lower but still adequate fuel limit
    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.max_fuel = 10_000_000; // 10M instructions (lower but should work)

    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin with low fuel limit");

    plugin.init().await.expect("Failed to initialize plugin");

    // Simple actions should still work with lower limit
    let context = TriggerContext::default();
    let result = plugin.execute("play", &[], &context).await;
    assert!(result.is_ok(), "Simple action should work with 10M fuel");
}

#[tokio::test]
async fn test_system_utils_plugin_limits() {
    let Some(wasm_path) = fixture_or_skip("system-utils") else {
        return;
    };

    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load system utils plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    // System utils plugin should work within limits
    let context = TriggerContext::default();
    let result = plugin.execute("mute_system", &[], &context).await;
    assert!(
        result.is_ok(),
        "System utils plugin should work within limits"
    );
}

#[tokio::test]
async fn test_config_limits_preserved_across_store_creations() {
    let Some(wasm_path) = fixture_or_skip("spotify") else {
        return;
    };

    // Create custom config
    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.max_fuel = 200_000_000; // 200M instructions
    config.max_memory_bytes = 256 * 1024 * 1024; // 256 MB

    let mut plugin = WasmPlugin::load(&wasm_path, config.clone())
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    // Execute multiple times - each execution creates new store with same limits
    let context = TriggerContext::default();
    for _ in 0..3 {
        let result = plugin.execute("play", &[], &context).await;
        assert!(
            result.is_ok(),
            "Limits should be preserved across store creations"
        );
    }
}
