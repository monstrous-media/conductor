// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Test backward compatibility layer - verifies old import paths work

// Test old-style imports through the compatibility layer
use conductor::actions::Action;
use conductor::config::{ActionConfig, Config, Trigger};
use conductor::device_profile::{DeviceProfile, PadPageMapping};
use conductor::event_processor::EventProcessor;
use conductor::feedback::LightingScheme;
use conductor::mappings::MappingEngine;
use conductor_daemon::ActionExecutor; // ActionExecutor moved to daemon in Phase 2

#[test]
fn test_config_module_accessible() {
    // Verify config types accessible via old path
    let _: Option<Config> = None;
    let _: Option<Trigger> = None;
    let _: Option<ActionConfig> = None;
}

#[test]
fn test_event_processor_module_accessible() {
    // Verify EventProcessor accessible via old path
    let _processor = EventProcessor::new();
}

#[test]
fn test_actions_module_accessible() {
    // Verify actions types are accessible via the old path. Type-only and
    // display-independent, so it runs on Linux too — the display-dependent
    // ActionExecutor construction is split into its own guarded test below, so
    // this runtime accessibility check is no longer skipped on Linux CI (#1527).
    let _: Option<Action> = None;
}

#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "ActionExecutor::default() builds an Enigo backend that needs a display server"
)]
fn test_action_executor_default_constructs() {
    // The one display-dependent check: ActionExecutor::default() must still
    // construct (constructor / runtime compatibility). Kept behind the Linux
    // ignore because Enigo::new() panics on a headless runner; the
    // accessibility / import checks in this file now cover Linux at runtime
    // (#1527).
    let _executor = ActionExecutor::default();
}

#[test]
fn test_mappings_module_accessible() {
    // Verify MappingEngine accessible via old path
    let _engine = MappingEngine::new();
}

#[test]
fn test_feedback_module_accessible() {
    // Verify feedback types accessible via old path
    let schemes = LightingScheme::list_all();
    assert!(schemes.contains(&"reactive"));
}

#[test]
fn test_device_profile_module_accessible() {
    // Verify device profile types accessible via old path
    let _: Option<DeviceProfile> = None;
    let _: Option<PadPageMapping> = None;
}

#[test]
fn test_root_level_imports_work() {
    // Root-level re-exports. Type-only checks that run on all platforms,
    // including Linux — previously skipped on Linux because the
    // ActionExecutor construction shared this test (now in
    // `test_action_executor_default_constructs`) (#1527).
    use conductor::{Config, MidiEvent, ProcessedEvent};

    let _: Option<Config> = None;
    let _: Option<MidiEvent> = None;
    let _: Option<ProcessedEvent> = None;
}
