// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]

//! Integration tests for plugin signing and verification
//!
//! This test suite verifies the complete signing workflow:
//! 1. Generate keypair
//! 2. Sign plugin
//! 3. Verify signature
//! 4. Load signed plugin
//! 5. Reject tampered plugins
//! 6. Trust management

#![cfg(all(test, feature = "plugin-wasm", feature = "plugin-signing"))]

use conductor_core::plugin::{
    key_rotation::{fingerprint_of, rotation_payload},
    signing::{SignatureMetadata, sign_plugin, verify_plugin_signature},
    wasm_runtime::{WasmConfig, WasmPlugin},
};
use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use std::path::PathBuf;

/// Get path to a test plugin
fn get_test_plugin_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .unwrap()
        .join("plugins")
        .join("wasm-spotify")
        .join("target")
        .join("wasm32-wasip1")
        .join("release")
        .join("conductor_wasm_spotify.wasm")
}

/// Create a temporary directory for test files
fn create_temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("Failed to create temp dir")
}

#[tokio::test]
async fn test_sign_and_verify_workflow() {
    let plugin_path = get_test_plugin_path();

    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    // Generate signing key
    let signing_key = SigningKey::generate(&mut OsRng);
    let private_key = signing_key.to_bytes();
    let public_key = hex::encode(signing_key.verifying_key().to_bytes());

    // Create temp directory for test
    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");
    let test_sig_path = temp_dir.path().join("test_plugin.wasm.sig");

    // Copy plugin to temp directory
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Sign the plugin
    let result = sign_plugin(
        &test_plugin_path,
        &private_key,
        "Test Developer",
        "test@example.com",
    );
    assert!(result.is_ok(), "Failed to sign plugin: {:?}", result.err());

    // Verify signature file was created
    assert!(test_sig_path.exists(), "Signature file should be created");

    // Verify signature with the public key
    let verify_result = verify_plugin_signature(&test_plugin_path, &test_sig_path, &[public_key]);
    assert!(
        verify_result.is_ok(),
        "Signature verification should succeed"
    );
}

#[tokio::test]
async fn test_load_signed_plugin_with_self_signed_mode() {
    let plugin_path = get_test_plugin_path();

    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    // Generate signing key
    let signing_key = SigningKey::generate(&mut OsRng);
    let private_key = signing_key.to_bytes();

    // Create temp directory for test
    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");

    // Copy plugin to temp directory
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Sign the plugin
    sign_plugin(
        &test_plugin_path,
        &private_key,
        "Test Developer",
        "test@example.com",
    )
    .expect("Failed to sign plugin");

    // Load plugin with self-signed mode enabled
    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.allow_self_signed = true;

    let result = WasmPlugin::load(&test_plugin_path, config).await;
    assert!(
        result.is_ok(),
        "Should load self-signed plugin: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_reject_unsigned_plugin_when_required() {
    let plugin_path = get_test_plugin_path();

    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    // Create temp directory for test
    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");

    // Copy plugin to temp directory (without signing)
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Try to load with require_signature = true
    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.require_signature = true;

    let result = WasmPlugin::load(&test_plugin_path, config).await;
    assert!(
        result.is_err(),
        "Should reject unsigned plugin when signature required"
    );
    if let Err(e) = result {
        assert!(e.to_string().contains("signature required"));
    }
}

#[tokio::test]
async fn test_reject_tampered_plugin() {
    let plugin_path = get_test_plugin_path();

    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    // Generate signing key
    let signing_key = SigningKey::generate(&mut OsRng);
    let private_key = signing_key.to_bytes();
    let public_key = hex::encode(signing_key.verifying_key().to_bytes());

    // Create temp directory for test
    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");
    let test_sig_path = temp_dir.path().join("test_plugin.wasm.sig");

    // Copy plugin to temp directory
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Sign the plugin
    sign_plugin(
        &test_plugin_path,
        &private_key,
        "Test Developer",
        "test@example.com",
    )
    .expect("Failed to sign plugin");

    // Tamper with the plugin (append a byte)
    let mut plugin_bytes = std::fs::read(&test_plugin_path).unwrap();
    plugin_bytes.push(0xFF);
    std::fs::write(&test_plugin_path, plugin_bytes).unwrap();

    // Verify should fail
    let verify_result = verify_plugin_signature(&test_plugin_path, &test_sig_path, &[public_key]);
    assert!(verify_result.is_err(), "Should reject tampered plugin");
    assert!(
        verify_result
            .unwrap_err()
            .to_string()
            .contains("size mismatch")
    );
}

#[tokio::test]
async fn test_reject_invalid_signature() {
    let plugin_path = get_test_plugin_path();

    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    // Generate two different keys
    let signing_key1 = SigningKey::generate(&mut OsRng);
    let signing_key2 = SigningKey::generate(&mut OsRng);

    let private_key1 = signing_key1.to_bytes();
    let public_key2 = hex::encode(signing_key2.verifying_key().to_bytes());

    // Create temp directory for test
    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");
    let test_sig_path = temp_dir.path().join("test_plugin.wasm.sig");

    // Copy plugin to temp directory
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Sign with key1
    sign_plugin(
        &test_plugin_path,
        &private_key1,
        "Test Developer",
        "test@example.com",
    )
    .expect("Failed to sign plugin");

    // Try to verify with key2 (wrong key)
    let verify_result = verify_plugin_signature(&test_plugin_path, &test_sig_path, &[public_key2]);
    assert!(
        verify_result.is_err(),
        "Should reject signature from wrong key"
    );
    let err_msg = verify_result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Signature verification failed") || err_msg.contains("untrusted key"),
        "Expected signature verification error, got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_signature_metadata_format() {
    let plugin_path = get_test_plugin_path();

    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    // Generate signing key
    let signing_key = SigningKey::generate(&mut OsRng);
    let private_key = signing_key.to_bytes();

    // Create temp directory for test
    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");
    let test_sig_path = temp_dir.path().join("test_plugin.wasm.sig");

    // Copy plugin to temp directory
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Sign the plugin
    sign_plugin(
        &test_plugin_path,
        &private_key,
        "Test Developer",
        "test@example.com",
    )
    .expect("Failed to sign plugin");

    // Read and parse signature metadata
    let sig_json = std::fs::read_to_string(&test_sig_path).expect("Failed to read signature file");

    let metadata: SignatureMetadata =
        serde_json::from_str(&sig_json).expect("Failed to parse signature metadata");

    // Verify metadata fields
    assert_eq!(metadata.version, 1);
    assert_eq!(metadata.algorithm, "Ed25519");
    assert_eq!(metadata.developer.name, "Test Developer");
    assert_eq!(metadata.developer.email, "test@example.com");
    assert!(!metadata.plugin_hash.is_empty());
    assert!(!metadata.signature.is_empty());
    assert!(!metadata.signed_at.is_empty());
}

#[tokio::test]
async fn test_load_unsigned_plugin_when_not_required() {
    let plugin_path = get_test_plugin_path();

    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    // Create temp directory for test
    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");

    // Copy plugin to temp directory (without signing)
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Load plugin with default config (signatures not required)
    let config = WasmConfig::new("test-plugin").expect("safe id");
    assert!(
        !config.require_signature,
        "Default config should not require signatures"
    );

    let result = WasmPlugin::load(&test_plugin_path, config).await;
    assert!(
        result.is_ok(),
        "Should load unsigned plugin when not required: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_multiple_executions_with_signed_plugin() {
    let plugin_path = get_test_plugin_path();

    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    // Generate signing key
    let signing_key = SigningKey::generate(&mut OsRng);
    let private_key = signing_key.to_bytes();

    // Create temp directory for test
    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");

    // Copy plugin to temp directory
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Sign the plugin
    sign_plugin(
        &test_plugin_path,
        &private_key,
        "Test Developer",
        "test@example.com",
    )
    .expect("Failed to sign plugin");

    // Load plugin with self-signed mode
    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.allow_self_signed = true;

    let mut plugin = WasmPlugin::load(&test_plugin_path, config)
        .await
        .expect("Failed to load signed plugin");

    plugin.init().await.expect("Failed to initialize plugin");

    // Execute multiple times to ensure signature verification doesn't interfere
    let context = conductor_core::plugin::TriggerContext::default();
    for i in 0..3 {
        let result = plugin.execute("play", &[], &context).await;
        assert!(result.is_ok(), "Execution {} should succeed", i + 1);
    }
}

#[test]
fn test_key_size_validation() {
    let plugin_path = get_test_plugin_path();

    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    // Create temp directory for test
    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");

    // Copy plugin to temp directory
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Try to sign with wrong key size
    let wrong_size_key = vec![0u8; 64]; // Wrong size (should be 32)
    let result = sign_plugin(
        &test_plugin_path,
        &wrong_size_key,
        "Test Developer",
        "test@example.com",
    );

    assert!(result.is_err(), "Should reject wrong key size");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid private key size")
    );
}

#[tokio::test]
async fn test_signature_deterministic() {
    let plugin_path = get_test_plugin_path();

    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    // Generate signing key
    let signing_key = SigningKey::generate(&mut OsRng);
    let private_key = signing_key.to_bytes();

    // Create temp directory for test
    let temp_dir = create_temp_dir();
    let test_plugin_path1 = temp_dir.path().join("test_plugin1.wasm");
    let test_sig_path1 = temp_dir.path().join("test_plugin1.wasm.sig");
    let test_plugin_path2 = temp_dir.path().join("test_plugin2.wasm");
    let test_sig_path2 = temp_dir.path().join("test_plugin2.wasm.sig");

    // Copy same plugin twice
    std::fs::copy(&plugin_path, &test_plugin_path1).expect("Failed to copy plugin 1");
    std::fs::copy(&plugin_path, &test_plugin_path2).expect("Failed to copy plugin 2");

    // Sign both copies with same key
    sign_plugin(&test_plugin_path1, &private_key, "Dev", "dev@example.com")
        .expect("Failed to sign plugin 1");
    sign_plugin(&test_plugin_path2, &private_key, "Dev", "dev@example.com")
        .expect("Failed to sign plugin 2");

    // Read both signature files
    let sig1_json = std::fs::read_to_string(&test_sig_path1).unwrap();
    let sig2_json = std::fs::read_to_string(&test_sig_path2).unwrap();

    let metadata1: SignatureMetadata = serde_json::from_str(&sig1_json).unwrap();
    let metadata2: SignatureMetadata = serde_json::from_str(&sig2_json).unwrap();

    // Same plugin + same key should produce same hash and signature
    assert_eq!(metadata1.plugin_hash, metadata2.plugin_hash);
    assert_eq!(metadata1.signature, metadata2.signature);
    assert_eq!(metadata1.public_key, metadata2.public_key);
}

// ── ADR-027 D9: rotation-chain trust wired into the load path ───────────────

/// Build a 2-key (`root → head`) D9 rotation manifest JSON, with the head key
/// endorsed by a real Ed25519 signature from the root.
fn rotation_manifest_json(root_sk: &SigningKey, head_sk: &SigningKey) -> String {
    let root_pk = root_sk.verifying_key().to_bytes();
    let head_pk = head_sk.verifying_key().to_bytes();
    let chain_id = fingerprint_of(&root_pk);
    let root_valid_from = 1_000i64;
    let head_valid_from = 2_000i64;
    let payload = rotation_payload(&chain_id, 1, head_valid_from, &root_pk, &head_pk);
    let sig = root_sk.sign(&payload).to_bytes();
    let to_iso = |u: i64| chrono::DateTime::from_timestamp(u, 0).unwrap().to_rfc3339();
    format!(
        r#"{{ "signing_keys": [ {{ "seq": 0, "public_key": "{root}", "valid_from": "{rvf}" }}, {{ "seq": 1, "public_key": "{head}", "valid_from": "{hvf}", "rotation_signed_by": "{rfp}", "rotation_signature": "{sig}" }} ] }}"#,
        root = hex::encode(root_pk),
        rvf = to_iso(root_valid_from),
        head = hex::encode(head_pk),
        hvf = to_iso(head_valid_from),
        rfp = hex::encode(chain_id),
        sig = hex::encode(sig),
    )
}

/// Render a `trusted_keys.toml` containing the given hex public keys.
fn trusted_keys_toml(pubkeys_hex: &[String]) -> String {
    pubkeys_hex
        .iter()
        .map(|pk| {
            format!(
                "[[keys]]\nname = \"Author\"\nemail = \"a@example.com\"\n\
                 public_key = \"{}\"\nadded_at = \"2026-01-01T00:00:00Z\"\n",
                pk
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A plugin signed by a *rotated* head key loads when the user trusts only the
/// chain ROOT — trust flows transitively through the rotation manifest. This is
/// the core D9 promise: rotating a signing key does not force users to re-trust.
#[tokio::test]
async fn test_load_plugin_via_rotation_chain_transitive_trust() {
    let plugin_path = get_test_plugin_path();
    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    let root_sk = SigningKey::from_bytes(&[7u8; 32]);
    let head_sk = SigningKey::from_bytes(&[9u8; 32]);
    let root_pk = root_sk.verifying_key().to_bytes();

    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Sign the plugin with the HEAD key (the current, rotated key).
    sign_plugin(
        &test_plugin_path,
        &head_sk.to_bytes(),
        "Author",
        "a@example.com",
    )
    .expect("Failed to sign plugin");

    // Rotation manifest beside the plugin: <plugin>.keys.json
    let manifest_path = test_plugin_path.with_extension("keys.json");
    std::fs::write(&manifest_path, rotation_manifest_json(&root_sk, &head_sk))
        .expect("Failed to write manifest");

    // Trust ONLY the root — the head was never directly trusted.
    let trust_path = temp_dir.path().join("trusted_keys.toml");
    std::fs::write(&trust_path, trusted_keys_toml(&[hex::encode(root_pk)]))
        .expect("Failed to write trust store");

    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.require_signature = true;
    config.trusted_keys_path = Some(trust_path);
    // Hermetic: point the CRL at a non-existent temp path so the default
    // ~/.config location is never consulted (empty revocation set).
    config.revoked_keys_path = Some(temp_dir.path().join("revoked_keys.toml"));

    let result = WasmPlugin::load(&test_plugin_path, config).await;
    assert!(
        result.is_ok(),
        "rotated-key plugin should load via transitive trust: {:?}",
        result.err()
    );
}

/// When the user trusts neither the root nor the head, the rotation chain has no
/// trust anchor and the load HARD-FAILS — never silently falling back to the
/// bare trusted-key check.
#[tokio::test]
async fn test_load_plugin_via_rotation_chain_untrusted_root_rejected() {
    let plugin_path = get_test_plugin_path();
    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    let root_sk = SigningKey::from_bytes(&[7u8; 32]);
    let head_sk = SigningKey::from_bytes(&[9u8; 32]);
    let stranger_pk = SigningKey::from_bytes(&[42u8; 32])
        .verifying_key()
        .to_bytes();

    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    sign_plugin(
        &test_plugin_path,
        &head_sk.to_bytes(),
        "Author",
        "a@example.com",
    )
    .expect("Failed to sign plugin");

    let manifest_path = test_plugin_path.with_extension("keys.json");
    std::fs::write(&manifest_path, rotation_manifest_json(&root_sk, &head_sk))
        .expect("Failed to write manifest");

    // Trust a stranger key — NOT the chain root.
    let trust_path = temp_dir.path().join("trusted_keys.toml");
    std::fs::write(&trust_path, trusted_keys_toml(&[hex::encode(stranger_pk)]))
        .expect("Failed to write trust store");

    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.require_signature = true;
    config.trusted_keys_path = Some(trust_path);
    // Hermetic: point the CRL at a non-existent temp path so the default
    // ~/.config location is never consulted (empty revocation set).
    config.revoked_keys_path = Some(temp_dir.path().join("revoked_keys.toml"));

    let result = WasmPlugin::load(&test_plugin_path, config).await;
    assert!(
        result.is_err(),
        "plugin with an untrusted rotation root must be rejected"
    );
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("rotation chain rejected"),
        "expected a rotation-chain hard-fail, got: {msg}"
    );
}

/// SECURITY REGRESSION (Council R1): a plugin signed by a *rotated-away-from
/// predecessor* key must be rejected, even though that key is a valid,
/// trusted-rooted member of the chain. The loader binds to the key active at
/// the verifier's clock (the head, here), not to mere chain membership — so a
/// compromised predecessor key whose window has closed can no longer sign an
/// accepted artifact. (Before the fix, the load path trusted every historical
/// chain key and this plugin would have loaded.)
#[tokio::test]
async fn test_load_plugin_signed_by_rotated_predecessor_key_rejected() {
    let plugin_path = get_test_plugin_path();
    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    let root_sk = SigningKey::from_bytes(&[7u8; 32]);
    let head_sk = SigningKey::from_bytes(&[9u8; 32]);
    let root_pk = root_sk.verifying_key().to_bytes();

    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");

    // Sign the plugin with the ROOT key — the OLD, rotated-away-from key, whose
    // active window [1970-ish, head.valid_from) closed long before `now`.
    sign_plugin(
        &test_plugin_path,
        &root_sk.to_bytes(),
        "Author",
        "a@example.com",
    )
    .expect("Failed to sign plugin");

    // Same valid root → head chain; head's open-ended window contains `now`.
    let manifest_path = test_plugin_path.with_extension("keys.json");
    std::fs::write(&manifest_path, rotation_manifest_json(&root_sk, &head_sk))
        .expect("Failed to write manifest");

    // Trust the root directly — the chain is fully valid and trusted-rooted.
    let trust_path = temp_dir.path().join("trusted_keys.toml");
    std::fs::write(&trust_path, trusted_keys_toml(&[hex::encode(root_pk)]))
        .expect("Failed to write trust store");

    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.require_signature = true;
    config.trusted_keys_path = Some(trust_path);
    // Hermetic: point the CRL at a non-existent temp path so the default
    // ~/.config location is never consulted (empty revocation set).
    config.revoked_keys_path = Some(temp_dir.path().join("revoked_keys.toml"));

    let result = WasmPlugin::load(&test_plugin_path, config).await;
    assert!(
        result.is_err(),
        "a plugin signed by a rotated-away-from predecessor key must be rejected"
    );
    // Rejection comes from the active-key binding: the active key is the head,
    // so the root-signed signature fails the trusted-key check inside
    // verify_plugin_signature ("untrusted key"), not the chain validator.
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("untrusted key"),
        "expected an active-key binding rejection, got: {msg}"
    );
}

// ── ADR-027 D9: CRL (revocation list) wired into the load path ──────────────

/// Render a `revoked_keys.toml` listing the given hex fingerprints.
fn revoked_keys_toml(fingerprints_hex: &[String]) -> String {
    fingerprints_hex
        .iter()
        .map(|fp| format!("[[revoked]]\nfingerprint = \"{fp}\"\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A directly-trusted (non-rotating) signing key whose fingerprint is on the
/// CRL is refused at load — revocation applies to the BARE trusted-key path, so
/// it can't be bypassed by simply not shipping a rotation manifest.
#[tokio::test]
async fn test_load_plugin_rejected_when_signing_key_revoked() {
    let plugin_path = get_test_plugin_path();
    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    let key = SigningKey::from_bytes(&[11u8; 32]);
    let pk = key.verifying_key().to_bytes();

    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");
    sign_plugin(
        &test_plugin_path,
        &key.to_bytes(),
        "Author",
        "a@example.com",
    )
    .expect("Failed to sign plugin");

    // Trust the key directly (no rotation manifest → bare path).
    let trust_path = temp_dir.path().join("trusted_keys.toml");
    std::fs::write(&trust_path, trusted_keys_toml(&[hex::encode(pk)])).unwrap();

    // ...but revoke its fingerprint.
    let revoked_path = temp_dir.path().join("revoked_keys.toml");
    std::fs::write(
        &revoked_path,
        revoked_keys_toml(&[hex::encode(fingerprint_of(&pk))]),
    )
    .unwrap();

    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.require_signature = true;
    config.trusted_keys_path = Some(trust_path);
    config.revoked_keys_path = Some(revoked_path);

    let result = WasmPlugin::load(&test_plugin_path, config).await;
    assert!(result.is_err(), "a revoked trusted key must be refused");
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("revoked"),
        "expected a CRL rejection, got: {msg}"
    );
}

/// A revoked key anywhere in the rotation chain — here the ROOT, while the
/// plugin is signed by the (non-revoked) head — burns the whole chain. The
/// chain validator rejects it even though the active signer itself is not on
/// the CRL, because its trust anchor is compromised.
#[tokio::test]
async fn test_load_plugin_rejected_when_revoked_root_in_rotation_chain() {
    let plugin_path = get_test_plugin_path();
    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    let root_sk = SigningKey::from_bytes(&[7u8; 32]);
    let head_sk = SigningKey::from_bytes(&[9u8; 32]);
    let root_pk = root_sk.verifying_key().to_bytes();

    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");
    sign_plugin(
        &test_plugin_path,
        &head_sk.to_bytes(),
        "Author",
        "a@example.com",
    )
    .expect("Failed to sign plugin");

    let manifest_path = test_plugin_path.with_extension("keys.json");
    std::fs::write(&manifest_path, rotation_manifest_json(&root_sk, &head_sk)).unwrap();

    let trust_path = temp_dir.path().join("trusted_keys.toml");
    std::fs::write(&trust_path, trusted_keys_toml(&[hex::encode(root_pk)])).unwrap();

    // Revoke the ROOT (chain anchor), not the head signer.
    let revoked_path = temp_dir.path().join("revoked_keys.toml");
    std::fs::write(
        &revoked_path,
        revoked_keys_toml(&[hex::encode(fingerprint_of(&root_pk))]),
    )
    .unwrap();

    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.require_signature = true;
    config.trusted_keys_path = Some(trust_path);
    config.revoked_keys_path = Some(revoked_path);

    let result = WasmPlugin::load(&test_plugin_path, config).await;
    assert!(
        result.is_err(),
        "a chain with a revoked root must be rejected"
    );
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("Revoked") || msg.contains("chain rejected"),
        "expected a chain-validator revocation rejection, got: {msg}"
    );
}

/// Negative control: a CRL that lists an UNRELATED key does not block a valid,
/// trusted, non-revoked plugin.
#[tokio::test]
async fn test_load_plugin_loads_when_crl_lists_unrelated_key() {
    let plugin_path = get_test_plugin_path();
    if !plugin_path.exists() {
        eprintln!("Skipping test: Spotify plugin not built");
        return;
    }

    let key = SigningKey::from_bytes(&[11u8; 32]);
    let pk = key.verifying_key().to_bytes();
    let stranger_fp = fingerprint_of(
        &SigningKey::from_bytes(&[42u8; 32])
            .verifying_key()
            .to_bytes(),
    );

    let temp_dir = create_temp_dir();
    let test_plugin_path = temp_dir.path().join("test_plugin.wasm");
    std::fs::copy(&plugin_path, &test_plugin_path).expect("Failed to copy plugin");
    sign_plugin(
        &test_plugin_path,
        &key.to_bytes(),
        "Author",
        "a@example.com",
    )
    .expect("Failed to sign plugin");

    let trust_path = temp_dir.path().join("trusted_keys.toml");
    std::fs::write(&trust_path, trusted_keys_toml(&[hex::encode(pk)])).unwrap();

    // CRL lists a different key — must not affect this plugin.
    let revoked_path = temp_dir.path().join("revoked_keys.toml");
    std::fs::write(
        &revoked_path,
        revoked_keys_toml(&[hex::encode(stranger_fp)]),
    )
    .unwrap();

    let mut config = WasmConfig::new("test-plugin").expect("safe id");
    config.require_signature = true;
    config.trusted_keys_path = Some(trust_path);
    config.revoked_keys_path = Some(revoked_path);

    let result = WasmPlugin::load(&test_plugin_path, config).await;
    assert!(
        result.is_ok(),
        "an unrelated CRL entry must not block a valid plugin: {:?}",
        result.err()
    );
}
