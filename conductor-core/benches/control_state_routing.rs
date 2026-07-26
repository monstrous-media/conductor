// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! Control state routing benchmarks (ADR-025 Phase 1).
//!
//! Validates the latency target for the ingress observation path:
//!   - observe_event overhead: p50 ≤ 500ns added vs a no-op baseline
//!   - get (lookup) on warm key: p50 ≤ 500ns, p99 ≤ 2µs
//!
//! Phase 2 will add PC-context-switch and CC-range benches once the
//! lowering is in place. This bench exists now so we can lock in the
//! ingress-path number before Phase 2 work starts.

use conductor_core::control_state::{PhysicalControlStateStore, StateKey};
use conductor_core::event_processor::MidiEvent;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn bench_observe_pc(c: &mut Criterion) {
    let store = PhysicalControlStateStore::default();
    let mut pc = 0u8;
    c.bench_function("control_state::observe_pc", |b| {
        b.iter(|| {
            pc = pc.wrapping_add(1) % 128;
            let evt = black_box(MidiEvent::ProgramChange {
                program: pc,
                channel: 0,
                time: Instant::now(),
            });
            store.observe_event("fcb1010", &evt);
        })
    });
}

fn bench_observe_cc(c: &mut Criterion) {
    let store = PhysicalControlStateStore::default();
    let mut v = 0u8;
    c.bench_function("control_state::observe_cc", |b| {
        b.iter(|| {
            v = v.wrapping_add(1);
            let evt = black_box(MidiEvent::ControlChange {
                cc: 7,
                value: v,
                channel: 0,
                time: Instant::now(),
            });
            store.observe_event("fcb1010", &evt);
        })
    });
}

fn bench_observe_note_on(c: &mut Criterion) {
    let store = PhysicalControlStateStore::default();
    let mut n = 0u8;
    c.bench_function("control_state::observe_note_on", |b| {
        b.iter(|| {
            n = n.wrapping_add(1) % 128;
            let evt = black_box(MidiEvent::NoteOn {
                note: n,
                velocity: 100,
                channel: 0,
                time: Instant::now(),
            });
            store.observe_event("k1", &evt);
        })
    });
}

fn bench_get_pc_warm(c: &mut Criterion) {
    let store = PhysicalControlStateStore::default();
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 12,
            channel: 0,
            time: Instant::now(),
        },
    );
    let key = StateKey::ProgramChange {
        device: "fcb1010".into(),
        channel: 0,
    };
    c.bench_function("control_state::get_pc_warm", |b| {
        b.iter(|| {
            let v = store.get(black_box(&key));
            black_box(v)
        })
    });
}

/// Reviewer-requested measurement (PR #844): what the condition
/// evaluator actually pays per call. Rebuilds the `StateKey` every
/// iter with a fresh `String` for device — this is the alloc path
/// that `evaluate_active_pc_is` and siblings take on every condition
/// check. Use the gap between this and `get_pc_warm` as the upper
/// bound on the alloc overhead to decide whether a zero-alloc lookup
/// API is worth the refactor.
fn bench_get_pc_cold_alloc(c: &mut Criterion) {
    let store = PhysicalControlStateStore::default();
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 12,
            channel: 0,
            time: Instant::now(),
        },
    );
    let device: &str = "fcb1010";
    c.bench_function("control_state::get_pc_cold_alloc", |b| {
        b.iter(|| {
            let key = StateKey::ProgramChange {
                device: black_box(device).to_string(),
                channel: 0,
            };
            let v = store.get(&key);
            black_box(v)
        })
    });
}

fn bench_get_miss(c: &mut Criterion) {
    let store = PhysicalControlStateStore::default();
    let key = StateKey::ProgramChange {
        device: "does_not_exist".into(),
        channel: 0,
    };
    c.bench_function("control_state::get_miss", |b| {
        b.iter(|| {
            let v = store.get(black_box(&key));
            black_box(v)
        })
    });
}

fn bench_observe_many_channels(c: &mut Criterion) {
    // Realistic scenario: 16 channels on one device, CC #7 on each.
    let store = PhysicalControlStateStore::default();
    for ch in 0u8..16 {
        store.observe_event(
            "keyboard",
            &MidiEvent::ControlChange {
                cc: 7,
                value: 100,
                channel: ch,
                time: Instant::now(),
            },
        );
    }
    let mut ch = 0u8;
    c.bench_function("control_state::observe_cycling_16_channels", |b| {
        b.iter(|| {
            ch = (ch + 1) & 0x0F;
            let evt = black_box(MidiEvent::ControlChange {
                cc: 7,
                value: 64,
                channel: ch,
                time: Instant::now(),
            });
            store.observe_event("keyboard", &evt);
        })
    });
}

fn bench_concurrent_read_under_writes(c: &mut Criterion) {
    let store = Arc::new(PhysicalControlStateStore::default());
    store.observe_event(
        "fcb1010",
        &MidiEvent::ProgramChange {
            program: 12,
            channel: 0,
            time: Instant::now(),
        },
    );

    // Spawn a writer thread that keeps observing CC#7 values.
    let writer_store = Arc::clone(&store);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_stop = Arc::clone(&stop);
    let writer = thread::spawn(move || {
        let mut v = 0u8;
        while !writer_stop.load(std::sync::atomic::Ordering::Relaxed) {
            v = v.wrapping_add(1);
            writer_store.observe_event(
                "fcb1010",
                &MidiEvent::ControlChange {
                    cc: 7,
                    value: v,
                    channel: 0,
                    time: Instant::now(),
                },
            );
        }
    });

    let key = StateKey::ProgramChange {
        device: "fcb1010".into(),
        channel: 0,
    };
    c.bench_function("control_state::get_pc_under_writer_contention", |b| {
        b.iter(|| {
            let v = store.get(black_box(&key));
            black_box(v)
        })
    });

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();
}

fn bench_note_held_ttl_eviction(c: &mut Criterion) {
    // Short TTL so reads trigger eviction. Writer refreshes the entry
    // periodically so the eviction-then-reread branch gets exercised.
    let store = PhysicalControlStateStore::new(5); // 5ms TTL

    c.bench_function("control_state::note_held_with_ttl", |b| {
        b.iter(|| {
            store.observe_event(
                "k1",
                &MidiEvent::NoteOn {
                    note: 60,
                    velocity: 100,
                    channel: 0,
                    time: Instant::now(),
                },
            );
            std::thread::sleep(Duration::from_micros(100));
            let _ = store.get(black_box(&StateKey::NoteHeld {
                device: "k1".into(),
                channel: 0,
                note: 60,
            }));
        })
    });
}

criterion_group!(
    benches,
    bench_observe_pc,
    bench_observe_cc,
    bench_observe_note_on,
    bench_get_pc_warm,
    bench_get_pc_cold_alloc,
    bench_get_miss,
    bench_observe_many_channels,
    bench_concurrent_read_under_writes,
    bench_note_held_ttl_eviction,
);
criterion_main!(benches);
