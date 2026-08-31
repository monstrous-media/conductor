// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Sliding window rate limiter implementation

use crate::daemon::mcp_types::ToolRiskTier;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;

/// Default window size (1 minute)
const DEFAULT_WINDOW_SECS: u64 = 60;

/// Every [`ToolRiskTier`] that usage reporting must surface, in a single
/// place so a new tier can't silently drop out of [`RateLimiter::get_usage`]
/// the way `ArtifactRender` did (#2144). The companion
/// [`assert_all_tiers_reported`] is an exhaustiveness guard: because
/// `ToolRiskTier` is deliberately not `#[non_exhaustive]`, adding a variant
/// breaks that `match` and forces this array to be updated.
const ALL_REPORTED_TIERS: [ToolRiskTier; 6] = [
    ToolRiskTier::ReadOnly,
    ToolRiskTier::Stateful,
    ToolRiskTier::ArtifactRender,
    ToolRiskTier::ConfigChange,
    ToolRiskTier::HardwareIO,
    ToolRiskTier::Privileged,
];

/// Compile-time guard for [`ALL_REPORTED_TIERS`] (#2144). Never called: it
/// exists so a newly-added `ToolRiskTier` variant fails to compile here,
/// pointing the author at the array above. Keep one match arm per variant
/// and do NOT add a `_` arm — the whole point is exhaustiveness.
#[allow(dead_code)]
fn assert_all_tiers_reported(tier: ToolRiskTier) {
    // One arm per variant, no `_` arm: a new `ToolRiskTier` breaks this
    // match, pointing the author at `ALL_REPORTED_TIERS` above.
    match tier {
        ToolRiskTier::ReadOnly => {}
        ToolRiskTier::Stateful => {}
        ToolRiskTier::ArtifactRender => {}
        ToolRiskTier::ConfigChange => {}
        ToolRiskTier::HardwareIO => {}
        ToolRiskTier::Privileged => {}
    }
}

/// Rate limit error
#[derive(Debug, Error, Clone)]
pub enum RateLimitError {
    #[error(
        "Rate limit exceeded for {tier:?}: {current}/{limit} requests in window. Retry after {retry_after_secs}s"
    )]
    Exceeded {
        tier: ToolRiskTier,
        current: u32,
        limit: u32,
        retry_after_secs: u64,
    },
}

/// Result of a rate limit check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitResult {
    /// Whether the request is allowed
    pub allowed: bool,
    /// Current request count in window
    pub current: u32,
    /// Maximum allowed requests in window
    pub limit: u32,
    /// Remaining requests in current window
    pub remaining: u32,
    /// Seconds until window resets
    pub reset_after_secs: u64,
    /// If not allowed, seconds until retry is possible
    pub retry_after_secs: Option<u64>,
}

impl RateLimitResult {
    /// Create an allowed result
    pub fn allowed(current: u32, limit: u32, reset_after_secs: u64) -> Self {
        Self {
            allowed: true,
            current,
            limit,
            remaining: limit.saturating_sub(current),
            reset_after_secs,
            retry_after_secs: None,
        }
    }

    /// Create a denied result
    pub fn denied(current: u32, limit: u32, retry_after_secs: u64) -> Self {
        Self {
            allowed: false,
            current,
            limit,
            remaining: 0,
            reset_after_secs: retry_after_secs,
            retry_after_secs: Some(retry_after_secs),
        }
    }
}

/// Rate limits per tier
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierLimits {
    /// Requests per window for ReadOnly operations
    pub read_only: u32,
    /// Requests per window for Stateful operations
    pub stateful: u32,
    /// Requests per window for ConfigChange operations
    pub config_change: u32,
    /// Requests per window for HardwareIO operations
    pub hardware_io: u32,
    /// Requests per window for Privileged operations
    pub privileged: u32,
}

impl Default for TierLimits {
    fn default() -> Self {
        Self {
            read_only: 100,    // High limit for reads
            stateful: 30,      // Moderate limit for stateful ops
            config_change: 10, // Low limit for config changes
            hardware_io: 5,    // Very low limit for hardware I/O
            privileged: 3,     // Minimal limit for privileged ops
        }
    }
}

impl TierLimits {
    /// Get limit for a specific tier
    pub fn get(&self, tier: &ToolRiskTier) -> u32 {
        match tier {
            ToolRiskTier::ReadOnly => self.read_only,
            ToolRiskTier::Stateful | ToolRiskTier::ArtifactRender => self.stateful,
            ToolRiskTier::ConfigChange => self.config_change,
            ToolRiskTier::HardwareIO => self.hardware_io,
            ToolRiskTier::Privileged => self.privileged,
        }
    }
}

/// Configuration for the rate limiter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Whether rate limiting is enabled
    pub enabled: bool,
    /// Window size in seconds
    pub window_secs: u64,
    /// Limits per tier
    pub tier_limits: TierLimits,
    /// Global limit across all tiers (0 = disabled)
    pub global_limit: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window_secs: DEFAULT_WINDOW_SECS,
            tier_limits: TierLimits::default(),
            global_limit: 200, // Total requests per window
        }
    }
}

/// Entry in the sliding window
#[derive(Debug, Clone)]
struct WindowEntry {
    /// Timestamp of the request
    timestamp: Instant,
}

/// Per-client rate limit state
#[derive(Debug)]
struct ClientState {
    /// Requests per tier
    tier_requests: HashMap<ToolRiskTier, Vec<WindowEntry>>,
    /// All requests (for global limit)
    all_requests: Vec<WindowEntry>,
}

/// The instant `window` ago, or `None` when the process has not been up
/// that long — every recorded entry is then necessarily within the window.
/// `checked_sub` avoids the `Instant - Duration` underflow panic that fires
/// when uptime < window, e.g. a daemon serving requests in the first
/// `window` seconds after boot (#1699).
fn window_cutoff(window: Duration) -> Option<Instant> {
    Instant::now().checked_sub(window)
}

/// Whether `timestamp` is within the window defined by `cutoff`. A `None`
/// cutoff (uptime < window) keeps everything — nothing is old enough to
/// have expired yet.
fn within_window(timestamp: Instant, cutoff: Option<Instant>) -> bool {
    cutoff.is_none_or(|c| timestamp > c)
}

/// Whole seconds until the window frees up, rounded UP so a sub-second
/// remainder never reports `0` while the client is still rate-limited
/// (#1474). `oldest` is the oldest in-window entry; `None` falls back to
/// the full window length.
fn retry_after_secs(oldest: Option<Instant>, window: Duration, full_window_secs: u64) -> u64 {
    match oldest {
        Some(t) => {
            let elapsed = t.elapsed();
            if elapsed < window {
                let remaining = window - elapsed;
                remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0)
            } else {
                0
            }
        }
        None => full_window_secs,
    }
}

impl ClientState {
    fn new() -> Self {
        Self {
            tier_requests: HashMap::new(),
            all_requests: Vec::new(),
        }
    }

    /// Clean up expired entries
    fn cleanup(&mut self, window: Duration) {
        let cutoff = window_cutoff(window);

        for entries in self.tier_requests.values_mut() {
            entries.retain(|e| within_window(e.timestamp, cutoff));
        }

        self.all_requests
            .retain(|e| within_window(e.timestamp, cutoff));
    }

    /// True when no requests remain (typically right after `cleanup`).
    /// Such a state holds no rate-limit information and can be evicted.
    fn is_empty(&self) -> bool {
        self.all_requests.is_empty() && self.tier_requests.values().all(Vec::is_empty)
    }

    /// Count requests for a tier within the window
    fn count_tier(&self, tier: &ToolRiskTier, window: Duration) -> u32 {
        let cutoff = window_cutoff(window);
        self.tier_requests
            .get(tier)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| within_window(e.timestamp, cutoff))
                    .count() as u32
            })
            .unwrap_or(0)
    }

    /// Count all requests within the window
    fn count_global(&self, window: Duration) -> u32 {
        let cutoff = window_cutoff(window);
        self.all_requests
            .iter()
            .filter(|e| within_window(e.timestamp, cutoff))
            .count() as u32
    }

    /// Record a request
    fn record(&mut self, tier: ToolRiskTier) {
        let entry = WindowEntry {
            timestamp: Instant::now(),
        };

        self.tier_requests
            .entry(tier)
            .or_default()
            .push(entry.clone());

        self.all_requests.push(entry);
    }

    /// Get oldest entry timestamp for retry calculation
    fn oldest_entry(&self, tier: &ToolRiskTier, window: Duration) -> Option<Instant> {
        let cutoff = window_cutoff(window);
        self.tier_requests.get(tier).and_then(|entries| {
            entries
                .iter()
                .filter(|e| within_window(e.timestamp, cutoff))
                .min_by_key(|e| e.timestamp)
                .map(|e| e.timestamp)
        })
    }
}

/// Rate limiter with sliding window algorithm
pub struct RateLimiter {
    config: RateLimitConfig,
    /// Per-client state
    clients: Arc<Mutex<HashMap<String, ClientState>>>,
    /// Timestamp of the last opportunistic eviction sweep (#1472). The
    /// sweep that drops expired idle clients is amortised to at most once
    /// per window, so per-request overhead stays O(1) while unbounded
    /// unique-client-id growth is still reclaimed by ordinary activity.
    last_sweep: Mutex<Instant>,
}

impl RateLimiter {
    /// Create a new rate limiter with default config
    pub fn new() -> Self {
        Self {
            config: RateLimitConfig::default(),
            clients: Arc::new(Mutex::new(HashMap::new())),
            last_sweep: Mutex::new(Instant::now()),
        }
    }

    /// Create a rate limiter with custom config
    pub fn with_config(config: RateLimitConfig) -> Self {
        Self {
            config,
            clients: Arc::new(Mutex::new(HashMap::new())),
            last_sweep: Mutex::new(Instant::now()),
        }
    }

    /// Check if a request is allowed (without recording)
    pub fn check(&self, client_id: &str, tier: &ToolRiskTier) -> RateLimitResult {
        if !self.config.enabled {
            return RateLimitResult::allowed(0, u32::MAX, 0);
        }

        let window = Duration::from_secs(self.config.window_secs);
        let tier_limit = self.config.tier_limits.get(tier);

        let clients = self.clients.lock().unwrap();

        let (tier_count, global_count, oldest) = if let Some(state) = clients.get(client_id) {
            (
                state.count_tier(tier, window),
                state.count_global(window),
                state.oldest_entry(tier, window),
            )
        } else {
            (0, 0, None)
        };

        // Check tier limit
        if tier_count >= tier_limit {
            let retry_after = retry_after_secs(oldest, window, self.config.window_secs);
            return RateLimitResult::denied(tier_count, tier_limit, retry_after);
        }

        // Check global limit
        if self.config.global_limit > 0 && global_count >= self.config.global_limit {
            let retry_after = self.config.window_secs; // Approximate
            return RateLimitResult::denied(global_count, self.config.global_limit, retry_after);
        }

        let reset_after = self.config.window_secs;
        RateLimitResult::allowed(tier_count, tier_limit, reset_after)
    }

    /// Check and record a request (atomic operation)
    pub fn check_and_record(
        &self,
        client_id: &str,
        tier: ToolRiskTier,
    ) -> Result<RateLimitResult, RateLimitError> {
        if !self.config.enabled {
            return Ok(RateLimitResult::allowed(0, u32::MAX, 0));
        }

        let window = Duration::from_secs(self.config.window_secs);
        let tier_limit = self.config.tier_limits.get(&tier);

        let mut clients = self.clients.lock().unwrap();

        // Get or create client state
        let state = clients
            .entry(client_id.to_string())
            .or_insert_with(ClientState::new);

        // Cleanup expired entries
        state.cleanup(window);

        let tier_count = state.count_tier(&tier, window);
        let global_count = state.count_global(window);

        // Check tier limit
        if tier_count >= tier_limit {
            let retry_after = retry_after_secs(
                state.oldest_entry(&tier, window),
                window,
                self.config.window_secs,
            );

            return Err(RateLimitError::Exceeded {
                tier,
                current: tier_count,
                limit: tier_limit,
                retry_after_secs: retry_after,
            });
        }

        // Check global limit
        if self.config.global_limit > 0 && global_count >= self.config.global_limit {
            return Err(RateLimitError::Exceeded {
                tier,
                current: global_count,
                limit: self.config.global_limit,
                retry_after_secs: self.config.window_secs,
            });
        }

        // Record the request
        state.record(tier);

        // Opportunistic idle-client eviction (#1472). Without this, every
        // distinct `client_id` that makes one request and never returns
        // leaves a `ClientState` behind forever — `cleanup_all()` is the
        // only evictor and nothing schedules it. Sweep the whole map at
        // most once per window (guarded by `last_sweep`) so the per-request
        // cost is amortised O(1); the current client was just recorded so
        // it is never dropped here.
        let window_elapsed = self
            .last_sweep
            .try_lock()
            .ok()
            .filter(|last| last.elapsed() >= window);
        if let Some(mut last) = window_elapsed {
            clients.retain(|id, state| {
                if id == client_id {
                    return true;
                }
                state.cleanup(window);
                !state.is_empty()
            });
            *last = Instant::now();
        }

        let reset_after = self.config.window_secs;
        Ok(RateLimitResult::allowed(
            tier_count + 1,
            tier_limit,
            reset_after,
        ))
    }

    /// Get current usage for a client.
    ///
    /// Reports `(count, limit)` for **every** [`ToolRiskTier`] (see
    /// [`ALL_REPORTED_TIERS`]). A tier with no recorded requests reports
    /// `(0, limit)`. #2144: `ArtifactRender` records under its own bucket
    /// key, so a usage report that iterated a hand-maintained subset of
    /// tiers dropped it silently — every tier is now sourced from the one
    /// exhaustiveness-checked array.
    pub fn get_usage(&self, client_id: &str) -> HashMap<ToolRiskTier, (u32, u32)> {
        let window = Duration::from_secs(self.config.window_secs);
        let clients = self.clients.lock().unwrap();
        let state = clients.get(client_id);

        ALL_REPORTED_TIERS
            .iter()
            .map(|&tier| {
                // Absent client (or no entries for this tier) → 0.
                let count = state.map_or(0, |s| s.count_tier(&tier, window));
                let limit = self.config.tier_limits.get(&tier);
                (tier, (count, limit))
            })
            .collect()
    }

    /// Reset rate limits for a client
    pub fn reset_client(&self, client_id: &str) {
        let mut clients = self.clients.lock().unwrap();
        clients.remove(client_id);
    }

    /// Cleanup all expired entries
    pub fn cleanup_all(&self) {
        let window = Duration::from_secs(self.config.window_secs);
        let mut clients = self.clients.lock().unwrap();

        for state in clients.values_mut() {
            state.cleanup(window);
        }

        // Remove empty client states
        clients.retain(|_, state| !state.is_empty());
    }

    /// Get number of tracked clients
    pub fn client_count(&self) -> usize {
        let clients = self.clients.lock().unwrap();
        clients.len()
    }

    /// Update configuration
    pub fn update_config(&mut self, config: RateLimitConfig) {
        self.config = config;
    }

    /// Get current configuration
    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_allows_under_limit() {
        let limiter = RateLimiter::new();

        let result = limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();

        assert!(result.allowed);
        assert_eq!(result.current, 1);
        assert_eq!(result.limit, 100);
        assert_eq!(result.remaining, 99);
    }

    #[test]
    fn test_rate_limiter_denies_over_limit() {
        let config = RateLimitConfig {
            enabled: true,
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 3,
                ..Default::default()
            },
            global_limit: 0, // Disable global limit for this test
        };

        let limiter = RateLimiter::with_config(config);

        // First 3 requests should succeed
        for i in 1..=3 {
            let result = limiter
                .check_and_record("client1", ToolRiskTier::ReadOnly)
                .unwrap();
            assert!(result.allowed);
            assert_eq!(result.current, i);
        }

        // 4th request should fail
        let result = limiter.check_and_record("client1", ToolRiskTier::ReadOnly);
        assert!(result.is_err());

        match result {
            Err(RateLimitError::Exceeded {
                tier,
                current,
                limit,
                ..
            }) => {
                assert_eq!(tier, ToolRiskTier::ReadOnly);
                assert_eq!(current, 3);
                assert_eq!(limit, 3);
            }
            Ok(_) => panic!("Expected rate limit error"),
        }
    }

    #[test]
    fn test_rate_limiter_separate_clients() {
        let config = RateLimitConfig {
            enabled: true,
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 2,
                ..Default::default()
            },
            global_limit: 0,
        };

        let limiter = RateLimiter::with_config(config);

        // Client 1 uses their limit
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        assert!(
            limiter
                .check_and_record("client1", ToolRiskTier::ReadOnly)
                .is_err()
        );

        // Client 2 should still have their own limit
        let result = limiter
            .check_and_record("client2", ToolRiskTier::ReadOnly)
            .unwrap();
        assert!(result.allowed);
    }

    #[test]
    fn test_rate_limiter_separate_tiers() {
        let config = RateLimitConfig {
            enabled: true,
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 2,
                stateful: 2,
                ..Default::default()
            },
            global_limit: 0,
        };

        let limiter = RateLimiter::with_config(config);

        // Exhaust ReadOnly limit
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        assert!(
            limiter
                .check_and_record("client1", ToolRiskTier::ReadOnly)
                .is_err()
        );

        // Stateful should still work
        let result = limiter
            .check_and_record("client1", ToolRiskTier::Stateful)
            .unwrap();
        assert!(result.allowed);
    }

    #[test]
    fn test_rate_limiter_global_limit() {
        let config = RateLimitConfig {
            enabled: true,
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 100,
                stateful: 100,
                ..Default::default()
            },
            global_limit: 3, // Low global limit
        };

        let limiter = RateLimiter::with_config(config);

        // Use global limit across tiers
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        limiter
            .check_and_record("client1", ToolRiskTier::Stateful)
            .unwrap();
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();

        // 4th request should fail due to global limit
        assert!(
            limiter
                .check_and_record("client1", ToolRiskTier::ReadOnly)
                .is_err()
        );
    }

    #[test]
    fn test_rate_limiter_disabled() {
        let config = RateLimitConfig {
            enabled: false,
            ..Default::default()
        };

        let limiter = RateLimiter::with_config(config);

        // Should always allow when disabled
        for _ in 0..1000 {
            let result = limiter
                .check_and_record("client1", ToolRiskTier::ReadOnly)
                .unwrap();
            assert!(result.allowed);
        }
    }

    #[test]
    fn test_rate_limiter_check_without_record() {
        let config = RateLimitConfig {
            enabled: true,
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 2,
                ..Default::default()
            },
            global_limit: 0,
        };

        let limiter = RateLimiter::with_config(config);

        // Check without recording
        let result1 = limiter.check("client1", &ToolRiskTier::ReadOnly);
        assert!(result1.allowed);
        assert_eq!(result1.current, 0);

        // Check again - should still be 0
        let result2 = limiter.check("client1", &ToolRiskTier::ReadOnly);
        assert!(result2.allowed);
        assert_eq!(result2.current, 0);

        // Now record
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();

        // Check should now show 1
        let result3 = limiter.check("client1", &ToolRiskTier::ReadOnly);
        assert!(result3.allowed);
        assert_eq!(result3.current, 1);
    }

    #[test]
    fn test_rate_limiter_get_usage() {
        let limiter = RateLimiter::new();

        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        limiter
            .check_and_record("client1", ToolRiskTier::Stateful)
            .unwrap();

        let usage = limiter.get_usage("client1");

        assert_eq!(usage.get(&ToolRiskTier::ReadOnly), Some(&(2, 100)));
        assert_eq!(usage.get(&ToolRiskTier::Stateful), Some(&(1, 30)));
        assert_eq!(usage.get(&ToolRiskTier::ConfigChange), Some(&(0, 10)));
    }

    /// #2144: `ArtifactRender` requests are recorded under their own tier
    /// key (`record`/`count_tier` key on the exact tier passed) but
    /// `get_usage` iterated a hardcoded tier list that omitted
    /// `ArtifactRender`, so those requests never surfaced in the usage
    /// report. Recording one and asking for usage must now report it —
    /// with the (shared, stateful) limit — and must NOT inflate the
    /// distinct `Stateful` bucket's count.
    #[test]
    fn get_usage_reports_artifact_render_bucket() {
        let limiter = RateLimiter::new();

        limiter
            .check_and_record("client1", ToolRiskTier::ArtifactRender)
            .unwrap();
        limiter
            .check_and_record("client1", ToolRiskTier::ArtifactRender)
            .unwrap();

        let usage = limiter.get_usage("client1");

        // The recorded ArtifactRender requests are now reported, under
        // the limit it shares with Stateful (30).
        assert_eq!(
            usage.get(&ToolRiskTier::ArtifactRender),
            Some(&(2, 30)),
            "ArtifactRender usage must be reported, not omitted (#2144)"
        );
        // ArtifactRender is a SEPARATE bucket from Stateful — recording
        // ArtifactRender must not be counted against Stateful.
        assert_eq!(
            usage.get(&ToolRiskTier::Stateful),
            Some(&(0, 30)),
            "ArtifactRender requests must not inflate the Stateful bucket"
        );
    }

    /// #2144: the usage report's key set must be EXACTLY the canonical
    /// `ALL_REPORTED_TIERS` — for both a known client and an unknown one
    /// (the zero-fill branch). We compare against the const rather than a
    /// second hardcoded literal so this test can't drift independently;
    /// the compile-time `assert_all_tiers_reported` guard pins the const
    /// to the `ToolRiskTier` enum, so transitively the report covers every
    /// variant. This is what catches a future `get_usage` that drops (or
    /// invents) a key relative to the source of truth.
    #[test]
    fn get_usage_key_set_matches_all_reported_tiers() {
        use std::collections::HashSet;
        let limiter = RateLimiter::new();
        let expected: HashSet<ToolRiskTier> = ALL_REPORTED_TIERS.iter().copied().collect();

        // Unknown client → zero-fill branch.
        let unknown: HashSet<ToolRiskTier> = limiter.get_usage("never-seen").into_keys().collect();
        assert_eq!(
            unknown, expected,
            "unknown-client usage key set must equal ALL_REPORTED_TIERS (#2144)"
        );

        // Known client → recorded branch.
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        let known: HashSet<ToolRiskTier> = limiter.get_usage("client1").into_keys().collect();
        assert_eq!(
            known, expected,
            "known-client usage key set must equal ALL_REPORTED_TIERS (#2144)"
        );
    }

    #[test]
    fn test_rate_limiter_reset_client() {
        let config = RateLimitConfig {
            enabled: true,
            window_secs: 60,
            tier_limits: TierLimits {
                read_only: 2,
                ..Default::default()
            },
            global_limit: 0,
        };

        let limiter = RateLimiter::with_config(config);

        // Exhaust limit
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        assert!(
            limiter
                .check_and_record("client1", ToolRiskTier::ReadOnly)
                .is_err()
        );

        // Reset
        limiter.reset_client("client1");

        // Should work again
        let result = limiter
            .check_and_record("client1", ToolRiskTier::ReadOnly)
            .unwrap();
        assert!(result.allowed);
        assert_eq!(result.current, 1);
    }

    #[test]
    fn test_tier_limits_default() {
        let limits = TierLimits::default();

        assert_eq!(limits.read_only, 100);
        assert_eq!(limits.stateful, 30);
        assert_eq!(limits.config_change, 10);
        assert_eq!(limits.hardware_io, 5);
        assert_eq!(limits.privileged, 3);
    }

    #[test]
    fn test_rate_limit_result_remaining() {
        let result = RateLimitResult::allowed(3, 10, 60);
        assert_eq!(result.remaining, 7);

        let result2 = RateLimitResult::denied(10, 10, 30);
        assert_eq!(result2.remaining, 0);
    }
}
