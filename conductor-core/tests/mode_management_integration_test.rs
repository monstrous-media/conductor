// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! End-to-End Integration Tests for Mode Management (Issue #321 - Phase 5)
//!
//! These tests verify the complete mode management system across all interfaces:
//! - MCP tools → config persistence → daemon reload
//! - Gamepad chords → config persistence → startup resolution
//! - GUI mode changes → daemon synchronization
//! - Cross-restart persistence and fallback chains
//!
//! All tests use temporary config files to avoid side effects.

use conductor_core::config::types::{
    Config, ConnectorDirection, EndpointConfig, EndpointKind, Mapping, Mode,
};
use conductor_core::{ActionConfig, Trigger};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// Test utilities for creating temporary configs
struct TestConfigManager {
    /// RAII guard — keeps the temp directory alive for the lifetime of
    /// this manager. `config_path` points inside it, so dropping this
    /// would delete the config file. Held only for its `Drop`, never
    /// read directly — hence the `dead_code` allow.
    #[allow(dead_code)]
    temp_dir: TempDir,
    config_path: PathBuf,
}

impl TestConfigManager {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp directory");
        let config_path = temp_dir.path().join("conductor.toml");

        Self {
            temp_dir,
            config_path,
        }
    }

    fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Create a config with multiple modes for testing
    fn create_multi_mode_config(
        &self,
        last_selected: Option<&str>,
        default_mode: Option<&str>,
    ) -> Config {
        Config {
            mcp: Default::default(),
            config_meta: Default::default(),
            security: Default::default(),
            endpoints: vec![EndpointConfig {
                alias: "testpad".to_string(),
                direction: ConnectorDirection::Input,
                protocol: None,
                description: None,
                enabled: true,
                channels: vec![],
                kind: EndpointKind::Matcher {
                    matchers: vec![conductor_core::identity::DeviceMatcher::name_contains(
                        "Test",
                    )],
                    input_matchers: vec![],
                    output_matchers: vec![],
                    no_probe: false,
                },
            }],
            last_selected_mode: last_selected.map(|s| s.to_string()),
            per_app_modes: None,
            default_mode: default_mode.map(|s| s.to_string()),
            led: None,
            event_console: None,
            routes: vec![],
            modes: vec![
                Mode {
                    name: "Performance".to_string(),
                    color: Some("blue".to_string()),
                    mappings: vec![Mapping {
                        trigger: Trigger::Note {
                            note: 36,
                            velocity_min: None,
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Launch {
                            app: "performance_app".to_string(),
                        },
                        description: None,
                        let_through: false,
                    }],
                },
                Mode {
                    name: "DJ".to_string(),
                    color: Some("red".to_string()),
                    mappings: vec![Mapping {
                        trigger: Trigger::Note {
                            note: 37,
                            velocity_min: None,
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::ModeChange {
                            mode: "Performance".to_string(),
                        },
                        description: None,
                        let_through: false,
                    }],
                },
                Mode {
                    name: "Empty".to_string(),
                    color: Some("gray".to_string()),
                    mappings: vec![], // No mappings: valid mode that won't match any input events
                },
            ],
            global_mappings: vec![Mapping {
                trigger: Trigger::Note {
                    note: 64,
                    velocity_min: None,
                    channel: None,
                    device: None,
                },
                action: ActionConfig::Launch {
                    app: "global_app".to_string(),
                },
                description: None,
                let_through: false,
            }],
            logging: None,
            advanced_settings: conductor_core::config::types::AdvancedSettings::default(),
        }
    }

    fn save_config(&self, config: &Config) -> Result<(), Box<dyn std::error::Error>> {
        config.save(&self.config_path.to_string_lossy())?;
        Ok(())
    }

    fn load_config(&self) -> Result<Config, Box<dyn std::error::Error>> {
        let toml_str = fs::read_to_string(&self.config_path)?;
        let config: Config = toml::from_str(&toml_str)?;
        Ok(config)
    }

    /// Resolve the startup mode INDEX via the REAL production resolver,
    /// [`Config::resolve_startup_mode`] in conductor-core (#1567). The daemon's
    /// `engine_manager::resolve_startup_mode` delegates to the same function, so
    /// the index this test asserts is the one production computes.
    ///
    /// The returned name is a TEST-side convenience for the assertions below: the
    /// resolved mode's name, or `"Global"` as a placeholder when there are no
    /// modes (index 0, global-mappings-only). It is not the daemon's own
    /// empty-modes status label — only the index is the production contract here.
    fn resolve_startup_mode(&self, config: &Config) -> (usize, String) {
        let idx = config.resolve_startup_mode();
        let name = config
            .modes
            .get(idx)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "Global".to_string());
        (idx, name)
    }

    /// Validate an MCP `switch_mode` via the REAL production validator,
    /// [`Config::resolve_mode_switch`] (#1567). The MCP `switch_mode` tool
    /// delegates to the same function, so the "Mode not found … Available
    /// modes …" contract asserted here is the one production emits.
    fn validate_mcp_mode_switch(
        &self,
        config: &Config,
        mode_name: &str,
    ) -> Result<(usize, String), String> {
        config
            .resolve_mode_switch(mode_name)
            .map(|idx| (idx, mode_name.to_string()))
    }

    /// Persist a mode change the way the daemon does: set `last_selected_mode`
    /// and write the config through the REAL `Config::save` serialization.
    fn persist_mode_change(&self, mode_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut config = self.load_config()?;
        config.last_selected_mode = Some(mode_name.to_string());
        self.save_config(&config)?;
        Ok(())
    }
}

// =============================================================================
// 1. Cross-interface mode consistency tests
// =============================================================================

#[test]
fn test_cross_interface_mode_consistency() {
    let test_mgr = TestConfigManager::new();

    // Step 1: Create config with initial mode
    let mut config = test_mgr.create_multi_mode_config(Some("Performance"), Some("DJ"));
    test_mgr
        .save_config(&config)
        .expect("Failed to save initial config");

    // Step 2: Resolve startup mode (simulates daemon startup)
    let (initial_index, initial_mode) = test_mgr.resolve_startup_mode(&config);
    assert_eq!(initial_mode, "Performance");
    assert_eq!(initial_index, 0);

    // Step 3: Simulate gamepad chord triggering mode change
    // This would normally go: Action::ModeChange → DispatchOutcome::ModeChangeRequested → persist_mode_change
    test_mgr
        .persist_mode_change("DJ")
        .expect("Failed to persist mode change");

    // Step 4: Reload config and verify mode change persisted
    config = test_mgr.load_config().expect("Failed to reload config");
    assert_eq!(config.last_selected_mode, Some("DJ".to_string()));

    // Step 5: Resolve mode again (simulates daemon restart)
    let (new_index, new_mode) = test_mgr.resolve_startup_mode(&config);
    assert_eq!(new_mode, "DJ");
    assert_eq!(new_index, 1);

    // Step 6: Verify MCP tool would see the same mode
    let mcp_result = test_mgr.validate_mcp_mode_switch(&config, "DJ");
    assert!(mcp_result.is_ok());
    let (mcp_index, mcp_mode) = mcp_result.unwrap();
    assert_eq!(mcp_mode, new_mode);
    assert_eq!(mcp_index, new_index);
}

#[test]
fn test_mcp_tool_config_persistence_flow() {
    let test_mgr = TestConfigManager::new();

    // Step 1: Create config
    let config = test_mgr.create_multi_mode_config(Some("Performance"), Some("DJ"));
    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Step 2: MCP tool validates mode (read-only step)
    let validation_result = test_mgr.validate_mcp_mode_switch(&config, "DJ");
    assert!(validation_result.is_ok());

    // Step 3: If validation succeeded, daemon would persist the change
    test_mgr
        .persist_mode_change("DJ")
        .expect("Failed to persist MCP mode change");

    // Step 4: Verify config was updated
    let updated_config = test_mgr.load_config().expect("Failed to reload config");
    assert_eq!(updated_config.last_selected_mode, Some("DJ".to_string()));

    // Step 5: Verify startup resolution uses new mode
    let (index, mode) = test_mgr.resolve_startup_mode(&updated_config);
    assert_eq!(mode, "DJ");
    assert_eq!(index, 1);
}

// =============================================================================
// 2. Persistence across simulated restart
// =============================================================================

#[test]
fn test_persistence_across_simulated_restart() {
    let test_mgr = TestConfigManager::new();

    // Session 1: Initial setup and mode change
    {
        let config = test_mgr.create_multi_mode_config(Some("Performance"), Some("DJ"));
        test_mgr
            .save_config(&config)
            .expect("Failed to save config");

        // Simulate mode change via gamepad chord
        test_mgr
            .persist_mode_change("DJ")
            .expect("Failed to persist mode change");
    }

    // Simulate daemon restart - fresh config load
    {
        let config = test_mgr
            .load_config()
            .expect("Failed to load config after restart");
        let (index, mode) = test_mgr.resolve_startup_mode(&config);

        assert_eq!(mode, "DJ", "Mode should persist across restart");
        assert_eq!(index, 1);
        assert_eq!(config.last_selected_mode, Some("DJ".to_string()));
    }

    // Session 2: Another mode change
    {
        test_mgr
            .persist_mode_change("Performance")
            .expect("Failed to persist second mode change");
    }

    // Simulate another restart
    {
        let config = test_mgr
            .load_config()
            .expect("Failed to load config after second restart");
        let (index, mode) = test_mgr.resolve_startup_mode(&config);

        assert_eq!(mode, "Performance", "Second mode change should persist");
        assert_eq!(index, 0);
    }
}

// =============================================================================
// 3. Fallback chain end-to-end tests
// =============================================================================

#[test]
fn test_fallback_chain_invalid_last_selected() {
    let test_mgr = TestConfigManager::new();

    // Create config with invalid last_selected_mode but valid default_mode
    let config = test_mgr.create_multi_mode_config(Some("NonExistent"), Some("DJ"));
    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Resolve startup mode - should fall back to default_mode
    let (index, mode) = test_mgr.resolve_startup_mode(&config);
    assert_eq!(
        mode, "DJ",
        "Should fall back to default_mode when last_selected_mode is invalid"
    );
    assert_eq!(index, 1);
}

#[test]
fn test_fallback_chain_both_invalid() {
    let test_mgr = TestConfigManager::new();

    // Create config with both invalid last_selected_mode and invalid default_mode
    let config = test_mgr.create_multi_mode_config(Some("NonExistent1"), Some("NonExistent2"));
    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Should fall back to first mode with mappings
    let (index, mode) = test_mgr.resolve_startup_mode(&config);
    assert_eq!(mode, "Performance", "Should fall back to first valid mode");
    assert_eq!(index, 0);
}

#[test]
fn test_fallback_chain_empty_first_mode() {
    let test_mgr = TestConfigManager::new();

    // Create config where first mode has no mappings
    let mut config = test_mgr.create_multi_mode_config(None, None);

    // Move Empty mode to position 0, Performance to position 1
    let empty_mode = config.modes.remove(2); // Remove Empty
    config.modes.insert(0, empty_mode); // Insert at beginning

    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Current behavior: selects the first mode even if it has no mappings
    let (index, mode) = test_mgr.resolve_startup_mode(&config);
    assert_eq!(
        mode, "Empty",
        "Should select the first mode even if it has no mappings"
    );
    assert_eq!(index, 0);
}

#[test]
fn test_fallback_chain_no_valid_modes() {
    let test_mgr = TestConfigManager::new();

    // Create config with only modes that have no mappings
    let mut config = test_mgr.create_multi_mode_config(None, None);
    // Remove all mappings from all modes
    for mode in &mut config.modes {
        mode.mappings.clear();
    }

    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // In production, startup_mode index 0 is used and the mode name is taken from config.modes[0]
    let (index, mode) = test_mgr.resolve_startup_mode(&config);
    assert_eq!(
        mode, config.modes[0].name,
        "Should use the first mode when all modes have empty mappings (matches production behavior)"
    );
    assert_eq!(index, 0);
}

// =============================================================================
// 4. Error handling tests
// =============================================================================

#[test]
fn test_invalid_mode_names_rejected() {
    let test_mgr = TestConfigManager::new();

    let config = test_mgr.create_multi_mode_config(Some("Performance"), Some("DJ"));
    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Test MCP tool validation rejects invalid mode
    let result = test_mgr.validate_mcp_mode_switch(&config, "InvalidMode");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Mode not found: InvalidMode"));

    // Test that available modes are listed in error
    let result = test_mgr.validate_mcp_mode_switch(&config, "AnotherInvalidMode");
    assert!(result.is_err());
    let error_msg = result.unwrap_err();
    assert!(error_msg.contains("Performance"));
    assert!(error_msg.contains("DJ"));
}

#[test]
fn test_config_corruption_recovery() {
    let test_mgr = TestConfigManager::new();

    // Create valid config first
    let config = test_mgr.create_multi_mode_config(Some("Performance"), Some("DJ"));
    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Corrupt the config by writing malformed TOML
    let corrupted_toml = r#"
        [invalid
        last_selected_mode = "Performance"
        # Missing closing bracket
        modes = []
    "#;
    fs::write(test_mgr.config_path(), corrupted_toml).expect("Failed to write corrupted config");

    // Try to load corrupted config - should fail gracefully
    let load_result = test_mgr.load_config();
    assert!(load_result.is_err(), "Should fail to load corrupted config");

    // In a real daemon, this would trigger fallback to default config
    // For this test, we simulate by creating a minimal recovery config
    let recovery_config = test_mgr.create_multi_mode_config(None, Some("Performance"));
    test_mgr
        .save_config(&recovery_config)
        .expect("Failed to save recovery config");

    // Verify recovery config works
    let recovered_config = test_mgr
        .load_config()
        .expect("Failed to load recovery config");
    let (_index, mode) = test_mgr.resolve_startup_mode(&recovered_config);
    assert_eq!(mode, "Performance", "Recovery should use default_mode");
}

// =============================================================================
// 5. Edge cases
// =============================================================================

#[test]
fn test_empty_modes_list() {
    let test_mgr = TestConfigManager::new();

    // Create config with no modes
    let mut config = test_mgr.create_multi_mode_config(Some("Performance"), Some("DJ"));
    config.modes.clear();
    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Should fall back to global mappings
    let (index, mode) = test_mgr.resolve_startup_mode(&config);
    assert_eq!(mode, "Global");
    assert_eq!(index, 0);

    // MCP tool should handle empty modes gracefully
    let result = test_mgr.validate_mcp_mode_switch(&config, "AnyMode");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Mode not found: AnyMode"));
}

#[test]
fn test_mode_deleted_from_config() {
    let test_mgr = TestConfigManager::new();

    // Create config and set mode
    let mut config = test_mgr.create_multi_mode_config(Some("DJ"), Some("Performance"));
    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Remove DJ mode from config (simulates user editing config file)
    config.modes.retain(|m| m.name != "DJ");
    assert_eq!(config.last_selected_mode, Some("DJ".to_string())); // Still points to deleted mode
    test_mgr
        .save_config(&config)
        .expect("Failed to save modified config");

    // Startup resolution should fall back gracefully
    let (index, mode) = test_mgr.resolve_startup_mode(&config);
    assert_eq!(
        mode, "Performance",
        "Should fall back to default_mode when last_selected is deleted"
    );
    assert_eq!(index, 0);
}

#[test]
fn test_rapid_sequential_mode_changes() {
    let test_mgr = TestConfigManager::new();

    let config = test_mgr.create_multi_mode_config(Some("Performance"), Some("DJ"));
    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Simulate rapid mode changes (last one should win)
    test_mgr
        .persist_mode_change("DJ")
        .expect("Failed to persist first change");
    test_mgr
        .persist_mode_change("Performance")
        .expect("Failed to persist second change");
    test_mgr
        .persist_mode_change("DJ")
        .expect("Failed to persist third change");

    // Verify final state
    let final_config = test_mgr.load_config().expect("Failed to load final config");
    assert_eq!(final_config.last_selected_mode, Some("DJ".to_string()));

    let (index, mode) = test_mgr.resolve_startup_mode(&final_config);
    assert_eq!(mode, "DJ", "Last mode change should win");
    assert_eq!(index, 1);
}

// =============================================================================
// 6. Performance and consistency tests
// =============================================================================

#[test]
fn test_config_persistence_atomic() {
    let test_mgr = TestConfigManager::new();

    let config = test_mgr.create_multi_mode_config(Some("Performance"), Some("DJ"));
    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Verify file exists and is readable immediately after save
    let loaded_config = test_mgr
        .load_config()
        .expect("Config should be immediately readable");
    assert_eq!(
        loaded_config.last_selected_mode,
        Some("Performance".to_string())
    );

    // Persist mode change and verify correctness
    test_mgr
        .persist_mode_change("DJ")
        .expect("Failed to persist mode change");

    // Verify change was persisted correctly
    let updated_config = test_mgr
        .load_config()
        .expect("Updated config should be readable");
    assert_eq!(updated_config.last_selected_mode, Some("DJ".to_string()));
}

#[test]
fn test_mode_resolution_consistency() {
    let test_mgr = TestConfigManager::new();

    // Create config and verify resolution is deterministic
    let config = test_mgr.create_multi_mode_config(Some("DJ"), Some("Performance"));
    test_mgr
        .save_config(&config)
        .expect("Failed to save config");

    // Multiple resolutions should return same result
    for _i in 0..10 {
        let (index, mode) = test_mgr.resolve_startup_mode(&config);
        assert_eq!(mode, "DJ");
        assert_eq!(index, 1);
    }

    // Test with different config scenarios
    let scenarios = vec![
        (Some("Performance"), Some("DJ"), "Performance", 0),
        (Some("DJ"), Some("Performance"), "DJ", 1),
        (None, Some("Performance"), "Performance", 0),
        (None, Some("DJ"), "DJ", 1),
        (Some("Invalid"), Some("DJ"), "DJ", 1),
        (None, None, "Performance", 0), // First valid mode
    ];

    for (last_selected, default_mode, expected_mode, expected_index) in scenarios {
        let test_config = test_mgr.create_multi_mode_config(last_selected, default_mode);
        let (index, mode) = test_mgr.resolve_startup_mode(&test_config);
        assert_eq!(
            mode, expected_mode,
            "Scenario failed: last={:?}, default={:?}",
            last_selected, default_mode
        );
        assert_eq!(index, expected_index);
    }
}
