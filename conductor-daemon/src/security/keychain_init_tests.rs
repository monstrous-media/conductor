// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Unit tests for keychain init-race + rotation cadence. In a `#[path]`
//! sibling to keep the impl file within the Council verify content budget.

use super::*;
use conductor_core::security::keychain::{HmacKey, KeyMetadata, KeychainError, KeychainStore};
use std::time::{Instant, SystemTime};

/// Keychain whose reported key age is fixed, for cadence/hard-expiry tests.
struct AgedKeychain {
    age_days: u64,
}
/// The fixed key `AgedKeychain` hands out, and its fingerprint.
fn aged_key() -> HmacKey {
    HmacKey::from_bytes([3u8; 32])
}
impl KeychainStore for AgedKeychain {
    fn get_or_create_hmac_key(&self) -> Result<HmacKey, KeychainError> {
        Ok(aged_key())
    }
    fn rotate_hmac_key(&self) -> Result<HmacKey, KeychainError> {
        HmacKey::generate()
    }
    fn key_metadata(&self) -> Result<KeyMetadata, KeychainError> {
        // Metadata fingerprint MATCHES the key get_or_create hands out (the
        // consistent case init_keychain_with requires).
        Ok(KeyMetadata {
            fingerprint: aged_key().fingerprint(),
            created_at: SystemTime::now(),
            created_at_monotonic: Instant::now(),
            age_days: self.age_days,
        })
    }
}

/// A keychain whose metadata fingerprint never matches the key it returns,
/// simulating a `rotate-hmac` racing every init read — `init_keychain_with`
/// must give up rather than apply an expiry decision to the wrong key.
struct DesyncKeychain;
impl KeychainStore for DesyncKeychain {
    fn get_or_create_hmac_key(&self) -> Result<HmacKey, KeychainError> {
        Ok(HmacKey::from_bytes([4u8; 32]))
    }
    fn rotate_hmac_key(&self) -> Result<HmacKey, KeychainError> {
        HmacKey::generate()
    }
    fn key_metadata(&self) -> Result<KeyMetadata, KeychainError> {
        Ok(KeyMetadata {
            fingerprint: "0000000000000000".into(), // never matches [4u8;32]
            created_at: SystemTime::now(),
            created_at_monotonic: Instant::now(),
            age_days: 5,
        })
    }
}

fn lock_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("security");
    (tmp, dir)
}

#[test]
fn rotation_level_ladder() {
    use RotationLevel::*;
    let cases = [
        (0, Ok),
        (179, Ok),
        (180, ConsiderRotation),
        (269, ConsiderRotation),
        (270, ShouldRotate),
        (299, ShouldRotate),
        (300, ApproachingExpiry),
        (364, ApproachingExpiry),
        (365, Deprecated),
        (729, Deprecated),
        (730, HardExpired),
        (5000, HardExpired),
    ];
    for (age, expected) in cases {
        assert_eq!(
            RotationLevel::from_age_days(age),
            expected,
            "age {age} → wrong level"
        );
    }
    assert!(RotationLevel::from_age_days(730).is_hard_expired());
    assert!(!RotationLevel::from_age_days(729).is_hard_expired());
    // Healthy key has no status tag / message; an aged one does.
    assert_eq!(Ok.status_tag(), None);
    assert!(Ok.message(10).is_none());
    assert_eq!(Deprecated.status_tag(), Some("deprecated"));
    assert!(Deprecated.message(400).unwrap().contains("rotate"));
}

#[test]
fn init_healthy_key_succeeds() {
    let (_g, dir) = lock_dir();
    let init = init_keychain_with(&AgedKeychain { age_days: 10 }, &dir).unwrap();
    assert_eq!(init.rotation, RotationLevel::Ok);
    assert_eq!(init.metadata.age_days, 10);
    assert_eq!(init.key.as_bytes(), &[3u8; 32]);
}

#[test]
fn init_deprecated_key_still_starts_with_warning() {
    let (_g, dir) = lock_dir();
    let init = init_keychain_with(&AgedKeychain { age_days: 400 }, &dir).unwrap();
    assert_eq!(init.rotation, RotationLevel::Deprecated);
}

#[test]
fn rotation_status_reports_without_refusing() {
    // Unlike init, the read-only status must REPORT a hard-expired key, not error.
    let s = key_rotation_status(&AgedKeychain { age_days: 800 }).unwrap();
    assert_eq!(s.level, RotationLevel::HardExpired);
    assert_eq!(s.age_days, 800);
    assert_eq!(s.warning_tag(), Some("hard_expired"));
    assert_eq!(s.fingerprint, aged_key().fingerprint());

    let healthy = key_rotation_status(&AgedKeychain { age_days: 5 }).unwrap();
    assert_eq!(healthy.level, RotationLevel::Ok);
    assert_eq!(healthy.warning_tag(), None);
}

#[test]
fn init_hard_expired_key_refuses_to_start() {
    let (_g, dir) = lock_dir();
    let err = init_keychain_with(&AgedKeychain { age_days: 731 }, &dir).unwrap_err();
    assert!(
        matches!(err, KeychainInitError::HardExpired { age_days: 731 }),
        "a >=730-day key must refuse start, got {err:?}"
    );
}

#[test]
fn init_gives_up_when_key_and_metadata_never_agree() {
    // If a rotation races every read so the held key and metadata describe
    // different keys, init must NOT proceed (an expiry decision could otherwise
    // be applied to the wrong key) — it errors instead.
    let (_g, dir) = lock_dir();
    let err = init_keychain_with(&DesyncKeychain, &dir).unwrap_err();
    assert!(
        matches!(err, KeychainInitError::Io { .. }),
        "persistent key/metadata disagreement must fail init, got {err:?}"
    );
}

#[test]
fn init_succeeds_when_key_and_metadata_agree() {
    // The consistent path returns a key whose fingerprint matches its metadata.
    let (_g, dir) = lock_dir();
    let init = init_keychain_with(&AgedKeychain { age_days: 10 }, &dir).unwrap();
    assert_eq!(init.key.fingerprint(), init.metadata.fingerprint);
}

#[test]
fn concurrent_init_serialises_without_deadlock() {
    // Two concurrent inits against the same lock dir must both complete (the
    // flock serialises them) rather than racing or deadlocking.
    let (_g, dir) = lock_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let d = dir.clone();
            std::thread::spawn(move || {
                init_keychain_with(&AgedKeychain { age_days: 5 }, &d).map(|i| i.key.fingerprint())
            })
        })
        .collect();
    let fps: Vec<String> = handles
        .into_iter()
        .map(|h| h.join().unwrap().expect("init under lock"))
        .collect();
    // All saw the same (single) key.
    assert!(fps.iter().all(|f| *f == fps[0]));
}
