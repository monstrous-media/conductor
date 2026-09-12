// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// conductor_switch_mode tests
// =========================================================================

#[tokio::test]
async fn test_switch_mode_valid_name() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({ "mode": "Default" });
    let result = executor
        .execute("conductor_switch_mode", Some(args), None)
        .await;

    match result {
        ExecutionResult::Logged { result, .. } => {
            assert!(result.is_error.is_none());
            let text = match &result.content[0] {
                crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                _ => panic!("Expected text content"),
            };
            let json: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(json["success"], true);
            assert_eq!(json["mode_name"], "Default");
            assert_eq!(json["mode_index"], 0);
        }
        _ => panic!("Expected Logged result for Stateful tool"),
    }
}

#[tokio::test]
async fn test_switch_mode_invalid_name() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({ "mode": "NonExistent" });
    let result = executor
        .execute("conductor_switch_mode", Some(args), None)
        .await;

    match result {
        ExecutionResult::Logged { result, .. } => {
            assert_eq!(result.is_error, Some(true));
            let text = match &result.content[0] {
                crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                _ => panic!("Expected text content"),
            };
            assert!(text.contains("not found"));
        }
        _ => panic!("Expected Logged result for Stateful tool"),
    }
}

#[tokio::test]
async fn test_switch_mode_missing_argument() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let result = executor
        .execute("conductor_switch_mode", Some(json!({})), None)
        .await;

    match result {
        ExecutionResult::Logged { result, .. } => {
            assert_eq!(result.is_error, Some(true));
            let text = match &result.content[0] {
                crate::daemon::mcp_types::ToolContent::Text { text } => text.clone(),
                _ => panic!("Expected text content"),
            };
            assert!(text.contains("Missing required argument"));
        }
        _ => panic!("Expected Logged result"),
    }
}

// ─── ADR-027 D6: multi-dimensional LLM budget enforcement ─────────

#[tokio::test]
async fn test_budget_halts_after_tool_call_quota() {
    // One tool call allowed for the whole session; the rest stay at ADR
    // defaults (high enough not to interfere with a 2-call test).
    let cfg = conductor_core::security::LlmBudgetConfig {
        max_tool_calls_per_session: 1,
        ..Default::default()
    };
    let mut executor = ToolExecutor::new(live_config_arc(create_test_config()));
    executor.set_budget_state(budget_state(cfg));

    // First ReadOnly call is admitted.
    let first = executor.execute("conductor_get_status", None, None).await;
    assert!(
        !matches!(&first, ExecutionResult::Error { message } if message.contains("budget")),
        "first call should be within budget, got {:?}",
        first
    );

    // Second call exhausts the per-session tool-call quota → halt.
    let second = executor.execute("conductor_get_status", None, None).await;
    match second {
        ExecutionResult::Error { message } => {
            assert!(message.contains("budget exceeded"), "got: {message}");
            assert!(
                message.contains("max_tool_calls_per_session"),
                "got: {message}"
            );
        }
        other => panic!("Expected budget halt Error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_budget_charges_config_change_dimension() {
    // ConfigChange-tier calls have their own per-session quota independent
    // of the total tool-call count.
    let cfg = conductor_core::security::LlmBudgetConfig {
        max_config_changes_per_session: 1,
        ..Default::default()
    };
    let mut executor = ToolExecutor::new(live_config_arc(create_test_config()));
    executor.set_budget_state(budget_state(cfg));

    let mapping = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 60, "channel": 0 },
        "action": { "type": "Keystroke", "keys": ["a"] }
    });

    // First ConfigChange tool call is admitted (charges the dimension to 1).
    let first = executor
        .execute("conductor_create_mapping", Some(mapping.clone()), None)
        .await;
    assert!(
        !matches!(&first, ExecutionResult::Error { message } if message.contains("budget")),
        "first config change should be within budget, got {:?}",
        first
    );

    // Second ConfigChange call trips the config-change quota.
    let second = executor
        .execute("conductor_create_mapping", Some(mapping), None)
        .await;
    match second {
        ExecutionResult::Error { message } => {
            assert!(
                message.contains("max_config_changes_per_session"),
                "got: {message}"
            );
        }
        other => panic!("Expected config-change budget halt, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_no_budget_state_means_no_enforcement() {
    // Without set_budget_state, the executor never charges — the historical
    // behaviour every existing constructor preserves.
    let executor = ToolExecutor::new(live_config_arc(create_test_config()));
    for _ in 0..5 {
        let r = executor.execute("conductor_get_status", None, None).await;
        assert!(
            !matches!(&r, ExecutionResult::Error { message } if message.contains("budget")),
            "unbudgeted executor must not enforce, got {:?}",
            r
        );
    }
}

// ─── ADR-025 Phase 2.H: conductor_set_context_mapping ──────

#[tokio::test]
async fn test_set_context_mapping_pc_creates_plan() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // Use Shell inner actions to keep the test focused on the
    // context-switch shape — SendMidi takes flat fields
    // (`controller`, `value`), not a `params` object, so any
    // SendMidi fixture here would need to match that exactly to
    // avoid silently testing against serde defaults.
    let args = json!({
        "mode": "Default",
        "trigger": { "type": "CC", "cc": 7, "channel": 0 },
        "action": {
            "type": "PcContextSwitch",
            "channel": 0,
            "device": "fcb1010",
            "mappings": {
                "0": { "type": "Shell", "command": "echo preset-0" },
                "12": { "type": "Shell", "command": "echo preset-12" }
            }
        },
        "description": "FCB1010 volume pedal → PC-routed"
    });

    let result = executor
        .execute("conductor_set_context_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 1);
            assert!(plan.description.contains("context"));
        }
        other => panic!("Expected PlanCreated for PcContextSwitch, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_set_context_mapping_cc_creates_plan() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "mode": "Default",
        "trigger": { "type": "CC", "cc": 7, "channel": 0 },
        "action": {
            "type": "CcContextSwitch",
            "cc": 64,
            "channel": 0,
            "device": "keyboard",
            "ranges": [
                { "min": 0, "max": 63, "action": { "type": "Shell", "command": "echo low" } },
                { "min": 64, "max": 127, "action": { "type": "Shell", "command": "echo high" } }
            ]
        }
    });

    let result = executor
        .execute("conductor_set_context_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 1);
        }
        other => panic!("Expected PlanCreated for CcContextSwitch, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_set_context_mapping_rejects_non_context_switch_action() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // A Keystroke action is valid ActionConfig but not a context-
    // switch; this tool should reject it and point the LLM back
    // at the generic create tool.
    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 36 },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
    });

    let result = executor
        .execute("conductor_set_context_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.contains("PcContextSwitch") && message.contains("CcContextSwitch"),
                "error should name the accepted action types, got: {}",
                message
            );
        }
        other => panic!(
            "Expected Error for non-context-switch action, got: {:?}",
            other
        ),
    }
}

#[tokio::test]
async fn test_set_context_mapping_mode_not_found() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "mode": "NonExistent",
        "trigger": { "type": "CC", "cc": 7, "channel": 0 },
        "action": {
            "type": "PcContextSwitch",
            "channel": 0,
            "device": "fcb1010",
            "mappings": {
                "0": { "type": "Shell", "command": "echo a" }
            }
        }
    });

    let result = executor
        .execute("conductor_set_context_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.contains("NonExistent") || message.contains("mode"),
                "expected mode-not-found error, got: {}",
                message
            );
        }
        other => panic!("Expected Error for mode-not-found, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_set_context_mapping_missing_mode_argument() {
    // Distinct from `mode_not_found`: this covers the path where
    // the caller omits `mode` entirely. The error should surface
    // at arg-parsing time, before any mode lookup.
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "trigger": { "type": "CC", "cc": 7, "channel": 0 },
        "action": {
            "type": "PcContextSwitch",
            "channel": 0,
            "device": "fcb1010",
            "mappings": {
                "0": { "type": "Shell", "command": "echo a" }
            }
        }
    });

    let result = executor
        .execute("conductor_set_context_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.contains("mode"),
                "expected missing-mode-argument error, got: {}",
                message
            );
        }
        other => panic!("Expected Error for missing mode argument, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_set_context_mapping_invalid_trigger() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Bogus" },
        "action": {
            "type": "PcContextSwitch",
            "channel": 0,
            "device": "fcb1010",
            "mappings": { "0": { "type": "Shell", "command": "echo a" } }
        }
    });

    let result = executor
        .execute("conductor_set_context_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.to_lowercase().contains("trigger"),
                "expected trigger error, got: {}",
                message
            );
        }
        other => panic!("Expected Error for invalid trigger, got: {:?}", other),
    }
}

// ── Daemon-side MIDI Learn timeout enforcement ───────────────
//
// Before this fix, `conductor_start_learn` was fire-and-forget: it
// flipped `midi_learn_active` to true and returned `timeout_seconds`
// as informational metadata, but the daemon had no timer. The implicit
// contract was "the LLM remembers to call conductor_stop_learn after
// timeout_seconds" — structurally unfulfillable since LLM agent loops
// are stateless across turns and have no async scheduling primitives.
// In practice sessions stayed active forever.

// Short real-time waits (~1.2s) instead of `start_paused` + `advance`
// because the latter doesn't reliably propagate timer wakeups into
// `tokio::spawn`-ed tasks under the current runtime: the spawned
// timer's `sleep().await` parks but never resumes after `advance`
// even with multiple `yield_now()` calls. The smallest timeout the
// public start tool accepts is 1 second (timeout_seconds is u64),
// so the test must wait at least that. Trade-off accepted — tests
// run in ~1.5s each, deterministic, no flakiness observed.

#[tokio::test]
async fn test_start_learn_auto_stops_after_timeout() {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};

    let config = create_test_config();
    let active = Arc::new(AtomicBool::new(false));
    let events = Arc::new(Mutex::new(VecDeque::new()));
    let executor =
        ToolExecutor::with_midi_learn_state(live_config_arc(config), active.clone(), events);

    // timeout_seconds is u64 so the smallest public-API value is 1
    // second. We pass that and wait ~1.2s of real time for the
    // spawned tokio timer's sleep().await to resolve — see the
    // module-level note above the test fixtures explaining why
    // start_paused + advance() doesn't reliably propagate wakes
    // into spawned tasks under the current runtime.
    let args = json!({ "timeout_seconds": 1 });
    let _ = executor
        .execute_stateful("conductor_start_learn", Some(args))
        .await;
    assert!(
        active.load(Ordering::SeqCst),
        "Learn should be active after start"
    );

    // Wait past the deadline. Real time so the spawned timer's
    // `sleep().await` actually resolves.
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    assert!(
        !active.load(Ordering::SeqCst),
        "Learn should be auto-stopped by daemon timeout"
    );
}

#[tokio::test]
async fn test_explicit_stop_cancels_pending_timeout_timer() {
    // Explicit conductor_stop_learn should cancel the timer so a
    // subsequent start can install a fresh one without the old timer
    // firing late and stopping the new session.
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};

    let config = create_test_config();
    let active = Arc::new(AtomicBool::new(false));
    let events = Arc::new(Mutex::new(VecDeque::new()));
    let executor =
        ToolExecutor::with_midi_learn_state(live_config_arc(config), active.clone(), events);

    // Start with a 1s timeout, immediately stop. The first timer
    // should be cancelled so it can't fire later.
    let args = json!({ "timeout_seconds": 1 });
    let _ = executor
        .execute_stateful("conductor_start_learn", Some(args))
        .await;
    let _ = executor
        .execute_stateful("conductor_stop_learn", None)
        .await;
    assert!(!active.load(Ordering::SeqCst), "stop sets active false");

    // Re-start with a longer timeout. Old (cancelled) timer must NOT
    // fire mid-session and prematurely stop us.
    let args = json!({ "timeout_seconds": 10 });
    let _ = executor
        .execute_stateful("conductor_start_learn", Some(args))
        .await;

    // Wait past the FIRST timer's would-be deadline.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert!(
        active.load(Ordering::SeqCst),
        "second session must still be active — old timer should have been cancelled"
    );
}

// Race window between `active.store(true)` and
// `prev.abort()` in start. If the prior timer's sleep wakes during
// that window, its body runs `active.swap(false, ...)` against the
// freshly-started session and silently stops it. Fix: each timer
// captures a session generation; subsequent start bumps the
// generation; the timer's body checks generation match before
// calling swap. This test rapid-restarts WITHOUT an intervening
// explicit stop (the existing test_explicit_stop_cancels_…
// scenario goes through stop, which already invalidated active=false
// so swap was a no-op even without the gen check).
#[tokio::test]
async fn test_subsequent_start_invalidates_prior_timer_via_session_generation() {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};

    let config = create_test_config();
    let active = Arc::new(AtomicBool::new(false));
    let events = Arc::new(Mutex::new(VecDeque::new()));
    let executor =
        ToolExecutor::with_midi_learn_state(live_config_arc(config), active.clone(), events);

    // Start with 1s timeout — T1 spawned.
    let _ = executor
        .execute_stateful(
            "conductor_start_learn",
            Some(json!({ "timeout_seconds": 1 })),
        )
        .await;
    // Immediately re-start with a 10s timeout — T2 replaces T1.
    // T1 may have been aborted before its sleep woke (most likely),
    // OR its body may run if abort lost the race. Either way, the
    // generation check in T1's body must prevent a swap that would
    // stop the freshly-started session.
    let _ = executor
        .execute_stateful(
            "conductor_start_learn",
            Some(json!({ "timeout_seconds": 10 })),
        )
        .await;

    // Wait past T1's would-be deadline.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    assert!(
        active.load(Ordering::SeqCst),
        "T2's session must still be active — T1's stale wake must not stop it"
    );
}

// When the LLM rapid-restarts conductor_start_learn
// (intentionally — user asked it to "run learn each time"), the LLM
// had no signal that it was preempting an active session. The tool
// result looked identical for fresh start vs restart, so the LLM
// didn't acknowledge the restart to the user. Surface a
// `was_already_active` flag in the response so the LLM can reason
// about it and a `message` that explicitly says RESTARTED.
#[tokio::test]
async fn test_start_learn_response_indicates_restart_when_already_active() {
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;

    let config = create_test_config();
    let active = Arc::new(AtomicBool::new(false));
    let events = Arc::new(Mutex::new(VecDeque::new()));
    let executor =
        ToolExecutor::with_midi_learn_state(live_config_arc(config), active.clone(), events);

    // First start: fresh session.
    let r1 = executor
        .execute_stateful(
            "conductor_start_learn",
            Some(json!({"timeout_seconds": 10})),
        )
        .await;
    let r1_text = match r1 {
        ExecutionResult::Logged { result, .. } => extract_text(&result),
        other => panic!("Expected Logged result, got: {:?}", other),
    };
    let r1_json: serde_json::Value = serde_json::from_str(&r1_text).unwrap();
    assert_eq!(
        r1_json.get("was_already_active").and_then(|v| v.as_bool()),
        Some(false),
        "first start: was_already_active should be false (fresh session)"
    );
    let r1_msg = r1_json
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        !r1_msg.to_lowercase().contains("restart"),
        "first start message should not mention restart: {}",
        r1_msg
    );

    // Second start: should be flagged as a restart.
    let r2 = executor
        .execute_stateful(
            "conductor_start_learn",
            Some(json!({"timeout_seconds": 10})),
        )
        .await;
    let r2_text = match r2 {
        ExecutionResult::Logged { result, .. } => extract_text(&result),
        other => panic!("Expected Logged result, got: {:?}", other),
    };
    let r2_json: serde_json::Value = serde_json::from_str(&r2_text).unwrap();
    assert_eq!(
        r2_json.get("was_already_active").and_then(|v| v.as_bool()),
        Some(true),
        "second start: was_already_active should be true (preempted active session)"
    );
    let r2_msg = r2_json
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        r2_msg.to_lowercase().contains("restart"),
        "second start message should explicitly say restarted: {}",
        r2_msg
    );
    // The daemon does NOT drain
    // midi_learn_events on restart — only conductor_stop_learn
    // drains. The message must NOT falsely claim events were
    // discarded; it should explain the buffer semantics instead.
    assert!(
        !r2_msg.to_lowercase().contains("discarded"),
        "restart message must NOT claim events were discarded (daemon does not drain on restart): {}",
        r2_msg
    );
    assert!(
        r2_msg.to_lowercase().contains("buffer"),
        "restart message should explain the events-buffer semantics: {}",
        r2_msg
    );
}
