// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Engine-manager test suite, split into themed child modules.
//! Shared `use`s + helper fns live here and are
//! re-exported to the child files via `pub(crate) use super::*;`.

pub(crate) use super::*;
use std::time::Duration;
use tokio::sync::mpsc;

mod bind_gate_e2e;
mod e2e;
mod ipc;
mod lifecycle;
mod mode_autoswitch;
mod mode_lock;
mod monitor;
mod monitor_events;
mod mutation;
mod resolve;
mod routing;
mod routing_edge;
mod simulate;

fn create_test_config() -> Config {
    Config::default_config()
}

fn make_output_map(
    entries: Vec<(&str, &str, bool)>,
) -> std::collections::HashMap<String, crate::daemon::output_resolver::OutputResolution> {
    entries
        .into_iter()
        .map(|(alias, port, auto)| {
            (
                alias.to_string(),
                crate::daemon::output_resolver::OutputResolution {
                    port_name: port.to_string(),
                    auto_paired: auto,
                },
            )
        })
        .collect()
}

/// Build a minimal MIDI `Matcher` endpoint with the given direction —
/// `resolve_device_io` only reads `alias` + `direction` (ADR-035).
fn test_endpoint(
    alias: &str,
    direction: conductor_core::config::types::ConnectorDirection,
) -> conductor_core::config::types::EndpointConfig {
    conductor_core::config::types::EndpointConfig {
        alias: alias.to_string(),
        direction,
        protocol: None,
        description: None,
        enabled: true,
        channels: vec![],
        kind: conductor_core::config::types::EndpointKind::Matcher {
            matchers: vec![conductor_core::identity::DeviceMatcher::name_contains("X")],
            input_matchers: vec![],
            output_matchers: vec![],
            no_probe: false,
        },
    }
}

fn make_binding(
    alias: &str,
    port: &str,
    connected: bool,
    is_configured: bool,
) -> (DeviceId, String, bool, bool) {
    (
        DeviceId::from_alias(alias),
        port.to_string(),
        connected,
        is_configured,
    )
}

/// Helper: create a config with known modes and mappings for simulate tests
fn create_simulate_test_config() -> Config {
    use conductor_core::config::types::{ActionConfig, Mapping, Trigger};
    Config {
        config_meta: Default::default(),
        endpoints: vec![],
        modes: vec![
            Mode {
                name: "Mix".to_string(),
                color: Some("blue".to_string()),
                mappings: vec![
                    Mapping {
                        trigger: Trigger::Note {
                            note: 36,
                            velocity_min: Some(1),
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Keystroke {
                            keys: "c".to_string(),
                            modifiers: vec!["cmd".to_string()],
                        },
                        description: Some("Copy shortcut".to_string()),
                        let_through: false,
                    },
                    Mapping {
                        trigger: Trigger::CC {
                            cc: 7,
                            value_min: None,
                            channel: None,
                            device: None,
                        },
                        action: ActionConfig::Launch {
                            app: "Spotify".to_string(),
                        },
                        description: None,
                        let_through: false,
                    },
                ],
            },
            Mode {
                name: "Edit".to_string(),
                color: Some("green".to_string()),
                mappings: vec![Mapping {
                    trigger: Trigger::Note {
                        note: 40,
                        velocity_min: None,
                        channel: None,
                        device: None,
                    },
                    action: ActionConfig::ModeChange {
                        mode: "Mix".to_string(),
                    },
                    description: Some("Switch to Mix".to_string()),
                    let_through: false,
                }],
            },
        ],
        global_mappings: vec![Mapping {
            trigger: Trigger::Note {
                note: 60,
                velocity_min: Some(1),
                channel: None,
                device: None,
            },
            action: ActionConfig::Keystroke {
                keys: "a".to_string(),
                modifiers: vec!["cmd".to_string()],
            },
            description: Some("Select all".to_string()),
            let_through: false,
        }],
        event_console: Some(conductor_core::config::types::EventConsoleConfig {
            capture_actions: true,
            ..Default::default()
        }),
        ..Config::default_config()
    }
}

/// Helper: create EngineManager from a config for simulate tests
async fn create_simulate_manager(config: Config) -> EngineManager {
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    EngineManager::new(
        config,
        PathBuf::from("/tmp/test-simulate.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("Failed to create EngineManager for simulate test")
}

/// The timer-tick (D12) + hot-plug (Phase 4) tasks are daemon-lifetime
/// and `connect_multi_device` re-runs on every MIDI `DeviceReconnected`, so the
/// spawn must be idempotent — otherwise each reconnect leaked a fresh task pair.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new → ActionExecutor → Enigo::new panics on headless Linux
async fn background_tasks_spawn_at_most_once_across_reconnects() {
    let manager = create_simulate_manager(create_simulate_test_config()).await;
    assert!(
        manager.spawn_background_tasks_once(),
        "first call must spawn the timer-tick + hot-plug pair"
    );
    assert!(
        !manager.spawn_background_tasks_once(),
        "a reconnect must NOT respawn — no duplicate background tasks (the #2404 leak)"
    );
    assert!(
        !manager.spawn_background_tasks_once(),
        "spawn guard stays idempotent across further reconnects"
    );
}

/// Minimal config for the suppression tests below.
///
/// Note: the existing `create_simulate_test_config()` maps note 36
/// → `Keystroke "c" cmd`. The simulate-tests use `execute: false`
/// so the action never reaches the OS. The Learn-suppression tests below DO
/// fire real dispatches through `process_device_event`, so they
/// must use a benign action that doesn't synthesise input events
/// (ModeChange just toggles a flag — no Enigo invocation when the
/// action runs, no clipboard side-effects on macOS). Note that
/// the `#[cfg_attr(target_os = "linux", ignore)]` on each test is
/// still required: `EngineManager::new` constructs an
/// `ActionExecutor` which calls `Enigo::new()`, and that panics
/// on headless Linux during construction regardless of which
/// action the test eventually dispatches. ModeChange shrinks the
/// blast radius at dispatch time, not the constructor cost.
fn create_learn_suppression_test_config() -> Config {
    use conductor_core::config::types::{ActionConfig, Mapping, Trigger};
    Config {
        config_meta: Default::default(),
        endpoints: vec![],
        modes: vec![
            Mode {
                name: "Mix".to_string(),
                color: Some("blue".to_string()),
                mappings: vec![Mapping {
                    trigger: Trigger::Note {
                        note: 36,
                        velocity_min: Some(1),
                        channel: None,
                        device: None,
                    },
                    action: ActionConfig::ModeChange {
                        mode: "Edit".to_string(),
                    },
                    description: Some("Switch to Edit (test-safe)".to_string()),
                    let_through: false,
                }],
            },
            Mode {
                name: "Edit".to_string(),
                color: Some("green".to_string()),
                mappings: vec![],
            },
        ],
        global_mappings: vec![],
        event_console: Some(conductor_core::config::types::EventConsoleConfig {
            capture_actions: true,
            ..Default::default()
        }),
        ..Config::default_config()
    }
}

/// Drain `mapping_matched` events from the broadcast receiver
/// without sleeping. `mapping_matched` is pushed synchronously
/// inside `process_device_event` after `try_dispatch` returns Ok,
/// so by the time the await resolves any matched event is already
/// in the channel. Returns true iff at least one was observed.
fn drain_for_mapping_matched(rx: &mut tokio::sync::broadcast::Receiver<MonitorEvent>) -> bool {
    let mut found = false;
    while let Ok(ev) = rx.try_recv() {
        if ev.event_type == "mapping_matched" {
            found = true;
        }
    }
    found
}

/// Helper: same as `drain_for_mapping_matched` but returns the first
/// matched event so callers can assert on its fields (e.g. `device_id`).
fn drain_first_mapping_matched(
    rx: &mut tokio::sync::broadcast::Receiver<MonitorEvent>,
) -> Option<MonitorEvent> {
    while let Ok(ev) = rx.try_recv() {
        if ev.event_type == "mapping_matched" {
            return Some(ev);
        }
    }
    None
}

fn create_route_only_test_config() -> Config {
    use conductor_core::config::types::{
        ConnectorDirection, ConnectorProtocol, EndpointConfig, EndpointKind, RouteConfig,
    };
    use conductor_core::identity::DeviceMatcher;

    Config {
        mcp: Default::default(),
        per_app_modes: None,
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![
            // Source binding "pads" — no trigger configured so the rule
            // matcher will miss and fall through to stage 9.
            EndpointConfig {
                alias: "pads".to_string(),
                direction: ConnectorDirection::Input,
                protocol: None,
                description: Some("source — no triggers, only routes".to_string()),
                enabled: true,
                channels: vec![],
                kind: EndpointKind::Matcher {
                    matchers: vec![],
                    input_matchers: vec![],
                    output_matchers: vec![],
                    no_probe: true,
                },
            },
            EndpointConfig {
                alias: "absynth".to_string(),
                direction: ConnectorDirection::Output,
                protocol: Some(ConnectorProtocol::Midi),
                description: None,
                enabled: true,
                channels: vec![],
                kind: EndpointKind::Matcher {
                    input_matchers: Vec::new(),
                    output_matchers: Vec::new(),
                    matchers: vec![DeviceMatcher::ExactName {
                        value: "Absynth".to_string(),
                    }],
                    no_probe: false,
                },
            },
        ],
        // No modes/mappings — the rule matcher will return None for
        // every event, so stage 9 must fire if a route exists.
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![],
        }],
        global_mappings: vec![],
        advanced_settings: Default::default(),
        logging: None,
        last_selected_mode: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![RouteConfig {
            from: "pads".to_string(),
            to: "absynth".to_string(),
            transform: None,
            filter: None,
            enabled: true,
            description: Some("pads → absynth (slice-1 test)".to_string()),
            modes: Vec::new(),
        }],
    }
}

fn drain_for_event_type(
    rx: &mut tokio::sync::broadcast::Receiver<MonitorEvent>,
    wanted: &str,
) -> Option<MonitorEvent> {
    while let Ok(ev) = rx.try_recv() {
        if ev.event_type == wanted {
            return Some(ev);
        }
    }
    None
}

fn lt_test_envelope(let_through: bool, mapping_id: Option<usize>) -> ActionEnvelope {
    ActionEnvelope {
        action: conductor_core::actions::Action::Text("x".to_string()),
        device_id: Some("pads".to_string()),
        matched_rule: Some("rule".to_string()),
        mode_name: Some("Default".to_string()),
        let_through,
        mapping_id,
        network_origin: None,
    }
}

fn create_aftertouch_e2e_test_config() -> Config {
    use conductor_core::config::types::{ActionConfig, Mapping, Trigger};
    Config {
        config_meta: Default::default(),
        endpoints: vec![],
        modes: vec![
            Mode {
                name: "Mix".to_string(),
                color: Some("blue".to_string()),
                mappings: vec![Mapping {
                    trigger: Trigger::Aftertouch {
                        pressure_min: Some(64),
                        channel: None,
                        device: None,
                    },
                    action: ActionConfig::ModeChange {
                        mode: "Edit".to_string(),
                    },
                    description: Some("Aftertouch ≥64 → Edit mode (test-safe)".to_string()),
                    let_through: false,
                }],
            },
            Mode {
                name: "Edit".to_string(),
                color: Some("green".to_string()),
                mappings: vec![],
            },
        ],
        global_mappings: vec![],
        event_console: Some(conductor_core::config::types::EventConsoleConfig {
            capture_actions: true,
            ..Default::default()
        }),
        ..Config::default_config()
    }
}

fn make_engine_for_ui_mode_test() -> EngineManager {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _) = broadcast::channel(1);
    EngineManager::new(
        config,
        PathBuf::from("/tmp/test-uimode.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap()
}
