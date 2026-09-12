// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// =========================================================================
// Batch Operations Tests (P3-07)
// =========================================================================

#[tokio::test]
async fn test_batch_changes_creates_multi_change_plan() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": [
            {
                "type": "create_mapping",
                "mode": "Default",
                "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
                "description": "Paste"
            },
            {
                "type": "create_mapping",
                "mode": "Default",
                "trigger": { "type": "Note", "note": 38, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "z", "modifiers": ["cmd"] },
                "description": "Undo"
            }
        ]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 2);
            assert!(plan.description.contains("2 changes"));
        }
        _ => panic!("Expected PlanCreated result for batch changes"),
    }
}

#[tokio::test]
async fn test_batch_changes_with_multiple_operation_types() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": [
            {
                "type": "create_mapping",
                "mode": "Default",
                "trigger": { "type": "Note", "note": 40, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "a", "modifiers": ["cmd"] }
            },
            {
                "type": "update_mapping",
                "mode": "Default",
                "index": 0,
                "trigger": { "type": "Note", "note": 36, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "x", "modifiers": ["cmd"] },
                "description": "Cut"
            }
        ]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { plan } => {
            assert_eq!(plan.changes.len(), 2);
            match &plan.changes[0] {
                ConfigChange::CreateMapping { mode, .. } => assert_eq!(mode, "Default"),
                _ => panic!("Expected CreateMapping"),
            }
            match &plan.changes[1] {
                ConfigChange::UpdateMapping { index, .. } => assert_eq!(*index, 0),
                _ => panic!("Expected UpdateMapping"),
            }
        }
        _ => panic!("Expected PlanCreated"),
    }
}

#[tokio::test]
async fn test_batch_changes_fails_on_invalid_mode() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": [
            {
                "type": "create_mapping",
                "mode": "NonExistent",
                "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "v", "modifiers": [] }
            }
        ]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(message.contains("NonExistent") || message.contains("Mode not found"));
        }
        _ => panic!("Expected Error for invalid mode"),
    }
}

#[tokio::test]
async fn test_batch_changes_empty_operations_fails() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "operations": []
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(message.contains("empty"));
        }
        _ => panic!("Expected Error for empty operations"),
    }
}

#[tokio::test]
async fn test_apply_batch_plan_atomic() {
    let config = create_test_config();
    let config_arc = live_config_arc(config);
    let executor = ToolExecutor::new(config_arc.clone());

    let args = json!({
        "operations": [
            {
                "type": "create_mapping",
                "mode": "Default",
                "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] },
                "description": "Paste"
            },
            {
                "type": "create_mapping",
                "mode": "Default",
                "trigger": { "type": "Note", "note": 38, "velocity_min": 1 },
                "action": { "type": "Keystroke", "keys": "z", "modifiers": ["cmd"] },
                "description": "Undo"
            }
        ]
    });

    let result = executor
        .execute("conductor_batch_changes", Some(args), None)
        .await;

    let plan_id = match result {
        ExecutionResult::PlanCreated { plan } => plan.id,
        _ => panic!("Expected PlanCreated"),
    };

    // Apply the plan
    let changes_applied = executor
        .apply_plan(&plan_id)
        .await
        .expect("Failed to apply plan");
    assert_eq!(changes_applied, 2);

    // D4.A.3.3.B.1: Verify config was updated via LiveConfig snapshot.
    let snap = config_arc.load();
    let config = snap.config.as_ref();
    assert_eq!(config.modes[0].mappings.len(), 3); // 1 original + 2 new
}
