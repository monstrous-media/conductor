// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Per-device event rate limiting (ADR-009 D9)
//!
//! Prevents malfunctioning USB devices from flooding the event bus.
//! Uses a fixed window counter per device with configurable limits.

use conductor_core::identity::DeviceId;
use std::collections::HashMap;
use std::time::Instant;

/// Maximum number of tracked devices to prevent unbounded memory growth.
/// With ~64 bytes per entry, this caps memory at ~16 KB.
const MAX_TRACKED_DEVICES: usize = 256;

/// Per-device rate limiter using fixed window counters.
///
/// Each device gets an independent counter that resets every second.
/// When a device exceeds its limit, subsequent events are dropped
/// until the window resets.
///
/// The counters map is capped at [`MAX_TRACKED_DEVICES`] entries.
/// When full, events from unknown devices are dropped to prevent
/// quota-reset attacks (an attacker cannot evict tracked devices).
/// Call [`gc()`](Self::gc) periodically to reclaim stale entries.
pub struct DeviceRateLimiter {
    counters: HashMap<DeviceId, WindowCounter>,
    default_limit: u32,
}

struct WindowCounter {
    count: u32,
    window_start: Instant,
}

impl DeviceRateLimiter {
    /// Create a new rate limiter with the given default events/sec limit.
    pub fn new(default_limit: u32) -> Self {
        Self {
            counters: HashMap::new(),
            default_limit,
        }
    }

    /// Check if an event from the given device should be allowed.
    ///
    /// Returns `true` if the event is within the rate limit, `false` if it
    /// should be dropped. When at capacity, events from untracked devices
    /// are dropped rather than evicting existing entries.
    pub fn check(&mut self, device_id: &DeviceId) -> bool {
        let now = Instant::now();
        let limit = self.default_limit;

        // Fast path: device already tracked — no allocation
        if let Some(counter) = self.counters.get_mut(device_id) {
            return Self::check_counter(counter, now, limit);
        }

        // At capacity: auto-GC stale entries (self-healing), then retry.
        // Only drop if still at capacity after GC (prevents permanent lockout).
        if self.counters.len() >= MAX_TRACKED_DEVICES {
            self.gc_expired(now);
            if self.counters.len() >= MAX_TRACKED_DEVICES {
                return false;
            }
        }

        // New device within capacity — track it
        let counter = self
            .counters
            .entry(device_id.clone())
            .or_insert(WindowCounter {
                count: 0,
                window_start: now,
            });

        Self::check_counter(counter, now, limit)
    }

    /// Shared counter check logic for both fast and slow paths.
    fn check_counter(counter: &mut WindowCounter, now: Instant, limit: u32) -> bool {
        // Reset window if more than 1 second has elapsed.
        // Use checked_duration_since to handle potential clock monotonicity edge cases.
        let elapsed = now
            .checked_duration_since(counter.window_start)
            .unwrap_or_default();
        if elapsed.as_secs() >= 1 {
            counter.count = 0;
            counter.window_start = now;
        }

        counter.count = counter.count.saturating_add(1);

        counter.count <= limit
    }

    /// Remove entries whose 1-second window has expired (lazy self-healing).
    /// Called automatically when at capacity during `check()`.
    fn gc_expired(&mut self, now: Instant) {
        self.counters.retain(|_, counter| {
            let elapsed = now
                .checked_duration_since(counter.window_start)
                .unwrap_or_default();
            elapsed.as_secs() < 1
        });
    }

    /// Remove stale entries whose window expired more than `stale_secs` ago.
    /// Call periodically (e.g., on config reload or hot-plug rescan) to reclaim capacity.
    pub fn gc(&mut self, stale_secs: u64) {
        let now = Instant::now();
        self.counters.retain(|_, counter| {
            let elapsed = now
                .checked_duration_since(counter.window_start)
                .unwrap_or_default();
            elapsed.as_secs() < stale_secs
        });
    }

    /// Reset all counters (e.g., on config reload).
    pub fn reset(&mut self) {
        self.counters.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str) -> DeviceId {
        DeviceId::raw(name)
    }

    #[test]
    fn test_under_limit_passes() {
        let mut limiter = DeviceRateLimiter::new(10);
        let dev = device("Port A");

        for _ in 0..10 {
            assert!(limiter.check(&dev));
        }
    }

    #[test]
    fn test_at_limit_passes() {
        let mut limiter = DeviceRateLimiter::new(5);
        let dev = device("Port A");

        for _ in 0..5 {
            assert!(limiter.check(&dev));
        }
    }

    #[test]
    fn test_over_limit_drops() {
        let mut limiter = DeviceRateLimiter::new(3);
        let dev = device("Port A");

        assert!(limiter.check(&dev)); // 1
        assert!(limiter.check(&dev)); // 2
        assert!(limiter.check(&dev)); // 3
        assert!(!limiter.check(&dev)); // 4 — dropped
        assert!(!limiter.check(&dev)); // 5 — dropped
    }

    #[test]
    fn test_independent_counters_per_device() {
        let mut limiter = DeviceRateLimiter::new(2);
        let dev_a = device("Port A");
        let dev_b = device("Port B");

        assert!(limiter.check(&dev_a)); // A: 1
        assert!(limiter.check(&dev_a)); // A: 2
        assert!(!limiter.check(&dev_a)); // A: 3 — dropped

        // Device B should still work
        assert!(limiter.check(&dev_b)); // B: 1
        assert!(limiter.check(&dev_b)); // B: 2
        assert!(!limiter.check(&dev_b)); // B: 3 — dropped
    }

    #[test]
    fn test_reset_clears_counters() {
        let mut limiter = DeviceRateLimiter::new(2);
        let dev = device("Port A");

        assert!(limiter.check(&dev));
        assert!(limiter.check(&dev));
        assert!(!limiter.check(&dev)); // dropped

        limiter.reset();

        // After reset, should pass again
        assert!(limiter.check(&dev));
        assert!(limiter.check(&dev));
    }

    #[test]
    fn test_capacity_drops_unknown_devices() {
        let mut limiter = DeviceRateLimiter::new(100);

        // Fill to MAX_TRACKED_DEVICES with devices
        for i in 0..MAX_TRACKED_DEVICES {
            let dev = device(&format!("Port {}", i));
            assert!(limiter.check(&dev));
        }
        assert_eq!(limiter.counters.len(), MAX_TRACKED_DEVICES);

        // New device at capacity should be DROPPED (not evict existing)
        let new_dev = device("Port NEW");
        assert!(
            !limiter.check(&new_dev),
            "Unknown device at capacity should be dropped"
        );
        assert_eq!(limiter.counters.len(), MAX_TRACKED_DEVICES);
        assert!(!limiter.counters.contains_key(&new_dev));
    }

    #[test]
    fn test_capacity_existing_device_still_works() {
        let mut limiter = DeviceRateLimiter::new(100);
        let dev_a = device("Port 0");

        // Fill to capacity
        for i in 0..MAX_TRACKED_DEVICES {
            let dev = device(&format!("Port {}", i));
            assert!(limiter.check(&dev));
        }
        assert_eq!(limiter.counters.len(), MAX_TRACKED_DEVICES);

        // Checking an existing device should still work
        assert!(limiter.check(&dev_a));
        assert_eq!(limiter.counters.len(), MAX_TRACKED_DEVICES);
    }

    #[test]
    fn test_capacity_self_heals_with_stale_entries() {
        let mut limiter = DeviceRateLimiter::new(100);

        // Fill to capacity
        for i in 0..MAX_TRACKED_DEVICES {
            let dev = device(&format!("Port {}", i));
            assert!(limiter.check(&dev));
        }
        assert_eq!(limiter.counters.len(), MAX_TRACKED_DEVICES);

        // Age all entries to expire their windows
        for counter in limiter.counters.values_mut() {
            counter.window_start = Instant::now() - std::time::Duration::from_secs(5);
        }

        // New device should now succeed — stale entries auto-GC'd
        let new_dev = device("Port NEW");
        assert!(
            limiter.check(&new_dev),
            "Self-healing should allow new device after stale GC"
        );
        assert!(limiter.counters.contains_key(&new_dev));
    }

    #[test]
    fn test_gc_removes_stale_entries() {
        let mut limiter = DeviceRateLimiter::new(100);
        let dev = device("Port A");
        assert!(limiter.check(&dev));
        assert_eq!(limiter.counters.len(), 1);

        // Manually age the entry
        if let Some(counter) = limiter.counters.get_mut(&dev) {
            counter.window_start = Instant::now() - std::time::Duration::from_secs(120);
        }

        limiter.gc(60); // GC entries older than 60s
        assert_eq!(limiter.counters.len(), 0, "Stale entry should be GC'd");
    }

    #[test]
    fn test_gc_keeps_fresh_entries() {
        let mut limiter = DeviceRateLimiter::new(100);
        let dev = device("Port A");
        assert!(limiter.check(&dev));

        limiter.gc(60); // Entry is fresh, should be kept
        assert_eq!(limiter.counters.len(), 1, "Fresh entry should be kept");
    }

    #[test]
    fn test_window_reset_after_one_second() {
        let mut limiter = DeviceRateLimiter::new(2);
        let dev = device("Port A");

        assert!(limiter.check(&dev));
        assert!(limiter.check(&dev));
        assert!(!limiter.check(&dev)); // dropped

        // Manually advance the window start to simulate time passing
        if let Some(counter) = limiter.counters.get_mut(&dev) {
            counter.window_start = Instant::now() - std::time::Duration::from_secs(2);
        }

        // After window reset, should pass again
        assert!(limiter.check(&dev));
    }
}
