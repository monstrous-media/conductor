// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Lock-free rule engine benchmarks (v4.24.0 - ADR-009 Phase 6)
//!
//! Benchmarks CompiledRuleSet::match_event() latency for single-device,
//! multi-device, concurrent readers, scaling, and compilation.

use arc_swap::ArcSwap;
use conductor_core::config::types::{ConnectorDirection, EndpointConfig, EndpointKind};
use conductor_core::events::{ProcessedEvent, VelocityLevel};
use conductor_core::identity::DeviceMatcher;
use conductor_core::{ActionConfig, Config, Mapping, Mode, Trigger};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use std::thread;

/// Build a config with n_devices, each having n_mappings
fn build_multi_device_config(n_devices: usize, n_mappings: usize) -> Config {
    let endpoints: Vec<EndpointConfig> = (0..n_devices)
        .map(|i| EndpointConfig {
            alias: format!("device-{}", i),
            direction: ConnectorDirection::Input,
            protocol: None,
            description: None,
            enabled: true,
            channels: vec![],
            kind: EndpointKind::Matcher {
                matchers: vec![DeviceMatcher::name_contains(format!("Device {}", i))],
                input_matchers: vec![],
                output_matchers: vec![],
                no_probe: false,
            },
        })
        .collect();

    let mut mappings = Vec::new();
    for d in 0..n_devices {
        for m in 0..n_mappings {
            mappings.push(Mapping {
                trigger: Trigger::Note {
                    note: (36 + (m % 50)) as u8,
                    velocity_min: None,
                    channel: None,
                    device: Some(format!("device-{}", d)),
                },
                action: ActionConfig::Keystroke {
                    keys: "a".to_string(),
                    modifiers: vec![],
                },
                description: None,
                let_through: false,
            });
        }
    }

    Config {
        mcp: Default::default(),
        config_meta: Default::default(),
        security: Default::default(),
        endpoints,
        modes: vec![Mode {
            name: "Default".to_string(),
            color: None,
            mappings,
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
    }
}

/// Build a single-device config with n_mappings
fn build_single_device_config(n_mappings: usize) -> Config {
    build_multi_device_config(1, n_mappings)
}

fn pad_pressed(note: u8) -> ProcessedEvent {
    ProcessedEvent::PadPressed {
        note,
        velocity: 64,
        velocity_level: VelocityLevel::Medium,
        channel: Some(0),
    }
}

/// Benchmark: single device, 20 mappings
fn bench_match_single_device(c: &mut Criterion) {
    let config = build_single_device_config(20);
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);
    let event = pad_pressed(36);

    c.bench_function("rule_engine::match_single_device", |b| {
        b.iter(|| black_box(rule_set.match_event(black_box(&event), 0, Some("device-0"))))
    });
}

/// Benchmark: 4 devices, 20 mappings each
fn bench_match_multi_device(c: &mut Criterion) {
    let config = build_multi_device_config(4, 20);
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);
    let event = pad_pressed(36);

    c.bench_function("rule_engine::match_multi_device", |b| {
        b.iter(|| black_box(rule_set.match_event(black_box(&event), 0, Some("device-2"))))
    });
}

/// Benchmark: scaling with device count
fn bench_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("rule_engine::scaling");

    for n_devices in [1, 2, 4, 8] {
        let config = build_multi_device_config(n_devices, 20);
        let rule_set = conductor_core::rule_compiler::compile(&config, 1);
        let event = pad_pressed(36);
        let device_id = format!("device-{}", n_devices - 1);

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_devices", n_devices)),
            &n_devices,
            |b, _| {
                b.iter(|| black_box(rule_set.match_event(black_box(&event), 0, Some(&device_id))))
            },
        );
    }

    group.finish();
}

/// Benchmark: ArcSwap::load() overhead
fn bench_arcswap_load(c: &mut Criterion) {
    let config = build_single_device_config(20);
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);
    let arc_swap = ArcSwap::from_pointee(rule_set);

    c.bench_function("rule_engine::arcswap_load", |b| {
        b.iter(|| {
            let guard = arc_swap.load();
            black_box(&*guard);
        })
    });
}

/// Benchmark: 4 concurrent reader threads each doing 100 match_event calls
fn bench_concurrent_readers(c: &mut Criterion) {
    let config = build_multi_device_config(4, 20);
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);
    let arc_swap = Arc::new(ArcSwap::from_pointee(rule_set));

    c.bench_function("rule_engine::concurrent_4_readers", |b| {
        b.iter(|| {
            let mut handles = Vec::new();
            for t in 0..4 {
                let swap = arc_swap.clone();
                let device_id = format!("device-{}", t);
                handles.push(thread::spawn(move || {
                    let guard = swap.load();
                    let event = pad_pressed(36);
                    for _ in 0..100 {
                        black_box(guard.match_event(&event, 0, Some(&device_id)));
                    }
                }));
            }
            for h in handles {
                h.join().unwrap();
            }
        })
    });
}

/// Benchmark: no-match worst case (scans all 4 priority levels)
fn bench_no_match_worst_case(c: &mut Criterion) {
    let config = build_multi_device_config(4, 20);
    let rule_set = conductor_core::rule_compiler::compile(&config, 1);
    let event = pad_pressed(127); // Note not in any mapping

    c.bench_function("rule_engine::no_match_worst_case", |b| {
        b.iter(|| black_box(rule_set.match_event(black_box(&event), 0, Some("device-0"))))
    });
}

/// Benchmark: compilation latency by config size
fn bench_compile(c: &mut Criterion) {
    let mut group = c.benchmark_group("rule_engine::compile");

    for (label, n_devices, n_mappings) in [("1d_10m", 1, 10), ("4d_20m", 4, 20), ("8d_50m", 8, 50)]
    {
        let config = build_multi_device_config(n_devices, n_mappings);

        group.bench_with_input(BenchmarkId::from_parameter(label), &config, |b, config| {
            b.iter(|| black_box(conductor_core::rule_compiler::compile(config, 1)))
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_match_single_device,
    bench_match_multi_device,
    bench_scaling,
    bench_arcswap_load,
    bench_concurrent_readers,
    bench_no_match_worst_case,
    bench_compile,
);
criterion_main!(benches);
