// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase B-early Slice B.1 — cross-platform OS-keyring roundtrip.
//!
//! Kept separate from `keychain_test.rs` (which is Unix-only, using
//! `libc`/`O_NOFOLLOW`) so the keyring path also **compiles on Windows** and is
//! not masked behind a `#![cfg(unix)]` gate.
//!
//! Gated to macOS + Windows: those CI/dev runners have an interactive-user
//! keychain. Linux is excluded here because the `keyring` `linux-native`
//! backend is the kernel keyutils keyring, whose availability/persistence
//! varies across CI runners — the deterministic trait-contract coverage on
//! Linux comes from the `FilePermsKeychain` tests in `keychain_test.rs`.
//!
//! Tolerant of an unavailable/locked backend (common on CI): it skips rather
//! than failing, so it never produces a false red.
//!
//! Execution is opt-in via `CONDUCTOR_KEYCHAIN_INTEGRATION=1`. Touching the real
//! OS keychain can trigger a first-access ACL prompt (≈60s timeout on a fresh
//! macOS runner) or hang on a locked store, so it does not run in the default
//! CI suite — the `FilePermsKeychain` tests give the deterministic coverage.
//! The file still *compiles* on macOS + Windows regardless of the env var, so
//! the keyring path can't silently rot.

#![cfg(any(target_os = "macos", target_os = "windows"))]

use conductor_core::security::keychain::{KeychainStore, KeyringKeychain};

#[test]
fn keyring_roundtrip_when_available() {
    if std::env::var_os("CONDUCTOR_KEYCHAIN_INTEGRATION").is_none() {
        return; // opt-in only; see module docs.
    }
    let kc = KeyringKeychain::new();
    let first = match kc.get_or_create_hmac_key() {
        Ok(k) => k,
        // Backend unavailable/locked (e.g. a headless CI keychain) → skip.
        Err(_) => return,
    };
    let again = kc.get_or_create_hmac_key().expect("reload from keychain");
    assert_eq!(
        first.fingerprint(),
        again.fingerprint(),
        "second get_or_create must return the persisted key"
    );

    let rotated = kc.rotate_hmac_key().expect("rotate");
    assert_ne!(
        first.fingerprint(),
        rotated.fingerprint(),
        "rotation must produce a different key"
    );
}
