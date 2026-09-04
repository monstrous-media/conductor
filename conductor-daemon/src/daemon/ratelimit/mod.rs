// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Rate limiting for MCP tool executions (P4-05)
//!
//! Provides sliding window rate limiting with per-tier and per-client tracking.
//! Prevents abuse and ensures fair resource usage across clients.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      RateLimiter                            │
//! │  ┌───────────────────────────────────────────────────────┐  │
//! │  │                 TierConfig                            │  │
//! │  │  - ReadOnly: 100 req/min                              │  │
//! │  │  - Stateful: 30 req/min                               │  │
//! │  │  - ConfigChange: 10 req/min                           │  │
//! │  │  - HardwareIO: 5 req/min                              │  │
//! │  └───────────────────────────────────────────────────────┘  │
//! │                           │                                 │
//! │                           ▼                                 │
//! │  ┌───────────────────────────────────────────────────────┐  │
//! │  │              SlidingWindowCounter                     │  │
//! │  │  - Per-client buckets                                 │  │
//! │  │  - Per-tier tracking                                  │  │
//! │  │  - Auto-cleanup of expired entries                    │  │
//! │  └───────────────────────────────────────────────────────┘  │
//! └─────────────────────────────────────────────────────────────┘
//! ```

mod limiter;

pub use limiter::{RateLimitConfig, RateLimitError, RateLimitResult, RateLimiter, TierLimits};

#[cfg(test)]
mod tests;
