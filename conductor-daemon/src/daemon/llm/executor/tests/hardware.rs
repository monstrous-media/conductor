// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// =========================================================================
// HardwareIO Tier Tests (P4-01)
// =========================================================================

#[tokio::test]
async fn test_hardware_io_sysex_requires_confirmation() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // SysEx with unknown manufacturer requires confirmation
    let args = json!({
        "device": "Test Device",
        "data": [0x55, 0x01, 0x02, 0x03]
    });

    let result = executor
        .execute("conductor_send_sysex", Some(args), None)
        .await;

    match result {
        ExecutionResult::HardwareIoConfirmation { status, tool_name } => {
            assert_eq!(tool_name, "conductor_send_sysex");
            match status {
                ConfirmationStatus::RequiresConfirmation { token, .. } => {
                    assert!(!token.id.is_empty());
                }
                _ => panic!("Expected RequiresConfirmation status"),
            }
        }
        _ => panic!("Expected HardwareIoConfirmation result"),
    }
}

#[tokio::test]
async fn test_hardware_io_sysex_low_risk_auto_approved() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // Identity request is low risk and auto-approved
    let args = json!({
        "device": "Test Device",
        "data": [0x7E, 0x00, 0x06, 0x01]  // Universal Non-Realtime Identity Request
    });

    let result = executor
        .execute("conductor_send_sysex", Some(args), None)
        .await;

    match result {
        ExecutionResult::HardwareIoConfirmation { status, .. } => match status {
            ConfirmationStatus::Confirmed { .. } => {}
            _ => panic!("Expected Confirmed status for low-risk operation"),
        },
        _ => panic!("Expected HardwareIoConfirmation result"),
    }
}

#[tokio::test]
async fn test_hardware_io_sysex_confirmation_flow() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // Step 1: Request without token
    let args1 = json!({
        "device": "Test Device",
        "data": [0x55, 0x01, 0x02, 0x03]
    });

    let result1 = executor
        .execute("conductor_send_sysex", Some(args1), None)
        .await;

    let token_id = match result1 {
        ExecutionResult::HardwareIoConfirmation { status, .. } => match status {
            ConfirmationStatus::RequiresConfirmation { token, .. } => token.id,
            _ => panic!("Expected RequiresConfirmation"),
        },
        _ => panic!("Expected HardwareIoConfirmation"),
    };

    // Step 2: Confirm with token
    let args2 = json!({
        "device": "Test Device",
        "data": [0x55, 0x01, 0x02, 0x03],
        "confirmation_token": token_id
    });

    let result2 = executor
        .execute("conductor_send_sysex", Some(args2), None)
        .await;

    match result2 {
        ExecutionResult::HardwareIoConfirmation { status, .. } => match status {
            ConfirmationStatus::Confirmed { .. } => {}
            _ => panic!("Expected Confirmed status after token submission"),
        },
        _ => panic!("Expected HardwareIoConfirmation"),
    }
}

#[tokio::test]
async fn test_hardware_io_device_reset_requires_confirmation() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "device": "Test Device",
        "reset_type": "factory"
    });

    let result = executor
        .execute("conductor_device_reset", Some(args), None)
        .await;

    match result {
        ExecutionResult::HardwareIoConfirmation { status, tool_name } => {
            assert_eq!(tool_name, "conductor_device_reset");
            match status {
                ConfirmationStatus::RequiresConfirmation {
                    risk_assessment, ..
                } => {
                    assert_eq!(risk_assessment.level, "high");
                    assert!(!risk_assessment.reversible);
                }
                _ => panic!("Expected RequiresConfirmation status"),
            }
        }
        _ => panic!("Expected HardwareIoConfirmation result"),
    }
}

#[tokio::test]
async fn test_hardware_io_blocked_firmware_update() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // Large data block that looks like firmware
    let mut data = vec![0x00, 0x21, 0x09]; // NI manufacturer
    data.extend(vec![0xAA; 2000]); // Large block

    let args = json!({
        "device": "Test Device",
        "data": data
    });

    let result = executor
        .execute("conductor_send_sysex", Some(args), None)
        .await;

    match result {
        ExecutionResult::HardwareIoConfirmation { status, .. } => match status {
            ConfirmationStatus::Blocked { reason } => {
                assert!(reason.contains("Blocked") || reason.contains("firmware"));
            }
            _ => panic!("Expected Blocked status for firmware-like data"),
        },
        _ => panic!("Expected HardwareIoConfirmation result"),
    }
}

#[tokio::test]
async fn test_hardware_io_missing_device_argument() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "data": [0x7E, 0x00, 0x06, 0x01]
        // Missing "device" argument
    });

    let result = executor
        .execute("conductor_send_sysex", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(message.contains("device"));
        }
        _ => panic!("Expected Error for missing device argument"),
    }
}
