// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]

//! Integration test for the Spotify WASM plugin.
//!
//! #1515: the Spotify `.wasm` artifact is produced by a *separate* release
//! build, not by this test crate:
//!
//! ```text
//! cd plugins/wasm-spotify && cargo build --target wasm32-wasip1 --release
//! ```
//!
//! These tests previously checked `if !path.exists() { return; }` and so
//! reported as PASSED on any run without that artifact — including
//! `cargo test --workspace --features plugin-wasm` — hiding regressions in
//! loading / metadata / execution behind a green suite.
//!
//! They are now `#[ignore]`d, so a default run shows them as *skipped* (not
//! passed). To actually run them, build the artifact (above), then:
//!
//! ```text
//! cargo test -p conductor-core --features plugin-wasm -- --ignored
//! ```
//!
//! If you pass `--ignored` WITHOUT building the artifact, `spotify_plugin_path()`
//! asserts its existence and the test fails LOUDLY with the build command —
//! it no longer silently passes.

#![cfg(all(test, feature = "plugin-wasm"))]

use conductor_core::plugin::{
    TriggerContext,
    wasm_runtime::{WasmConfig, WasmPlugin},
};
use std::path::PathBuf;

const BUILD_HINT: &str = "Spotify WASM artifact missing. Build it first: \
    cd plugins/wasm-spotify && cargo build --target wasm32-wasip1 --release";

/// Path to the built Spotify WASM plugin, asserting it exists so an
/// `--ignored` run without the artifact fails clearly (#1515).
fn spotify_plugin_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins/wasm-spotify/target/wasm32-wasip1/release/conductor_wasm_spotify.wasm");
    assert!(path.exists(), "{BUILD_HINT} (looked for {path:?})");
    path
}

#[tokio::test]
#[ignore = "requires the built Spotify WASM artifact; see module header (#1515)"]
async fn test_load_spotify_plugin() {
    let wasm_path = spotify_plugin_path();

    let config = WasmConfig::new("test-plugin").expect("safe id");
    let result = WasmPlugin::load(&wasm_path, config).await;
    assert!(
        result.is_ok(),
        "Failed to load Spotify plugin: {:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore = "requires the built Spotify WASM artifact; see module header (#1515)"]
async fn test_spotify_plugin_metadata() {
    let wasm_path = spotify_plugin_path();

    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    let metadata = plugin.init().await.expect("Failed to initialize plugin");

    assert_eq!(metadata.name, "spotify_control");
    assert_eq!(metadata.version, "0.2.0");
    assert_eq!(metadata.author, "Amiable Team");
    assert_eq!(metadata.license, "MIT");
    assert!(metadata.description.contains("Spotify"));

    // Verify capabilities (Network capability required for Spotify Web API)
    assert!(
        !metadata.capabilities.is_empty(),
        "Spotify plugin should request Network capability"
    );
}

#[tokio::test]
#[ignore = "requires the built Spotify WASM artifact; see module header (#1515)"]
async fn test_spotify_play_pause_action() {
    let wasm_path = spotify_plugin_path();

    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::new();
    let result = plugin.execute("play_pause", &[], &context).await;

    assert!(
        result.is_ok(),
        "play_pause action failed: {:?}",
        result.err()
    );
}

#[tokio::test]
#[ignore = "requires the built Spotify WASM artifact; see module header (#1515)"]
async fn test_spotify_all_actions() {
    let wasm_path = spotify_plugin_path();

    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::with_velocity(100);

    // The 9 no-argument actions. `set_volume` — the 10th supported action
    // (lib.rs `match action`) — is exercised separately below because it
    // requires a volume_percent parameter; it was previously omitted from
    // this "all actions" list, so a regression in it went uncovered (#1516).
    let no_arg_actions = vec![
        "play_pause",
        "play",
        "pause",
        "next_track",
        "previous_track",
        "volume_up",
        "volume_down",
        "shuffle",
        "repeat",
    ];

    for action in &no_arg_actions {
        let result = plugin.execute(action, &[], &context).await;
        assert!(
            result.is_ok(),
            "Action '{}' failed: {:?}",
            action,
            result.err()
        );
    }

    // 10th action: set_volume takes a volume_percent argument (0-100).
    let set_volume = plugin
        .execute("set_volume", &["50".to_string()], &context)
        .await;
    assert!(
        set_volume.is_ok(),
        "Action 'set_volume' failed: {:?}",
        set_volume.err()
    );
}

#[tokio::test]
#[ignore = "requires the built Spotify WASM artifact; see module header (#1515)"]
async fn test_spotify_unknown_action() {
    let wasm_path = spotify_plugin_path();

    let config = WasmConfig::new("test-plugin").expect("safe id");
    let mut plugin = WasmPlugin::load(&wasm_path, config)
        .await
        .expect("Failed to load plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    let context = TriggerContext::new();
    let result = plugin.execute("unknown_action", &[], &context).await;

    // Should fail with error code 3 (action execution failed)
    assert!(result.is_err(), "Unknown action should fail");
}
