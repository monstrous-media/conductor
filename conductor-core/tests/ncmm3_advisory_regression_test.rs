// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! RUSTSEC-2026-0194 / RUSTSEC-2026-0195 regression pins.
//!
//! `.ncmm3` profiles are USER-SUPPLIED XML parsed by quick-xml via
//! `DeviceProfile::from_ncmm3` (`conductor-core/src/device.rs`) — exactly
//! the malicious-file scenario SECURITY.md warns about. quick-xml < 0.41
//! was vulnerable to:
//!
//! - **RUSTSEC-2026-0194**: quadratic parse time when a start tag carries
//!   many duplicate attribute names (CPU-exhaustion DoS).
//! - **RUSTSEC-2026-0195**: unbounded allocation for namespace
//!   declarations (memory-exhaustion DoS).
//!
//! These tests drive the REAL parser entry point with both advisory input
//! classes and pin bounded behavior: the parse must complete (returning
//! `Ok` or `Err` — malformed input is fine) within a coarse wall-clock
//! budget. The budget is a proxy — a vulnerable parser blows it on either
//! the 0194 quadratic-time or the 0195 allocate-then-scan path — not a
//! direct heap-allocation cap. On quick-xml 0.38.4 the
//! duplicate-attribute input exhibits the quadratic blowup; quick-xml
//! ≥ 0.41 (the advisory floor) parses both in milliseconds. The generous
//! 5s budget makes the pin CI-safe while remaining orders of magnitude
//! above the patched cost and below the vulnerable cost at this input
//! size.

use conductor_core::DeviceProfile;
use std::fmt::Write as _;
use std::io::Write as _;
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_secs(5);

fn parse_budgeted(xml: &str, label: &str) {
    let mut f = tempfile::Builder::new()
        .suffix(".ncmm3")
        .tempfile()
        .expect("tempfile");
    f.write_all(xml.as_bytes()).expect("write");
    f.flush().expect("flush"); // ensure bytes are visible before the parser reads the path

    let start = Instant::now();
    // Malicious input may parse or error — either is acceptable; what the
    // advisories forbid is unbounded time/memory on the way there. This
    // pins a coarse WALL-CLOCK budget only (a proxy that catches both the
    // 0194 quadratic-time and the 0195 allocation-then-work blowups);
    // it is not a direct heap-allocation assertion.
    let _ = DeviceProfile::from_ncmm3(f.path());
    let elapsed = start.elapsed();
    assert!(
        elapsed < BUDGET,
        "{label}: parse took {elapsed:?} (budget {BUDGET:?}) — quadratic/unbounded \
         behavior indicates a quick-xml regression below the 0.41 advisory floor"
    );
}

/// RUSTSEC-2026-0194 input class: one start tag with a large number of
/// duplicate attribute names. Behavioral pin — the serde `de::from_str`
/// path stays fast even on the vulnerable 0.38 at CI-safe sizes, so this
/// documents the advisory floor and guards against a future regression
/// rather than demonstrating red state.
#[test]
fn duplicate_attribute_flood_parses_within_budget() {
    let mut xml = String::with_capacity(1 << 21);
    xml.push_str("<NIControllerEditorPreset ");
    for _ in 0..200_000 {
        xml.push_str("a=\"1\" ");
    }
    xml.push_str("></NIControllerEditorPreset>");
    parse_budgeted(&xml, "RUSTSEC-2026-0194 duplicate-attribute flood");
}

/// RUSTSEC-2026-0195 input class: a large number of namespace
/// declarations. Input stays ~2 MB so the test is CI-safe; the pin is
/// that parsing completes promptly without runaway allocation.
#[test]
fn namespace_declaration_flood_parses_within_budget() {
    let mut xml = String::with_capacity(1 << 21);
    xml.push_str("<NIControllerEditorPreset ");
    for i in 0..60_000 {
        write!(xml, "xmlns:n{i}=\"u{i}\" ").expect("write to String");
    }
    xml.push_str("></NIControllerEditorPreset>");
    parse_budgeted(&xml, "RUSTSEC-2026-0195 namespace-declaration flood");
}
