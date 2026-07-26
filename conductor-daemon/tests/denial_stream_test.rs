//! ADR-027 D13a — audit denial observability (#1167).
//!
//! Implementation-spec test-matrix case 1J:
//!   - Denial event emitted on the live stream when a tool is denied.
//!   - The streamed event carries the reason + caller identity.
//!   - **Slow consumer**: events stay durable in BOTH the broadcast
//!     ring and the persistent SQLite log; when the consumer falls
//!     behind, `Lagged` is surfaced explicitly rather than silently
//!     dropping.
//!
//! These tests exercise the `AuditLogger` broadcast layer directly —
//! the IPC + conductorctl surface is thin glue on top and is covered
//! by the daemon's own dispatch tests.

// ADR-045 D1 (#2492): exercises the SQLite audit logger; only exists in audit-db builds.
#![cfg(feature = "audit-db")]

use conductor_daemon::daemon::audit::{
    AuditEventType, AuditLogger, AuditQuery, AuditRiskTier, UserContext,
};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// Hard cap on any individual `rx.recv().await` to keep CI from
/// hanging indefinitely if the broadcast / persistence semantics
/// regress. The audit broadcast is bounded and synchronous-on-send,
/// so a healthy logger delivers events well within 100ms even on
/// slow CI runners — 2 s is comfortably generous for the
/// pass-path while still failing fast on a real hang (Council
/// review on PR #1210).
const RECV_TIMEOUT: Duration = Duration::from_secs(2);

/// `AUDIT_BROADCAST_CAPACITY` (1024) ×4. The slow-consumer test
/// needs to flood materially past the ring capacity to force a
/// Lagged signal; 4× is a deliberate margin so the assertion isn't
/// brittle against a 1-or-2-slot capacity tweak. The cap itself
/// is private to the logger module — encoding the multiplier
/// rather than the absolute number keeps the test loosely
/// coupled to the implementation detail (Council review on
/// PR #1210).
const FLOOD_PAST_CAPACITY: u32 = 4096;

/// A denial logged after a subscription is live MUST reach the
/// subscriber.
#[tokio::test]
async fn denial_event_emitted_on_subscribe() {
    let logger = AuditLogger::in_memory().expect("in-memory logger");
    let mut rx = logger.subscribe();

    logger.log_tool_denied(
        "conductor_send_midi",
        AuditRiskTier::HardwareIO,
        "UntrustedPeer: caller not in trust store",
        Some(UserContext::local_user()),
    );

    let entry = tokio::time::timeout(RECV_TIMEOUT, rx.recv())
        .await
        .expect("recv timed out — broadcast did not deliver the denial")
        .expect("should receive the denial entry");
    assert_eq!(entry.tool_name.as_deref(), Some("conductor_send_midi"));
    assert_eq!(
        entry.event_type,
        AuditEventType::ToolDenied,
        "broadcast must preserve the event-type classification",
    );
    assert_eq!(
        entry.risk_tier,
        AuditRiskTier::HardwareIO,
        "broadcast must preserve the risk tier",
    );
    assert!(entry.is_error, "denial entries are flagged is_error");
}

/// The streamed denial event must carry enough to act on: the reason
/// code (error_message) and the caller identity (user_context).
#[tokio::test]
async fn denial_event_includes_reason_and_identity() {
    let logger = AuditLogger::in_memory().expect("in-memory logger");
    let mut rx = logger.subscribe();

    let ctx = UserContext::remote_client("claude-desktop".to_string(), None);
    logger.log_tool_denied(
        "conductor_apply_plan",
        AuditRiskTier::ConfigChange,
        "RequirePlan: ConfigChange tier needs an approved plan",
        Some(ctx),
    );

    let entry = tokio::time::timeout(RECV_TIMEOUT, rx.recv())
        .await
        .expect("recv timed out — broadcast did not deliver the denial")
        .expect("should receive the denial entry");

    // Reason code — the gate decision string.
    assert_eq!(
        entry.error_message.as_deref(),
        Some("RequirePlan: ConfigChange tier needs an approved plan"),
    );
    // Caller identity — CallerContext (D1) projected into UserContext.
    let user = entry.user_context.expect("denial carries caller identity");
    assert_eq!(user.client_id.as_deref(), Some("claude-desktop"));
}

/// Slow-consumer durability: a subscriber that falls behind the
/// broadcast channel capacity MUST see an explicit `Lagged(n)` — and
/// every denial MUST still be recoverable from the persistent SQLite
/// log regardless of how far the live consumer lagged.
#[tokio::test]
async fn slow_consumer_sees_lagged_but_log_stays_durable() {
    let logger = AuditLogger::in_memory().expect("in-memory logger");
    let mut rx = logger.subscribe();

    // Flood well past the broadcast ring capacity without draining
    // `rx`. See `FLOOD_PAST_CAPACITY` for the multiplier rationale.
    let flood = FLOOD_PAST_CAPACITY;
    for i in 0..flood {
        logger.log_tool_denied(
            "conductor_send_midi",
            AuditRiskTier::HardwareIO,
            &format!("denial #{i}"),
            None,
        );
    }

    // The lagging consumer must be TOLD it lagged — not silently
    // handed a truncated view.
    let mut saw_lagged = false;
    loop {
        match rx.try_recv() {
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                assert!(n > 0, "Lagged count must be positive");
                saw_lagged = true;
                // keep draining what's left in the ring
                continue;
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                // Closed means every Sender (logger.event_tx) was
                // dropped. The logger is held in scope for the
                // duration of this test, so Closed here would
                // indicate the broadcast channel internal-state
                // regression that we DO want to catch (Council
                // review on PR #1210).
                panic!("broadcast channel closed unexpectedly — logger still in scope",);
            }
        }
    }
    assert!(
        saw_lagged,
        "a consumer flooded with {flood} events past ring capacity \
         must observe an explicit Lagged signal",
    );

    // Durability: the persistent SQLite log lost nothing — every
    // denial is still queryable even though the live ring dropped
    // most of them for the slow consumer.
    // Filter by event_type (NOT errors_only) — this test's contract
    // is denial-durability, and `errors_only=true` would also catch
    // ToolError / plan-related errors if they ever leak into this
    // test's logger (Council review on PR #1210).
    let denied = logger
        .query(&AuditQuery {
            event_type: Some(AuditEventType::ToolDenied),
            limit: Some(flood + 100),
            ..Default::default()
        })
        .expect("query persistent log");
    assert_eq!(
        denied.len(),
        flood as usize,
        "every denial must survive in the persistent log regardless \
         of broadcast-ring pressure",
    );
}

/// `RecvError::Lagged` is the async-path equivalent of the `try_recv`
/// check above — the daemon's `SubscribeAudit` stream relies on it to
/// emit an explicit lag marker to the CLI client.
#[tokio::test]
async fn async_recv_surfaces_lagged() {
    let logger = AuditLogger::in_memory().expect("in-memory logger");
    let mut rx = logger.subscribe();

    for i in 0..FLOOD_PAST_CAPACITY {
        logger.log_tool_denied(
            "conductor_send_midi",
            AuditRiskTier::HardwareIO,
            &format!("denial #{i}"),
            None,
        );
    }

    // First async recv on a lagged receiver yields Lagged(n).
    match tokio::time::timeout(RECV_TIMEOUT, rx.recv()).await {
        Ok(Err(RecvError::Lagged(n))) => assert!(n > 0),
        Ok(other) => panic!("expected Lagged, got {other:?}"),
        Err(_) => panic!("recv timed out — expected Lagged on a flooded subscriber"),
    }
    // After the lag is surfaced, the receiver recovers and keeps
    // delivering the still-buffered tail.
    let recovered = tokio::time::timeout(RECV_TIMEOUT, rx.recv())
        .await
        .expect("recv timed out — receiver did not recover after Lagged");
    assert!(
        recovered.is_ok(),
        "receiver recovers after surfacing Lagged",
    );
}

/// `conductorctl audit denied` maps to a `QueryAudit` with
/// `event_type = ToolDenied`. Verify the query-side filter actually
/// excludes non-denial events so the denied stream is clean.
#[tokio::test]
async fn denied_only_query_excludes_non_denial_events() {
    let logger = AuditLogger::in_memory().expect("in-memory logger");

    // A mix: one denial, one successful tool start, one completion.
    logger.log_tool_denied(
        "conductor_send_midi",
        AuditRiskTier::HardwareIO,
        "Deny(UntrustedPeer)",
        None,
    );
    logger.log_tool_start("conductor_get_config", AuditRiskTier::ReadOnly, None, None);
    logger.log_tool_complete(
        "conductor_get_config",
        AuditRiskTier::ReadOnly,
        None,
        None,
        std::time::Duration::from_millis(2),
        None,
    );

    // denied_only → event_type filter on ToolDenied.
    let denied = logger
        .query(&AuditQuery {
            event_type: Some(AuditEventType::ToolDenied),
            limit: Some(100),
            ..Default::default()
        })
        .expect("query");
    assert_eq!(denied.len(), 1, "only the denial should match");
    assert_eq!(denied[0].event_type, AuditEventType::ToolDenied);

    // Unfiltered → exactly the three events we logged (one denial,
    // one start, one complete). Strict `== 3` rather than `>= 3` so
    // the test fails fast if the test scaffolding starts injecting
    // extra audit rows (Council review on PR #1210).
    let all = logger
        .query(&AuditQuery {
            limit: Some(100),
            ..Default::default()
        })
        .expect("query");
    assert_eq!(
        all.len(),
        3,
        "unfiltered query sees exactly the three events we logged",
    );
}
