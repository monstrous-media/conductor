// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Probe ingress filter benchmark (ADR-026 Phase 1.C).
//!
//! Measures the hot-path cost of `ProbeCoordinator::observe_reply` —
//! the SysEx side-observing hook that the daemon's midir callback
//! invokes for every SysEx frame. Intended to be run with Criterion's
//! `--save-baseline main` / `--baseline main` workflow rather than as
//! an absolute-threshold check, because CI-shared runners are noisy
//! (see implementation-spec.md §Latency validation plan).
//!
//! ```bash
//! # On main, anchor a baseline:
//! cargo bench --bench probe_ingress_overhead -- --save-baseline main
//!
//! # On a PR, compare against that baseline:
//! cargo bench --bench probe_ingress_overhead -- --baseline main
//! ```
//!
//! The four cases cover:
//!
//! 1. `empty_map_non_sysex` — baseline path: no pending probes, and
//!    the bytes fail the is_identity_reply_shape pre-check inside
//!    `observe_reply` (a Note-On has no F0/F7 framing). In production
//!    the daemon's is_sysex gate would short-circuit before even
//!    reaching the coordinator, so this anchors the COORDINATOR-side
//!    cold-path cost — i.e. what every MIDI byte would pay if the
//!    daemon gate were ever removed or broken.
//! 2. `empty_map_sysex_frame` — realistic path when a valid-shape
//!    Identity Reply arrives with NO pending probe on that port:
//!    shape-check passes, the pending-map lock IS taken, `get` misses,
//!    lock released. This is the case that fires for every Identity
//!    Reply or similar-shape SysEx we observe while no probe is
//!    in-flight.
//! 3. `pending_mismatched_port` — another pending probe is in flight
//!    on port A, reply lands on port B: shape passes, lock taken, get
//!    misses (different key), lock released. Checks the cross-port
//!    case when multiple ports each emit unrelated SysEx traffic.
//! 4. `sysex_shape_check_only` — valid Identity-Reply bytes against a
//!    port with no pending slot. Close in cost to case (2) but
//!    deliberately without the background pending probe, so the bench
//!    is self-contained.
//!
//! The happy-path "pending-matches-reply" case — a real delivered
//! reply with `try_send` success and slot-removal — is NOT benched
//! here because (a) every iteration consumes the slot and needs
//! re-arming, requiring a custom iteration routine, and (b) the path
//! fires at most once per second in production. Any regression there
//! would show up in end-to-end latency tests, not this micro-bench.

use conductor_core::device_intelligence::probe::ProbeCoordinator;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;

/// Canned KORG Identity Reply (15 bytes, 1-byte manufacturer ID).
const KORG_REPLY: &[u8] = &[
    0xF0, 0x7E, 0x00, 0x06, 0x02, 0x42, 0x34, 0x00, 0x01, 0x00, 0x01, 0x02, 0x03, 0x04, 0xF7,
];

/// Note-On message — not SysEx, used to measure the gate-short-circuit
/// path. In production the daemon's is_sysex check rejects this before
/// reaching observe_reply; here we deliberately bypass the gate to
/// measure the coordinator's own early-return cost.
const NOTE_ON: &[u8] = &[0x90, 0x3C, 0x7F];

fn bench_observe_reply_empty_map_non_sysex(c: &mut Criterion) {
    let coord = Arc::new(ProbeCoordinator::new());
    c.bench_function("probe_ingress::empty_map_non_sysex", |b| {
        b.iter(|| {
            coord.observe_reply(black_box("port-x"), black_box(NOTE_ON));
        });
    });
}

fn bench_observe_reply_empty_map_sysex(c: &mut Criterion) {
    let coord = Arc::new(ProbeCoordinator::new());
    c.bench_function("probe_ingress::empty_map_sysex_frame", |b| {
        b.iter(|| {
            coord.observe_reply(black_box("port-x"), black_box(KORG_REPLY));
        });
    });
}

fn bench_observe_reply_pending_mismatched_port(c: &mut Criterion) {
    // Register a pending probe on a different port than the one we
    // fire observe_reply against. Shape check passes; lock acquired;
    // the get-by-port-name misses; no parse happens; lock released.
    //
    // Important: the probe thread we spawn here must be cleaned up
    // after the bench completes — otherwise it blocks on recv_timeout
    // for up to a minute and adds noise to subsequent benchmarks in
    // the same process. We `invalidate` to drop its Sender (waker
    // with Disconnected), then join the handle.
    let coord = Arc::new(ProbeCoordinator::new().with_timeout(std::time::Duration::from_secs(60)));
    let coord_bg = coord.clone();
    let (registered_tx, registered_rx) = crossbeam_channel::bounded::<()>(1);
    let probe_handle = std::thread::spawn(move || {
        let _ = coord_bg.probe("probe-port-A", |_| {
            registered_tx.send(()).unwrap();
            Ok(())
        });
    });
    registered_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("probe should register pending slot");

    c.bench_function("probe_ingress::pending_mismatched_port", |b| {
        b.iter(|| {
            // observe against a different port — goes through the
            // structural check, then the pending-map lookup misses.
            coord.observe_reply(black_box("port-B"), black_box(KORG_REPLY));
        });
    });

    // Clean up the pending probe so its thread exits before the next
    // benchmark runs (or the bench binary returns). Without this the
    // thread would sit on `recv_timeout(60s)` and add noise.
    coord.invalidate("probe-port-A");
    probe_handle
        .join()
        .expect("probe thread should exit cleanly after invalidate");
}

fn bench_sysex_shape_check_no_pending(c: &mut Criterion) {
    // Valid-shape Identity Reply against a port with no pending slot.
    // Shape check passes; lock taken; get misses; lock released.
    // Equivalent cost profile to `empty_map_sysex_frame` above — this
    // function exists as a regression anchor in case the two paths
    // diverge (e.g. a future optimisation adds a pending-count check
    // before taking the lock, which would change one but not the
    // other).
    let coord = Arc::new(ProbeCoordinator::new());
    c.bench_function("probe_ingress::sysex_shape_check_no_pending", |b| {
        b.iter(|| {
            coord.observe_reply(black_box("no-pending-port"), black_box(KORG_REPLY));
        });
    });
}

criterion_group!(
    benches,
    bench_observe_reply_empty_map_non_sysex,
    bench_observe_reply_empty_map_sysex,
    bench_observe_reply_pending_mismatched_port,
    bench_sysex_shape_check_no_pending,
);
criterion_main!(benches);
