// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! CI workflow macOS cargo-isolation policy test (#1669, #1675).
//!
//! Root cause (#1669): the macOS dev-box hosts two self-hosted runners
//! (`MacBookPro.localdomain-runner-1` / `-2`) as the same user with the
//! same default `~/.cargo`. Two concurrent macOS jobs extract `tauri-*`
//! into the same `registry/src/` and race, producing the intermittent
//! `bundle.global.js: No such file or directory` proc-macro failure.
//!
//! #1676 fixed tauri-build.yml's macOS jobs by isolating the cargo
//! registry per runner (`CARGO_HOME=$HOME/.cargo-<runner.name>`), after
//! #1670's concurrency-serialisation approach backfired by
//! mass-cancelling cross-PR jobs (#1675). But ci.yml's macOS jobs run on
//! the SAME dev-box and share the SAME `~/.cargo` — they need the same
//! isolation or they re-open the race (and race against tauri-build's
//! jobs too). The two ci.yml jobs that land on the dev-box:
//!
//!   - `build` job, `aarch64-apple-darwin` matrix cell (macOS-gated)
//!   - `build-intel-mac` job (push-only, macOS-only)
//!   - `test-stable` job, `macos-latest` display cell (#1888 / FU-9 — moved
//!     from GitHub-hosted to the self-hosted dev-box once the CoreMIDI tests
//!     learned to skip headlessly)
//!
//! This file pins the structural invariants of the ci.yml side:
//!
//!   1. Workflow-level concurrency is per-REF dedup
//!      (`${{ github.workflow }}-${{ github.ref }}`, cancel-in-progress
//!      true) — NOT a repo-wide serialisation group (the #1670 mistake).
//!   2. Neither `build` nor `build-intel-mac` has a job-level
//!      `concurrency:` block.
//!   3. `build` has a macOS-gated step that sets per-runner `CARGO_HOME`
//!      *and* `RUSTUP_HOME` keyed on `${{ runner.name }}` (#1669 cargo race
//!      + #1690 rustup race).
//!   4. `build-intel-mac` has the same per-runner `CARGO_HOME` + `RUSTUP_HOME`
//!      step.
//!   5. `test-stable` has the same macOS-gated per-runner `CARGO_HOME` +
//!      `RUSTUP_HOME` step (its macOS cell now lands on the dev-box, #1888).
//!
//! Uses `serde_yaml` typed traversal, matching
//! `tests/tauri_build_concurrency_test.rs` (#1675) and
//! `tests/tauri_path_filter_test.rs` (Council R1 on PR #1654).

use serde_yaml::Value;
use std::path::PathBuf;

fn ci_workflow_yaml() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_yaml::from_str(&raw).unwrap_or_else(|e| panic!("parse ci.yml as YAML: {e}"))
}

fn job<'a>(workflow: &'a Value, job_id: &str) -> &'a Value {
    workflow
        .get("jobs")
        .unwrap_or_else(|| panic!("workflow has no `jobs` key"))
        .get(job_id)
        .unwrap_or_else(|| {
            panic!(
                "no `jobs.{job_id}` in ci.yml — structure changed; \
                 update the test for the new layout"
            )
        })
}

fn job_steps<'a>(workflow: &'a Value, job_id: &str) -> &'a Vec<Value> {
    job(workflow, job_id)
        .get("steps")
        .and_then(Value::as_sequence)
        .unwrap_or_else(|| panic!("jobs.{job_id}.steps must be a sequence"))
}

/// Find the per-runner cargo+rustup isolation step in a job, or None.
/// Identified by a `run` body that writes BOTH `CARGO_HOME=...cargo-...` and
/// `RUSTUP_HOME=...rustup-...` to `$GITHUB_ENV`, with an `env` that maps
/// something to `${{ runner.name }}`. Requiring RUSTUP_HOME as well as
/// CARGO_HOME means dropping rustup isolation (re-opening the #1690 rustup
/// race) fails the test, not just dropping cargo isolation (#1669).
/// Same shape as `tests/tauri_build_concurrency_test.rs::find_isolation_step`.
fn find_isolation_step<'a>(workflow: &'a Value, job_id: &str) -> Option<&'a Value> {
    job_steps(workflow, job_id).iter().find(|s| {
        let run_ok = s
            .get("run")
            .and_then(Value::as_str)
            .map(|r| {
                r.contains("CARGO_HOME=")
                    && r.contains(".cargo-")
                    && r.contains("RUSTUP_HOME=")
                    && r.contains(".rustup-")
                    && r.contains("$GITHUB_ENV")
            })
            .unwrap_or(false);
        let keyed_on_runner_name = s
            .get("env")
            .and_then(Value::as_mapping)
            .map(|m| {
                m.values().any(|v| {
                    v.as_str()
                        .map(|s| s.contains("runner.name"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        run_ok && keyed_on_runner_name
    })
}

/// Workflow-level concurrency must be per-REF dedup, NOT a repo-wide
/// serialisation group. The #1670 mistake (repo-wide group) is what
/// mass-cancelled cross-PR jobs (#1675); ci.yml must not repeat it.
#[test]
fn ci_workflow_concurrency_is_per_ref_dedup() {
    let workflow = ci_workflow_yaml();
    let conc = workflow
        .get("concurrency")
        .unwrap_or_else(|| panic!("ci.yml must declare a top-level `concurrency:` block"));
    let group = conc
        .get("group")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("concurrency.group must be a scalar string"));
    assert!(
        group.contains("github.ref"),
        "ci.yml concurrency.group must be per-REF (contain `github.ref`) so a \
         new push to the same ref dedups the stale run — NOT a repo-wide \
         group, which mass-cancels cross-PR jobs (#1675). Found: {group:?}"
    );
}

/// Neither dev-box-touching ci.yml job may carry a job-level
/// `concurrency:` block — that was the #1670 cancellation cause (#1675).
#[test]
fn dev_box_jobs_have_no_job_level_concurrency() {
    let workflow = ci_workflow_yaml();
    for job_id in ["build", "build-intel-mac"] {
        assert!(
            job(&workflow, job_id).get("concurrency").is_none(),
            "jobs.{job_id} must NOT declare a job-level `concurrency:` block — \
             cross-PR serialisation mass-cancels macOS jobs (#1675); the race \
             is fixed by per-runner CARGO_HOME isolation instead."
        );
    }
}

/// The `build` job's macOS cell must isolate CARGO_HOME + RUSTUP_HOME per runner, and
/// the step must be macOS-gated (`if: runner.os == 'macOS'`) so the
/// Linux cell — which runs on a different host — is unaffected.
#[test]
fn build_job_has_macos_gated_cargo_isolation() {
    let workflow = ci_workflow_yaml();
    let step = find_isolation_step(&workflow, "build").unwrap_or_else(|| {
        panic!(
            "jobs.build must contain a per-runner CARGO_HOME + RUSTUP_HOME \
             isolation step (run body writing `CARGO_HOME=$HOME/.cargo-<runner.name>` \
             AND `RUSTUP_HOME=$HOME/.rustup-<runner.name>` to $GITHUB_ENV, keyed on \
             runner.name) so the aarch64-apple-darwin cell can't share the dev-box's \
             ~/.cargo/registry/src (#1669) or ~/.rustup/toolchains (#1690) with \
             another macOS job."
        )
    });
    let cond = step.get("if").and_then(Value::as_str).unwrap_or_else(|| {
        panic!(
            "the build job's cargo-isolation step must be macOS-gated with \
             `if: runner.os == 'macOS'` — only the aarch64-apple-darwin cell \
             lands on the dev-box; the Linux cell must not run it."
        )
    });
    assert!(
        cond.contains("runner.os") && cond.contains("macOS"),
        "build job's cargo-isolation step `if:` must gate on \
         `runner.os == 'macOS'`. Found: {cond:?}"
    );
}

/// The `test-stable` job's macOS cell now lands on the self-hosted dev-box
/// (#1888 / FU-9), so it must carry the same macOS-gated per-runner
/// CARGO_HOME + RUSTUP_HOME isolation step — otherwise it re-opens the #1669 cargo /
/// #1690 rustup race against the build jobs on the shared ~/.cargo / ~/.rustup.
#[test]
fn test_stable_has_macos_gated_cargo_isolation() {
    let workflow = ci_workflow_yaml();
    let step = find_isolation_step(&workflow, "test-stable").unwrap_or_else(|| {
        panic!(
            "jobs.test-stable must contain a per-runner CARGO_HOME + RUSTUP_HOME \
             isolation step — its macOS cell now runs on the dev-box (#1888), sharing \
             ~/.cargo/registry/src and ~/.rustup/toolchains with the build jobs. \
             Without it, test-stable re-opens the #1669 cargo race and the #1690 \
             rustup race."
        )
    });
    let cond = step.get("if").and_then(Value::as_str).unwrap_or_else(|| {
        panic!(
            "test-stable's cargo-isolation step must be macOS-gated with \
             `if: runner.os == 'macOS'` — only the macOS cell lands on the \
             dev-box; the Linux cell must not run it."
        )
    });
    assert!(
        cond.contains("runner.os") && cond.contains("macOS"),
        "test-stable's cargo-isolation step `if:` must gate on \
         `runner.os == 'macOS'`. Found: {cond:?}"
    );
}

/// `build-intel-mac` (push-only, macOS-only) must carry the same
/// per-runner CARGO_HOME + RUSTUP_HOME isolation step.
#[test]
fn build_intel_mac_has_cargo_isolation() {
    let workflow = ci_workflow_yaml();
    assert!(
        find_isolation_step(&workflow, "build-intel-mac").is_some(),
        "jobs.build-intel-mac must contain the per-runner CARGO_HOME + RUSTUP_HOME \
         isolation step — it runs on the dev-box, extracting to ~/.cargo/registry/src \
         (#1669) and installing into ~/.rustup/toolchains (#1690), the same race \
         surface as tauri-build's macOS jobs."
    );
}

/// Cross-file consistency: the dev-box CARGO_HOME isolation pattern must
/// also be present in tauri-build.yml (#1676). If tauri-build.yml ever
/// reverts to the concurrency approach, the two workflows would diverge
/// on how they protect the shared dev-box ~/.cargo — flag it here.
#[test]
fn tauri_build_still_uses_cargo_isolation() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/tauri-build.yml");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let tauri: Value =
        serde_yaml::from_str(&raw).unwrap_or_else(|e| panic!("parse tauri-build.yml as YAML: {e}"));
    assert!(
        find_isolation_step(&tauri, "tauri-build").is_some(),
        "tauri-build.yml's tauri-build job is expected to use per-runner \
         CARGO_HOME isolation (#1676); if it changed, reconcile ci.yml's \
         dev-box jobs to match so both workflows protect the shared \
         ~/.cargo the same way."
    );
}
