// Copyright 2025 Amiable Team
// SPDX-License-Identifier: MIT

//! CLI tool for signing and verifying WASM plugins
//!
//! This tool provides commands for:
//! - Generating Ed25519 keypairs
//! - Signing plugins with private keys
//! - Verifying plugin signatures
//! - Managing trusted keys

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::PathBuf;
use std::process;

fn print_usage() {
    println!("conductor-sign - Plugin Signing Tool");
    println!();
    println!("USAGE:");
    println!("  conductor-sign generate-key <output-path>           Generate new Ed25519 keypair");
    println!("  conductor-sign sign <plugin> <key> [options]        Sign a plugin");
    println!("  conductor-sign verify <plugin>                      Verify plugin signature");
    println!(
        "  conductor-sign migrate-keys <plugin>                v2.7 .sig → root-only D9 manifest"
    );
    println!("  conductor-sign rotate-key <old> <new> <manifest>    Append a D9 key rotation");
    println!(
        "  conductor-sign sign-registry <reg.json> <key> <out>  Sign a registry.json (D10d-source)"
    );
    println!("  conductor-sign trust add <public-key> <name>        Add trusted key");
    println!("  conductor-sign trust list                           List trusted keys");
    println!("  conductor-sign trust remove <public-key>            Remove trusted key");
    println!("  conductor-sign trust verify <manifest.json>         Validate a D9 rotation chain");
    println!();
    println!("SIGN OPTIONS:");
    println!("  --name <name>        Developer name (required)");
    println!("  --email <email>      Developer email (required)");
    println!();
    println!("EXAMPLES:");
    println!("  # Generate keypair");
    println!("  conductor-sign generate-key ~/.conductor/my-key");
    println!();
    println!("  # Sign a plugin");
    println!("  conductor-sign sign plugin.wasm ~/.conductor/my-key \\");
    println!("    --name \"John Doe\" --email \"john@example.com\"");
    println!();
    println!("  # Verify a plugin");
    println!("  conductor-sign verify plugin.wasm");
    println!();
    println!("  # Add trusted key");
    println!("  conductor-sign trust add abcd1234... \"Official Conductor\"");
}

fn generate_keypair(output_path: &str) {
    #[cfg(not(feature = "plugin-signing"))]
    {
        eprintln!("Error: Plugin signing feature not enabled");
        eprintln!("Rebuild with: cargo build --package conductor-daemon --features plugin-signing");
        process::exit(1);
    }

    #[cfg(feature = "plugin-signing")]
    {
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        println!("Generating Ed25519 keypair...");

        let signing_key = SigningKey::generate(&mut OsRng);
        let private_key = signing_key.to_bytes();
        let public_key = signing_key.verifying_key().to_bytes();

        let private_path = format!("{}.private", output_path);
        let public_path = format!("{}.public", output_path);

        // Preflight (#1419): refuse if EITHER output already exists, before
        // creating anything. Otherwise a missing `.private` but existing
        // `.public` would let us create a fresh private key while silently
        // clobbering the existing public key — a trust-distribution
        // artifact. The `create_new` on both writes below is the atomic
        // safety net against a TOCTOU race after this check.
        for path in [&private_path, &public_path] {
            if std::path::Path::new(path).exists() {
                eprintln!(
                    "Error: {} already exists. Refusing to overwrite existing key \
                     material — delete it explicitly first if you really want to \
                     rotate the keypair.",
                    path
                );
                process::exit(1);
            }
        }

        // Write private key with restrictive permissions (0o600
        // owner-only) AND `create_new` so we never silently
        // overwrite an existing key.
        //
        // #1316: previously `std::fs::write` honoured the process
        // umask (typically 022 → 0644 file mode), leaving the
        // signing credential world-readable. The atomic
        // `OpenOptions::mode(0o600).create_new(true)` combination
        // closes both gaps: the file is created with the right
        // mode in one syscall, AND a second `generate-key` on the
        // same path errors out instead of clobbering.
        let write_private = || -> std::io::Result<()> {
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&private_path)?;
                file.write_all(&private_key)?;
                file.sync_all()?;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                // Windows has a different permission model; create_new
                // alone is the strongest cross-platform guarantee here.
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&private_path)?;
                file.write_all(&private_key)?;
                file.sync_all()?;
                Ok(())
            }
        };
        write_private().unwrap_or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                eprintln!(
                    "Error: {} already exists. Refusing to overwrite an existing \
                     private key — delete it explicitly first if you really want \
                     to rotate.",
                    private_path
                );
            } else {
                eprintln!("Error writing private key: {}", e);
            }
            process::exit(1);
        });

        // Write public key (hex-encoded) with `create_new` too (#1419), so
        // the no-overwrite guarantee covers the WHOLE keypair prefix, not
        // just the private half. Compute the encoding once so we can reuse
        // it for the file write AND the println.
        let public_key_hex = hex::encode(public_key);
        let write_public = || -> std::io::Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&public_path)?;
            file.write_all(public_key_hex.as_bytes())?;
            file.sync_all()?;
            Ok(())
        };
        write_public().unwrap_or_else(|e| {
            // The private key was already written above; a failed public
            // write (e.g. the `.public` was created in the TOCTOU window
            // after our preflight) would otherwise leave an orphaned
            // private key (#1419 partial state). Best-effort cleanup so we
            // never leave half a keypair behind.
            let _ = std::fs::remove_file(&private_path);
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                eprintln!(
                    "Error: {} already exists. Refusing to overwrite an existing \
                     public key; the just-created private key {} was removed.",
                    public_path, private_path
                );
            } else {
                eprintln!("Error writing public key: {}", e);
            }
            process::exit(1);
        });

        println!("✓ Keypair generated successfully!");
        println!();
        println!("Private key: {}", private_path);
        println!("Public key:  {}", public_path);
        println!();
        println!("Public key (hex): {}", public_key_hex);
        println!();
        println!("⚠️  Keep your private key secure and never share it!");
    }
}

fn sign_plugin(plugin_path: &str, key_path: &str, name: Option<&str>, email: Option<&str>) {
    #[cfg(not(feature = "plugin-signing"))]
    {
        let _ = (plugin_path, key_path, name, email);
        eprintln!("Error: Plugin signing feature not enabled");
        eprintln!("Rebuild with: cargo build --package conductor-daemon --features plugin-signing");
        process::exit(1);
    }

    #[cfg(feature = "plugin-signing")]
    {
        use conductor_core::plugin::signing::sign_plugin;

        let developer_name = name.unwrap_or_else(|| {
            eprintln!("Error: --name is required");
            process::exit(1);
        });

        let developer_email = email.unwrap_or_else(|| {
            eprintln!("Error: --email is required");
            process::exit(1);
        });

        println!("Signing plugin: {}", plugin_path);
        println!("Developer: {} <{}>", developer_name, developer_email);

        // Read private key
        let key_file = format!("{}.private", key_path);
        let private_key = std::fs::read(&key_file).unwrap_or_else(|e| {
            eprintln!("Error reading private key from {}: {}", key_file, e);
            eprintln!("Try: {}", key_path);

            // Try without .private extension
            std::fs::read(key_path).unwrap_or_else(|e| {
                eprintln!("Error reading private key from {}: {}", key_path, e);
                process::exit(1);
            })
        });

        if private_key.len() != 32 {
            eprintln!(
                "Error: Invalid private key size (expected 32 bytes, got {})",
                private_key.len()
            );
            process::exit(1);
        }

        // Sign plugin
        let plugin_pathbuf = PathBuf::from(plugin_path);
        sign_plugin(
            &plugin_pathbuf,
            &private_key,
            developer_name,
            developer_email,
        )
        .unwrap_or_else(|e| {
            eprintln!("Error signing plugin: {}", e);
            process::exit(1);
        });

        println!("✓ Plugin signed successfully!");
        println!("Signature file: {}.sig", plugin_path);
    }
}

fn verify_plugin(plugin_path: &str) {
    #[cfg(not(feature = "plugin-signing"))]
    {
        let _ = plugin_path;
        eprintln!("Error: Plugin signing feature not enabled");
        eprintln!("Rebuild with: cargo build --package conductor-daemon --features plugin-signing");
        process::exit(1);
    }

    #[cfg(feature = "plugin-signing")]
    {
        use conductor_core::plugin::signing::{load_trusted_keys, verify_plugin_signature};
        use std::path::Path;

        let plugin_pathbuf = PathBuf::from(plugin_path);
        let sig_path = plugin_pathbuf.with_extension("wasm.sig");

        if !sig_path.exists() {
            eprintln!("Error: No signature file found at {:?}", sig_path);
            process::exit(1);
        }

        println!("Verifying plugin: {}", plugin_path);
        println!("Signature file: {:?}", sig_path);

        // Load trusted keys. Fail-fast on a load error: silently continuing
        // with an empty trust list would evaluate EVERY signature as untrusted,
        // misreporting a genuinely-trusted plugin as untrusted (and hiding the
        // real fault — a broken trust store).
        let trusted_keys = load_trusted_keys().unwrap_or_else(|e| {
            eprintln!("Error: could not load trusted keys: {}", e);
            process::exit(1);
        });

        println!("Trusted keys: {}", trusted_keys.len());

        // Read signature metadata
        let sig_json = std::fs::read_to_string(&sig_path).unwrap_or_else(|e| {
            eprintln!("Error reading signature: {}", e);
            process::exit(1);
        });

        let sig_metadata: conductor_core::plugin::signing::SignatureMetadata =
            serde_json::from_str(&sig_json).unwrap_or_else(|e| {
                eprintln!("Error parsing signature: {}", e);
                process::exit(1);
            });

        // The string fields below come from an untrusted `.sig`; sanitize each
        // before printing so a crafted value can't inject terminal escape
        // sequences. (`version` is a numeric `u32` — no escape risk.)
        println!();
        println!("Signature Details:");
        println!("  Version:     {}", sig_metadata.version);
        println!(
            "  Algorithm:   {}",
            sanitize_for_terminal(&sig_metadata.algorithm)
        );
        println!(
            "  Signed at:   {}",
            sanitize_for_terminal(&sig_metadata.signed_at)
        );
        println!(
            "  Developer:   {} <{}>",
            sanitize_for_terminal(&sig_metadata.developer.name),
            sanitize_for_terminal(&sig_metadata.developer.email)
        );
        println!(
            "  Public key:  {}",
            sanitize_for_terminal(&sig_metadata.public_key)
        );
        println!();

        // Verify signature
        match verify_plugin_signature(Path::new(plugin_path), &sig_path, &trusted_keys) {
            Ok(()) => {
                println!("✓ Signature verified successfully!");
                println!("✓ Plugin signed by trusted key");
            }
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("untrusted key") {
                    println!("⚠️  Signature is valid but key is not trusted");
                    println!();
                    println!("To trust this key, run:");
                    println!(
                        "  conductor-sign trust add {} \"{}\"",
                        sanitize_for_terminal(&sig_metadata.public_key),
                        sanitize_for_terminal(&sig_metadata.developer.name)
                    );
                    // #1312: previously the function returned naturally
                    // here, giving an exit code of 0 — automation
                    // running `conductor-sign verify plugin.wasm` in CI
                    // would treat an untrusted signature as accepted.
                    // Exit non-zero so callers can distinguish
                    // trusted-and-valid from valid-but-untrusted.
                    // The trust-add guidance above already tells the
                    // operator how to lift the rejection.
                    process::exit(1);
                } else {
                    eprintln!("✗ Signature verification failed: {}", err_msg);
                    process::exit(1);
                }
            }
        }
    }
}

fn trust_add(public_key: &str, name: &str) {
    #[cfg(not(feature = "plugin-signing"))]
    {
        let _ = (public_key, name);
        eprintln!("Error: Plugin signing feature not enabled");
        eprintln!("Rebuild with: cargo build --package conductor-daemon --features plugin-signing");
        process::exit(1);
    }

    #[cfg(feature = "plugin-signing")]
    {
        use conductor_core::plugin::signing::add_trusted_key;

        println!("Adding trusted key:");
        println!("  Name: {}", name);
        println!("  Key:  {}", public_key);

        // Use empty email since it's just a trusted key, not a developer identity
        add_trusted_key(public_key, name, "").unwrap_or_else(|e| {
            eprintln!("Error adding trusted key: {}", e);
            process::exit(1);
        });

        println!("✓ Trusted key added successfully!");
    }
}

fn trust_list() {
    #[cfg(not(feature = "plugin-signing"))]
    {
        eprintln!("Error: Plugin signing feature not enabled");
        eprintln!("Rebuild with: cargo build --package conductor-daemon --features plugin-signing");
        process::exit(1);
    }

    #[cfg(feature = "plugin-signing")]
    {
        use conductor_core::plugin::signing::load_trusted_keys;

        let trusted_keys = load_trusted_keys().unwrap_or_else(|e| {
            eprintln!("Warning: Could not load trusted keys: {}", e);
            Vec::new()
        });

        if trusted_keys.is_empty() {
            println!("No trusted keys configured");
            return;
        }

        println!("Trusted keys ({}):", trusted_keys.len());
        for key in trusted_keys {
            println!("  {}", key);
        }
    }
}

fn trust_remove(public_key: &str) {
    #[cfg(not(feature = "plugin-signing"))]
    {
        let _ = public_key;
        eprintln!("Error: Plugin signing feature not enabled");
        eprintln!("Rebuild with: cargo build --package conductor-daemon --features plugin-signing");
        process::exit(1);
    }

    #[cfg(feature = "plugin-signing")]
    {
        // #1315: use the full-record load/save helpers so retained
        // keys' name/email/added_at survive. The string-only
        // `load_trusted_keys` + `save_trusted_keys` round-trip
        // reconstructed every record with empty name/email and a
        // fresh `added_at`, silently wiping metadata for every
        // retained entry as a side-effect of removing ONE key.
        use conductor_core::plugin::signing::{load_trusted_keys_full, save_trusted_keys_full};

        let mut trusted_keys = load_trusted_keys_full().unwrap_or_else(|e| {
            eprintln!("Error loading trusted keys: {}", e);
            process::exit(1);
        });

        let before_count = trusted_keys.len();
        trusted_keys.retain(|k| k.public_key != public_key);
        let after_count = trusted_keys.len();

        if before_count == after_count {
            eprintln!("Error: Key not found in trusted list");
            process::exit(1);
        }

        save_trusted_keys_full(&trusted_keys).unwrap_or_else(|e| {
            eprintln!("Error saving trusted keys: {}", e);
            process::exit(1);
        });

        println!("✓ Trusted key removed successfully!");
    }
}

/// Validate a plugin's ADR-027 D9 signing-key rotation manifest against a set of
/// directly-trusted public keys (the trust store's hex keys). Pure and I/O-free
/// so it is unit-testable; [`trust_verify`] supplies the file + store.
#[cfg(feature = "plugin-signing")]
fn verify_rotation_manifest(
    manifest_json: &str,
    trusted_pubkeys_hex: &[String],
) -> Result<conductor_core::plugin::key_rotation::VerifiedChain, String> {
    use conductor_core::plugin::key_rotation::{
        PluginKeyManifestJson, fingerprint_of, validate_chain,
    };
    use std::collections::HashSet;

    let manifest = PluginKeyManifestJson::parse(manifest_json)
        .map_err(|e| e.to_string())?
        .into_manifest()
        .map_err(|e| e.to_string())?;

    // The engine trusts by *fingerprint*; the trust store holds hex public keys.
    let mut trusted: HashSet<[u8; 32]> = HashSet::new();
    for hex_pk in trusted_pubkeys_hex {
        let bytes = hex::decode(hex_pk.trim())
            .map_err(|_| format!("trusted key is not valid hex: {hex_pk}"))?;
        let pk: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| format!("trusted key is not 32 bytes: {hex_pk}"))?;
        trusted.insert(fingerprint_of(&pk));
    }

    validate_chain(&manifest, &trusted).map_err(|e| e.to_string())
}

/// `conductor-sign trust verify <manifest.json>` — validate a plugin's
/// signing-key rotation chain (ADR-027 D9) against the local trust store, so an
/// operator can confirm transitive trust before installing a rotated plugin.
fn trust_verify(manifest_path: &str) {
    #[cfg(not(feature = "plugin-signing"))]
    {
        let _ = manifest_path;
        eprintln!("Error: Plugin signing feature not enabled");
        eprintln!("Rebuild with: cargo build --package conductor-daemon --features plugin-signing");
        process::exit(1);
    }

    #[cfg(feature = "plugin-signing")]
    {
        use conductor_core::plugin::signing::load_trusted_keys;

        let manifest_json = std::fs::read_to_string(manifest_path).unwrap_or_else(|e| {
            eprintln!("Error: could not read manifest {manifest_path}: {e}");
            process::exit(1);
        });
        let trusted = load_trusted_keys().unwrap_or_else(|e| {
            eprintln!("Error: could not load trusted keys: {e}");
            process::exit(1);
        });

        match verify_rotation_manifest(&manifest_json, &trusted) {
            Ok(chain) => {
                let head = chain.keys.last().expect("a validated chain is non-empty");
                println!("✓ Valid rotation chain");
                println!("  Keys:       {}", chain.keys.len());
                println!("  Root (fp):  {}", hex::encode(chain.chain_id));
                println!(
                    "  Head (fp):  {}  (seq {})",
                    hex::encode(head.fingerprint),
                    head.seq
                );
                println!("  Transitive trust verified from a directly-trusted root.");
            }
            Err(reason) => {
                eprintln!("✗ Rejected: {reason}");
                process::exit(1);
            }
        }
    }
}

/// Render a root-only (non-rotating) D9 manifest — the migration shape for a
/// legacy v2.7 single-key plugin. A root entry carries no rotation signature, so
/// this is a complete, valid 1-key chain on its own.
/// Strip control / escape characters before printing an untrusted string (e.g.
/// a plugin author's name or timestamp read from a `.sig`) to the terminal, so
/// it cannot inject ANSI escape sequences and spoof terminal output.
#[cfg(feature = "plugin-signing")]
fn sanitize_for_terminal(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(feature = "plugin-signing")]
fn root_manifest_json(public_key_hex: &str, valid_from_iso: &str) -> String {
    // Build with serde_json so the externally-sourced fields (read from a
    // plugin's `.sig`) are properly escaped — NEVER string-interpolate
    // untrusted input into a JSON document (JSON-injection).
    let value = serde_json::json!({
        "signing_keys": [
            { "seq": 0, "public_key": public_key_hex, "valid_from": valid_from_iso }
        ]
    });
    // Serializing a `serde_json::Value` built from a literal cannot fail.
    serde_json::to_string_pretty(&value).expect("serializing a json! value is infallible") + "\n"
}

/// `conductor-sign migrate-keys <plugin>` — produce a D9 root-only rotation
/// manifest (`<plugin>.keys.json`) from a legacy v2.7 `<plugin>.wasm.sig`, so an
/// existing single-key plugin is forward-compatible with chain validation. The
/// signer's `public_key` becomes the chain root and its `signed_at` the root's
/// `valid_from`.
fn migrate_keys(plugin_path: &str) {
    #[cfg(not(feature = "plugin-signing"))]
    {
        let _ = plugin_path;
        eprintln!("Error: Plugin signing feature not enabled");
        eprintln!("Rebuild with: cargo build --package conductor-daemon --features plugin-signing");
        process::exit(1);
    }

    #[cfg(feature = "plugin-signing")]
    {
        let sig_path = PathBuf::from(plugin_path).with_extension("wasm.sig");
        let manifest_path = PathBuf::from(plugin_path).with_extension("keys.json");

        let sig_json = std::fs::read_to_string(&sig_path).unwrap_or_else(|e| {
            eprintln!("Error: could not read signature {sig_path:?}: {e}");
            process::exit(1);
        });
        let sig: conductor_core::plugin::signing::SignatureMetadata =
            serde_json::from_str(&sig_json).unwrap_or_else(|e| {
                eprintln!("Error: could not parse signature {sig_path:?}: {e}");
                process::exit(1);
            });

        let json = root_manifest_json(&sig.public_key, &sig.signed_at);

        // Refuse to overwrite an existing manifest (a trust-distribution
        // artifact); `create_new` is the atomic guard.
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest_path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                eprintln!(
                    "Error: {manifest_path:?} already exists; refusing to overwrite. \
                     Delete it explicitly first if you really want to regenerate."
                );
                process::exit(1);
            }
            Err(e) => {
                eprintln!("Error: could not create {manifest_path:?}: {e}");
                process::exit(1);
            }
        };
        use std::io::Write;
        file.write_all(json.as_bytes()).unwrap_or_else(|e| {
            eprintln!("Error writing manifest {manifest_path:?}: {e}");
            process::exit(1);
        });

        // `sig.public_key` is from the untrusted `.sig`; sanitize before
        // printing (the value written to the manifest is serde-escaped above).
        let public_key_display = sanitize_for_terminal(&sig.public_key);
        println!("✓ Migrated to a root-only rotation manifest");
        println!("  Wrote:           {manifest_path:?}");
        println!("  Root public key: {public_key_display}");
        println!("  Trust it with:   conductor-sign trust add {public_key_display} \"<author>\"");
        println!("  Verify with:     conductor-sign trust verify {manifest_path:?}");
    }
}

/// Serialize an engine [`PluginKeyManifest`] to the JSON transport shape (hex +
/// RFC3339), built with `serde_json` so all fields are escaped.
///
/// [`PluginKeyManifest`]: conductor_core::plugin::key_rotation::PluginKeyManifest
#[cfg(feature = "plugin-signing")]
fn manifest_to_json(manifest: &conductor_core::plugin::key_rotation::PluginKeyManifest) -> String {
    let entries: Vec<serde_json::Value> = manifest
        .keys
        .iter()
        .map(|e| {
            let iso = chrono::DateTime::from_timestamp(e.valid_from_unix, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| e.valid_from_unix.to_string());
            let mut obj = serde_json::json!({
                "seq": e.seq,
                "public_key": hex::encode(e.public_key),
                "valid_from": iso,
            });
            if let Some(fp) = e.rotation_signed_by {
                obj["rotation_signed_by"] = serde_json::Value::String(hex::encode(fp));
            }
            if let Some(sig) = e.rotation_signature {
                obj["rotation_signature"] = serde_json::Value::String(hex::encode(sig));
            }
            obj
        })
        .collect();
    let value = serde_json::json!({ "signing_keys": entries });
    serde_json::to_string_pretty(&value).expect("serializing a json! value is infallible") + "\n"
}

/// Append a rotation to a plugin's key chain (ADR-027 D9), endorsing `new_public`
/// with `old_private` (which must be the chain's current head). With no existing
/// manifest, bootstraps the chain with the old key as the root. Returns the
/// updated manifest JSON. Pure and I/O-free so it is unit-testable.
#[cfg(feature = "plugin-signing")]
fn build_rotation(
    existing_manifest: Option<&str>,
    old_private: &[u8; 32],
    old_public: &[u8; 32],
    new_public: &[u8; 32],
    valid_from_unix: i64,
) -> Result<String, String> {
    use conductor_core::plugin::key_rotation::{
        PluginKeyManifest, PluginKeyManifestJson, SigningKeyEntry, fingerprint_of, rotation_payload,
    };
    use ed25519_dalek::{Signer, SigningKey};

    let mut entries: Vec<SigningKeyEntry> = match existing_manifest {
        Some(json) => {
            PluginKeyManifestJson::parse(json)
                .map_err(|e| e.to_string())?
                .into_manifest()
                .map_err(|e| e.to_string())?
                .keys
        }
        // Bootstrap: the old key becomes the root, active just before the first
        // rotation so the root window is non-empty and strictly precedes it.
        None => vec![SigningKeyEntry {
            seq: 0,
            public_key: *old_public,
            valid_from_unix: valid_from_unix - 1,
            rotation_signed_by: None,
            rotation_signature: None,
        }],
    };
    entries.sort_by_key(|e| e.seq);
    let root = entries.first().ok_or("manifest has no entries")?;
    let chain_id = fingerprint_of(&root.public_key);
    let head = entries.last().ok_or("manifest has no entries")?;

    // The old key MUST be the current chain head — that's who endorses the new
    // key. Rotating from a stale interior key would produce a broken chain.
    if fingerprint_of(old_public) != fingerprint_of(&head.public_key) {
        return Err("the old key is not the current chain head; rotate from the head key".into());
    }
    // The engine forbids key reuse; reject it here with a clearer message.
    let new_fp = fingerprint_of(new_public);
    if entries
        .iter()
        .any(|e| fingerprint_of(&e.public_key) == new_fp)
    {
        return Err("the new key already appears in the chain (reuse is forbidden)".into());
    }

    let new_seq = head.seq + 1;
    // Strictly-increasing valid_from (clamp up past the head if the supplied
    // timestamp isn't already later — e.g. a fast rotation or clock skew).
    let new_valid_from = valid_from_unix.max(head.valid_from_unix + 1);

    let payload = rotation_payload(&chain_id, new_seq, new_valid_from, old_public, new_public);
    let signature = SigningKey::from_bytes(old_private)
        .sign(&payload)
        .to_bytes();

    entries.push(SigningKeyEntry {
        seq: new_seq,
        public_key: *new_public,
        valid_from_unix: new_valid_from,
        rotation_signed_by: Some(fingerprint_of(old_public)),
        rotation_signature: Some(signature),
    });

    Ok(manifest_to_json(&PluginKeyManifest { keys: entries }))
}

#[cfg(feature = "plugin-signing")]
fn read_private_key(key_path: &str) -> Result<[u8; 32], String> {
    let p = format!("{key_path}.private");
    let bytes = std::fs::read(&p).map_err(|e| format!("could not read private key {p}: {e}"))?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("private key {p} is not 32 bytes"))
}

#[cfg(feature = "plugin-signing")]
fn read_public_key(key_path: &str) -> Result<[u8; 32], String> {
    let p = format!("{key_path}.public");
    let hexstr =
        std::fs::read_to_string(&p).map_err(|e| format!("could not read public key {p}: {e}"))?;
    hex::decode(hexstr.trim())
        .map_err(|_| format!("public key {p} is not valid hex"))?
        .as_slice()
        .try_into()
        .map_err(|_| format!("public key {p} is not 32 bytes"))
}

/// `conductor-sign rotate-key <old-key> <new-key> <manifest.json>` — endorse a
/// new signing key with the current head key (ADR-027 D9), appending it to (or
/// bootstrapping) the plugin's rotation manifest.
fn rotate_key(old_key: &str, new_key: &str, manifest_path: &str) {
    #[cfg(not(feature = "plugin-signing"))]
    {
        let _ = (old_key, new_key, manifest_path);
        eprintln!("Error: Plugin signing feature not enabled");
        eprintln!("Rebuild with: cargo build --package conductor-daemon --features plugin-signing");
        process::exit(1);
    }

    #[cfg(feature = "plugin-signing")]
    {
        let on_err = |e: String| -> ! {
            eprintln!("Error: {e}");
            process::exit(1);
        };
        let old_private = read_private_key(old_key).unwrap_or_else(|e| on_err(e));
        let old_public = read_public_key(old_key).unwrap_or_else(|e| on_err(e));
        let new_public = read_public_key(new_key).unwrap_or_else(|e| on_err(e));
        let existing = std::fs::read_to_string(manifest_path).ok();
        let bootstrapping = existing.is_none();
        let now = chrono::Utc::now().timestamp();

        let json = build_rotation(
            existing.as_deref(),
            &old_private,
            &old_public,
            &new_public,
            now,
        )
        .unwrap_or_else(|e| on_err(e));

        std::fs::write(manifest_path, &json)
            .unwrap_or_else(|e| on_err(format!("could not write manifest {manifest_path}: {e}")));

        if bootstrapping {
            println!("✓ Bootstrapped a rotation chain and added the new key");
        } else {
            println!("✓ Rotated: appended the new key to the chain");
        }
        println!("  Manifest:    {manifest_path}");
        println!(
            "  New key fp:  {}",
            hex::encode(conductor_core::plugin::key_rotation::fingerprint_of(
                &new_public
            ))
        );
        println!("  Verify with: conductor-sign trust verify {manifest_path}");
    }
}

/// Build a signed-registry envelope JSON (ADR-027 D10d-source) for `payload`,
/// signed by `private_key` under `key_id`. Pure (no I/O) so it round-trips
/// against the client verifier (`registry_trust::verify_signed_registry`) in
/// tests. The `payload` is signed VERBATIM — never re-serialised — so the bytes
/// the client hashes are exactly these (no canonicalisation malleability).
#[cfg(feature = "plugin-signing")]
fn build_signed_registry(
    payload: &str,
    private_key: &[u8; 32],
    key_id: &str,
) -> Result<String, String> {
    use conductor_core::plugin::registry_trust::{SignedRegistryEnvelope, registry_signed_message};
    use ed25519_dalek::{Signer, SigningKey};

    let message = registry_signed_message(key_id, payload);
    let signature = SigningKey::from_bytes(private_key)
        .sign(&message)
        .to_bytes();
    let envelope = SignedRegistryEnvelope {
        payload: payload.to_string(),
        signature: hex::encode(signature),
        key_id: key_id.to_string(),
        // `sign-registry` produces a single-key (non-rotated) envelope; rotation
        // manifests are attached by the (follow-up) rotate-registry-key flow.
        key_manifest: None,
    };
    serde_json::to_string_pretty(&envelope)
        .map_err(|e| format!("could not serialise signed-registry envelope: {e}"))
}

/// Advisory check: the client REQUIRES `sequence_number` (strictly increasing)
/// and `published_at` (RFC 3339) inside the payload; warn the signer if absent
/// so a document that the daemon would reject isn't published unnoticed.
#[cfg(feature = "plugin-signing")]
fn warn_if_missing_rollback_fields(payload: &str) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
        if v.get("sequence_number").is_none() {
            eprintln!(
                "Warning: payload has no `sequence_number` — the client requires a \
                 strictly-increasing sequence_number for rollback protection"
            );
        }
        if v.get("published_at").is_none() {
            eprintln!(
                "Warning: payload has no `published_at` — the client REQUIRES \
                 published_at (RFC 3339); this document would be rejected on fetch"
            );
        }
    }
}

/// `conductor-sign sign-registry <registry.json> <key> <output.json> [--key-id <id>]`
/// — produce a signed-registry envelope (ADR-027 D10d-source) that the daemon
/// validates against the pinned registry key on fetch.
fn sign_registry(registry_path: &str, key_path: &str, output_path: &str, key_id: &str) {
    #[cfg(not(feature = "plugin-signing"))]
    {
        let _ = (registry_path, key_path, output_path, key_id);
        eprintln!("Error: Plugin signing feature not enabled");
        eprintln!("Rebuild with: cargo build --package conductor-daemon --features plugin-signing");
        process::exit(1);
    }

    #[cfg(feature = "plugin-signing")]
    {
        let on_err = |e: String| -> ! {
            eprintln!("Error: {e}");
            process::exit(1);
        };
        let payload = std::fs::read_to_string(registry_path)
            .unwrap_or_else(|e| on_err(format!("could not read registry {registry_path}: {e}")));
        let private = read_private_key(key_path).unwrap_or_else(|e| on_err(e));
        warn_if_missing_rollback_fields(&payload);
        let envelope =
            build_signed_registry(&payload, &private, key_id).unwrap_or_else(|e| on_err(e));
        std::fs::write(output_path, &envelope)
            .unwrap_or_else(|e| on_err(format!("could not write {output_path}: {e}")));

        println!("✓ Signed registry document (ADR-027 D10d-source)");
        println!("  Input:   {registry_path}");
        println!("  Output:  {output_path}");
        println!("  key_id:  {key_id}");
        println!("  The daemon verifies this against the pinned registry key on fetch.");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    match args[1].as_str() {
        "generate-key" => {
            if args.len() < 3 {
                eprintln!("Error: Missing output path");
                print_usage();
                process::exit(1);
            }
            generate_keypair(&args[2]);
        }

        "sign" => {
            if args.len() < 4 {
                eprintln!("Error: Missing arguments");
                print_usage();
                process::exit(1);
            }

            let plugin_path = &args[2];
            let key_path = &args[3];

            // Parse optional arguments
            let mut name = None;
            let mut email = None;

            let mut i = 4;
            while i < args.len() {
                match args[i].as_str() {
                    "--name" => {
                        if i + 1 < args.len() {
                            name = Some(args[i + 1].as_str());
                            i += 2;
                        } else {
                            eprintln!("Error: --name requires a value");
                            process::exit(1);
                        }
                    }
                    "--email" => {
                        if i + 1 < args.len() {
                            email = Some(args[i + 1].as_str());
                            i += 2;
                        } else {
                            eprintln!("Error: --email requires a value");
                            process::exit(1);
                        }
                    }
                    _ => {
                        eprintln!("Error: Unknown option: {}", args[i]);
                        process::exit(1);
                    }
                }
            }

            sign_plugin(plugin_path, key_path, name, email);
        }

        "verify" => {
            if args.len() < 3 {
                eprintln!("Error: Missing plugin path");
                print_usage();
                process::exit(1);
            }
            verify_plugin(&args[2]);
        }

        "migrate-keys" => {
            if args.len() < 3 {
                eprintln!("Error: Missing plugin path");
                print_usage();
                process::exit(1);
            }
            migrate_keys(&args[2]);
        }

        "rotate-key" => {
            if args.len() < 5 {
                eprintln!("Error: rotate-key requires <old-key> <new-key> <manifest.json>");
                print_usage();
                process::exit(1);
            }
            rotate_key(&args[2], &args[3], &args[4]);
        }

        "sign-registry" => {
            if args.len() < 5 {
                eprintln!("Error: sign-registry requires <registry.json> <key> <output.json>");
                print_usage();
                process::exit(1);
            }
            // Default key_id matches conductor-core's REGISTRY_PINNED_KEY_ID
            // (referenced as a literal here since `main` is not feature-gated).
            let mut key_id = "conductor-registry-v1".to_string();
            let mut i = 5;
            while i < args.len() {
                if args[i] == "--key-id" {
                    match args.get(i + 1) {
                        Some(v) => {
                            key_id = v.clone();
                            i += 2;
                            continue;
                        }
                        None => {
                            eprintln!("Error: --key-id requires a value");
                            process::exit(1);
                        }
                    }
                }
                i += 1;
            }
            sign_registry(&args[2], &args[3], &args[4], &key_id);
        }

        "trust" => {
            if args.len() < 3 {
                eprintln!("Error: Missing trust command");
                print_usage();
                process::exit(1);
            }

            match args[2].as_str() {
                "add" => {
                    if args.len() < 5 {
                        eprintln!("Error: trust add requires <public-key> <name>");
                        process::exit(1);
                    }
                    trust_add(&args[3], &args[4]);
                }
                "list" => {
                    trust_list();
                }
                "remove" => {
                    if args.len() < 4 {
                        eprintln!("Error: trust remove requires <public-key>");
                        process::exit(1);
                    }
                    trust_remove(&args[3]);
                }
                "verify" => {
                    if args.len() < 4 {
                        eprintln!("Error: trust verify requires <manifest.json>");
                        process::exit(1);
                    }
                    trust_verify(&args[3]);
                }
                _ => {
                    eprintln!("Error: Unknown trust command: {}", args[2]);
                    print_usage();
                    process::exit(1);
                }
            }
        }

        "--help" | "-h" | "help" => {
            print_usage();
        }

        _ => {
            eprintln!("Error: Unknown command: {}", args[1]);
            print_usage();
            process::exit(1);
        }
    }
}

#[cfg(all(test, feature = "plugin-signing"))]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    /// Hex of a *valid* Ed25519 public key derived from a fixed seed.
    fn valid_pk_hex(seed: u8) -> String {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        hex::encode(sk.verifying_key().to_bytes())
    }

    // ───── sign-registry (ADR-027 D10d-source) producer↔consumer round-trip ─────

    const REG_PAYLOAD: &str = r#"{"version":"1","sequence_number":1,"published_at":"2026-05-27T00:00:00Z","plugins":[],"categories":[]}"#;

    #[test]
    fn sign_registry_round_trips_against_client_verifier() {
        use conductor_core::plugin::registry_trust::{
            REGISTRY_PINNED_KEY_ID, RegistryTrustState, verify_signed_registry,
        };
        let sk = SigningKey::from_bytes(&[3u8; 32]);
        let envelope =
            build_signed_registry(REG_PAYLOAD, &sk.to_bytes(), REGISTRY_PINNED_KEY_ID).unwrap();
        // The client verifier accepts what the CLI produced.
        let v = verify_signed_registry(
            &envelope,
            &sk.verifying_key(),
            REGISTRY_PINNED_KEY_ID,
            &RegistryTrustState::default(),
        )
        .expect("signed registry must verify against the matching key");
        assert_eq!(v.payload, REG_PAYLOAD); // payload preserved verbatim
        assert_eq!(v.new_state.last_sequence_number, 1);
    }

    #[test]
    fn sign_registry_rejected_by_wrong_key() {
        use conductor_core::plugin::registry_trust::{
            REGISTRY_PINNED_KEY_ID, RegistryTrustError, RegistryTrustState, verify_signed_registry,
        };
        let signer = SigningKey::from_bytes(&[3u8; 32]);
        let other = SigningKey::from_bytes(&[4u8; 32]);
        let envelope =
            build_signed_registry(REG_PAYLOAD, &signer.to_bytes(), REGISTRY_PINNED_KEY_ID).unwrap();
        let err = verify_signed_registry(
            &envelope,
            &other.verifying_key(),
            REGISTRY_PINNED_KEY_ID,
            &RegistryTrustState::default(),
        )
        .unwrap_err();
        assert_eq!(err, RegistryTrustError::SignatureInvalid);
    }

    #[test]
    fn sign_registry_key_id_is_bound() {
        // Signing under one key_id but presenting another must not verify
        // (key_id is bound into the signed message).
        use conductor_core::plugin::registry_trust::{
            RegistryTrustError, RegistryTrustState, verify_signed_registry,
        };
        let sk = SigningKey::from_bytes(&[3u8; 32]);
        let envelope = build_signed_registry(REG_PAYLOAD, &sk.to_bytes(), "conductor-registry-v1")
            .unwrap()
            // tamper the advertised key_id only
            .replace("conductor-registry-v1", "conductor-registry-v2");
        let err = verify_signed_registry(
            &envelope,
            &sk.verifying_key(),
            "conductor-registry-v2",
            &RegistryTrustState::default(),
        )
        .unwrap_err();
        assert_eq!(err, RegistryTrustError::SignatureInvalid);
    }

    /// A root-only (non-rotating) manifest — the v2.7 single-key shape, a valid
    /// 1-key chain needing no rotation signature.
    fn root_only_manifest(pk_hex: &str) -> String {
        format!(
            r#"{{ "signing_keys": [ {{ "seq": 0, "public_key": "{pk_hex}", "valid_from": "2026-01-01T00:00:00Z" }} ] }}"#
        )
    }

    #[test]
    fn root_only_manifest_with_trusted_root_validates() {
        let pk = valid_pk_hex(1);
        let chain = verify_rotation_manifest(&root_only_manifest(&pk), &[pk])
            .expect("a trusted root validates");
        assert_eq!(chain.keys.len(), 1);
    }

    #[test]
    fn untrusted_root_is_rejected() {
        let pk = valid_pk_hex(2);
        assert!(verify_rotation_manifest(&root_only_manifest(&pk), &[]).is_err());
    }

    #[test]
    fn malformed_manifest_json_is_rejected() {
        assert!(verify_rotation_manifest("not json", &[valid_pk_hex(3)]).is_err());
    }

    #[test]
    fn non_hex_trusted_key_is_rejected() {
        let pk = valid_pk_hex(4);
        assert!(verify_rotation_manifest(&root_only_manifest(&pk), &["zz".to_string()]).is_err());
    }

    #[test]
    fn migration_manifest_round_trips_and_validates() {
        // A migrated v2.7 key becomes a root-only chain; the emitted JSON must
        // parse and validate as a 1-key chain when the root is trusted.
        let pk = valid_pk_hex(5);
        let json = root_manifest_json(&pk, "2026-01-01T00:00:00Z");
        let chain = verify_rotation_manifest(&json, &[pk])
            .expect("a migrated root-only manifest validates");
        assert_eq!(chain.keys.len(), 1);
        assert_eq!(chain.keys[0].seq, 0);
    }

    #[test]
    fn migration_manifest_untrusted_root_is_rejected() {
        let pk = valid_pk_hex(6);
        let json = root_manifest_json(&pk, "2026-01-01T00:00:00Z");
        assert!(verify_rotation_manifest(&json, &[]).is_err());
    }

    #[test]
    fn migration_manifest_escapes_untrusted_fields() {
        // Council R-high: a crafted `signed_at` (from an attacker's .sig) with
        // embedded JSON metacharacters must be ESCAPED into the string value,
        // never injected as document structure.
        let pk = valid_pk_hex(7);
        let evil = r#"2026-01-01T00:00:00Z", "injected": "x"#;
        let json = root_manifest_json(&pk, evil);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(
            parsed.get("injected").is_none(),
            "must not inject a top-level field"
        );
        let keys = parsed["signing_keys"].as_array().expect("array");
        assert_eq!(keys.len(), 1);
        // The malicious string is preserved verbatim inside valid_from.
        assert_eq!(keys[0]["valid_from"], evil);
    }

    /// (private, public) for a fixed seed — the seed bytes ARE the Ed25519
    /// private key, so `[seed; 32]` is the private and its verifying key the
    /// public.
    fn key_pair(seed: u8) -> ([u8; 32], [u8; 32]) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        ([seed; 32], sk.verifying_key().to_bytes())
    }

    #[test]
    fn rotation_bootstrap_then_validates_end_to_end() {
        let (old_priv, old_pub) = key_pair(1);
        let (_n_priv, new_pub) = key_pair(2);
        let json = build_rotation(None, &old_priv, &old_pub, &new_pub, 1_000)
            .expect("bootstrap rotation builds");
        // Trust the ROOT (old) key; the chain must validate with 2 keys.
        let chain = verify_rotation_manifest(&json, &[hex::encode(old_pub)])
            .expect("bootstrapped chain validates");
        assert_eq!(chain.keys.len(), 2);
        assert_eq!(chain.keys[1].public_key, new_pub);
    }

    #[test]
    fn rotation_append_extends_existing_chain() {
        let (old_priv, old_pub) = key_pair(1);
        let (k2_priv, k2_pub) = key_pair(2);
        let (_k3_priv, k3_pub) = key_pair(3);
        // Root(1) -> key2.
        let m1 = build_rotation(None, &old_priv, &old_pub, &k2_pub, 1_000).unwrap();
        // key2 is now the head; rotate to key3.
        let m2 = build_rotation(Some(&m1), &k2_priv, &k2_pub, &k3_pub, 2_000).unwrap();
        let chain =
            verify_rotation_manifest(&m2, &[hex::encode(old_pub)]).expect("3-key chain validates");
        assert_eq!(chain.keys.len(), 3);
    }

    #[test]
    fn rotation_from_non_head_key_is_rejected() {
        let (old_priv, old_pub) = key_pair(1);
        let (_k2_priv, k2_pub) = key_pair(2);
        let (_k3_priv, k3_pub) = key_pair(3);
        let m1 = build_rotation(None, &old_priv, &old_pub, &k2_pub, 1_000).unwrap();
        // key1 is no longer the head (key2 is) — rotating from it must fail.
        assert!(build_rotation(Some(&m1), &old_priv, &old_pub, &k3_pub, 2_000).is_err());
    }

    #[test]
    fn rotation_reusing_an_existing_key_is_rejected() {
        let (old_priv, old_pub) = key_pair(1);
        let (k2_priv, k2_pub) = key_pair(2);
        let m1 = build_rotation(None, &old_priv, &old_pub, &k2_pub, 1_000).unwrap();
        // Rotate from key2 back to key1 (the root) — reuse is forbidden.
        assert!(build_rotation(Some(&m1), &k2_priv, &k2_pub, &old_pub, 2_000).is_err());
    }

    #[test]
    fn terminal_sanitizer_strips_control_and_escape_sequences() {
        // ANSI escapes / control chars from an untrusted .sig field must not
        // reach the terminal verbatim (terminal-injection / output spoofing).
        let evil = "Author\x1b[2K\x1b[31mFAKE\x07\ninjected";
        let safe = sanitize_for_terminal(evil);
        assert!(!safe.contains('\x1b'), "no ESC");
        assert!(!safe.contains('\x07'), "no BEL");
        assert!(!safe.contains('\n'), "no newline");
        assert!(safe.contains("Author") && safe.contains("FAKE") && safe.contains("injected"));
    }
}
