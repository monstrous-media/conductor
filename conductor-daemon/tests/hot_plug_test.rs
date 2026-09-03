// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Integration tests for hot-plug detection (ADR-009 Phase 4a)
//!
//! These tests verify the rescan behavior and command variants for hot-plug
//! port detection. Since we can't create real MIDI ports in CI, tests focus
//! on the `filter_ports()` pure function and InputManager internal state.

use conductor_core::identity::DeviceId;
use conductor_core::resolver::PortInfo;
use conductor_daemon::daemon::types::{DaemonCommand, LifecycleState};
use conductor_daemon::input_manager::{InputManager, InputMode, filter_ports};

/// Test that HotPlugCheck command variant exists and compiles
#[test]
fn test_hot_plug_check_command_variant_exists() {
    let _cmd = DaemonCommand::HotPlugCheck;
    // If this compiles, the variant exists
}

/// Test that HotPlugCheck is a distinct variant from TimerTick
#[test]
fn test_hot_plug_check_distinct_from_timer_tick() {
    let cmd = DaemonCommand::HotPlugCheck;
    assert!(matches!(cmd, DaemonCommand::HotPlugCheck));
    assert!(!matches!(cmd, DaemonCommand::TimerTick));
}

/// Test rescan on empty InputManager (not in multi-device mode) returns (0,0)
#[tokio::test]
async fn test_rescan_not_multi_device_returns_zero() {
    let mut manager = InputManager::new(None, false, InputMode::MidiOnly);
    let config = conductor_core::Config::default_config();
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<
        conductor_core::identity::DeviceEvent<conductor_core::events::ProtocolEvent>,
    >(10);
    let (command_tx, _command_rx) = tokio::sync::mpsc::channel::<DaemonCommand>(10);

    // Not in multi-device mode, should return (0, 0, 0).
    // Third element is `rekeyed` — surfaces DeviceId
    // changes (existing ports whose binding resolution flipped).
    // rescan_ports now takes pre-enumerated ports; not-multi-device
    // early-returns before using them.
    let result = manager.rescan_ports(vec![], &config, &event_tx, &command_tx);
    assert!(result.is_ok());
    let (opened, removed, rekeyed) = result.unwrap();
    assert_eq!(opened, 0);
    assert_eq!(removed, 0);
    assert_eq!(rekeyed, 0);
}

/// Test filter_ports respects ignore_ports during rescan
#[test]
fn test_rescan_respects_ignore_ports() {
    let ports = vec![
        PortInfo::new("IAC Driver Bus 1".to_string(), 0),
        PortInfo::new("Mikro MK3 MIDI".to_string(), 1),
        PortInfo::new("MIDI Through Port-0".to_string(), 2),
    ];

    let result = filter_ports(
        ports,
        &["IAC Driver".to_string(), "MIDI Through".to_string()],
        32,
    );
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "Mikro MK3 MIDI");
}

/// Test filter_ports respects max_midi_ports cap
#[test]
fn test_rescan_respects_max_midi_ports() {
    let ports = vec![
        PortInfo::new("Port A".to_string(), 0),
        PortInfo::new("Port B".to_string(), 1),
        PortInfo::new("Port C".to_string(), 2),
        PortInfo::new("Port D".to_string(), 3),
    ];

    let result = filter_ports(ports, &[], 2);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].name, "Port A");
    assert_eq!(result[1].name, "Port B");
}

/// Test filter_ports with no ports returns empty
#[test]
fn test_filter_ports_empty_input() {
    let result = filter_ports(Vec::new(), &[], 32);
    assert!(result.is_empty());
}

/// Test filter_ports when all ports are ignored
#[test]
fn test_filter_ports_all_ignored() {
    let ports = vec![
        PortInfo::new("IAC Driver Bus 1".to_string(), 0),
        PortInfo::new("IAC Driver Bus 2".to_string(), 1),
    ];

    let result = filter_ports(ports, &["IAC Driver".to_string()], 32);
    assert!(result.is_empty());
}

/// Test filter_ports with no ignore patterns passes all through
#[test]
fn test_filter_ports_no_ignore() {
    let ports = vec![
        PortInfo::new("Port A".to_string(), 0),
        PortInfo::new("Port B".to_string(), 1),
    ];

    let result = filter_ports(ports, &[], 32);
    assert_eq!(result.len(), 2);
}

/// Test that InputManager multi_device_active flag defaults to false
#[test]
fn test_input_manager_not_multi_device_by_default() {
    let manager = InputManager::new(None, false, InputMode::MidiOnly);
    assert!(!manager.is_multi_device());
}

/// Test lifecycle transition: Reloading → Stopping
#[test]
fn test_lifecycle_reloading_to_stopping() {
    assert!(LifecycleState::Reloading.can_transition_to(LifecycleState::Stopping));
}

/// Test DeviceId instance disambiguation for duplicate port names
#[test]
fn test_device_id_instance_disambiguation() {
    // First instance gets no suffix
    let id0 = DeviceId::from_port_instance("nanoKONTROL2", 0);
    assert_eq!(id0.as_str(), "nanoKONTROL2");

    // Second instance gets " #2" suffix
    let id1 = DeviceId::from_port_instance("nanoKONTROL2", 1);
    assert_eq!(id1.as_str(), "nanoKONTROL2 #2");

    // Third instance gets " #3" suffix
    let id2 = DeviceId::from_port_instance("nanoKONTROL2", 2);
    assert_eq!(id2.as_str(), "nanoKONTROL2 #3");

    // Distinct IDs don't collide
    assert_ne!(id0.as_str(), id1.as_str());
    assert_ne!(id1.as_str(), id2.as_str());
}

/// Test lifecycle transition: Reloading → Reconnecting
#[test]
fn test_lifecycle_reloading_to_reconnecting() {
    assert!(LifecycleState::Reloading.can_transition_to(LifecycleState::Reconnecting));
}

/// Test lifecycle transition: Reconnecting → Stopping
#[test]
fn test_lifecycle_reconnecting_to_stopping() {
    assert!(LifecycleState::Reconnecting.can_transition_to(LifecycleState::Stopping));
}

/// Test that hot-plug irrelevant transitions are still invalid
#[test]
fn test_lifecycle_invalid_transitions_unchanged() {
    // Running → Reconnecting is not valid (must go via Degraded)
    assert!(!LifecycleState::Running.can_transition_to(LifecycleState::Reconnecting));
    // Stopped → Running is not valid (must go through Init → Starting)
    assert!(!LifecycleState::Stopped.can_transition_to(LifecycleState::Running));
}
