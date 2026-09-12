// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// =========================================================================
// Undo/Redo Tests (P4-06)
// =========================================================================

#[tokio::test]
async fn test_undo_last_change() {
    let config = create_test_config();
    let config_arc = live_config_arc(config);
    let executor = ToolExecutor::new(config_arc.clone());

    // Create and apply a plan
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

    executor.apply_plan(&plan_id).await.unwrap();

    // Verify mapping was added
    {
        let snap = config_arc.load();
        let config = snap.config.as_ref();
        assert_eq!(config.modes[0].mappings.len(), 2);
    }

    // Should be able to undo
    assert!(executor.can_undo().await);
    assert_eq!(executor.undo_count().await, 1);

    // Undo the change
    let description = executor.undo().await.unwrap();
    assert!(description.contains("Default"));

    // Verify mapping was removed
    {
        let snap = config_arc.load();
        let config = snap.config.as_ref();
        assert_eq!(config.modes[0].mappings.len(), 1);
    }

    // Should no longer be able to undo
    assert!(!executor.can_undo().await);
}

#[tokio::test]
async fn test_redo_undone_change() {
    let config = create_test_config();
    let config_arc = live_config_arc(config);
    let executor = ToolExecutor::new(config_arc.clone());

    // Create and apply a plan
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

    executor.apply_plan(&plan_id).await.unwrap();

    // Undo the change
    executor.undo().await.unwrap();

    // Verify mapping was removed
    {
        let snap = config_arc.load();
        let config = snap.config.as_ref();
        assert_eq!(config.modes[0].mappings.len(), 1);
    }

    // Should be able to redo
    assert!(executor.can_redo().await);
    assert_eq!(executor.redo_count().await, 1);

    // Redo the change
    let description = executor.redo().await.unwrap();
    assert!(description.contains("Default"));

    // Verify mapping was restored
    {
        let snap = config_arc.load();
        let config = snap.config.as_ref();
        assert_eq!(config.modes[0].mappings.len(), 2);
    }

    // Should no longer be able to redo
    assert!(!executor.can_redo().await);
}

#[tokio::test]
async fn test_undo_stack_limit() {
    let config = create_test_config();
    let config_arc = live_config_arc(config);
    let executor = ToolExecutor::new(config_arc.clone());

    // Apply multiple plans
    for i in 0..5 {
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 40 + i, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "a", "modifiers": ["cmd"] },
            "description": format!("Mapping {}", i)
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        executor.apply_plan(&plan_id).await.unwrap();
    }

    // Should have 5 undoable changes
    assert_eq!(executor.undo_count().await, 5);
}

#[tokio::test]
async fn test_undo_nothing_to_undo() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // No changes made, undo should fail
    assert!(!executor.can_undo().await);

    let result = executor.undo().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_redo_nothing_to_redo() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // No undone changes, redo should fail
    assert!(!executor.can_redo().await);

    let result = executor.redo().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_new_change_clears_redo_history() {
    let config = create_test_config();
    let config_arc = live_config_arc(config);
    let executor = ToolExecutor::new(config_arc.clone());

    // Apply two plans
    for i in 0..2 {
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 40 + i, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "a", "modifiers": ["cmd"] },
            "description": format!("Mapping {}", i)
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        executor.apply_plan(&plan_id).await.unwrap();
    }

    // Undo one
    executor.undo().await.unwrap();
    assert_eq!(executor.redo_count().await, 1);

    // Apply new change
    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 50, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "b", "modifiers": ["cmd"] },
        "description": "New mapping"
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;

    let plan_id = match result {
        ExecutionResult::PlanCreated { plan } => plan.id,
        _ => panic!("Expected PlanCreated"),
    };

    executor.apply_plan(&plan_id).await.unwrap();

    // Redo history should be cleared
    assert_eq!(executor.redo_count().await, 0);
    assert!(!executor.can_redo().await);
}

#[tokio::test]
async fn test_undo_summary() {
    let config = create_test_config();
    let config_arc = live_config_arc(config);
    let executor = ToolExecutor::new(config_arc.clone());

    // Apply multiple plans
    for i in 0..3 {
        let args = json!({
            "mode": "Default",
            "trigger": { "type": "Note", "note": 40 + i, "velocity_min": 1 },
            "action": { "type": "Keystroke", "keys": "a", "modifiers": ["cmd"] },
            "description": format!("Mapping {}", i)
        });

        let result = executor
            .execute("conductor_create_mapping", Some(args), None)
            .await;

        let plan_id = match result {
            ExecutionResult::PlanCreated { plan } => plan.id,
            _ => panic!("Expected PlanCreated"),
        };

        executor.apply_plan(&plan_id).await.unwrap();
    }

    // Get summary
    let summary = executor.undo_summary(10).await;
    assert_eq!(summary.len(), 3);

    // Most recent first - descriptions contain the mode name from plan creation
    for s in &summary {
        assert!(s.description.contains("Default"));
        assert_eq!(s.changes_count, 1);
    }
}

#[tokio::test]
async fn test_clear_undo_history() {
    let config = create_test_config();
    let config_arc = live_config_arc(config);
    let executor = ToolExecutor::new(config_arc.clone());

    // Apply a plan
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

    executor.apply_plan(&plan_id).await.unwrap();

    assert!(executor.can_undo().await);

    // Clear history
    executor.clear_undo_history().await;

    assert!(!executor.can_undo().await);
    assert_eq!(executor.undo_count().await, 0);
}
