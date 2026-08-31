// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]

//! Integration tests for the OBS control WASM plugin
//!
//! This test suite verifies that the OBS control plugin:
//! 1. Loads correctly and exposes required exports
//! 2. Returns valid metadata
//! 3. Executes all 7 actions successfully
//! 4. Handles parametrized actions correctly
//! 5. Validates parameter requirements

#![cfg(all(test, feature = "plugin-wasm"))]

use conductor_core::plugin::{
    TriggerContext,
    wasm_runtime::{WasmConfig, WasmPlugin},
};
use std::path::PathBuf;

/// Get path to the compiled OBS plugin WASM binary
fn get_obs_plugin_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .unwrap()
        .join("plugins")
        .join("wasm-obs-control")
        .join("target")
        .join("wasm32-wasip1")
        .join("release")
        .join("conductor_wasm_obs_control.wasm")
}

#[tokio::test]
#[ignore] // Requires pre-built OBS WASM plugin binary — not available in CI
async fn test_obs_plugin_loads() {
    let wasm_path = get_obs_plugin_path();
    assert!(
        wasm_path.exists(),
        "OBS plugin WASM binary not found. Run: cd plugins/wasm-obs-control && ./build.sh"
    );

    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load OBS plugin");

    let metadata = plugin.init().await.expect("Failed to initialize plugin");
    assert_eq!(metadata.name, "obs_control");
    assert_eq!(metadata.version, "0.1.0");
}

#[tokio::test]
#[ignore] // Requires pre-built OBS WASM plugin binary — not available in CI
async fn test_obs_metadata() {
    let wasm_path = get_obs_plugin_path();
    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    let metadata = plugin.init().await.expect("Failed to initialize plugin");
    assert_eq!(metadata.name, "obs_control");
    assert_eq!(
        metadata.description,
        "OBS Studio control via WebSocket protocol"
    );
    assert_eq!(metadata.author, "Monstrous Team");
    assert_eq!(metadata.license, "MIT");

    // Verify capabilities (Network capability required for OBS WebSocket)
    assert!(
        !metadata.capabilities.is_empty(),
        "OBS plugin should request Network capability"
    );
}

#[tokio::test]
#[ignore] // Requires pre-built OBS WASM plugin binary — not available in CI
async fn test_obs_start_stop_recording() {
    let wasm_path = get_obs_plugin_path();
    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::default();

    // Test start recording
    let result = plugin.execute("start_recording", &[], &context).await;
    assert!(result.is_ok(), "start_recording should succeed");

    // Test stop recording
    let result = plugin.execute("stop_recording", &[], &context).await;
    assert!(result.is_ok(), "stop_recording should succeed");
}

#[tokio::test]
#[ignore] // Requires pre-built OBS WASM plugin binary — not available in CI
async fn test_obs_start_stop_streaming() {
    let wasm_path = get_obs_plugin_path();
    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::default();

    // Test start streaming
    let result = plugin.execute("start_streaming", &[], &context).await;
    assert!(result.is_ok(), "start_streaming should succeed");

    // Test stop streaming
    let result = plugin.execute("stop_streaming", &[], &context).await;
    assert!(result.is_ok(), "stop_streaming should succeed");
}

#[tokio::test]
#[ignore] // Requires pre-built OBS WASM plugin binary — not available in CI
async fn test_obs_all_actions() {
    let wasm_path = get_obs_plugin_path();
    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::with_velocity(100);

    let actions = vec![
        "start_recording",
        "stop_recording",
        "start_streaming",
        "stop_streaming",
    ];

    for action in actions {
        let result = plugin.execute(action, &[], &context).await;
        assert!(result.is_ok(), "Action '{}' failed", action);
    }
}

#[tokio::test]
#[ignore] // Requires pre-built OBS WASM plugin binary — not available in CI
async fn test_obs_switch_scene() {
    let wasm_path = get_obs_plugin_path();
    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::default();

    // #1446: with the scene_name parameter supplied, switch_scene succeeds...
    let with_param = plugin
        .execute("switch_scene", &["Scene 1".to_string()], &context)
        .await;
    assert!(
        with_param.is_ok(),
        "switch_scene should succeed when the scene_name parameter is passed (#1446): {with_param:?}"
    );

    // ...and still fails when the required parameter is omitted.
    let without_param = plugin.execute("switch_scene", &[], &context).await;
    assert!(
        without_param.is_err(),
        "switch_scene should fail without the scene_name parameter"
    );
}

#[tokio::test]
#[ignore] // Requires pre-built OBS WASM plugin binary — not available in CI
async fn test_obs_toggle_mute() {
    let wasm_path = get_obs_plugin_path();
    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::default();

    // #1446: with the source_name parameter supplied, toggle_mute succeeds...
    let with_param = plugin
        .execute("toggle_mute", &["Mic/Aux".to_string()], &context)
        .await;
    assert!(
        with_param.is_ok(),
        "toggle_mute should succeed when the source_name parameter is passed (#1446): {with_param:?}"
    );

    // ...and still fails when the required parameter is omitted.
    let without_param = plugin.execute("toggle_mute", &[], &context).await;
    assert!(
        without_param.is_err(),
        "toggle_mute should fail without the source_name parameter"
    );
}

#[tokio::test]
#[ignore] // Requires pre-built OBS WASM plugin binary — not available in CI
async fn test_obs_unknown_action() {
    let wasm_path = get_obs_plugin_path();
    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::default();

    // Test unknown action
    let result = plugin.execute("invalid_action_xyz", &[], &context).await;
    assert!(result.is_err(), "Unknown actions should fail");
}

#[tokio::test]
#[ignore] // Requires pre-built OBS WASM plugin binary — not available in CI
async fn test_obs_binary_size() {
    let wasm_path = get_obs_plugin_path();
    let metadata = std::fs::metadata(&wasm_path).expect("Failed to read plugin file metadata");

    let size_kb = metadata.len() / 1024;

    // OBS plugin should be similar to Spotify plugin (~50-70KB)
    assert!(
        (50..=100).contains(&size_kb),
        "OBS plugin size ({} KB) outside expected range (50-100 KB)",
        size_kb
    );

    println!("OBS plugin size: {} KB", size_kb);
}
