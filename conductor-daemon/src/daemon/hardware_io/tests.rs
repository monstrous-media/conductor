// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Integration tests for HardwareIO module

use super::*;

#[test]
fn test_end_to_end_sysex_confirmation_flow() {
    let manager = ConfirmationManager::new();

    // Step 1: Request SysEx operation with moderate risk
    let sysex_data = &[0x41, 0x10, 0x00, 0x00, 0x04, 0x01]; // Roland parameter change
    let step1 = manager
        .request_sysex_confirmation("Maschine Mikro", sysex_data, None)
        .unwrap();

    let token_id = match step1 {
        ConfirmationStatus::RequiresConfirmation {
            token,
            risk_assessment,
            message,
        } => {
            // Verify risk assessment
            assert_eq!(risk_assessment.level, "medium");
            assert!(message.contains("confirmation"));
            token.id
        }
        ConfirmationStatus::Confirmed { .. } => {
            // Some operations may auto-approve
            return;
        }
        other => panic!("Unexpected status: {:?}", other),
    };

    // Step 2: Confirm with token
    let step2 = manager
        .request_sysex_confirmation("Maschine Mikro", sysex_data, Some(&token_id))
        .unwrap();

    match step2 {
        ConfirmationStatus::Confirmed { result } => {
            assert!(result.contains("confirmed"));
        }
        other => panic!("Expected Confirmed, got: {:?}", other),
    }
}

#[test]
fn test_end_to_end_device_reset_flow() {
    let manager = ConfirmationManager::new();

    // Step 1: Request factory reset
    let step1 = manager
        .request_reset_confirmation("Launchpad X", "factory", None)
        .unwrap();

    let token_id = match step1 {
        ConfirmationStatus::RequiresConfirmation {
            token,
            risk_assessment,
            ..
        } => {
            assert_eq!(risk_assessment.level, "high");
            assert!(!risk_assessment.reversible);
            assert!(risk_assessment.warnings.len() >= 2);
            token.id
        }
        other => panic!("Expected RequiresConfirmation, got: {:?}", other),
    };

    // Step 2: Confirm
    let step2 = manager
        .request_reset_confirmation("Launchpad X", "factory", Some(&token_id))
        .unwrap();

    match step2 {
        ConfirmationStatus::Confirmed { .. } => {}
        other => panic!("Expected Confirmed, got: {:?}", other),
    }
}

#[test]
fn test_sysex_validation_categories() {
    // Identity request (should be low risk, auto-approve)
    let identity_request = SysExValidator::validate(&[0x7E, 0x00, 0x06, 0x01]);
    assert_eq!(identity_request.category, SysExCategory::IdentityRequest);
    assert!(!identity_request.category.requires_confirmation());

    // Parameter query (should be low risk)
    let param_query = SysExValidator::validate(&[0x00, 0x21, 0x09, 0x60, 0x01]);
    assert_eq!(param_query.category, SysExCategory::ParameterQuery);
    assert!(!param_query.category.requires_confirmation());

    // Unknown manufacturer (should require confirmation)
    let unknown = SysExValidator::validate(&[0x55, 0x01, 0x02]);
    assert_eq!(unknown.category, SysExCategory::UnknownManufacturer);
    assert!(unknown.category.requires_confirmation());
}

#[test]
fn test_manufacturer_detection() {
    // Roland
    let roland = SysExValidator::validate(&[0x41, 0x10, 0x00]);
    assert_eq!(roland.manufacturer_name, Some("Roland".to_string()));

    // Yamaha
    let yamaha = SysExValidator::validate(&[0x43, 0x10, 0x00]);
    assert_eq!(yamaha.manufacturer_name, Some("Yamaha".to_string()));

    // Native Instruments (extended ID)
    let ni = SysExValidator::validate(&[0x00, 0x21, 0x09, 0x00]);
    assert_eq!(ni.manufacturer_name, Some("Native Instruments".to_string()));

    // Universal Non-Realtime
    let universal = SysExValidator::validate(&[0x7E, 0x00, 0x06, 0x01]);
    assert_eq!(
        universal.manufacturer_name,
        Some("Universal Non-Realtime".to_string())
    );
}

#[test]
fn test_dangerous_operation_blocking() {
    let manager = ConfirmationManager::new();

    // Large data that looks like firmware
    let mut firmware_like = vec![0x00, 0x21, 0x09]; // NI manufacturer
    firmware_like.extend(vec![0xAA; 2000]); // Large block

    let result = manager
        .request_sysex_confirmation("Test Device", &firmware_like, None)
        .unwrap();

    match result {
        ConfirmationStatus::Blocked { reason } => {
            assert!(reason.contains("Blocked") || reason.contains("firmware"));
        }
        other => panic!("Expected Blocked, got: {:?}", other),
    }
}

#[test]
fn test_risk_assessment_accuracy() {
    // Factory reset should be high risk and not reversible
    let manager = ConfirmationManager::new();
    let result = manager
        .request_reset_confirmation("Device", "factory", None)
        .unwrap();

    if let ConfirmationStatus::RequiresConfirmation {
        risk_assessment, ..
    } = result
    {
        assert_eq!(risk_assessment.level, "high");
        assert!(!risk_assessment.reversible);
        assert!(
            risk_assessment
                .warnings
                .iter()
                .any(|w| w.contains("erased"))
        );
    } else {
        panic!("Expected RequiresConfirmation");
    }

    // Soft reset should be medium risk and reversible
    let result2 = manager
        .request_reset_confirmation("Device", "soft", None)
        .unwrap();

    if let ConfirmationStatus::RequiresConfirmation {
        risk_assessment, ..
    } = result2
    {
        assert_eq!(risk_assessment.level, "medium");
        assert!(risk_assessment.reversible);
    } else {
        panic!("Expected RequiresConfirmation");
    }
}
