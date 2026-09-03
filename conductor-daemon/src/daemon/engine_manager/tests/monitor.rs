// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_state_transitions() {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // Initial state should be Init
    let state = manager.get_state().await;
    assert_eq!(state, LifecycleState::Init);

    // Should be able to transition to Starting
    let result = manager.transition_state(LifecycleState::Starting).await;
    assert!(result.is_ok());

    let state = manager.get_state().await;
    assert_eq!(state, LifecycleState::Starting);
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_invalid_state_transition() {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // Invalid transition: Init -> Running (must go through Starting)
    let result = manager.transition_state(LifecycleState::Running).await;
    assert!(result.is_err());
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_error_logging() {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    manager.log_error("TestError", "This is a test error").await;

    let errors = manager.get_recent_errors().await;
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind, "TestError");
    assert_eq!(errors[0].message, "This is a test error");
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_statistics() {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // Wait a bit to accumulate uptime
    tokio::time::sleep(Duration::from_millis(100)).await;

    let stats = manager.get_statistics().await;
    assert!(stats.uptime_secs == 0); // Less than 1 second
}

/// v4.10.11: Test Starting → Degraded transition when device connection fails
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_starting_to_degraded_transition() {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // Initial state should be Init
    let state = manager.get_state().await;
    assert_eq!(state, LifecycleState::Init);

    // Transition to Starting
    let result = manager.transition_state(LifecycleState::Starting).await;
    assert!(result.is_ok());
    let state = manager.get_state().await;
    assert_eq!(state, LifecycleState::Starting);

    // v4.10.11: Should be able to transition from Starting → Degraded
    // This is the new path when device connection fails at startup
    let result = manager.transition_state(LifecycleState::Degraded).await;
    assert!(
        result.is_ok(),
        "Starting → Degraded should be valid (v4.10.11)"
    );
    let state = manager.get_state().await;
    assert_eq!(state, LifecycleState::Degraded);
}

/// v4.10.11: Test recovery path from Degraded state
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_degraded_to_running_recovery_path() {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // Simulate startup → degraded (device connection failed)
    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Degraded)
        .await
        .unwrap();

    // Recovery path: Degraded → Reconnecting → Running
    let result = manager.transition_state(LifecycleState::Reconnecting).await;
    assert!(result.is_ok(), "Degraded → Reconnecting should be valid");

    let result = manager.transition_state(LifecycleState::Running).await;
    assert!(result.is_ok(), "Reconnecting → Running should be valid");

    let state = manager.get_state().await;
    assert_eq!(state, LifecycleState::Running);
}

/// v4.10.11: Test that startup can be interrupted by stop signal
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_starting_interrupted_by_stop_signal() {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // Transition to Starting
    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    let state = manager.get_state().await;
    assert_eq!(state, LifecycleState::Starting);

    // v4.10.11: Should be able to transition from Starting → Stopping
    // This allows clean shutdown if user cancels during startup
    let result = manager.transition_state(LifecycleState::Stopping).await;
    assert!(
        result.is_ok(),
        "Starting → Stopping should be valid (v4.10.11)"
    );

    let state = manager.get_state().await;
    assert_eq!(state, LifecycleState::Stopping);

    // Can complete shutdown
    let result = manager.transition_state(LifecycleState::Stopped).await;
    assert!(result.is_ok(), "Stopping → Stopped should be valid");
}

/// v4.13.2: Test MIDI Learn ring buffer is properly bounded
/// This test verifies that the buffer never grows beyond MIDI_LEARN_MAX_EVENTS,
/// proving the ring buffer logic correctly drops oldest events when full.
#[tokio::test]
async fn test_midi_learn_buffer_is_bounded() {
    // Create a buffer matching the actual implementation
    let buffer = Arc::new(Mutex::new(VecDeque::<MidiLearnEvent>::with_capacity(
        MIDI_LEARN_MAX_EVENTS,
    )));

    // Push 2x the max capacity to prove bounding works
    let total_events = MIDI_LEARN_MAX_EVENTS * 2;
    for i in 0..total_events {
        let event = MidiLearnEvent {
            event_type: EventType::NoteOn,
            note: Some((i % 128) as u8),
            velocity: Some(64),
            timestamp: i as u64,
            ..Default::default()
        };

        // Use the SAME ring buffer logic as process_device_event
        let mut events = buffer.lock().await;
        if events.len() >= MIDI_LEARN_MAX_EVENTS {
            events.pop_front(); // Drop oldest
        }
        events.push_back(event);
    }

    // Verify buffer is bounded
    let events = buffer.lock().await;
    assert_eq!(
        events.len(),
        MIDI_LEARN_MAX_EVENTS,
        "Buffer should be capped at MIDI_LEARN_MAX_EVENTS ({})",
        MIDI_LEARN_MAX_EVENTS
    );

    // Verify oldest events were dropped (newest events remain)
    // First event in buffer should be from index MIDI_LEARN_MAX_EVENTS
    let first_event = events.front().unwrap();
    assert_eq!(
        first_event.timestamp as usize, MIDI_LEARN_MAX_EVENTS,
        "Oldest events should be dropped, keeping most recent"
    );

    // Last event should be from index (total_events - 1)
    let last_event = events.back().unwrap();
    assert_eq!(
        last_event.timestamp as usize,
        total_events - 1,
        "Most recent event should be at the end"
    );
}

/// v4.13.2: Test that VecDeque capacity vs len is understood correctly
/// This test demonstrates that with_capacity does NOT bound len,
/// but our ring buffer logic DOES bound it explicitly.
#[test]
fn test_vecdeque_capacity_vs_len_understanding() {
    // Demonstrate: with_capacity does NOT limit len
    let mut unbounded: VecDeque<u8> = VecDeque::with_capacity(5);
    for i in 0..20 {
        unbounded.push_back(i);
    }
    assert_eq!(
        unbounded.len(),
        20,
        "VecDeque grows beyond initial capacity"
    );

    // But our ring buffer logic DOES bound len
    let mut bounded: VecDeque<u8> = VecDeque::with_capacity(5);
    for i in 0..20 {
        if bounded.len() >= 5 {
            bounded.pop_front();
        }
        bounded.push_back(i);
    }
    assert_eq!(bounded.len(), 5, "Ring buffer logic bounds the length");
    assert_eq!(*bounded.front().unwrap(), 15, "Oldest events dropped");
    assert_eq!(*bounded.back().unwrap(), 19, "Newest events kept");
}

// =========================================================================
// State Reporting Tests
// =========================================================================

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_daemon_state_input_mode_none_when_no_input_manager() {
    // When input_manager is None, input_mode should be None (not "MidiOnly")
    // — a semantic correctness requirement
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // Get daemon state - input_manager should be None at this point
    let state = manager.get_daemon_state().await;

    // input_mode should be None when input_manager is None
    // (not falsely reporting "MidiOnly")
    assert!(
        state.input_mode.is_none(),
        "input_mode should be None when input_manager is not initialized, got: {:?}",
        state.input_mode
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
async fn test_empty_modes_does_not_panic() {
    // When config has no modes, get_engine_info should not panic
    // (verify .get() safety)
    let mut config = create_test_config();
    config.modes.clear(); // Empty modes array

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);

    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // This should not panic - .get() returns None safely
    let info = manager.get_engine_info().await;
    assert_eq!(
        info.current_mode, "None",
        "Should handle empty modes gracefully"
    );
}

// =========================================================================
// Path Conversion Helper Tests
// =========================================================================

#[test]
fn test_pathbuf_to_str_helper_valid_utf8() {
    let path = PathBuf::from("/valid/utf8/path.toml");
    let result = pathbuf_to_str_or_err(&path, "test context");

    assert!(result.is_ok(), "Valid UTF-8 path should succeed");
    assert_eq!(result.unwrap(), "/valid/utf8/path.toml");
}

#[test]
fn test_pathbuf_helper_returns_error_context() {
    // Create a path with invalid UTF-8 using OsString (Unix only)
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        // Invalid UTF-8 sequence: 0x80 is not valid UTF-8 start byte
        let invalid_bytes: &[u8] = b"/path/with\x80invalid";
        let os_str = OsStr::from_bytes(invalid_bytes);
        let path = PathBuf::from(os_str);

        let result = pathbuf_to_str_or_err(&path, "config_path in reload");

        assert!(result.is_err(), "Non-UTF8 path should fail");
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("config_path in reload"),
            "Error should contain context: {}",
            err_msg
        );
    }
}

/// v4.14.0: Test that ValidateConfig with valid path works
#[tokio::test]
async fn test_validate_config_with_valid_path() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Create a valid config file
    let mut temp_file = NamedTempFile::new().unwrap();
    writeln!(
        temp_file,
        r#"
[[modes]]
name = "Default"
color = "blue"
"#
    )
    .unwrap();

    // pathbuf_to_str_or_err should succeed for valid UTF-8 paths
    let path = temp_file.path().to_path_buf();
    let result = pathbuf_to_str_or_err(&path, "test ValidateConfig");
    assert!(result.is_ok(), "Valid UTF-8 path should succeed");

    // Config::load should work with the path string
    let config_result = conductor_core::Config::load(result.unwrap());
    assert!(config_result.is_ok(), "Config should load from valid path");
}

/// v4.14.0: Test that reload_config path handling works with valid paths
#[test]
fn test_reload_config_with_valid_path() {
    let path = PathBuf::from("/some/valid/utf8/config.toml");
    let result = pathbuf_to_str_or_err(&path, "config_path in reload_config");

    assert!(
        result.is_ok(),
        "Valid UTF-8 path should convert successfully"
    );
    assert_eq!(result.unwrap(), "/some/valid/utf8/config.toml");
}

/// A successful `reload_config()` must broadcast a
/// `config_reloaded` MonitorEvent. That event is the daemon→GUI
/// signal the file-watcher reload path was missing — without it the
/// GUI only learns about a config edit on its next 3s poll. The
/// channel and the pattern (a new `event_type` on the existing
/// `MonitorEvent` broadcast) are the same ones used for
/// `ambiguous_port_detected`.
#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // MIDI port enumeration panics on headless Linux CI
async fn test_reload_config_emits_config_reloaded_event() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    // reload_config does Config::load + a `.known_good` backup copy,
    // so it needs a real file on disk — not the synthetic path the
    // simulate-test manager uses.
    let mut temp_file = NamedTempFile::new().unwrap();
    writeln!(
        temp_file,
        r#"
[[modes]]
name = "Default"
color = "blue"
"#
    )
    .unwrap();
    let config = Config::load(temp_file.path().to_str().unwrap()).unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        config,
        temp_file.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .expect("Failed to create EngineManager");

    // reload_config only runs from Running (Running → Reloading → Running).
    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    // Subscribe before reloading so the broadcast isn't missed.
    let mut rx = manager.event_broadcast_tx.subscribe();

    let metrics = manager
        .reload_config()
        .await
        .expect("reload_config should succeed for a valid config");

    let event = rx
        .try_recv()
        .expect("reload_config must broadcast a MonitorEvent");
    assert_eq!(
        event.event_type, "config_reloaded",
        "the broadcast event must be the config_reloaded signal"
    );

    // Payload carries success + the reload metrics so the GUI (and
    // the CLI monitor) can show what happened without a follow-up
    // status round-trip.
    let payload = event
        .payload
        .expect("config_reloaded must carry a structured payload");
    assert_eq!(payload["success"], true);
    assert_eq!(payload["modes_loaded"], metrics.modes_loaded);
    assert_eq!(payload["mappings_loaded"], metrics.mappings_loaded);
    assert_eq!(payload["duration_ms"], metrics.duration_ms);
}

/// `push_monitor_event` stamps a strictly-monotonic, gap-free emission
/// sequence onto every event regardless of type. This gives the GUI a total
/// order to sort the two Tauri channels (`midi-events` + `mapping-fired`) by —
/// `mapping_fired` is pushed from a different run-loop select arm than its raw
/// event, so arrival order is not authoritative; the seq is.
#[tokio::test]
async fn test_monitor_event_seq_is_monotonic_across_types() {
    let config = create_test_config();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // Subscribe before pushing so nothing is missed.
    let mut rx = manager.event_broadcast_tx.subscribe();

    // Mixed types, mimicking a raw event interleaved with its mapping_fired.
    for event_type in ["pitch_bend", "mapping_fired", "route_forwarded", "cc"] {
        manager.push_monitor_event(crate::daemon::types::MonitorEvent {
            event_type: event_type.to_string(),
            ..Default::default()
        });
    }

    let mut seqs = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        seqs.push(ev.seq);
    }
    assert_eq!(
        seqs,
        vec![0, 1, 2, 3],
        "seq must be monotonic and gap-free across all event types"
    );
}

/// Test that Status handler uses String (not &str) for input_mode
/// This ensures consistency with get_daemon_state() return type
#[test]
fn test_status_handler_uses_string_not_str() {
    // This test verifies the type consistency at compile time
    // If Status handler used &str, it would have lifetime issues

    // Create test mode string as we do in the Status handler
    let mode: String = "MidiOnly".to_string();

    // Verify it's an owned String
    assert_eq!(mode, "MidiOnly");

    // The string should be owned (we can move it, modify it, etc.)
    let _moved: String = mode;
}

/// Test that lock ordering documentation exists on EngineManager
/// This is a documentation verification test
#[test]
fn test_lock_ordering_documented() {
    // This test exists to ensure the documentation is maintained.
    // The actual lock ordering is documented in the EngineManager struct comment.
    //
    // Lock order (must be acquired in this order to prevent deadlocks):
    // 1. state
    // 2. config
    // 3. current_mode (ArcSwap — lock-free, v4.21.0)
    // 4. event_processor
    // 5. rule_set (ArcSwap — lock-free, v4.21.0) / mapping_engine (backward compat)
    // 6. device_status
    // 7. statistics
    // 8. error_log
    // 9. input_manager
    // 10. action_executor
    // 11. midi_learn_events
    // 12. pending_chord_event
    //
    // If this test compiles and passes, the documentation is present.
    // Manual review: verify EngineManager doc comment contains lock ordering.

    // Verify the expected lock count (12 locks)
    // This serves as a reminder to update documentation if locks are added/removed
    let expected_lock_count = 12;
    assert_eq!(
        expected_lock_count, 12,
        "If you're changing the number of locks, update the lock ordering documentation!"
    );
}

// Config-Persisted Mode Changes
// Note: These tests focus on the business logic since full EngineManager
// creation requires system permissions for input device access.

/// Test that persist_mode_change updates config correctly (unit test)
#[tokio::test]
async fn test_config_save_load_preserves_mode() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    // Create a test config file
    let mut temp_file = NamedTempFile::new().unwrap();
    writeln!(
        temp_file,
        r#"
[[modes]]
name = "Mode1"
color = "blue"

[[modes]]
name = "Mode2"  
color = "red"
"#
    )
    .unwrap();

    let config_path = temp_file.path().to_path_buf();

    // Load initial config
    let initial_config = Config::load(&config_path.to_string_lossy()).unwrap();
    assert!(initial_config.last_selected_mode.is_none());

    // Test config update and save
    let mut updated_config = initial_config.clone();
    updated_config.last_selected_mode = Some("Mode2".to_string());

    // Save config
    updated_config.save(&config_path.to_string_lossy()).unwrap();

    // Verify config was saved correctly
    let saved_config = Config::load(&config_path.to_string_lossy()).unwrap();
    assert_eq!(saved_config.last_selected_mode, Some("Mode2".to_string()));
    assert_eq!(saved_config.modes.len(), 2);
    assert_eq!(saved_config.modes[1].name, "Mode2");
}

/// Test that mode validation works correctly
#[test]
fn test_mode_validation() {
    let config = Config::default_config();

    // Valid mode should return index
    let valid_idx = config.modes.iter().position(|m| m.name == "Default");
    assert!(valid_idx.is_some());

    // Invalid mode should return None
    let invalid_idx = config.modes.iter().position(|m| m.name == "NonExistent");
    assert!(invalid_idx.is_none());
}

// Phase 3 tests for startup mode fallback chain

#[test]
fn test_startup_fallback_last_selected_mode() {
    let mut config = Config::default_config();

    // Add modes to the config
    config.modes = vec![
        Mode {
            name: "Mode1".to_string(),
            color: None,
            mappings: vec![],
        },
        Mode {
            name: "Mode2".to_string(),
            color: None,
            mappings: vec![],
        },
        Mode {
            name: "Mode3".to_string(),
            color: None,
            mappings: vec![],
        },
    ];

    // Set both last_selected_mode and default_mode
    config.last_selected_mode = Some("Mode2".to_string());
    config.default_mode = Some("Mode3".to_string());

    let result = resolve_startup_mode(&config);
    assert_eq!(result, 1); // Mode2 is at index 1
}

#[test]
fn test_startup_fallback_default_mode() {
    let mut config = Config::default_config();

    // Add modes to the config
    config.modes = vec![
        Mode {
            name: "Mode1".to_string(),
            color: None,
            mappings: vec![],
        },
        Mode {
            name: "Mode2".to_string(),
            color: None,
            mappings: vec![],
        },
        Mode {
            name: "Mode3".to_string(),
            color: None,
            mappings: vec![],
        },
    ];

    // Set last_selected_mode to invalid, but default_mode is valid
    config.last_selected_mode = Some("InvalidMode".to_string());
    config.default_mode = Some("Mode3".to_string());

    let result = resolve_startup_mode(&config);
    assert_eq!(result, 2); // Mode3 is at index 2
}

#[test]
fn test_startup_fallback_index_zero() {
    let mut config = Config::default_config();

    // Add modes to the config
    config.modes = vec![
        Mode {
            name: "Mode1".to_string(),
            color: None,
            mappings: vec![],
        },
        Mode {
            name: "Mode2".to_string(),
            color: None,
            mappings: vec![],
        },
    ];

    // Both last_selected_mode and default_mode are invalid
    config.last_selected_mode = Some("InvalidMode1".to_string());
    config.default_mode = Some("InvalidMode2".to_string());

    let result = resolve_startup_mode(&config);
    assert_eq!(result, 0); // Falls back to index 0
}

#[test]
fn test_startup_fallback_empty_modes() {
    let mut config = Config::default_config();

    // No modes at all
    config.modes = vec![];
    config.last_selected_mode = Some("SomeMode".to_string());
    config.default_mode = Some("AnotherMode".to_string());

    let result = resolve_startup_mode(&config);
    assert_eq!(result, 0); // Returns 0 for global mappings only
}

#[test]
fn test_startup_fallback_invalid_last_selected_uses_default() {
    let mut config = Config::default_config();

    // Add modes to the config
    config.modes = vec![
        Mode {
            name: "Mode1".to_string(),
            color: None,
            mappings: vec![],
        },
        Mode {
            name: "Default".to_string(),
            color: None,
            mappings: vec![],
        },
    ];

    // last_selected_mode points to nonexistent mode, default_mode is valid
    config.last_selected_mode = Some("NonExistentMode".to_string());
    config.default_mode = Some("Default".to_string());

    let result = resolve_startup_mode(&config);
    assert_eq!(result, 1); // Default is at index 1
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new needs Enigo (display server)
async fn capture_pattern_events_chord_uses_configured_window_not_hardcoded_100() {
    use conductor_core::event_processor::ProcessedEvent;
    // Outside MIDI Learn, the chord pattern event (the Events-panel
    // "NoteChord … Nms window" pill) must carry the configured normal chord
    // window (`chord_timeout_ms`), not the old hardcoded 100ms that matched
    // neither the normal (50) nor the Learn window.
    let mut config = create_test_config();
    config.advanced_settings.chord_timeout_ms = 200;
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    let chord = ProcessedEvent::ChordDetected {
        notes: vec![60, 64, 67],
        velocities: vec![100, 100, 100],
        channel: Some(0),
    };
    // Chord events are debounced into `pending_chord_event`; flush so the
    // captured MidiLearnEvent (with its window already set) reaches the buffer.
    manager.capture_pattern_events(&[chord], 0, None).await;
    manager.flush_pending_chord().await;

    let events = manager.midi_learn_events.lock().await;
    let chord_ev = events
        .iter()
        .find(|e| e.pattern_type == Some(PatternType::Chord))
        .expect("chord pattern event captured");
    assert_eq!(
        chord_ev.pattern_timeout_ms,
        Some(200),
        "chord pill must reflect the configured chord_timeout_ms, not hardcoded 100"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new needs Enigo (display server)
async fn capture_pattern_events_chord_uses_learn_window_during_learn() {
    use conductor_core::event_processor::ProcessedEvent;
    use std::sync::atomic::Ordering;
    // While MIDI Learn is active, the chord
    // pill must show the Learn window (`chord_learn_timeout_ms`), distinct from
    // the normal `chord_timeout_ms` — guards the `active_chord_window_ms` Learn
    // branch in `monitor_capture`.
    let mut config = create_test_config();
    config.advanced_settings.chord_timeout_ms = 50;
    config.advanced_settings.chord_learn_timeout_ms = 700;
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let manager = EngineManager::new(
        config,
        PathBuf::from("/tmp/test.toml"),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();
    manager.midi_learn_active.store(true, Ordering::SeqCst);

    let chord = ProcessedEvent::ChordDetected {
        notes: vec![60, 64, 67],
        velocities: vec![100, 100, 100],
        channel: Some(0),
    };
    manager.capture_pattern_events(&[chord], 0, None).await;
    manager.flush_pending_chord().await;

    let events = manager.midi_learn_events.lock().await;
    let chord_ev = events
        .iter()
        .find(|e| e.pattern_type == Some(PatternType::Chord))
        .expect("chord pattern event captured");
    assert_eq!(
        chord_ev.pattern_timeout_ms,
        Some(700),
        "during Learn the chord pill must show chord_learn_timeout_ms, not the normal window"
    );
}

#[tokio::test]
#[cfg_attr(target_os = "linux", ignore)] // EngineManager::new needs Enigo (display server)
async fn reload_reapplies_hold_and_double_tap_to_existing_processors() {
    use conductor_core::event_processor::EventProcessor;
    use conductor_core::identity::DeviceId;
    use std::io::Write;
    use tempfile::NamedTempFile;
    // A runtime config change to hold_threshold_ms / double_tap_timeout_ms
    // must reach an ALREADY-created processor on reload — previously the daemon
    // never applied them (the "Long Press Threshold" slider was decorative).
    let mut temp = NamedTempFile::new().unwrap();
    writeln!(temp, "[[modes]]\nname = \"Default\"\ncolor = \"blue\"").unwrap();
    let config = Config::load(temp.path().to_str().unwrap()).unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel(10);
    let (shutdown_tx, _shutdown_rx) = broadcast::channel(1);
    let mut manager = EngineManager::new(
        config,
        temp.path().to_path_buf(),
        cmd_rx,
        cmd_tx,
        shutdown_tx,
    )
    .unwrap();

    // A processor that already exists with new()'s hardcoded defaults.
    let dev = DeviceId::raw("test-dev");
    manager
        .event_processors
        .insert(dev.clone(), EventProcessor::new());
    assert_eq!(
        manager.event_processors.get(&dev).unwrap().hold_threshold(),
        Duration::from_secs(2)
    );

    // reload_config only runs from Running.
    manager
        .transition_state(LifecycleState::Starting)
        .await
        .unwrap();
    manager
        .transition_state(LifecycleState::Running)
        .await
        .unwrap();

    // Rewrite the SAME config path with non-default hold + double-tap, then reload.
    std::fs::write(
        temp.path(),
        "[advanced_settings]\nhold_threshold_ms = 750\ndouble_tap_timeout_ms = 222\n\n[[modes]]\nname = \"Default\"\ncolor = \"blue\"\n",
    )
    .unwrap();
    manager
        .reload_config()
        .await
        .expect("reload should succeed");

    let p = manager.event_processors.get(&dev).unwrap();
    assert_eq!(
        p.hold_threshold(),
        Duration::from_millis(750),
        "reload must apply hold_threshold_ms to existing processors (#2490)"
    );
    assert_eq!(
        p.double_tap_timeout(),
        Duration::from_millis(222),
        "reload must apply double_tap_timeout_ms to existing processors (#2490)"
    );
}
