// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

// conductor_send_midi tests
// =========================================================================

#[tokio::test]
async fn test_send_midi_auto_confirms() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "port": "Virtual Output",
        "messages": [
            { "type": "note_on", "channel": 1, "note": 60, "velocity": 100 }
        ]
    });

    let result = executor
        .execute("conductor_send_midi", Some(args), None)
        .await;

    match result {
        ExecutionResult::HardwareIoConfirmation { status, tool_name } => {
            assert_eq!(tool_name, "conductor_send_midi");
            match status {
                ConfirmationStatus::Confirmed { result } => {
                    assert!(result.contains("1 MIDI message"));
                    assert!(result.contains("[90, 3C, 64]"));
                }
                _ => panic!("Expected auto-Confirmed for standard MIDI"),
            }
        }
        _ => panic!("Expected HardwareIoConfirmation result"),
    }
}

#[tokio::test]
async fn test_send_midi_invalid_channel() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "port": "Virtual Output",
        "messages": [
            { "type": "note_on", "channel": 17, "note": 60, "velocity": 100 }
        ]
    });

    let result = executor
        .execute("conductor_send_midi", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(message.contains("Channel 17 out of range"));
        }
        _ => panic!("Expected Error for invalid channel"),
    }
}

// -----------------------------------------------------------------
// ADR-026 Phase 2 — SysEx identity MCP tools
// -----------------------------------------------------------------

#[tokio::test]
async fn test_probe_device_identity_missing_port_name() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let result = executor
        .execute("conductor_probe_device_identity", Some(json!({})), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.contains("port_name"),
                "missing-arg error should mention port_name; got: {}",
                message
            );
        }
        other => panic!("expected Error for missing port_name, got {:?}", other),
    }
}

#[tokio::test]
async fn test_probe_device_identity_auto_confirms_then_errors_without_state() {
    // The Identity Request (`F0 7E 7F 06 01 F7`) is a low-risk
    // universal SysEx message — `SysExValidator::validate()` must
    // categorise it as `IdentityRequest`, which auto-confirms
    // without any user prompt. If the validator misclassifies (e.g.
    // because the F0/F7 frame bytes leak into the payload it
    // sees), the path returns `RequiresConfirmation` and the
    // probe never dispatches — that's the bug the 8th-pass
    // review caught.
    //
    // After auto-confirmation succeeds, the executor tries to
    // dispatch the probe via the daemon command channel. With no
    // `SharedDaemonStateRefs` attached this errors with a clear
    // "Daemon state refs not available" message. That's the only
    // observable signal in this test that the auto-confirm path
    // ran to completion.
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let result = executor
        .execute(
            "conductor_probe_device_identity",
            Some(json!({ "port_name": "fake-port" })),
            None,
        )
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.contains("daemon") || message.contains("Daemon"),
                "expected the post-confirm dispatch to error with a daemon-state-refs \
                 message — if you see a SysEx-confirmation-related error here, the \
                 validator probably saw the F0/F7 frame bytes (regression)",
            );
        }
        ExecutionResult::HardwareIoConfirmation { status, .. } => {
            panic!(
                "probe must auto-confirm: expected Error after confirmation, got \
                 HardwareIoConfirmation status={:?}. RequiresConfirmation here means \
                 the validator misclassified the Identity Request — likely the F0/F7 \
                 frame bytes leaked into the validator input.",
                status
            );
        }
        other => panic!(
            "expected Error after auto-confirm dispatch, got {:?}",
            other
        ),
    }
}

#[tokio::test]
async fn test_get_device_identity_missing_port_name() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let result = executor
        .execute("conductor_get_device_identity", Some(json!({})), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.contains("port_name") || message.contains("daemon"),
                "should error on missing port_name OR missing state refs; got: {}",
                message
            );
        }
        other => panic!("expected Error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_get_device_identity_requires_daemon_state() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let result = executor
        .execute(
            "conductor_get_device_identity",
            Some(json!({ "port_name": "any-port" })),
            None,
        )
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.contains("daemon"),
                "should error on missing state refs; got: {}",
                message
            );
        }
        other => panic!("expected Error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_list_device_identities_requires_daemon_state() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let result = executor
        .execute("conductor_list_device_identities", None, None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(
                message.contains("daemon"),
                "should error on missing state refs; got: {}",
                message
            );
        }
        other => panic!("expected Error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_probe_tool_classified_as_hardware_io() {
    // Verify the risk-tier registration: probe is HardwareIO,
    // get / list are ReadOnly. Catches the regression where a new
    // tool's risk tier mapping is forgotten.
    use crate::daemon::mcp_tools::get_tool_risk_tier;
    use crate::daemon::mcp_types::ToolRiskTier;
    assert_eq!(
        get_tool_risk_tier("conductor_probe_device_identity"),
        ToolRiskTier::HardwareIO
    );
    assert_eq!(
        get_tool_risk_tier("conductor_get_device_identity"),
        ToolRiskTier::ReadOnly
    );
    assert_eq!(
        get_tool_risk_tier("conductor_list_device_identities"),
        ToolRiskTier::ReadOnly
    );
}

#[tokio::test]
async fn test_security_status_classified_as_readonly() {
    // ADR-042 B.7 — `conductor_security_status` reports the
    // network-approval HMAC key's rotation status; it only reads, so it
    // must be ReadOnly (never a mutating tier).
    use crate::daemon::mcp_tools::get_tool_risk_tier;
    use crate::daemon::mcp_types::ToolRiskTier;
    assert_eq!(
        get_tool_risk_tier("conductor_security_status"),
        ToolRiskTier::ReadOnly
    );
}

// ADR-042 B.7 — the `conductor_security_status` payload builders and
// their shape tests live in the sibling `security_status` module. Here we
// only pin the tool's risk-tier classification (above); the executor
// handler is a thin wrapper that audit-logs and returns Success.

#[tokio::test]
async fn test_send_midi_missing_port() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "messages": [
            { "type": "note_on", "channel": 1, "note": 60 }
        ]
    });

    let result = executor
        .execute("conductor_send_midi", Some(args), None)
        .await;

    match result {
        ExecutionResult::Error { message } => {
            assert!(message.contains("port"));
        }
        _ => panic!("Expected Error for missing port"),
    }
}

#[tokio::test]
async fn test_send_midi_multiple_messages() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    let args = json!({
        "port": "Virtual Output",
        "messages": [
            { "type": "note_on", "channel": 1, "note": 60, "velocity": 100 },
            { "type": "cc", "channel": 1, "controller": 7, "value": 64 },
            { "type": "program_change", "channel": 2, "program": 5 }
        ]
    });

    let result = executor
        .execute("conductor_send_midi", Some(args), None)
        .await;

    match result {
        ExecutionResult::HardwareIoConfirmation { status, .. } => match status {
            ConfirmationStatus::Confirmed { result } => {
                assert!(result.contains("3 MIDI message"));
            }
            _ => panic!("Expected Confirmed"),
        },
        _ => panic!("Expected HardwareIoConfirmation"),
    }
}

// P4-05: Rate Limiting Tests

#[tokio::test]
async fn test_rate_limiting_allows_under_limit() {
    use crate::daemon::ratelimit::TierLimits;

    let config = create_test_config();
    let rate_config = RateLimitConfig {
        enabled: true,
        window_secs: 60,
        tier_limits: TierLimits {
            read_only: 100,
            ..Default::default()
        },
        global_limit: 0,
    };

    let executor = ToolExecutor::with_rate_limit_config(live_config_arc(config), rate_config);

    // Should succeed under limit
    let result = executor.execute("conductor_get_config", None, None).await;

    match result {
        ExecutionResult::Success { .. } => {}
        ExecutionResult::RateLimited { .. } => panic!("Should not be rate limited under limit"),
        _ => panic!("Expected Success result"),
    }
}

#[tokio::test]
async fn test_rate_limiting_blocks_over_limit() {
    use crate::daemon::ratelimit::TierLimits;

    let config = create_test_config();
    let rate_config = RateLimitConfig {
        enabled: true,
        window_secs: 60,
        tier_limits: TierLimits {
            read_only: 2, // Very low limit
            ..Default::default()
        },
        global_limit: 0,
    };

    let executor = ToolExecutor::with_rate_limit_config(live_config_arc(config), rate_config);

    // First two requests should succeed
    for _ in 0..2 {
        let result = executor.execute("conductor_get_config", None, None).await;
        match result {
            ExecutionResult::Success { .. } => {}
            _ => panic!("Expected Success for requests under limit"),
        }
    }

    // Third request should be rate limited
    let result = executor.execute("conductor_get_config", None, None).await;

    match result {
        ExecutionResult::RateLimited {
            tier,
            current,
            limit,
            ..
        } => {
            assert_eq!(tier, ToolRiskTier::ReadOnly);
            assert_eq!(current, 2);
            assert_eq!(limit, 2);
        }
        _ => panic!("Expected RateLimited result"),
    }
}

#[tokio::test]
async fn test_rate_limiting_per_tier_isolation() {
    use crate::daemon::ratelimit::TierLimits;

    let config = create_test_config();
    let rate_config = RateLimitConfig {
        enabled: true,
        window_secs: 60,
        tier_limits: TierLimits {
            read_only: 2,
            config_change: 2,
            ..Default::default()
        },
        global_limit: 0,
    };

    let executor = ToolExecutor::with_rate_limit_config(live_config_arc(config), rate_config);

    // Exhaust ReadOnly limit
    for _ in 0..2 {
        executor.execute("conductor_get_config", None, None).await;
    }

    // ConfigChange should still work (different tier)
    let args = json!({
        "mode": "Default",
        "trigger": { "type": "Note", "note": 37, "velocity_min": 1 },
        "action": { "type": "Keystroke", "keys": "v", "modifiers": ["cmd"] }
    });

    let result = executor
        .execute("conductor_create_mapping", Some(args), None)
        .await;

    match result {
        ExecutionResult::PlanCreated { .. } => {}
        ExecutionResult::RateLimited { .. } => {
            panic!("ConfigChange should not be affected by ReadOnly limit")
        }
        _ => panic!("Expected PlanCreated result"),
    }
}

#[tokio::test]
async fn test_rate_limiting_disabled() {
    use crate::daemon::ratelimit::TierLimits;

    let config = create_test_config();
    let rate_config = RateLimitConfig {
        enabled: false, // Rate limiting disabled
        window_secs: 60,
        tier_limits: TierLimits {
            read_only: 1, // Would be exceeded if enabled
            ..Default::default()
        },
        global_limit: 0,
    };

    let executor = ToolExecutor::with_rate_limit_config(live_config_arc(config), rate_config);

    // Should succeed many times when disabled
    for _ in 0..10 {
        let result = executor.execute("conductor_get_config", None, None).await;
        match result {
            ExecutionResult::Success { .. } => {}
            ExecutionResult::RateLimited { .. } => {
                panic!("Should not be rate limited when disabled")
            }
            _ => panic!("Expected Success result"),
        }
    }
}

#[tokio::test]
async fn test_rate_limiting_global_limit() {
    use crate::daemon::ratelimit::TierLimits;

    let config = create_test_config();
    let rate_config = RateLimitConfig {
        enabled: true,
        window_secs: 60,
        tier_limits: TierLimits {
            read_only: 100,
            stateful: 100,
            ..Default::default()
        },
        global_limit: 3, // Low global limit
    };

    let executor = ToolExecutor::with_rate_limit_config(live_config_arc(config), rate_config);

    // Use 3 requests across different tiers
    executor.execute("conductor_get_config", None, None).await;
    executor
        .execute("conductor_start_midi_learn", None, None)
        .await;
    executor.execute("conductor_get_config", None, None).await;

    // 4th request should hit global limit
    let result = executor.execute("conductor_get_config", None, None).await;

    match result {
        ExecutionResult::RateLimited { .. } => {}
        _ => panic!("Expected RateLimited due to global limit"),
    }
}

#[tokio::test]
async fn test_rate_limiter_accessor() {
    let config = create_test_config();
    let executor = ToolExecutor::new(live_config_arc(config));

    // Should be able to access rate limiter for inspection/reset
    let limiter = executor.rate_limiter();
    let usage = limiter.get_usage("local");
    assert!(usage.contains_key(&ToolRiskTier::ReadOnly));
}
