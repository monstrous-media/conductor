// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Network-listener approval and security administration (Unix only).

use super::*;

/// `~/.conductor/network_approvals.json` — the HMAC-signed approval registry.
pub(crate) fn approval_registry_path() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("Could not determine home directory")?
        .join(".conductor")
        .join("network_approvals.json"))
}

/// Load the daemon config used to resolve a listener's host/port/ACL by alias.
pub(crate) fn load_listener_config(config: &Option<PathBuf>) -> Result<Config> {
    let path = match config {
        Some(p) => p.clone(),
        None => dirs::config_dir()
            .map(|d| d.join("conductor").join("config.toml"))
            .ok_or_else(|| anyhow!("Could not determine config directory"))?,
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config from {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("Failed to parse config {}", path.display()))
}

/// Resolve the network-approval HMAC key from the OS keychain.
pub(crate) fn approval_key() -> Result<conductor_core::security::keychain::HmacKey> {
    let kc = select_keychain().map_err(|e| anyhow!("keychain unavailable: {e}"))?;
    kc.get_or_create_hmac_key()
        .map_err(|e| anyhow!("keychain key: {e}"))
}

pub(crate) fn handle_listener_list(
    config: &Option<PathBuf>,
    json: bool,
    detailed: bool,
) -> Result<()> {
    let cfg = load_listener_config(config)?;
    let key = approval_key()?;
    let path = approval_registry_path()?;
    let statuses = approval_admin::statuses(&cfg, &path, &key);

    if json {
        let arr: Vec<Value> = statuses
            .iter()
            .map(|s| {
                serde_json::json!({
                    "alias": s.listener.alias,
                    "host": s.listener.host,
                    "port": s.listener.port,
                    "loopback": s.listener.is_loopback,
                    "approved": s.approved,
                    "registry_tampered": s.registry_tampered,
                    "requires_amplification_ack": s.listener.requires_amplification_ack,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "listeners": arr }))?
        );
        return Ok(());
    }

    if statuses.is_empty() {
        println!("No network listeners configured.");
        return Ok(());
    }
    for s in &statuses {
        let status = if s.listener.is_loopback {
            "loopback (auto-approved)".dimmed().to_string()
        } else if s.registry_tampered {
            "REGISTRY TAMPERED — re-approve".red().bold().to_string()
        } else if s.approved {
            "approved".green().to_string()
        } else {
            "PROMPT REQUIRED".yellow().to_string()
        };
        if detailed {
            let amp = if s.listener.requires_amplification_ack {
                "  (amplification ack required)"
            } else {
                ""
            };
            println!(
                "{:<20} {}:{:<6} [{}]{}",
                s.listener.alias.bold(),
                s.listener.host,
                s.listener.port,
                status,
                amp
            );
        } else {
            println!("{:<20} {}", s.listener.alias, status);
        }
    }
    Ok(())
}

pub(crate) fn handle_listener_approve(
    alias: &str,
    config: &Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let cfg = load_listener_config(config)?;
    let key = approval_key()?;
    let path = approval_registry_path()?;
    let info = approval_admin::approve(&cfg, &path, &key, alias, ApprovingSurface::Cli)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "ok", "alias": alias, "loopback": info.is_loopback,
            }))?
        );
    } else if info.is_loopback {
        println!(
            "{} '{}' is loopback — already auto-approved (no record needed).",
            "✓".green(),
            alias
        );
    } else {
        println!(
            "{} approved listener '{}' ({}:{}). The daemon applies it on next (re)bind.",
            "✓".green(),
            alias,
            info.host,
            info.port
        );
    }
    Ok(())
}

pub(crate) fn handle_listener_deny(
    alias: &str,
    config: &Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let cfg = load_listener_config(config)?;
    let key = approval_key()?;
    let path = approval_registry_path()?;
    let removed = approval_admin::deny(&cfg, &path, &key, alias)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "status": "ok", "alias": alias, "removed": removed })
            )?
        );
    } else if removed {
        println!("{} revoked approval for listener '{}'.", "✓".green(), alias);
    } else {
        println!("No approval on file for listener '{}'.", alias);
    }
    Ok(())
}

pub(crate) fn handle_security_status(json: bool) -> Result<()> {
    let kc = select_keychain().map_err(|e| anyhow!("keychain unavailable: {e}"))?;
    // Report-only: never the init hard-fail; show the level even if hard-expired.
    match conductor_daemon::security::key_rotation_status(kc.as_ref()) {
        Ok(status) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "hmac_key_fingerprint": status.fingerprint,
                        "hmac_key_age_days": status.age_days,
                        // Stable schema: a healthy key reports "ok", not null.
                        "hmac_key_warning": status.warning_tag().unwrap_or("ok"),
                    }))?
                );
            } else {
                println!("Network-approval HMAC key:");
                println!("  fingerprint:  {}", status.fingerprint);
                println!("  age:          {} days", status.age_days);
                match status.warning_tag() {
                    None => println!("  rotation:     {}", "ok".green()),
                    Some(tag) => {
                        let painted = if tag == "hard_expired" {
                            tag.red().bold().to_string()
                        } else {
                            tag.yellow().to_string()
                        };
                        println!("  rotation:     {painted}");
                        if let Some(msg) = status.level.message(status.age_days) {
                            println!("  {msg}");
                        }
                    }
                }
            }
        }
        Err(e) => {
            if json {
                println!(
                    "{}",
                    // Full schema even when unavailable: null fingerprint/age,
                    // warning "unavailable", plus a detail string.
                    serde_json::to_string_pretty(&serde_json::json!({
                        "hmac_key_fingerprint": serde_json::Value::Null,
                        "hmac_key_age_days": serde_json::Value::Null,
                        "hmac_key_warning": "unavailable",
                        "detail": e.to_string(),
                    }))?
                );
            } else {
                println!(
                    "Network-approval HMAC key: {} (no key initialized yet, or backend \
                     unavailable: {e})",
                    "unavailable".dimmed()
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn handle_security_rotate_hmac(json: bool) -> Result<()> {
    let kc = select_keychain().map_err(|e| anyhow!("keychain unavailable: {e}"))?;
    let path = approval_registry_path()?;
    let fingerprint = approval_admin::rotate_hmac(kc.as_ref(), &path)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "status": "ok", "new_fingerprint": fingerprint })
            )?
        );
    } else {
        println!(
            "{} rotated the network-approval HMAC key (new fingerprint {}). \
             Existing approvals were re-signed under the new key.",
            "✓".green(),
            fingerprint
        );
    }
    Ok(())
}
