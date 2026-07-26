// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Workflow security policy test (#1443).
//!
//! Apple signing/notarization secrets (`secrets.APPLE_*`) must never appear
//! in a *job-level* `env:` block of the release workflow. A job-level env
//! exposes them to every step — including `npm ci`, `npm run build`, and
//! `cargo build` (and any npm lifecycle script, `build.rs`, or proc-macro
//! they run) — long before the signing/notarization steps need them.
//!
//! Those secrets belong only on the specific signing/notarization steps.

use std::path::PathBuf;

fn release_workflow() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Leading-space count. Jobs are indented 2 spaces; their keys (`name`,
/// `runs-on`, `env`, `steps`, …) 4 spaces; step-level keys 8+ spaces.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

#[test]
fn apple_secrets_are_not_in_job_level_env() {
    let yaml = release_workflow();
    let mut in_job_env = false;
    let mut violations: Vec<(usize, String)> = Vec::new();

    for (i, line) in yaml.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if in_job_env {
            // A job-level `env:` block ends at the next key indented <= 4.
            if indent(line) <= 4 {
                in_job_env = false;
            } else {
                if line.contains("secrets.APPLE_") {
                    violations.push((i + 1, line.trim_end().to_string()));
                }
                continue;
            }
        }

        // A job-level `env:` block opens with `env:` at exactly 4 spaces.
        if indent(line) == 4 && trimmed == "env:" {
            in_job_env = true;
        }
    }

    assert!(
        violations.is_empty(),
        "Apple signing secrets must be scoped to signing/notarization steps, \
         not a job-level env block (#1443). Offending lines in \
         .github/workflows/release.yml:\n{}",
        violations
            .iter()
            .map(|(n, l)| format!("  {n}: {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
