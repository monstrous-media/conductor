// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! `conductorctl mcp revoke --exe-path <PATH>` does not
//! canonicalize the supplied path, but `mcp register` does. Revoking
//! the SAME logical binary via a different path representation
//! (symlink, `..`, etc.) silently no-ops and leaves the permission
//! grant in place — a security gap for a grant table.
//!
//! Fix: apply `std::fs::canonicalize` to `--exe-path` inside
//! `handle_mcp_revoke` the same way `handle_mcp_register` already
//! does. Also enforce absolute-path requirement for symmetry.
//!
//! Black-box subprocess test:
//! - tempdir-isolated HOME/XDG_DATA_HOME so the test never touches
//!   the user's real registry
//! - register a binary at its canonical path
//! - revoke it through a symlink that points at the same canonical
//!   path
//! - assert the registry no longer contains the entry
//!
//! Pre-fix: revoke returns `"removed": false`, registry still
//! contains the entry, test fails.
//! Post-fix: revoke canonicalizes the symlink, matches the stored
//! canonical entry, removes it.
//!
//! The prior canonicalize-or-die behavior meant a DELETED/moved registered
//! binary could never be revoked (canonicalize errors on a missing
//! file), so the stale grant survived. `mcp_revoke_removes_entry_for_deleted_binary`
//! pins the fix: canonicalization is now best-effort and revocation
//! works by exe path whether or not the binary still exists.

#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;

fn conductorctl_path() -> PathBuf {
    if let Some(p) = option_env!("CARGO_BIN_EXE_conductorctl") {
        return PathBuf::from(p);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("workspace root")
        .join("target")
        .join("debug")
        .join("conductorctl")
}

/// Resolve the on-disk `mcp_registry.json` path under our tempdir.
///
/// `dirs::data_local_dir()`:
///   - macOS: `$HOME/Library/Application Support`
///   - Linux: `XDG_DATA_HOME` or `$HOME/.local/share`
///
/// We try the platform-appropriate path first, then fall back.
fn locate_registry(tempdir: &std::path::Path) -> PathBuf {
    let candidates = if cfg!(target_os = "macos") {
        vec![
            tempdir
                .join("Library")
                .join("Application Support")
                .join("conductor")
                .join("mcp_registry.json"),
        ]
    } else {
        vec![
            tempdir
                .join("data")
                .join("conductor")
                .join("mcp_registry.json"),
            tempdir
                .join(".local")
                .join("share")
                .join("conductor")
                .join("mcp_registry.json"),
        ]
    };
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }
    panic!(
        "mcp_registry.json not found under tempdir; tried:\n  {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// Run `conductorctl` with HOME + XDG_DATA_HOME pinned to the
/// tempdir so the test never touches the user's real registry.
fn run_conductorctl(tempdir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(conductorctl_path())
        .args(args)
        .env("HOME", tempdir)
        .env("XDG_DATA_HOME", tempdir.join("data"))
        .env("XDG_CONFIG_HOME", tempdir.join("config"))
        .output()
        .unwrap_or_else(|e| panic!("run conductorctl {args:?}: {e}"))
}

/// THE regression test. Register via canonical path,
/// revoke via symlink to that same canonical path, assert the entry
/// is gone.
#[test]
fn mcp_revoke_canonicalizes_symlinked_exe_path() {
    let tempdir = tempfile::tempdir().expect("tempdir");

    // Create the "real" binary (an empty file is enough — register
    // only canonicalizes, it does not exec).
    let bin_dir = tempdir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create bin dir");
    let real_exe = bin_dir.join("mcp-client-bin");
    std::fs::write(&real_exe, b"dummy").expect("write real exe");

    // Canonicalize for comparison — on macOS the tempdir itself can
    // be under `/var → /private/var` (symlink), so the path we hand
    // to `register` should be the canonical one.
    let canonical_real = std::fs::canonicalize(&real_exe).expect("canonicalize real exe");

    // Create a symlink pointing at the real binary. Revoke will be
    // called through this symlink path.
    let symlink_exe = bin_dir.join("mcp-client-link");
    symlink(&canonical_real, &symlink_exe).expect("create symlink");

    // Sanity: symlink and real path differ as strings, but resolve
    // to the same canonical path.
    assert_ne!(
        symlink_exe.as_path(),
        canonical_real.as_path(),
        "symlink and canonical must differ as strings for the test \
         to be meaningful"
    );
    assert_eq!(
        std::fs::canonicalize(&symlink_exe).expect("canonicalize symlink"),
        canonical_real,
        "symlink must resolve to the same canonical path as real exe"
    );

    // Register via the canonical path. After this the registry
    // contains exactly one entry whose `exe_path` is `canonical_real`.
    let reg = run_conductorctl(
        tempdir.path(),
        &[
            "mcp",
            "register",
            "--name",
            "test-client",
            "--exe-path",
            canonical_real.to_str().unwrap(),
            "--tier",
            "ReadOnly",
        ],
    );
    assert!(
        reg.status.success(),
        "mcp register must succeed; stderr={}\nstdout={}",
        String::from_utf8_lossy(&reg.stderr),
        String::from_utf8_lossy(&reg.stdout),
    );

    let registry_path = locate_registry(tempdir.path());
    let after_register: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&registry_path).expect("read registry"))
            .expect("parse registry json");
    let entries_after_register = after_register["entries"].as_array().expect("entries array");
    assert_eq!(
        entries_after_register.len(),
        1,
        "registry must have exactly one entry after register; got:\n{after_register}"
    );

    // Now revoke via the SYMLINK path. Pre-fix this is a silent
    // no-op (lookup by literal symlink path against canonical entry
    // misses). Post-fix `handle_mcp_revoke` canonicalizes first and
    // matches the registered entry.
    let rev = run_conductorctl(
        tempdir.path(),
        &[
            "--json",
            "mcp",
            "revoke",
            "--exe-path",
            symlink_exe.to_str().unwrap(),
        ],
    );
    assert!(
        rev.status.success(),
        "mcp revoke must succeed (exit 0 is the documented contract \
         for both removed and no-op cases); stderr={}\nstdout={}",
        String::from_utf8_lossy(&rev.stderr),
        String::from_utf8_lossy(&rev.stdout),
    );

    // Read the registry again — Bob is gone, Alice survives. Wait,
    // wrong epic. Read the registry — the single entry should be
    // gone.
    let after_revoke: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&registry_path).expect("read registry"))
            .expect("parse registry json");
    let entries_after_revoke = after_revoke["entries"].as_array().expect("entries array");
    assert!(
        entries_after_revoke.is_empty(),
        "registry MUST be empty after revoke via symlink — pre-fix the \
         literal symlink path didn't match the canonical stored entry, \
         leaving the grant in place. Got:\n{after_revoke}"
    );

    // And the revoke output should report removed=true (not the
    // pre-fix silent no-op).
    let rev_json: serde_json::Value =
        serde_json::from_slice(&rev.stdout).expect("parse revoke json output");
    assert_eq!(
        rev_json["removed"],
        serde_json::Value::Bool(true),
        "revoke output must report removed=true after canonicalization \
         resolves symlink to the registered canonical path; got:\n{rev_json}"
    );
}

/// THE regression test. Register a binary, DELETE it, then
/// revoke by its (now non-existent) absolute path. Pre-fix
/// `handle_mcp_revoke` canonicalized unconditionally, so revoking a
/// deleted binary failed (`canonicalize` errors on a missing file) and
/// the stale grant survived — a future binary at the same path would
/// inherit the tier ceiling. Post-fix canonicalization is best-effort:
/// the literal absolute path (== the canonical path it was registered
/// at) still matches and the entry is removed.
#[test]
fn mcp_revoke_removes_entry_for_deleted_binary() {
    let tempdir = tempfile::tempdir().expect("tempdir");

    let bin_dir = tempdir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create bin dir");
    let real_exe = bin_dir.join("doomed-client");
    std::fs::write(&real_exe, b"dummy").expect("write real exe");

    // Register at the CANONICAL path (on macOS the tempdir can sit under
    // a `/var → /private/var` symlink, so canonicalize for an exact key).
    let canonical_real = std::fs::canonicalize(&real_exe).expect("canonicalize real exe");
    let reg = run_conductorctl(
        tempdir.path(),
        &[
            "mcp",
            "register",
            "--name",
            "doomed-client",
            "--exe-path",
            canonical_real.to_str().unwrap(),
            "--tier",
            "ReadOnly",
        ],
    );
    assert!(
        reg.status.success(),
        "mcp register must succeed; stderr={}",
        String::from_utf8_lossy(&reg.stderr),
    );

    let registry_path = locate_registry(tempdir.path());
    let after_register: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&registry_path).expect("read registry"))
            .expect("parse registry json");
    assert_eq!(
        after_register["entries"].as_array().expect("entries").len(),
        1,
        "registry must have one entry after register; got:\n{after_register}"
    );

    // Delete the binary — this is the whole point of the test.
    std::fs::remove_file(&canonical_real).expect("delete the registered binary");
    assert!(
        !canonical_real.exists(),
        "binary must be gone so canonicalize() would fail"
    );

    // Revoke by the now-deleted absolute path. Pre-fix: non-zero exit
    // (canonicalize fails) AND the grant survives. Post-fix: exit 0,
    // removed=true, entry gone.
    let rev = run_conductorctl(
        tempdir.path(),
        &[
            "--json",
            "mcp",
            "revoke",
            "--exe-path",
            canonical_real.to_str().unwrap(),
        ],
    );
    assert!(
        rev.status.success(),
        "mcp revoke of a DELETED registered binary must exit 0; stderr={}\nstdout={}",
        String::from_utf8_lossy(&rev.stderr),
        String::from_utf8_lossy(&rev.stdout),
    );

    let rev_json: serde_json::Value =
        serde_json::from_slice(&rev.stdout).expect("parse revoke json output");
    assert_eq!(
        rev_json["removed"],
        serde_json::Value::Bool(true),
        "revoke of a deleted-but-registered binary must report removed=true; got:\n{rev_json}"
    );

    let after_revoke: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&registry_path).expect("read registry"))
            .expect("parse registry json");
    assert!(
        after_revoke["entries"]
            .as_array()
            .expect("entries")
            .is_empty(),
        "registry MUST be empty after revoking a deleted binary — pre-fix the \
         stale grant survived because canonicalize() failed. Got:\n{after_revoke}"
    );
}

/// Symmetry with `mcp register`: revoke MUST also reject relative
/// paths. Pre-fix both handlers diverged — register validated,
/// revoke didn't. Same path-validation contract on both sides.
#[test]
fn mcp_revoke_rejects_relative_exe_path() {
    let tempdir = tempfile::tempdir().expect("tempdir");

    let out = run_conductorctl(
        tempdir.path(),
        &["mcp", "revoke", "--exe-path", "relative/path/binary"],
    );
    assert!(
        !out.status.success(),
        "mcp revoke with a relative --exe-path MUST exit non-zero \
         (matches mcp register's behaviour); stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("absolute") || stderr.contains("relative"),
        "error message should mention absolute/relative path \
         requirement; got:\n{stderr}"
    );
}
