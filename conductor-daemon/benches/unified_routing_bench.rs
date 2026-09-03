// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "benchmark prints its metrics block for the CI threshold gate"
)]

//! Unified routing passthrough-latency benchmark + CI gate
//! (ADR-036 / ADR-037 Slice 9; thresholds from ADR-036 D7).
//!
//! Measures the per-event cost of the dispatch hot path for a single
//! passthrough route: the 4-sub-bucket rule-engine matcher
//! (`CompiledRuleSet::match_event`, conductor-core) followed by the
//! post-mapping `RouteEngine::route_destinations` (conductor-daemon).
//! This is "dispatch-only" — it excludes async action
//! execution and real port I/O.
//!
//! Custom harness (`harness = false`): runs [`N_EVENTS`] timed
//! iterations after a warmup, computes median / P99 / stddev, and prints
//! a machine-parseable metrics block. The numbers are gated in CI by
//! `scripts/check_bench_thresholds.py` (median < 1.0 ms, P99 < 3.0 ms,
//! stddev < 0.5 ms). The ms-scale thresholds carry ~1000x headroom over
//! the sub-µs steady state on purpose — the gate catches catastrophic
//! regressions (accidental O(n) blowups, per-event allocation storms)
//! without flaking on loaded CI runners.

use std::hint::black_box;
use std::time::Instant;

use conductor_core::config::types::RouteConfig;
use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
use conductor_core::events::{ProcessedEvent, VelocityLevel};
use conductor_core::identity::DeviceMatcher;
use conductor_core::rule_set::CompiledRuleSet;
use conductor_core::{Config, Mode};
use conductor_daemon::route_engine::RouteEngine;

/// Events timed per run (ADR-036 §Slice 9: "runs 10,000 events").
const N_EVENTS: usize = 10_000;
/// Warmup iterations (excluded from the sample) so the measured window
/// reflects warm caches / branch predictors, not first-touch costs.
const WARMUP: usize = 2_000;

/// The passthrough pipeline under test: a 1-mode config with no mappings
/// (so the rule engine misses and falls through) plus a single
/// post-mapping passthrough route `pads → out`.
struct Pipeline {
    rule_set: CompiledRuleSet,
    route_engine: RouteEngine,
    event: ProcessedEvent,
    raw_midi: Vec<u8>,
}

fn build_pipeline() -> Pipeline {
    let config = Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints: vec![EndpointConfig {
            alias: "pads".to_string(),
            direction: ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers: vec![DeviceMatcher::name_contains("Pads")],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        }],
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            // No mappings: the matcher walks all 4 sub-buckets and
            // returns None — the passthrough case the route engine then
            // forwards.
            mappings: vec![],
        }],
        global_mappings: vec![],
        logging: None,
        advanced_settings: Default::default(),
        last_selected_mode: None,
        per_app_modes: None,
        default_mode: None,
        led: None,
        event_console: None,
        routes: vec![],
    };
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);

    // Single passthrough route: no filter, no transform, no mode scope.
    let route_engine = RouteEngine::compile(&[RouteConfig {
        from: "pads".to_string(),
        to: "out".to_string(),
        transform: None,
        filter: None,
        enabled: true,
        description: None,
        modes: Vec::new(),
    }]);

    Pipeline {
        rule_set,
        route_engine,
        event: ProcessedEvent::PadPressed {
            note: 36,
            velocity: 64,
            velocity_level: VelocityLevel::Medium,
            channel: Some(0),
        },
        raw_midi: vec![0x90, 36, 64],
    }
}

/// One passthrough dispatch: rule-engine match (miss) → route dispatch.
#[inline]
fn run_once(p: &Pipeline) {
    let matched = p.rule_set.match_event(black_box(&p.event), 0, Some("pads"));
    black_box(&matched);
    // ADR-039: measure the byte-core (the production hot path). The
    // `route_destinations(&ProtocolEvent)` shim delegates here for Input; the
    // hot path calls this directly with already-extracted bytes, so this is
    // the apples-to-apples comparison against pre-refactor main (where this
    // body was named `route_destinations`).
    let outputs = p
        .route_engine
        .route_destinations_midi("pads", black_box(&p.raw_midi), "Default");
    black_box(&outputs);
}

fn percentile(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    // Nearest-rank on a 0-based sorted slice.
    let rank = (pct / 100.0 * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn main() {
    let pipeline = build_pipeline();

    // Warmup — not sampled.
    for _ in 0..WARMUP {
        run_once(&pipeline);
    }

    // Time each event individually. Instant::now() adds a fixed ~tens-of-ns
    // overhead, negligible against the ms-scale thresholds this gate checks.
    let mut samples_ns: Vec<f64> = Vec::with_capacity(N_EVENTS);
    for _ in 0..N_EVENTS {
        let start = Instant::now();
        run_once(&pipeline);
        samples_ns.push(start.elapsed().as_nanos() as f64);
    }

    samples_ns.sort_by(|a, b| a.partial_cmp(b).expect("no NaN timings"));
    let median_ns = percentile(&samples_ns, 50.0);
    let p99_ns = percentile(&samples_ns, 99.0);
    let mean_ns = samples_ns.iter().sum::<f64>() / samples_ns.len() as f64;
    let variance_ns = samples_ns
        .iter()
        .map(|x| {
            let d = x - mean_ns;
            d * d
        })
        .sum::<f64>()
        / samples_ns.len() as f64;
    let stddev_ns = variance_ns.sqrt();

    let to_us = |ns: f64| ns / 1000.0;

    // Human summary.
    println!("Unified routing passthrough latency ({N_EVENTS} events):");
    println!("  median = {:.4} µs", to_us(median_ns));
    println!("  p99    = {:.4} µs", to_us(p99_ns));
    println!("  stddev = {:.4} µs", to_us(stddev_ns));

    // Machine-parseable block consumed by scripts/check_bench_thresholds.py.
    // Keys are `<name>_us <value>`; the script greps these exact tokens.
    println!("=== unified_routing_bench metrics ===");
    println!("events {N_EVENTS}");
    println!("median_us {:.4}", to_us(median_ns));
    println!("p99_us {:.4}", to_us(p99_ns));
    println!("stddev_us {:.4}", to_us(stddev_ns));
    println!("=== end metrics ===");
}
