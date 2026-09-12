// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// =========================================================================
// Audit Logging Integration Tests (P4-04)
// =========================================================================

#[tokio::test]
async fn test_executor_with_audit_logger() {
    use crate::daemon::audit::{AuditEventType, AuditLogger, AuditQuery};

    let config = create_test_config();
    let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
    let executor = ToolExecutor::with_audit_logger(live_config_arc(config), audit_logger.clone());

    // Execute a read-only tool
    let result = executor.execute("conductor_get_config", None, None).await;
    assert!(matches!(result, ExecutionResult::Success { .. }));

    // Verify audit entry was created
    let entries = audit_logger.query(&AuditQuery::default()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].event_type, AuditEventType::ToolComplete);
    assert_eq!(
        entries[0].tool_name,
        Some("conductor_get_config".to_string())
    );
}

#[tokio::test]
async fn test_audit_logs_plan_creation() {
    use crate::daemon::audit::{AuditEventType, AuditLogger, AuditQuery};

    let config = create_test_config();
    let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
    let executor = ToolExecutor::with_audit_logger(live_config_arc(config), audit_logger.clone());

    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;
    assert!(matches!(result, ExecutionResult::PlanCreated { .. }));

    // Verify plan created audit entry
    let entries = audit_logger
        .query(&AuditQuery {
            event_type: Some(AuditEventType::PlanCreated),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(entries.len(), 1);
}

#[tokio::test]
async fn test_audit_logs_plan_apply() {
    use crate::daemon::audit::{AuditEventType, AuditLogger, AuditQuery};

    let config = create_test_config();
    let config_arc = live_config_arc(config);
    let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
    let executor = ToolExecutor::with_audit_logger(config_arc.clone(), audit_logger.clone());

    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
    });

    let plan_id = match executor
        .execute("conductor_create_mapping", Some(args), None)
        .await
    {
        ExecutionResult::PlanCreated { plan } => plan.id,
        _ => panic!("Expected PlanCreated"),
    };

    // Apply the plan
    executor
        .apply_plan(&plan_id)
        .await
        .expect("Apply should succeed");

    // Verify plan applied audit entry
    let entries = audit_logger
        .query(&AuditQuery {
            event_type: Some(AuditEventType::PlanApplied),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].execution_time.is_some());
}

#[tokio::test]
async fn test_audit_logs_plan_rejection() {
    use crate::daemon::audit::{AuditEventType, AuditLogger, AuditQuery};

    let config = create_test_config();
    let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
    let executor = ToolExecutor::with_audit_logger(live_config_arc(config), audit_logger.clone());

    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
    });

    let plan_id = match executor
        .execute("conductor_create_mapping", Some(args), None)
        .await
    {
        ExecutionResult::PlanCreated { plan } => plan.id,
        _ => panic!("Expected PlanCreated"),
    };

    // Reject the plan
    executor
        .reject_plan(&plan_id)
        .await
        .expect("Reject should succeed");

    // Verify plan rejected audit entry
    let entries = audit_logger
        .query(&AuditQuery {
            event_type: Some(AuditEventType::PlanRejected),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(entries.len(), 1);
}

#[tokio::test]
async fn test_audit_logs_tool_error() {
    use crate::daemon::audit::{AuditLogger, AuditQuery};

    let config = create_test_config();
    let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
    let executor = ToolExecutor::with_audit_logger(live_config_arc(config), audit_logger.clone());

    // Invalid mode should cause an error in plan creation
    let args = json!({
        "mode": "NonExistent",
        "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;
    assert!(matches!(result, ExecutionResult::Error { .. }));

    // Verify error was logged
    let entries = audit_logger
        .query(&AuditQuery {
            errors_only: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_error);
    assert!(
        entries[0]
            .error_message
            .as_ref()
            .unwrap()
            .contains("NonExistent")
    );
}

#[tokio::test]
async fn test_audit_logs_stateful_tool() {
    use crate::daemon::audit::{AuditLogger, AuditQuery, AuditRiskTier as AuditTier};

    let config = create_test_config();
    let audit_logger = Arc::new(AuditLogger::in_memory().unwrap());
    let executor = ToolExecutor::with_audit_logger(live_config_arc(config), audit_logger.clone());

    // Execute a stateful tool
    let result = executor
        .execute("conductor_start_midi_learn", None, None)
        .await;
    assert!(matches!(result, ExecutionResult::Logged { .. }));

    // Verify audit entry was created with correct risk tier
    let entries = audit_logger
        .query(&AuditQuery {
            risk_tier: Some(AuditTier::Stateful),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].tool_name,
        Some("conductor_start_midi_learn".to_string())
    );
}
