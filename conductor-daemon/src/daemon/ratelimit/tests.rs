// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Integration tests for rate limiting

use super::*;
use crate::daemon::mcp_types::ToolRiskTier;
use std::time::Duration;

#[test]
fn test_rate_limiter_per_tier_isolation() {
    let config = RateLimitConfig {
        enabled: true,
        window_secs: 60,
        tier_limits: TierLimits {
            read_only: 5,
            stateful: 3,
            config_change: 2,
            hardware_io: 1,
            privileged: 1,
        },
        global_limit: 0,
    };

    let limiter = RateLimiter::with_config(config);

    // Use all of each tier's limit
    for _ in 0..5 {
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
    }
    for _ in 0..3 {
        limiter
            .check_and_record("client1", ToolRiskTier::Stateful)
            .unwrap();
    }
    for _ in 0..2 {
        limiter
            .check_and_record("client1", ToolRiskTier::ConfigChange)
            .unwrap();
    }
    limiter
        .check_and_record("client1", ToolRiskTier::HardwareIO)
        .unwrap();

    // All tiers should now be exhausted
    assert!(
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .is_err()
    );
    assert!(
        limiter
            .check_and_record("client1", ToolRiskTier::Stateful)
            .is_err()
    );
    assert!(
        limiter
            .check_and_record("client1", ToolRiskTier::ConfigChange)
            .is_err()
    );
    assert!(
        limiter
            .check_and_record("client1", ToolRiskTier::HardwareIO)
            .is_err()
    );

    // Privileged should still have capacity
    assert!(
        limiter
            .check_and_record("client1", ToolRiskTier::Privileged)
            .is_ok()
    );
    assert!(
        limiter
            .check_and_record("client1", ToolRiskTier::Privileged)
            .is_err()
    );
}

#[test]
fn test_rate_limiter_multi_client_fairness() {
    let config = RateLimitConfig {
        enabled: true,
        window_secs: 60,
        tier_limits: TierLimits {
            read_only: 10,
            ..Default::default()
        },
        global_limit: 0,
    };

    let limiter = RateLimiter::with_config(config);

    // Interleave requests from multiple clients
    for i in 0..10 {
        let client = format!("client{}", i % 3);
        limiter
            .check_and_record(&client, ToolRiskTier::ReadOnly)
            .unwrap();
    }

    // Each client should have their own count
    let usage0 = limiter.get_usage("client0");
    let usage1 = limiter.get_usage("client1");
    let usage2 = limiter.get_usage("client2");

    // client0: requests 0, 3, 6, 9 = 4 requests
    // client1: requests 1, 4, 7 = 3 requests
    // client2: requests 2, 5, 8 = 3 requests
    assert_eq!(usage0.get(&ToolRiskTier::ReadOnly).unwrap().0, 4);
    assert_eq!(usage1.get(&ToolRiskTier::ReadOnly).unwrap().0, 3);
    assert_eq!(usage2.get(&ToolRiskTier::ReadOnly).unwrap().0, 3);
}

#[test]
fn test_rate_limiter_error_message_format() {
    let config = RateLimitConfig {
        enabled: true,
        window_secs: 60,
        tier_limits: TierLimits {
            read_only: 1,
            ..Default::default()
        },
        global_limit: 0,
    };

    let limiter = RateLimiter::with_config(config);

    limiter
        .check_and_record("client1", ToolRiskTier::ReadOnly)
        .unwrap();
    let err = limiter
        .check_and_record("client1", ToolRiskTier::ReadOnly)
        .unwrap_err();

    let message = err.to_string();
    assert!(message.contains("Rate limit exceeded"));
    assert!(message.contains("ReadOnly"));
    assert!(message.contains("1/1"));
    assert!(message.contains("Retry after"));
}

#[test]
fn test_rate_limiter_cleanup() {
    let limiter = RateLimiter::new();

    // Add some clients
    limiter
        .check_and_record("client1", ToolRiskTier::ReadOnly)
        .unwrap();
    limiter
        .check_and_record("client2", ToolRiskTier::ReadOnly)
        .unwrap();
    limiter
        .check_and_record("client3", ToolRiskTier::ReadOnly)
        .unwrap();

    assert_eq!(limiter.client_count(), 3);

    // Reset one client
    limiter.reset_client("client2");
    assert_eq!(limiter.client_count(), 2);

    // Cleanup shouldn't remove non-expired entries
    limiter.cleanup_all();
    assert_eq!(limiter.client_count(), 2);
}

/// Without an external `cleanup_all()` scheduler, expired
/// one-shot clients used to accumulate in the `clients` map forever:
/// thousands of unique client IDs that each make one request and never
/// return are a memory-growth vector against the daemon rate limiter.
/// `check_and_record` must now evict expired idle clients
/// opportunistically (amortised, at most once per window) so ordinary
/// daemon activity reclaims them with no external task.
#[test]
fn test_expired_idle_clients_evicted_on_activity() {
    let config = RateLimitConfig {
        window_secs: 1,
        ..RateLimitConfig::default()
    };
    let limiter = RateLimiter::with_config(config);

    // 50 unique one-shot clients, one request each — they never return.
    for i in 0..50 {
        limiter
            .check_and_record(&format!("oneshot-{i}"), ToolRiskTier::ReadOnly)
            .unwrap();
    }
    assert_eq!(limiter.client_count(), 50);

    // Let the 1s window elapse so every recorded entry expires.
    std::thread::sleep(Duration::from_millis(1_100));

    // A single new request must trigger the amortised sweep, evicting
    // the 50 now-expired idle clients and leaving only the new one.
    limiter
        .check_and_record("trigger", ToolRiskTier::ReadOnly)
        .unwrap();
    assert_eq!(
        limiter.client_count(),
        1,
        "expired idle clients should be evicted opportunistically on activity"
    );
}

/// A sub-second remaining window must round UP. The pre-fix
/// `(window - elapsed).as_secs()` truncated, so a client with ~0.4s left
/// was told "retry after 0s" while still rate-limited.
#[test]
fn test_retry_after_never_reports_zero_while_limited() {
    let config = RateLimitConfig {
        window_secs: 1,
        tier_limits: TierLimits {
            read_only: 1,
            ..TierLimits::default()
        },
        ..RateLimitConfig::default()
    };
    let limiter = RateLimiter::with_config(config);

    limiter
        .check_and_record("c", ToolRiskTier::ReadOnly)
        .unwrap();
    // ~0.6s into the 1s window → ~0.4s remains, which truncated to 0.
    std::thread::sleep(Duration::from_millis(600));
    let err = limiter
        .check_and_record("c", ToolRiskTier::ReadOnly)
        .unwrap_err();

    match err {
        RateLimitError::Exceeded {
            retry_after_secs, ..
        } => assert!(
            retry_after_secs >= 1,
            "sub-second remaining must round up to >= 1, not truncate to 0; got {retry_after_secs}"
        ),
    }
}

/// `Instant::now() - window` panics when system uptime is shorter
/// than the window (e.g. a daemon serving requests in its first minute
/// after boot). A window larger than the process lifetime must not panic;
/// every recorded entry is simply within the window.
#[test]
fn test_no_panic_when_window_exceeds_uptime() {
    let config = RateLimitConfig {
        window_secs: u64::MAX,
        ..RateLimitConfig::default()
    };
    let limiter = RateLimiter::with_config(config);

    // Each call derives a cutoff from `Instant::now() - window`, which
    // underflows for a u64::MAX-second window and panicked pre-fix.
    limiter
        .check_and_record("c", ToolRiskTier::ReadOnly)
        .unwrap();
    let _ = limiter.check("c", &ToolRiskTier::ReadOnly);
    let _ = limiter.get_usage("c");
    limiter.cleanup_all();

    // The single recorded request is within the (enormous) window.
    assert_eq!(limiter.client_count(), 1);
}

#[test]
fn test_rate_limiter_config_update() {
    let mut limiter = RateLimiter::new();

    assert_eq!(limiter.config().tier_limits.read_only, 100);

    let new_config = RateLimitConfig {
        enabled: true,
        window_secs: 30,
        tier_limits: TierLimits {
            read_only: 50,
            ..Default::default()
        },
        global_limit: 100,
    };

    limiter.update_config(new_config);

    assert_eq!(limiter.config().tier_limits.read_only, 50);
    assert_eq!(limiter.config().window_secs, 30);
    assert_eq!(limiter.config().global_limit, 100);
}

#[test]
fn test_hardware_io_strict_limits() {
    let limiter = RateLimiter::new();

    // HardwareIO has very low default limit (5)
    for i in 1..=5 {
        let result = limiter
            .check_and_record("client1", ToolRiskTier::HardwareIO)
            .unwrap();
        assert!(result.allowed);
        assert_eq!(result.current, i);
        assert_eq!(result.limit, 5);
    }

    // 6th should fail
    let err = limiter
        .check_and_record("client1", ToolRiskTier::HardwareIO)
        .unwrap_err();

    match err {
        RateLimitError::Exceeded { tier, limit, .. } => {
            assert_eq!(tier, ToolRiskTier::HardwareIO);
            assert_eq!(limit, 5);
        }
    }
}

#[test]
fn test_global_limit_prevents_tier_abuse() {
    let config = RateLimitConfig {
        enabled: true,
        window_secs: 60,
        tier_limits: TierLimits {
            read_only: 100,
            stateful: 100,
            config_change: 100,
            hardware_io: 100,
            privileged: 100,
        },
        global_limit: 10, // Low global limit
    };

    let limiter = RateLimiter::with_config(config);

    // Spread requests across tiers
    for _ in 0..10 {
        let tier = match rand_tier() {
            0 => ToolRiskTier::ReadOnly,
            1 => ToolRiskTier::Stateful,
            2 => ToolRiskTier::ConfigChange,
            _ => ToolRiskTier::HardwareIO,
        };
        limiter.check_and_record("client1", tier).unwrap();
    }

    // 11th request should fail regardless of tier
    assert!(
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .is_err()
    );
}

fn rand_tier() -> u8 {
    // Simple deterministic "random" for testing
    static mut COUNTER: u8 = 0;
    unsafe {
        COUNTER = (COUNTER + 1) % 4;
        COUNTER
    }
}
