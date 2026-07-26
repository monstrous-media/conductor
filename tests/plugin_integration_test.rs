// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Integration tests for plugin system
//!
//! Tests plugin discovery, loading, and execution through the daemon's plugin manager.

// Integration test: diagnostic stdout/stderr output is legitimate here.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]

use conductor_core::plugin::{Capability, TriggerContext};
use conductor_daemon::plugin_manager::PluginManager;

#[test]
#[ignore] // Requires pre-built plugin binaries at ~/.config/conductor/plugins/
fn test_plugin_discovery() {
    // Get plugins directory
    let plugins_dir = dirs::config_dir()
        .expect("Failed to get config directory")
        .join("conductor")
        .join("plugins");

    // Create plugin manager
    let mut manager = PluginManager::new(plugins_dir.clone());

    // Discover plugins
    let count = manager
        .discover_plugins()
        .expect("Failed to discover plugins");

    println!("✅ Discovered {} plugins in {:?}", count, plugins_dir);

    // Verify we found our test plugins
    assert!(count >= 2, "Expected at least 2 plugins (spotify, obs)");

    // List available plugins
    let available = manager
        .list_available()
        .expect("Failed to list available plugins");

    println!("✅ Available plugins: {:?}", available);

    // Verify spotify and obs are available
    assert!(
        available.contains(&"spotify".to_string()),
        "Spotify plugin not found"
    );
    assert!(
        available.contains(&"obs".to_string()),
        "OBS plugin not found"
    );
}

#[test]
#[ignore] // Requires pre-built plugin binaries at ~/.config/conductor/plugins/
fn test_plugin_loading() {
    // Get plugins directory
    let plugins_dir = dirs::config_dir()
        .expect("Failed to get config directory")
        .join("conductor")
        .join("plugins");

    // Create plugin manager
    let mut manager = PluginManager::new(plugins_dir);

    // Discover plugins
    manager
        .discover_plugins()
        .expect("Failed to discover plugins");

    // Load Spotify plugin
    manager
        .load_plugin("spotify")
        .expect("Failed to load Spotify plugin");

    println!("✅ Loaded Spotify plugin");

    // Verify it's in loaded list
    let loaded = manager
        .list_loaded()
        .expect("Failed to list loaded plugins");
    assert!(
        loaded.contains(&"spotify".to_string()),
        "Spotify plugin not in loaded list"
    );

    // Get metadata
    let metadata = manager
        .get_metadata("spotify")
        .expect("Failed to get Spotify metadata");

    println!("✅ Spotify plugin metadata:");
    println!("   Name: {}", metadata.name);
    println!("   Version: {}", metadata.version);
    println!("   Description: {}", metadata.description);
    println!("   Capabilities: {:?}", metadata.capabilities);

    // Verify expected metadata
    assert_eq!(metadata.name, "spotify");
    assert_eq!(metadata.version, "0.1.0");
    assert!(metadata.capabilities.contains(&Capability::Network));
    assert!(metadata.capabilities.contains(&Capability::Filesystem));

    // Load OBS plugin
    manager
        .load_plugin("obs")
        .expect("Failed to load OBS plugin");

    println!("✅ Loaded OBS plugin");

    // Get OBS metadata
    let obs_metadata = manager
        .get_metadata("obs")
        .expect("Failed to get OBS metadata");

    println!("✅ OBS plugin metadata:");
    println!("   Name: {}", obs_metadata.name);
    println!("   Version: {}", obs_metadata.version);
    println!("   Description: {}", obs_metadata.description);
    println!("   Capabilities: {:?}", obs_metadata.capabilities);

    // Verify OBS metadata
    assert_eq!(obs_metadata.name, "obs");
    assert_eq!(obs_metadata.version, "0.1.0");
    assert!(obs_metadata.capabilities.contains(&Capability::Network));
}

#[test]
#[ignore] // Requires pre-built plugin binaries at ~/.config/conductor/plugins/
fn test_plugin_capability_management() {
    // Get plugins directory
    let plugins_dir = dirs::config_dir()
        .expect("Failed to get config directory")
        .join("conductor")
        .join("plugins");

    // Create plugin manager
    let mut manager = PluginManager::new(plugins_dir);

    // Discover and load plugins
    manager
        .discover_plugins()
        .expect("Failed to discover plugins");
    manager
        .load_plugin("spotify")
        .expect("Failed to load Spotify plugin");

    // Grant Network capability
    manager
        .grant_capability("spotify", Capability::Network)
        .expect("Failed to grant Network capability");

    println!("✅ Granted Network capability to Spotify plugin");

    // Grant Filesystem capability
    manager
        .grant_capability("spotify", Capability::Filesystem)
        .expect("Failed to grant Filesystem capability");

    println!("✅ Granted Filesystem capability to Spotify plugin");

    // Enable the plugin
    manager
        .enable_plugin("spotify")
        .expect("Failed to enable Spotify plugin");

    println!("✅ Enabled Spotify plugin");

    // Get stats
    let stats = manager
        .get_stats("spotify")
        .expect("Failed to get plugin stats");

    println!("✅ Spotify plugin stats:");
    println!("   Executions: {}", stats.executions);
    println!("   Failures: {}", stats.failures);
}

#[test]
#[ignore] // Requires Spotify credentials
fn test_plugin_execution() {
    // Get plugins directory
    let plugins_dir = dirs::config_dir()
        .expect("Failed to get config directory")
        .join("conductor")
        .join("plugins");

    // Create plugin manager
    let mut manager = PluginManager::new(plugins_dir);

    // Discover and load plugins
    manager
        .discover_plugins()
        .expect("Failed to discover plugins");
    manager
        .load_plugin("spotify")
        .expect("Failed to load Spotify plugin");

    // Grant capabilities
    manager
        .grant_capability("spotify", Capability::Network)
        .expect("Failed to grant Network capability");
    manager
        .grant_capability("spotify", Capability::Filesystem)
        .expect("Failed to grant Filesystem capability");

    // Enable plugin
    manager
        .enable_plugin("spotify")
        .expect("Failed to enable Spotify plugin");

    // Create action data (play/pause)
    let action_data = serde_json::json!({
        "type": "play_pause"
    });

    // Create trigger context
    let context = TriggerContext {
        velocity: Some(100),
        current_mode: Some(0),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    };

    // Execute plugin action
    let result = manager.execute_plugin("spotify", action_data, Some(context));

    match result {
        Ok(_) => println!("✅ Successfully executed Spotify play_pause action"),
        Err(e) => println!("⚠️  Expected error (no credentials): {}", e),
    }

    // Get updated stats
    let stats = manager
        .get_stats("spotify")
        .expect("Failed to get plugin stats");

    println!("✅ Plugin stats after execution:");
    println!("   Executions: {}", stats.executions);
    println!("   Failures: {}", stats.failures);

    // Execution count should have increased
    assert!(
        stats.executions > 0,
        "Execution count should be greater than 0"
    );
}

#[test]
#[ignore] // Requires pre-built plugin binaries at ~/.config/conductor/plugins/
fn test_plugin_enable_disable() {
    // Get plugins directory
    let plugins_dir = dirs::config_dir()
        .expect("Failed to get config directory")
        .join("conductor")
        .join("plugins");

    // Create plugin manager
    let mut manager = PluginManager::new(plugins_dir);

    // Discover and load plugins
    manager
        .discover_plugins()
        .expect("Failed to discover plugins");
    manager
        .load_plugin("spotify")
        .expect("Failed to load Spotify plugin");

    // Grant capabilities
    manager
        .grant_capability("spotify", Capability::Network)
        .expect("Failed to grant capability");
    manager
        .grant_capability("spotify", Capability::Filesystem)
        .expect("Failed to grant capability");

    // Enable plugin
    manager
        .enable_plugin("spotify")
        .expect("Failed to enable plugin");

    println!("✅ Plugin enabled");

    // Disable plugin
    manager
        .disable_plugin("spotify")
        .expect("Failed to disable plugin");

    println!("✅ Plugin disabled");

    // Try to execute while disabled (should fail)
    let action_data = serde_json::json!({ "type": "play_pause" });
    let context = TriggerContext {
        velocity: Some(100),
        current_mode: Some(0),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    };

    let result = manager.execute_plugin("spotify", action_data, Some(context));
    assert!(
        result.is_err(),
        "Execution should fail when plugin is disabled"
    );

    println!("✅ Plugin correctly blocked execution when disabled");

    // Re-enable plugin
    manager
        .enable_plugin("spotify")
        .expect("Failed to re-enable plugin");

    println!("✅ Plugin re-enabled");
}

#[test]
#[ignore] // Requires pre-built plugin binaries at ~/.config/conductor/plugins/
fn test_plugin_unload() {
    // Get plugins directory
    let plugins_dir = dirs::config_dir()
        .expect("Failed to get config directory")
        .join("conductor")
        .join("plugins");

    // Create plugin manager
    let mut manager = PluginManager::new(plugins_dir);

    // Discover and load plugins
    manager
        .discover_plugins()
        .expect("Failed to discover plugins");
    manager
        .load_plugin("spotify")
        .expect("Failed to load Spotify plugin");

    println!("✅ Loaded Spotify plugin");

    // Verify it's loaded
    let loaded = manager
        .list_loaded()
        .expect("Failed to list loaded plugins");
    assert!(loaded.contains(&"spotify".to_string()));

    // Unload the plugin
    manager
        .unload_plugin("spotify")
        .expect("Failed to unload Spotify plugin");

    println!("✅ Unloaded Spotify plugin");

    // Verify it's no longer in loaded list
    let loaded = manager
        .list_loaded()
        .expect("Failed to list loaded plugins");
    assert!(!loaded.contains(&"spotify".to_string()));

    println!("✅ Plugin removed from loaded list");
}

/// #1507: every other test in this file is `#[ignore]`d because it needs
/// pre-built plugin binaries under `~/.config/conductor/plugins/` (or Spotify
/// credentials), so `cargo test --workspace` executes none of them — plugin
/// manager regressions slip through the default suite.
///
/// This test is NON-ignored and fully hermetic: it points the PluginManager at
/// an explicit empty temp directory (not the default `~/.config` path) and
/// exercises the core discovery + error paths that don't require a real plugin
/// artifact:
/// - the manager is configured with the given directory (`plugins_dir()`);
/// - discovery over an empty dir returns zero, with no real plugin setup;
/// - the available/loaded lists start empty;
/// - loading / inspecting a plugin that doesn't exist fails with
///   `PluginNotFound` rather than panicking or fabricating success;
/// - a failed load leaves nothing loaded.
///
/// The happy-path load/enable/execute/unload flow still needs a real plugin
/// binary, so those remain `#[ignore]`d above.
#[test]
fn plugin_manager_hermetic_discovery_and_error_paths() {
    use conductor_daemon::plugin_manager::PluginManagerError;

    let tmp = tempfile::TempDir::new().expect("create temp dir");
    let mut manager = PluginManager::new(tmp.path().to_path_buf());

    // The manager operates on the directory it was given, not a default like
    // ~/.config/conductor/plugins — so the rest of this test is hermetic.
    assert_eq!(
        manager.plugins_dir(),
        tmp.path(),
        "manager must use the configured directory"
    );

    // Discovery over the empty directory: zero plugins, no panic.
    let count = manager
        .discover_plugins()
        .expect("discovery over an empty dir should succeed");
    assert_eq!(count, 0, "an empty plugins dir must discover zero plugins");

    assert!(
        manager.list_available().expect("list_available").is_empty(),
        "no plugins should be available from an empty dir"
    );
    assert!(
        manager.list_loaded().expect("list_loaded").is_empty(),
        "nothing should be loaded initially"
    );

    // Loading a plugin that was never discovered must fail with PluginNotFound.
    let err = manager
        .load_plugin("does-not-exist")
        .expect_err("loading a missing plugin must fail");
    assert!(
        matches!(err, PluginManagerError::PluginNotFound(_)),
        "expected PluginNotFound, got {err:?}"
    );

    // Metadata / stats for an unknown plugin must fail with PluginNotFound,
    // not fabricate a value.
    assert!(
        matches!(
            manager.get_metadata("does-not-exist"),
            Err(PluginManagerError::PluginNotFound(_))
        ),
        "metadata for an unknown plugin must be PluginNotFound"
    );
    assert!(
        matches!(
            manager.get_stats("does-not-exist"),
            Err(PluginManagerError::PluginNotFound(_))
        ),
        "stats for an unknown plugin must be PluginNotFound"
    );

    // The failed load must not have registered anything.
    assert!(
        manager.list_loaded().expect("list_loaded").is_empty(),
        "a failed load must leave nothing loaded"
    );
}
