// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! Workflow concurrency + cargo-isolation policy test (#1669, #1675).
//!
//! Root cause (#1669): the macOS dev-box hosts two self-hosted runners
//! (`MacBookPro.localdomain-runner-1` / `-2`) as the same user with the
//! same default `~/.cargo`. Two concurrent macOS tauri builds extract
//! `tauri-*` into the same `registry/src/` and race, producing the
//! intermittent `bundle.global.js: No such file or directory` failure.
//!
//! #1670 tried to fix it with a repo-wide per-job concurrency group +
//! `cancel-in-progress: false`. That backfired (#1675): GitHub
//! concurrency holds at most ONE pending entry per group and cancels
//! any earlier pending one, so a backlog of PRs mass-cancelled their
//! macOS jobs — `main` included. Concurrency is a dedup primitive, not
//! a queue.
//!
//! The correct fix isolates the cargo registry per runner via
//! `CARGO_HOME=$HOME/.cargo-<runner.name>` on the dev-box jobs, so the
//! two runners physically cannot share `registry/src/`. No cross-PR
//! serialisation needed; PRs run in parallel.
//!
//! This file pins the structural invariants of that fix:
//!
//!   1. Workflow-level concurrency is per-REF dedup
//!      (`${{ github.workflow }}-${{ github.ref }}`, cancel-in-progress
//!      true) — NOT a repo-wide serialisation group.
//!   2. Neither `tauri-build` nor `tauri-lint` has a job-level
//!      `concurrency:` block (that's what mass-cancelled jobs in #1670).
//!   3. `tauri-build` has a macOS-gated step that sets a per-runner
//!      `CARGO_HOME` keyed on `${{ runner.name }}`.
//!   4. `tauri-lint` has the same per-runner `CARGO_HOME` step.
//!   5. The #1645 Bundle C cleanup step is absent.
//!   6. No step under the build/lint jobs `rm -rf`s the cargo registry
//!      source dir.
//!
//! Uses `serde_yaml` typed traversal, matching the style of
//! `tests/tauri_path_filter_test.rs`.

use serde_yaml::Value;
use std::path::PathBuf;

fn tauri_workflow_yaml() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/tauri-build.yml");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_yaml::from_str(&raw).unwrap_or_else(|e| panic!("parse tauri-build.yml as YAML: {e}"))
}

fn ci_workflow_yaml() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_yaml::from_str(&raw).unwrap_or_else(|e| panic!("parse ci.yml as YAML: {e}"))
}

fn job<'a>(workflow: &'a Value, job_id: &str) -> &'a Value {
    let jobs = workflow
        .get("jobs")
        .unwrap_or_else(|| panic!("workflow has no `jobs` key"));
    jobs.get(job_id).unwrap_or_else(|| {
        panic!(
            "no `jobs.{job_id}` in workflow — structure changed; \
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

fn step_name(step: &Value) -> Option<&str> {
    step.get("name").and_then(Value::as_str)
}

/// Find the per-runner cargo + rustup isolation step in a job, or None.
/// Identified by a `run` body that writes BOTH `CARGO_HOME=...cargo-...`
/// AND `RUSTUP_HOME=...rustup-...` to `$GITHUB_ENV`, and an `env` that
/// maps something to `${{ runner.name }}`.
///
/// Requiring RUSTUP_HOME is the #1690 follow-up to #1675: the dev-box
/// runners also share `~/.rustup`, so concurrent `dtolnay/rust-toolchain`
/// installs corrupt the toolchain dir. Both env vars must be set in the
/// same step, before the toolchain install, so dtolnay honours them.
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

#[test]
fn workflow_concurrency_is_per_ref_dedup() {
    let workflow = tauri_workflow_yaml();
    let conc = workflow.get("concurrency").unwrap_or_else(|| {
        panic!("workflow must declare a top-level `concurrency:` block (per-ref dedup)")
    });
    let group = conc
        .get("group")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("concurrency.group must be a scalar string"));
    assert!(
        group.contains("github.ref"),
        "workflow concurrency.group must be per-REF (contain `github.ref`) \
         so a new push to the same ref dedups the stale run — NOT a \
         repo-wide group, which mass-cancels cross-PR jobs (#1675). \
         Found: {group:?}"
    );
    let cancel = conc
        .get("cancel-in-progress")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("concurrency.cancel-in-progress must be a bool"));
    assert!(
        cancel,
        "workflow concurrency.cancel-in-progress must be `true` for \
         per-ref dedup (cancel the stale run on a new push to the same \
         ref). Found: {cancel}"
    );
}

#[test]
fn build_and_lint_have_no_job_level_concurrency() {
    let workflow = tauri_workflow_yaml();
    for job_id in ["tauri-build", "tauri-lint"] {
        assert!(
            job(&workflow, job_id).get("concurrency").is_none(),
            "jobs.{job_id} must NOT declare a job-level `concurrency:` \
             block — #1670's repo-wide per-job group mass-cancelled \
             macOS jobs (#1675). Isolation is via per-runner CARGO_HOME \
             instead."
        );
    }
}

#[test]
fn tauri_build_isolates_cargo_and_rustup_per_runner_on_macos() {
    let workflow = tauri_workflow_yaml();
    let step = find_isolation_step(&workflow, "tauri-build").unwrap_or_else(|| {
        panic!(
            "jobs.tauri-build must include a step that sets per-runner \
             CARGO_HOME *and* RUSTUP_HOME (run writes both \
             `CARGO_HOME=...cargo-...` and `RUSTUP_HOME=...rustup-...` to \
             $GITHUB_ENV, env maps to runner.name) — #1675 + #1690"
        )
    });
    let if_expr = step.get("if").and_then(Value::as_str).unwrap_or_else(|| {
        panic!("the tauri-build isolation step must be macOS-gated with an `if:`")
    });
    assert!(
        if_expr.contains("runner.os == 'macOS'"),
        "the tauri-build isolation step must be gated on `runner.os == \
         'macOS'` — the Linux matrix cell runs on a different host and \
         must keep the default CARGO_HOME / RUSTUP_HOME. Found: {if_expr:?}"
    );
}

#[test]
fn tauri_lint_isolates_cargo_and_rustup_per_runner() {
    let workflow = tauri_workflow_yaml();
    find_isolation_step(&workflow, "tauri-lint").unwrap_or_else(|| {
        panic!(
            "jobs.tauri-lint must include a per-runner CARGO_HOME + \
             RUSTUP_HOME step (it runs on the dev-box and does cargo + \
             rustup work via clippy/fmt — same race surface as the macOS \
             build, #1675 + #1690)"
        )
    });
}

#[test]
fn defensive_cleanup_step_is_absent() {
    let workflow = tauri_workflow_yaml();
    for job_id in ["tauri-build", "tauri-lint"] {
        let found = job_steps(&workflow, job_id).iter().any(|s| {
            step_name(s)
                .map(|n| n.contains("Defensive cleanup of tauri registry source"))
                .unwrap_or(false)
        });
        assert!(
            !found,
            "the #1622 / #1645 Bundle C cleanup step must NOT be in \
             jobs.{job_id}.steps — superseded by per-runner CARGO_HOME \
             isolation (#1675)"
        );
    }
}

#[test]
fn no_step_rm_rfs_cargo_registry_src() {
    let workflow = tauri_workflow_yaml();
    for job_id in ["tauri-build", "tauri-lint"] {
        for step in job_steps(&workflow, job_id) {
            let Some(run) = step.get("run").and_then(Value::as_str) else {
                continue;
            };
            let rms_registry =
                (run.contains("rm -rf") || run.contains("rm -fr")) && run.contains("registry/src");
            assert!(
                !rms_registry,
                "no step in jobs.{job_id}.steps may `rm -rf` the cargo \
                 registry source dir — that's the failure mode of #1669. \
                 Step name: {:?}\nrun:\n{run}",
                step_name(step)
            );
        }
    }
}

// ── ci.yml coverage for the #1690 rustup follow-up ────────────────
//
// `ci.yml` has the same per-runner isolation pattern as `tauri-build.yml`
// in its macOS-touching jobs: the `build` matrix (macOS-gated isolation
// step) and `build-intel-mac` (dev-box, unconditional). Both must set
// CARGO_HOME *and* RUSTUP_HOME, or the rustup race in #1690 recurs.

#[test]
fn ci_build_isolates_cargo_and_rustup_per_runner_on_macos() {
    let workflow = ci_workflow_yaml();
    let step = find_isolation_step(&workflow, "build").unwrap_or_else(|| {
        panic!(
            "jobs.build in ci.yml must include a step that sets per-runner \
             CARGO_HOME *and* RUSTUP_HOME (#1675 + #1690). Without \
             RUSTUP_HOME isolation, concurrent dtolnay/rust-toolchain \
             installs corrupt the shared ~/.rustup."
        )
    });
    let if_expr = step.get("if").and_then(Value::as_str).unwrap_or_else(|| {
        panic!(
            "the ci.yml build isolation step must be macOS-gated with an \
             `if:` (the target matrix also covers Linux)"
        )
    });
    assert!(
        if_expr.contains("runner.os == 'macOS'"),
        "the ci.yml build isolation step must be gated on `runner.os == \
         'macOS'` — the Linux matrix cell runs on a different host. \
         Found: {if_expr:?}"
    );
}

#[test]
fn ci_build_intel_mac_isolates_cargo_and_rustup_per_runner() {
    let workflow = ci_workflow_yaml();
    find_isolation_step(&workflow, "build-intel-mac").unwrap_or_else(|| {
        panic!(
            "jobs.build-intel-mac in ci.yml must include a per-runner \
             CARGO_HOME + RUSTUP_HOME step — it runs on the same dev-box \
             pool as tauri-build's macOS cell and races the same \
             ~/.cargo + ~/.rustup (#1675 + #1690)"
        )
    });
}
