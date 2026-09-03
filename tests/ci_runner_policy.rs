// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Workflow security policy test (#1444).
//!
//! Self-hosted runners must never execute code from fork pull requests:
//! cargo build scripts, proc-macros, tests, and benches run arbitrary
//! contributor code, and a persistent self-hosted runner exposes local
//! state, caches, and credentials.
//!
//! Therefore any job in `ci.yml` that pins a literal `self-hosted` runner
//! must be gated to non-pull-request events (push only). Jobs that run on
//! pull requests must select their runner via the `runner-policy` job,
//! which falls back to GitHub-hosted runners for fork PRs.

use std::path::PathBuf;

fn ci_workflow() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[test]
fn self_hosted_runners_are_gated_off_pull_requests() {
    let yaml = ci_workflow();
    let lines: Vec<&str> = yaml.lines().collect();
    let jobs_start = lines
        .iter()
        .position(|l| *l == "jobs:")
        .expect("ci.yml has a `jobs:` key");

    let mut violations: Vec<String> = Vec::new();
    let mut i = jobs_start + 1;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        // Job headers are `  <id>:` at exactly 2-space indent.
        let is_job_header =
            indent(line) == 2 && trimmed.ends_with(':') && !trimmed.starts_with('#');
        if !is_job_header {
            i += 1;
            continue;
        }
        let job = trimmed.trim_end_matches(':').to_string();

        // Collect the job body — lines until the next indent <= 2.
        let mut body: Vec<&str> = Vec::new();
        let mut j = i + 1;
        while j < lines.len() {
            let l = lines[j];
            if !l.trim().is_empty() && indent(l) <= 2 {
                break;
            }
            body.push(l);
            j += 1;
        }
        i = j;

        // Does the job pin a literal self-hosted runner (in `runs-on:` or a
        // matrix `os:` entry — not inside a `run:` script)?
        let pins_self_hosted = body.iter().any(|l| {
            let t = l.trim_start();
            (t.starts_with("runs-on:") || t.starts_with("os:") || t.starts_with("- os:"))
                && t.contains("self-hosted")
        });
        if !pins_self_hosted {
            continue;
        }

        // Capture the job-level `if:` block (4-space indent + continuations).
        let mut if_expr = String::new();
        let mut k = 0;
        while k < body.len() {
            let l = body[k];
            if indent(l) == 4 && l.trim_start().starts_with("if:") {
                if_expr.push_str(l.trim());
                let mut m = k + 1;
                while m < body.len() && indent(body[m]) > 4 {
                    if_expr.push(' ');
                    if_expr.push_str(body[m].trim());
                    m += 1;
                }
                break;
            }
            k += 1;
        }

        // Push-gated = the `if:` guarantees fork-PR code never runs the job:
        // `event_name == 'push'`, or `event_name != 'pull_request'` not
        // widened back onto fork PRs by an `||`.
        //
        // A bare `|| <other>` re-includes pull requests and IS widening. The
        // exception is `|| ...head.repo.full_name == github.repository`: that
        // admits only same-repository (trusted) PRs, never forks, so it is a
        // safe gate, not a widening one.
        let widening_or = if_expr.contains("||")
            && !if_expr
                .contains("|| github.event.pull_request.head.repo.full_name == github.repository");
        let push_gated = if_expr.contains("github.event_name == 'push'")
            || (if_expr.contains("github.event_name != 'pull_request'") && !widening_or);

        if !push_gated {
            violations.push(job);
        }
    }

    assert!(
        violations.is_empty(),
        "Jobs pin a literal `self-hosted` runner without gating off \
         pull_request events (#1444) — fork-PR code could execute on a \
         self-hosted runner. Offending jobs in .github/workflows/ci.yml: {}. \
         Route pull-request jobs through the `runner-policy` job.",
        violations.join(", ")
    );
}

#[test]
fn ci_workflow_concurrency_is_per_ref_dedup() {
    // ci.yml must declare a top-level `concurrency:` block whose group is
    // per-REF (contains `github.ref`) so a new push to the same ref dedups
    // the stale run — NOT a repo-wide group, which mass-cancels cross-PR
    // jobs.
    let yaml = ci_workflow();
    let lines: Vec<&str> = yaml.lines().collect();
    let conc_idx = lines
        .iter()
        .position(|l| *l == "concurrency:")
        .expect("ci.yml must declare a top-level `concurrency:` block");
    let group_line = lines[conc_idx..]
        .iter()
        .take_while(|l| l.trim().is_empty() || indent(l) > 0 || **l == "concurrency:")
        .find(|l| l.trim_start().starts_with("group:"))
        .expect("concurrency block must declare a `group:` key");
    assert!(
        group_line.contains("github.ref"),
        "ci.yml concurrency.group must be per-REF (contain `github.ref`); \
         found: {group_line:?}"
    );
}
