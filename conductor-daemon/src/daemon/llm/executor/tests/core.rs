// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

#[tokio::test]
async fn test_tool_executor_readonly_auto_executes() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let result = executor.execute("conductor_get_config", None, None).await;

    match result {
        ExecutionResult::Success { result } => {
            assert!(result.is_error.is_none());
        }
        _ => panic!("Expected Success result for ReadOnly tool"),
    }
}

#[tokio::test]
async fn test_tool_executor_stateful_logs_execution() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // Register conductor_start_midi_learn as Stateful for this test
    let result = executor
        .execute_stateful("conductor_start_midi_learn", None)
        .await;

    match result {
        ExecutionResult::Logged { result, log_entry } => {
            assert!(result.is_error.is_none());
            assert_eq!(log_entry.tool_name, "conductor_start_midi_learn");
        }
        _ => panic!("Expected Logged result for Stateful tool"),
    }

    // Check log was stored
    let log = executor.get_execution_log().await;
    assert_eq!(log.len(), 1);
}

#[tokio::test]
async fn test_tool_executor_config_change_returns_plan() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
        "description": "Paste"
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 1);
            assert!(plan.description.contains("Default"));

            // Plan should be stored
            let pending = executor.list_pending_plans().await;
            assert_eq!(pending.len(), 1);
        }
        _ => panic!("Expected PlanCreated result for ConfigChange tool"),
    }
}

#[tokio::test]
async fn test_apply_plan() {
    let config = create_test_config();
    let config_arc = live_config_arc(config);
    let executor = ToolExecutor::new(config_arc.clone());

    // Create a plan
    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
        "description": "Paste"
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;

    let plan_id = match result {
        ExecutionResult::PlanCreated { plan } => plan.id,
        _ => panic!("Expected PlanCreated"),
    };

    // Apply the plan
    executor
        .apply_plan(&plan_id)
        .await
        .expect("Failed to apply plan");

    // D4.A.3.3.B.1: Verify config was updated via LiveConfig snapshot.
    let snap = config_arc.load();
    let config = snap.config.as_ref();
    assert_eq!(config.modes[0].mappings.len(), 2);
    assert_eq!(
        config.modes[0].mappings[1].description,
        Some("Paste".to_string())
    );

    // Plan should be removed
    let pending = executor.list_pending_plans().await;
    assert!(pending.is_empty());
}

#[tokio::test]
async fn test_reject_plan() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // Create a plan
    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;

    let plan_id = match result {
        ExecutionResult::PlanCreated { plan } => plan.id,
        _ => panic!("Expected PlanCreated"),
    };

    // Reject the plan
    executor
        .reject_plan(&plan_id)
        .await
        .expect("Failed to reject plan");

    // Plan should be removed
    let pending = executor.list_pending_plans().await;
    assert!(pending.is_empty());
}

#[tokio::test]
async fn test_create_mapping_validates_trigger_format() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // Invalid trigger format
    let args = json!({
        "mode": "Default",
        "trigger": { "type": "InvalidType" },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(message.contains("trigger") || message.contains("Trigger"));
        }
        _ => panic!("Expected Error for invalid trigger"),
    }
}

#[tokio::test]
async fn test_create_mapping_validates_action_format() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // Invalid action format
    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
        "action": { "type": "InvalidAction" }
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(message.contains("action") || message.contains("Action"));
        }
        _ => panic!("Expected Error for invalid action"),
    }
}

/// ADR-038: conductor_create_mapping accepts `let_through` + a `Tap`
/// action and threads both into the planned ConfigChange.
#[tokio::test]
async fn test_create_mapping_threads_let_through_and_tap() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 40 },
        "action": { "type": "Tap", "message": "note {note} vel {velocity}" },
        "let_through": true
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;

    let ExecutionResult::PlanCreated { plan } = result else {
        panic!("expected PlanCreated, got {result:?}");
    };
    match &plan.changes[0] {
        ConfigChange::CreateMapping {
            action,
            let_through,
            ..
        } => {
            assert!(*let_through, "let_through arg must thread into the change");
            assert!(
                matches!(action, conductor_core::ActionConfig::Tap { message } if message == "note {note} vel {velocity}"),
                "Tap action must parse from the create_mapping arg, got {action:?}"
            );
        }
        other => panic!("expected CreateMapping, got {other:?}"),
    }
}

/// `let_through` defaults to false when omitted (pre-ADR-038 swallow).
#[tokio::test]
async fn test_create_mapping_let_through_defaults_false() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 41 },
        "action": { "type": "Keystroke", "keys": "x", "modifiers": [] }
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;

    let ExecutionResult::PlanCreated { plan } = result else {
        panic!("expected PlanCreated, got {result:?}");
    };
    match &plan.changes[0] {
        ConfigChange::CreateMapping { let_through, .. } => {
            assert!(!*let_through, "omitted let_through must default to false");
        }
        other => panic!("expected CreateMapping, got {other:?}"),
    }
}

#[tokio::test]
async fn test_delete_mapping_returns_plan() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "mode": "Default",
        "index": 0
    });

    let result = executor
        .execute("conductor_delete_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 1);
            match &plan.changes[0] {
                ConfigChange::DeleteMapping { mode, index } => {
                    assert_eq!(mode, "Default");
                    assert_eq!(*index, 0);
                }
                _ => panic!("Expected DeleteMapping change"),
            }
        }
        _ => panic!("Expected PlanCreated result"),
    }
}

#[tokio::test]
async fn test_update_mapping_returns_plan() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "mode": "Default",
        "index": 0,
        "trigger": { "type": "Note", "note": 36, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "x", "modifiers": ["cmd"] },
        "description": "Cut"
    });

    let result = executor
        .execute("conductor_update_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 1);
            match &plan.changes[0] {
                ConfigChange::UpdateMapping {
                    mode,
                    index,
                    description,
                    ..
                } => {
                    assert_eq!(mode, "Default");
                    assert_eq!(*index, 0);
                    assert_eq!(description, &Some("Cut".to_string()));
                }
                _ => panic!("Expected UpdateMapping change"),
            }
        }
        _ => panic!("Expected PlanCreated result"),
    }
}

/// `cleanup_expired_plans` removes exactly the expired plans — live
/// plans with future `expires_at` must survive the sweep.
#[tokio::test]
async fn test_cleanup_expired_plans_removes_only_expired() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // Two pending plans
    for note in [37, 38] {
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": note, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
        });
        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;
        assert!(matches!(result, ExecutionResult::PlanCreated { .. }));
    }

    // Force one past its TTL
    let expired_id = {
        let mut plans = executor.pending_plans.write().await;
        let id = *plans.keys().next().expect("two plans pending");
        plans.get_mut(&id).unwrap().expires_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        id
    };

    executor.cleanup_expired_plans().await;

    let pending = executor.list_pending_plans().await;
    assert_eq!(pending.len(), 1, "only the expired plan is swept");
    assert!(pending.iter().all(|p| p.id != expired_id));
}

/// `clear_execution_log` empties the log; entries from prior stateful
/// executions must not linger.
#[tokio::test]
async fn test_clear_execution_log_empties_log() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let _ = executor
        .execute_stateful("conductor_start_midi_learn", None)
        .await;
    assert_eq!(executor.get_execution_log().await.len(), 1);

    executor.clear_execution_log().await;
    assert!(executor.get_execution_log().await.is_empty());
}

/// `summarize_result` maps the is_error flag to the exact audit-log
/// strings: Some(true) → "Error", Some(false)/None → "Success".
#[tokio::test]
async fn test_summarize_result_maps_error_flag() {
    use crate::daemon::mcp_types::ToolCallResult;
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let ok = ToolCallResult {
        content: vec![],
        is_error: None,
    };
    let explicit_ok = ToolCallResult {
        content: vec![],
        is_error: Some(false),
    };
    let err = ToolCallResult {
        content: vec![],
        is_error: Some(true),
    };

    assert_eq!(executor.summarize_result(&ok), "Success");
    assert_eq!(executor.summarize_result(&explicit_ok), "Success");
    assert_eq!(executor.summarize_result(&err), "Error");
}

#[tokio::test]
async fn test_batch_changes_with_create_route_operation() {
    // ADR-031 P3 § 5.4 — a batch containing a
    // single `create_route` op produces a one-change plan with
    // the `CreateRoute` variant populated end-to-end (JSON parse
    // → ConfigChange enum → plan storage).
    use crate::daemon::llm::ConfigChange;
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": [{
            "type": "create_route",
            "from": "mikro",
            "to": "absynth",
            "description": "split lower keys"
        }]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 1);
            match &plan.changes[0] {
                ConfigChange::CreateRoute {
                    from,
                    to,
                    enabled,
                    description,
                    ..
                } => {
                    assert_eq!(from, "mikro");
                    assert_eq!(to, "absynth");
                    assert!(enabled, "enabled defaults to true");
                    assert_eq!(description.as_deref(), Some("split lower keys"));
                }
                other => panic!("Expected CreateRoute, got {other:?}"),
            }
        }
        other => panic!("Expected PlanCreated, got {other:?}"),
    }
}

#[tokio::test]
async fn test_batch_changes_rejects_create_route_missing_from() {
    // Required-field enforcement at batch-dispatch time — the
    // user gets a clear error rather than a synthetic empty-from
    // plan.
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": [{
            "type": "create_route",
            "to": "absynth"
        }]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.to_lowercase().contains("from"),
                "error message must name the missing field; got: {message}"
            );
        }
        other => panic!("Expected Error for missing 'from', got {other:?}"),
    }
}

#[tokio::test]
async fn test_batch_changes_with_update_route_operation() {
    // ADR-031 P3 § 5.4 — `update_route` op
    // produces a `ConfigChange::UpdateRoute` populated end-to-end
    // (JSON parse → enum → plan).
    use crate::daemon::llm::ConfigChange;
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": [{
            "type": "update_route",
            "index": 0,
            "from": "mikro",
            "to": "absynth",
            "enabled": false,
            "description": "muted route"
        }]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 1);
            match &plan.changes[0] {
                ConfigChange::UpdateRoute {
                    index,
                    from,
                    to,
                    enabled,
                    description,
                    ..
                } => {
                    assert_eq!(*index, 0);
                    assert_eq!(from, "mikro");
                    assert_eq!(to, "absynth");
                    assert!(!enabled, "explicit enabled=false must propagate");
                    assert_eq!(description.as_deref(), Some("muted route"));
                }
                other => panic!("Expected UpdateRoute, got {other:?}"),
            }
        }
        other => panic!("Expected PlanCreated, got {other:?}"),
    }
}

#[tokio::test]
async fn test_batch_changes_rejects_update_route_missing_index() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": [{
            "type": "update_route",
            "from": "a",
            "to": "b"
        }]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.to_lowercase().contains("index"),
                "error must mention missing 'index'; got: {message}"
            );
        }
        other => panic!("Expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_batch_changes_with_delete_route_operation() {
    // ADR-031 P3 § 5.4 — `delete_route` op
    // parses the required `index` field and lands a
    // `ConfigChange::DeleteRoute` in the plan.
    use crate::daemon::llm::ConfigChange;
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": [{
            "type": "delete_route",
            "index": 0
        }]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 1);
            match &plan.changes[0] {
                ConfigChange::DeleteRoute { index } => {
                    assert_eq!(*index, 0);
                }
                other => panic!("Expected DeleteRoute, got {other:?}"),
            }
        }
        other => panic!("Expected PlanCreated, got {other:?}"),
    }
}

#[tokio::test]
async fn test_batch_changes_rejects_delete_route_missing_index() {
    // `index` is required for DeleteRoute — there's no sensible
    // default (which route would it delete?). The arm must
    // surface a clear "missing index" error.
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": [{ "type": "delete_route" }]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.to_lowercase().contains("index"),
                "error must mention the missing field; got: {message}"
            );
        }
        other => panic!("Expected Error for missing 'index', got {other:?}"),
    }
}

#[tokio::test]
async fn test_batch_changes_create_route_alongside_create_mapping() {
    // The whole point of batch is that you can combine route +
    // mapping creation in one approval round-trip. Pins that
    // shape: 1 route + 1 mapping produce a single 2-change plan.
    use crate::daemon::llm::ConfigChange;
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": [
            {
                "type": "create_route",
                "from": "mikro",
                "to": "absynth"
            },
            {
                "type": "create_mapping",
                "mode": "Default",
                "trigger": { "type": "Note", "note": 40, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
            }
        ]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 2);
            assert!(
                matches!(plan.changes[0], ConfigChange::CreateRoute { .. }),
                "first change must be CreateRoute"
            );
            assert!(
                matches!(plan.changes[1], ConfigChange::CreateMapping { .. }),
                "second change must be CreateMapping"
            );
        }
        other => panic!("Expected PlanCreated, got {other:?}"),
    }
}
