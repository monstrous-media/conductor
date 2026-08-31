// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! #1487: keep `SECURITY.md`'s supported-versions table in sync with the active
//! release line.
//!
//! The table had drifted to `0.2.x` / `0.1.x` while the workspace shipped
//! `5.6.1-alpha`, leaving the current published line with no documented
//! security-support status. This test derives the current `major.minor` line
//! from the workspace version (`CARGO_PKG_VERSION` of the root crate, which is
//! `version.workspace = true`) and fails if `SECURITY.md` doesn't mark that line
//! as supported — so a new release train can't publish without refreshing the
//! table.

#[test]
fn security_md_lists_current_release_line_as_supported() {
    // Root-crate tests run with the repo root as the working directory.
    let security = std::fs::read_to_string("SECURITY.md").expect("SECURITY.md at repo root");

    // Workspace version, e.g. "5.6.1-alpha" → line "5.6.x".
    let version = env!("CARGO_PKG_VERSION");
    let mut parts = version.split('.');
    let major = parts.next().expect("version has a major component");
    let minor = parts
        .next()
        .expect("version has a minor component")
        // strip any pre-release suffix on the minor (defensive; normally on patch)
        .split(['-', '+'])
        .next()
        .unwrap();
    let supported_line = format!("{major}.{minor}.x");

    // The supported line must appear on a row marked supported (✅).
    let row = security
        .lines()
        .find(|l| l.contains(&supported_line))
        .unwrap_or_else(|| {
            panic!(
                "SECURITY.md supported-versions table is missing the current release line \
                 `{supported_line}` (workspace version is `{version}`). Update the table when \
                 starting a new release train."
            )
        });

    assert!(
        row.contains(":white_check_mark:") || row.contains('✅'),
        "SECURITY.md lists `{supported_line}` but does not mark it supported \
         (expected `:white_check_mark:` or ✅); row was: {row:?}"
    );
}
