// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 D5/D1 — the shared admission sequence (`ToolExecutor::admit`).
//!
//! `SecurityPolicy::default()` is shadow-mode under `cfg(test)`, so the
//! IPC-level tests in `engine_manager/tests/mcp_admission.rs` can only
//! observe the rate-limiter and budget halves; the gate half is pinned
//! here with an explicit enforcing policy via `admit_with_policy`.

use super::*;
use crate::daemon::llm::executor::AdmitRoute;
use crate::security::{CallerContext, SecurityPolicy, TrustLevel};

fn enforcing() -> SecurityPolicy {
    SecurityPolicy::with_shadow_mode(false)
}

/// HardwareIO + unpinned peer (synthetic Untrusted) → gate `Deny` under
/// the production decision table, surfaced as the executor's standard
/// gate-denial `ExecutionResult::Error`.
#[tokio::test]
async fn admit_denies_hardware_io_probe_for_unpinned_peer_under_enforcement() {
    let executor = ToolExecutor::new(live_config_arc(create_test_config()));
    let result = executor
        .admit_with_policy("conductor_probe_device_identity", None, &enforcing())
        .await;
    match result {
        Err(ExecutionResult::Error { message }) => assert!(
            message.contains("Gate denied"),
            "denial must carry the gate's reason; got: {message}"
        ),
        other => panic!("expected gate denial for unpinned HardwareIO, got {other:?}"),
    }
}

/// HardwareIO + verified CLI peer → `RequireConfirmation`, surfaced as
/// the `ToHardwareIo` route (the direct probe arm treats this as
/// admitted: its bytes are the hardcoded `IDENTITY_REQUEST`, so the
/// confirmation step is satisfied by construction).
#[tokio::test]
async fn admit_routes_hardware_io_probe_to_confirmation_for_cli_peer() {
    let executor = ToolExecutor::new(live_config_arc(create_test_config()));
    let ctx = CallerContext::new(TrustLevel::CliTrusted);
    let result = executor
        .admit_with_policy("conductor_probe_device_identity", Some(&ctx), &enforcing())
        .await;
    assert_eq!(
        result.expect("trusted HardwareIO caller is admitted"),
        AdmitRoute::ToHardwareIo
    );
}

/// Stateful + unpinned peer → `Allow` (the gate's decision table admits
/// every local caller at this tier); the rate limiter and budget still
/// apply downstream.
#[tokio::test]
async fn admit_allows_stateful_switch_profile_for_unpinned_peer_under_enforcement() {
    let executor = ToolExecutor::new(live_config_arc(create_test_config()));
    let result = executor
        .admit_with_policy("conductor_switch_profile", None, &enforcing())
        .await;
    assert_eq!(
        result.expect("Stateful tier admits unpinned local callers"),
        AdmitRoute::FallThrough
    );
}

/// The HardwareIO tier window (5/60 s by default) is charged by `admit`
/// itself — six admissions in one window must end `RateLimited`.
/// (Runs on every platform, unlike the Enigo-bound IPC-level tests.)
#[tokio::test]
async fn admit_rate_limits_after_the_hardware_io_window_is_spent() {
    let executor = ToolExecutor::new(live_config_arc(create_test_config()));
    for i in 0..5 {
        let result = executor
            .admit("conductor_probe_device_identity", None)
            .await;
        assert!(result.is_ok(), "call {i} is within the window: {result:?}");
    }
    match executor
        .admit("conductor_probe_device_identity", None)
        .await
    {
        Err(ExecutionResult::RateLimited { limit, .. }) => assert_eq!(limit, 5),
        other => panic!("expected RateLimited on the 6th HardwareIO admission, got {other:?}"),
    }
}

/// An attached budget is charged by `admit`, and exhaustion is refused
/// with the standard budget error (HardwareIO also charges the MIDI-out
/// dimension, exactly as `execute()` does).
#[tokio::test]
async fn admit_charges_an_attached_budget_and_refuses_exhaustion() {
    let executor = ToolExecutor::new(live_config_arc(create_test_config()));
    executor.set_budget_state(budget_state(conductor_core::security::LlmBudgetConfig {
        max_midi_out_per_session: 1,
        ..Default::default()
    }));

    assert!(
        executor
            .admit("conductor_probe_device_identity", None)
            .await
            .is_ok(),
        "first probe consumes the single budgeted MIDI-out charge"
    );
    match executor
        .admit("conductor_probe_device_identity", None)
        .await
    {
        Err(ExecutionResult::Error { message }) => assert!(
            message.contains("max_midi_out_per_session"),
            "refusal must name the exhausted dimension; got: {message}"
        ),
        other => panic!("expected budget refusal on the 2nd probe admission, got {other:?}"),
    }
}
