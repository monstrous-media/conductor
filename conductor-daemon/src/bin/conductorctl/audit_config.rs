// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Audit tail/resume, MCP registration, and the config IPC surface.

use super::*;

/// ADR-027 D13a — observe the security audit log.
///
/// Always prints the recent backlog (`QueryAudit`). In `--follow`
/// mode, the live subscription is opened FIRST and the backlog is
/// queried second so entries logged in the hand-off window are
/// buffered on the stream rather than silently missed. The small
/// overlap is de-duplicated client-side by audit-entry id.
///
/// An explicit `{"lagged": n}` marker from the daemon is surfaced as
/// a stderr warning telling the operator to backfill from the
/// persistent log.
/// `conductorctl audit resume` — recover from the fail-closed
/// audit-unavailable brick (ADR-034 §D8).
///
/// Sends `ResumeAudit`, which asks the daemon to reopen the audit outbox:
/// a corrupt chain is rotated aside to `audit-outbox.log.corrupt-<ms>` and a
/// fresh chain is started whose first record attests the operator-driven
/// reset, after which the daemon leaves `AuditDegraded` and resumes accepting
/// config mutations. A healthy outbox is a no-op.
pub(crate) async fn handle_audit_resume(client: &mut IpcClient, json_output: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::ResumeAudit, Value::Null)
        .await
        .context("Failed to send audit resume command")?;

    if let Some(err) = &response.error {
        bail!("Audit resume failed: {}", err.message);
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    let recovered = response
        .data
        .as_ref()
        .and_then(|d| d.get("recovered"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let rotated_path = response
        .data
        .as_ref()
        .and_then(|d| d.get("rotated_path"))
        .and_then(|v| v.as_str());

    if recovered {
        match rotated_path {
            Some(path) => println!(
                "{} Audit resumed — rotated corrupt outbox to {}",
                "✓".green(),
                path.bold()
            ),
            None => println!("{} Audit resumed.", "✓".green()),
        }
    } else {
        println!(
            "{} Audit outbox already healthy — nothing to recover.",
            "✓".green()
        );
    }

    Ok(())
}

pub(crate) async fn handle_audit(
    client: &mut IpcClient,
    denied_only: bool,
    follow: bool,
    last: u32,
    json_output: bool,
) -> Result<()> {
    use tokio::io::AsyncBufReadExt;

    // --- Follow setup: subscribe BEFORE querying backlog so entries
    // logged in the hand-off window are buffered on the stream rather
    // than disappearing between the backlog snapshot and the
    // subscription becoming live.
    let mut stream_client = if follow {
        let socket_path = get_socket_path()
            .context("Failed to determine IPC socket path")?
            .to_string_lossy()
            .to_string();
        let mut stream_client = IpcClient::new(socket_path)
            .await
            .context("Failed to open audit stream connection")?;
        let ack = stream_client
            .send_command(
                IpcCommand::SubscribeAudit,
                serde_json::json!({ "denied_only": denied_only }),
            )
            .await
            .context("Failed to subscribe to audit stream")?;
        if let Some(err) = &ack.error {
            bail!("Audit subscription refused: {}", err.message);
        }
        Some(stream_client)
    } else {
        None
    };

    // --- Backlog: one-shot query of the persistent log ---
    let response = client
        .send_command(
            IpcCommand::QueryAudit,
            serde_json::json!({ "denied_only": denied_only, "limit": last }),
        )
        .await
        .context("Failed to query audit log")?;

    if let Some(err) = &response.error {
        bail!("Audit query failed: {}", err.message);
    }

    let mut backlog_ids = HashSet::new();
    if let Some(entries) = response
        .data
        .as_ref()
        .and_then(|d| d.get("entries"))
        .and_then(|e| e.as_array())
    {
        if entries.is_empty() && !json_output {
            let what = if denied_only { "denial" } else { "audit" };
            println!("(no {what} entries in the audit log yet)");
        }
        // The daemon returns most-recent-first; print oldest-first so
        // a follow stream reads naturally as a continuation.
        for entry in entries.iter().rev() {
            if follow && let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
                backlog_ids.insert(id.to_string());
            }
            print_audit_entry(entry, json_output);
        }
    }

    if !follow {
        return Ok(());
    }

    if !json_output {
        eprintln!(
            "{}",
            "Following audit log (Ctrl+C to stop)...".bold().cyan()
        );
    }

    let stream_client = stream_client
        .take()
        .expect("follow mode initialized stream client");
    let mut reader = stream_client.into_reader();
    let mut line = String::new();
    loop {
        line.clear();
        tokio::select! {
            read = reader.read_line(&mut line) => {
                let n = read.context("Audit stream read error")?;
                if n == 0 {
                    // EOF — daemon closed the connection.
                    if !json_output {
                        eprintln!("{}", "Audit stream closed by daemon.".dimmed());
                    }
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(trimmed)
                    .context("Malformed audit stream line")?;
                // Explicit lag marker (impl-spec case 1J): tell the
                // operator to backfill from the persistent log.
                if let Some(lagged) = value.get("lagged").and_then(|v| v.as_u64()) {
                    eprintln!(
                        "{}",
                        format!(
                            "⚠ audit stream lagged by {lagged} events — \
                             re-run `conductorctl audit tail --last {}` to backfill",
                            lagged.max(last as u64)
                        )
                        .yellow()
                    );
                    continue;
                }
                if let Some(id) = value.get("id").and_then(|v| v.as_str())
                    && backlog_ids.remove(id)
                {
                    continue;
                }
                print_audit_entry(&value, json_output);
            }
            _ = tokio::signal::ctrl_c() => {
                if !json_output {
                    eprintln!("\n{}", "Stopped following audit log.".dimmed());
                }
                break;
            }
        }
    }

    Ok(())
}

/// Render a single audit entry. JSON mode prints the entry verbatim
/// (one JSON object per line — pipe-friendly). Text mode formats a
/// compact human line, highlighting denials.
pub(crate) fn print_audit_entry(entry: &Value, json_output: bool) {
    if json_output {
        println!("{}", entry);
        return;
    }

    let event_type = entry
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let tool = entry
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let created_at = entry
        .get("created_at")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    // created_at is Unix milliseconds.
    let ts = chrono::DateTime::from_timestamp_millis(created_at)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| created_at.to_string());

    let is_denial = event_type == "tool_denied";
    let label = if is_denial {
        event_type.red().bold()
    } else {
        event_type.normal()
    };

    let mut line = format!("{}  {}  {}", ts.dimmed(), label, tool);
    // For denials the reason lives in error_message — the gate
    // decision string (Deny(...) / RequirePlan / RequireConfirmation).
    if let Some(reason) = entry.get("error_message").and_then(|v| v.as_str()) {
        line.push_str(&format!("  {}", reason.yellow()));
    }
    // Caller identity, when present.
    if let Some(uc) = entry.get("user_context") {
        if let Some(client_id) = uc.get("client_id").and_then(|v| v.as_str()) {
            line.push_str(&format!("  {}", format!("[{client_id}]").cyan()));
        } else if let Some(uid) = uc.get("uid").and_then(|v| v.as_u64()) {
            line.push_str(&format!("  {}", format!("[uid={uid}]").cyan()));
        }
    }
    println!("{line}");
}

/// ADR-027 §D18 — `conductorctl mcp register`. Adds or
/// updates a registration in the on-disk table. Idempotent: same
/// `exe-path` overwrites name + tier rather than duplicating.
pub(crate) fn handle_mcp_register(
    name: &str,
    exe_path: &Path,
    tier: conductor_daemon::daemon::audit::AuditRiskTier,
    json_output: bool,
) -> Result<()> {
    use conductor_daemon::daemon::mcp_registry::{
        McpRegistration, McpRegistry, default_registry_path,
    };

    // Canonicalize
    // the user-supplied `--exe-path` (resolve symlinks, normalise
    // `..`/`.`) so the stored key matches what the kernel will
    // report at connect time. `PinnedPeer::initial_exe` is always
    // canonical; storing a non-canonical user input here would
    // silently never match.
    //
    // Canonicalization requires the file to EXIST — registering an
    // uninstalled binary is unsupported by design. The error
    // surface ("file not found") is the right UX for that case.
    if exe_path.is_relative() {
        anyhow::bail!(
            "--exe-path must be an absolute path (got: {})",
            exe_path.display()
        );
    }
    let canonical_exe = std::fs::canonicalize(exe_path).with_context(|| {
        format!(
            "Failed to canonicalize --exe-path {}. Does the file exist?",
            exe_path.display()
        )
    })?;
    let exe_path: &Path = &canonical_exe;

    let path = default_registry_path()
        .context("Failed to resolve default registry path (no data_local_dir)")?;
    let mut registry = McpRegistry::load(&path).context("Failed to load MCP registry")?;
    let was_existing = registry.lookup_tier(exe_path).is_some();
    registry.register(McpRegistration {
        name: name.to_string(),
        exe_path: exe_path.to_path_buf(),
        tier,
    });
    registry
        .save(&path)
        .context("Failed to save MCP registry")?;

    if json_output {
        println!(
            "{}",
            serde_json::json!({
                "status": "ok",
                "action": if was_existing { "updated" } else { "created" },
                "name": name,
                "exe_path": exe_path,
                "tier": tier.as_str(),
                "registry_path": path,
            })
        );
    } else {
        let verb = if was_existing {
            "Updated"
        } else {
            "Registered"
        };
        println!(
            "{} MCP client {} ({}) at tier {}",
            verb.green().bold(),
            name.bold(),
            exe_path.display(),
            tier.as_str().yellow(),
        );
        println!("  {}: {}", "Registry".dimmed(), path.display());
    }
    Ok(())
}

/// ADR-027 §D18 — `conductorctl mcp list`.
pub(crate) fn handle_mcp_list(json_output: bool) -> Result<()> {
    use conductor_daemon::daemon::mcp_registry::{McpRegistry, default_registry_path};

    let path = default_registry_path()
        .context("Failed to resolve default registry path (no data_local_dir)")?;
    let registry = McpRegistry::load(&path).context("Failed to load MCP registry")?;

    if json_output {
        println!(
            "{}",
            serde_json::json!({
                "registry_path": path,
                "count": registry.entries.len(),
                "entries": registry.entries,
            })
        );
        return Ok(());
    }

    if registry.entries.is_empty() {
        println!("(no MCP clients registered)");
        println!("  {}: {}", "Registry".dimmed(), path.display());
        return Ok(());
    }
    println!("{}", "Registered MCP clients:".bold());
    for entry in &registry.entries {
        println!(
            "  {}  {}  {}",
            entry.tier.as_str().yellow(),
            entry.name.bold(),
            entry.exe_path.display().to_string().dimmed(),
        );
    }
    println!("\n  {}: {}", "Registry".dimmed(), path.display());
    Ok(())
}

/// ADR-027 §D18 — `conductorctl mcp revoke`. Idempotent:
/// revoking a non-registered exe is a no-op (exit 0).
///
/// Canonicalize `--exe-path` the same way `handle_mcp_register`
/// does, so revoking through a symlink / `..`-bearing path that
/// resolves to the registered canonical entry actually removes it.
/// Pre-fix the literal user path was looked up against the canonical
/// stored entry and silently no-op'd — a permission-grant gap when
/// the operator believed they had revoked.
///
/// That canonicalize-or-die approach introduced a *second* gap: a
/// deleted/moved binary can't be canonicalized, so revocation
/// failed outright and the stale grant survived. Canonicalization is
/// now best-effort — see the body — so revocation by exe path works
/// whether or not the binary still exists on disk.
pub(crate) fn handle_mcp_revoke(exe_path: &Path, json_output: bool) -> Result<()> {
    use conductor_daemon::daemon::mcp_registry::{McpRegistry, default_registry_path};

    // Symmetry with `handle_mcp_register` — same absolute-path
    // requirement. The relative-path contract is unchanged.
    if exe_path.is_relative() {
        anyhow::bail!(
            "--exe-path must be an absolute path (got: {})",
            exe_path.display()
        );
    }

    // Canonicalize ONLY when the binary still exists. Pre-fix this
    // handler canonicalized unconditionally and bailed when the registered
    // client binary had been deleted/moved/was temporarily absent — so a
    // stale grant could *never* be revoked, and a future binary dropped at
    // the same path would inherit the stale tier ceiling. Revocation is "by
    // exe path" and must survive an absent binary.
    //
    //   - If the path resolves, revoke the canonical key — a symlinked /
    //     `..`-bearing path still matches the canonical stored entry.
    //   - If it does not resolve (deleted/moved), fall back to the literal
    //     absolute path, which matches an entry registered at an
    //     already-canonical path (the common case).
    //
    // `revoke()` removes only EXACT matches, so attempting both keys can
    // never remove an unintended entry.
    let literal = exe_path.to_path_buf();
    let canonical = std::fs::canonicalize(exe_path).ok();
    let revoke_key: &Path = canonical.as_deref().unwrap_or(&literal);

    let path = default_registry_path()
        .context("Failed to resolve default registry path (no data_local_dir)")?;
    let mut registry = McpRegistry::load(&path).context("Failed to load MCP registry")?;
    let removed = registry.revoke(revoke_key)
        // Belt-and-suspenders for a resolvable-but-symlinked path whose entry
        // was somehow stored non-canonically: also try the literal path.
        || (revoke_key != literal.as_path() && registry.revoke(&literal));
    let exe_path: &Path = revoke_key;
    // Skip the write when nothing
    // changed. A no-op revoke shouldn't touch the file's mtime,
    // shouldn't trigger config-watchers, and shouldn't cause an
    // atomic-rename round-trip for zero semantic effect.
    if removed {
        registry
            .save(&path)
            .context("Failed to save MCP registry")?;
    }

    if json_output {
        println!(
            "{}",
            serde_json::json!({
                "status": "ok",
                "removed": removed,
                "exe_path": exe_path,
                "registry_path": path,
            })
        );
    } else if removed {
        println!(
            "{} {} from MCP registry",
            "Revoked".green().bold(),
            exe_path.display()
        );
    } else {
        println!(
            "{} {} was not registered (no-op)",
            "Note:".yellow(),
            exe_path.display()
        );
    }
    Ok(())
}

/// Convert an IPC response into a Result so the caller can
/// propagate non-zero exit on daemon-reported errors. Pre-fix, both
/// rollback handlers printed `response.error.message` to stderr and
/// then returned `Ok(())` — automation invoking `conductorctl
/// rollback-config` in CI would treat a failed rollback as
/// successful (exit 0).
///
/// Returns `Err` if `response.status` is `Error` OR `response.error`
/// is Some. The error message includes the daemon's reported code
/// and message so callers can grep / dispatch on it.
///
/// Caller passes a `ctx` string (e.g. `"rollback"`) prepended to the
/// error for readability in shell scrollback.
pub(crate) fn check_ipc_response(
    response: &conductor_daemon::daemon::types::IpcResponse,
    ctx: &str,
) -> Result<()> {
    if matches!(response.status, conductor_daemon::ResponseStatus::Error)
        || response.error.is_some()
    {
        let msg = response
            .error
            .as_ref()
            .map(|e| format!("{} (code {})", e.message, e.code))
            .unwrap_or_else(|| "unknown error".to_string());
        bail!("{ctx} failed: {msg}");
    }
    Ok(())
}

pub(crate) async fn handle_rollback_config(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::RollbackConfig, Value::Null)
        .await
        .context("Failed to rollback config")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = &response.data {
        // D4.B.4: handler returns state_generation +
        // previous_generation + revision now (not modes/mappings).
        println!(
            "Config rolled back to known-good snapshot: gen {} → {} (revision: {})",
            data["previous_generation"], data["state_generation"], data["revision"]
        );
    } else if let Some(error) = &response.error {
        eprintln!("Rollback failed: {}", error.message);
    }

    // Propagate daemon-reported errors as non-zero exit, in
    // both JSON and non-JSON modes. Pre-fix, both modes returned
    // Ok(()) regardless — automation couldn't distinguish a failed
    // rollback from a successful one.
    check_ipc_response(&response, "rollback")
}

/// Break-glass non-CAS rollback (ADR-034 §D6 / D4.B.4).
/// Daemon-side enforces CLI-only + non-empty reason; we still echo
/// the operator's stated reason locally so a copy lands in the
/// shell scrollback alongside the daemon-log entry.
pub(crate) async fn handle_rollback_config_force(
    client: &mut IpcClient,
    reason: &str,
    json: bool,
) -> Result<()> {
    let response = client
        .send_command(
            IpcCommand::RollbackConfigForce,
            serde_json::json!({ "reason": reason }),
        )
        .await
        .context("Failed to force-rollback config")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = &response.data {
        println!(
            "BREAK-GLASS rollback: gen {} → {} (revision: {})",
            data["previous_generation"], data["state_generation"], data["revision"]
        );
        println!("Reason: {reason}");
    } else if let Some(error) = &response.error {
        eprintln!("Force-rollback failed: {}", error.message);
    }

    // Same fix as plain rollback — propagate daemon errors.
    check_ipc_response(&response, "force-rollback")
}

// ============================================================================
// Config IPC surface (ADR-034 §D4.C / §D9)
// ============================================================================

/// `conductorctl config drift` — query whether the on-disk user config has
/// drifted from the daemon's live config (ADR-034 §D9). Read-only.
pub(crate) async fn handle_config_drift(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::ConfigDriftStatus, Value::Null)
        .await
        .context("Failed to query config-drift status")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = &response.data {
        println!("Config drift status:");
        match data.as_object() {
            Some(fields) => {
                for (key, value) in fields {
                    println!("  {key}: {value}");
                }
            }
            None => println!("  {data}"),
        }
    } else if let Some(error) = &response.error {
        eprintln!("Drift query failed: {}", error.message);
    }

    check_ipc_response(&response, "config drift")
}

/// `conductorctl config mark-known-good` — mark the daemon's current live config
/// as the known-good snapshot (ADR-034 §D6) that a subsequent rollback targets.
pub(crate) async fn handle_config_mark_known_good(
    client: &mut IpcClient,
    json: bool,
) -> Result<()> {
    let response = client
        .send_command(IpcCommand::MarkKnownGood, Value::Null)
        .await
        .context("Failed to mark config known-good")?;

    // Decide success/failure FIRST: `check_ipc_response` also fails on an Error
    // status with no `error` field, so gate the success line on the real verdict
    // — never print "marked …" and then exit non-zero (Copilot review).
    let outcome = check_ipc_response(&response, "config mark-known-good");
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if outcome.is_ok() {
        println!("Marked the current live config as the known-good snapshot.");
    } else if let Some(error) = &response.error {
        eprintln!("mark-known-good failed: {}", error.message);
    }

    outcome
}

/// Fetch the daemon's current config generation — the CAS base a reload / import
/// pins its mutation on top of (ADR-034 §D2.1 optimistic concurrency). A
/// concurrent change bumps the generation and the daemon then returns
/// `StaleBaseGeneration`, so the operator knows to re-run against fresh state.
pub(crate) async fn fetch_base_generation(client: &mut IpcClient) -> Result<u64> {
    let response = client
        .send_command(IpcCommand::GetConfigSnapshot, Value::Null)
        .await
        .context("Failed to fetch the current config generation")?;
    check_ipc_response(&response, "config snapshot")?;
    response
        .data
        .as_ref()
        .and_then(|d| d.get("state_generation"))
        .and_then(serde_json::Value::as_u64)
        .context("daemon config snapshot did not include a u64 state_generation")
}

/// Render a reload/import success line + propagate the daemon verdict. Shared by
/// `config reload` and `config import` (their only difference is the message).
pub(crate) fn report_reload_outcome(
    response: &conductor_daemon::daemon::types::IpcResponse,
    json: bool,
    ctx: &str,
    success_msg: impl FnOnce(&str),
) -> Result<()> {
    let outcome = check_ipc_response(response, ctx);
    if json {
        println!("{}", serde_json::to_string_pretty(response)?);
    } else if outcome.is_ok() {
        let generation = response
            .data
            .as_ref()
            .and_then(|d| d.get("state_generation"))
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        success_msg(&generation);
    } else if let Some(error) = &response.error {
        eprintln!("{ctx} failed: {}", error.message);
    }
    outcome
}

/// `conductorctl config reload [--path PATH]` — re-read the daemon's config file
/// (or `--path`) from disk and republish it (ADR-034 §D2.2).
pub(crate) async fn handle_config_reload(
    client: &mut IpcClient,
    path: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    let base_generation = fetch_base_generation(client).await?;
    let mut args = serde_json::json!({ "base_generation": base_generation });
    if let Some(p) = path {
        args["path"] = serde_json::Value::String(p.display().to_string());
    }
    let response = client
        .send_command(IpcCommand::ReloadFromDisk, args)
        .await
        .context("Failed to reload config from disk")?;

    report_reload_outcome(&response, json, "config reload", |generation| match path {
        Some(p) => println!(
            "Reloaded config from {} (now at generation {generation}).",
            p.display()
        ),
        None => {
            println!("Reloaded the daemon's config from disk (now at generation {generation}).")
        }
    })
}

/// `conductorctl config import PATH` — import a config from an explicit
/// allowlisted `.toml` path (ADR-034 §D2.2).
pub(crate) async fn handle_config_import(
    client: &mut IpcClient,
    path: &std::path::Path,
    json: bool,
) -> Result<()> {
    let base_generation = fetch_base_generation(client).await?;
    let args = serde_json::json!({
        "base_generation": base_generation,
        "path": path.display().to_string(),
    });
    let response = client
        .send_command(IpcCommand::ImportConfig, args)
        .await
        .context("Failed to import config")?;

    report_reload_outcome(&response, json, "config import", |generation| {
        println!(
            "Imported config from {} (now at generation {generation}).",
            path.display()
        );
    })
}

/// `conductorctl config save [--base-generation N]` — commit a config read from
/// **stdin** via `SaveConfig` (ADR-034 §D4.C).
/// "save = bodies, import = paths": `save` sends a wholly
/// client-constructed config body, so it never touches the daemon's §D2.2 path
/// allowlist — a positional path is therefore rejected with a redirect to
/// `config import`, which DOES apply the allowlist (no path-shaped bypass).
pub(crate) async fn handle_config_save(
    client: &mut IpcClient,
    path: Option<&std::path::Path>,
    base_generation: Option<u64>,
    json: bool,
) -> Result<()> {
    // Reject a positional path — `save` is stdin-only by design.
    if let Some(p) = path {
        bail!(
            "`config save` commits a config body read from stdin; it does not take a path. \
             To load a config FILE (under the daemon's path allowlist), use: \
             `conductorctl config import {}`",
            p.display()
        );
    }
    // Don't hang on an interactive terminal — require piped input.
    if std::io::stdin().is_terminal() {
        bail!(
            "`config save` reads the config from stdin — pipe one in \
             (e.g. `cat config.toml | conductorctl config save`), or use \
             `conductorctl config import PATH` to load a file"
        );
    }

    let mut toml_text = String::new();
    // Lock stdin into a local (consistent with the other handlers; avoids a
    // `&mut` borrow of the temporary `std::io::stdin()` returns).
    let mut stdin = std::io::stdin().lock();
    std::io::Read::read_to_string(&mut stdin, &mut toml_text)
        .context("Failed to read config from stdin")?;
    if toml_text.trim().is_empty() {
        bail!("`config save` read an empty config from stdin");
    }
    let config: Config = toml::from_str(&toml_text).context("stdin is not valid config TOML")?;
    let config_json =
        serde_json::to_value(&config).context("failed to serialize the parsed config")?;

    // Use the explicit base generation, else pin the daemon's current one (CAS).
    let base_generation = match base_generation {
        Some(g) => g,
        None => fetch_base_generation(client).await?,
    };
    let args = serde_json::json!({
        "config": config_json,
        "base_generation": base_generation,
    });
    let response = client
        .send_command(IpcCommand::SaveConfig, args)
        .await
        .context("Failed to save config")?;

    report_reload_outcome(&response, json, "config save", |generation| {
        println!("Saved config from stdin (now at generation {generation}).");
    })
}

// ============================================================================
// Config Migration Implementation (ADR-009 Phase 6)
// ============================================================================

/// Resolve the config path for `migrate-config`, defaulting to the SAME location
/// the daemon and GUI use (`dirs::config_dir()/conductor/config.toml`) rather
/// than `~/.conductor/config.toml`. On macOS those differ (`config_dir()` is
/// `~/Library/Application Support/conductor`), so the old home-based default
/// pointed `migrate-config` at a file that doesn't exist — it errored/no-op'd
/// and the user's real config (the one the daemon loaded) was never migrated,
/// breaking the migration path ADR-035's deprecation warning advertises.
pub(crate) fn resolve_migrate_config_path(config_path: &Option<PathBuf>) -> Result<PathBuf> {
    match config_path {
        Some(p) => Ok(p.clone()),
        None => Ok(dirs::config_dir()
            .context("Could not determine config directory")?
            .join("conductor")
            .join("config.toml")),
    }
}

/// Handle the `migrate-config --routing` subcommand (ADR-036).
///
/// Rewrites legacy `Trigger::Raw` + `MidiForward` mappings into top-level
/// `[[routes]]` entries, preserving TOML comments via `toml_edit`. (ADR-036
/// Phase 3 removed the inverse `--reverse` direction — all routes are
/// post-mapping.)
pub(crate) fn handle_migrate_routing(
    config_path: &Option<PathBuf>,
    dry_run: bool,
    no_backup: bool,
    json: bool,
) -> Result<()> {
    use conductor_daemon::migration::migrate_raw_to_routes;

    let path = resolve_migrate_config_path(config_path)?;

    if !path.exists() {
        bail!("Config file not found: {}", path.display());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;

    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

    let report = migrate_raw_to_routes(&mut doc).map_err(|e| anyhow!("{}", e))?;

    let new_toml = doc.to_string();

    if json {
        let r = serde_json::json!({
            "status": if dry_run { "dry_run" } else { "migrated" },
            "direction": "forward",
            "path": path.display().to_string(),
            "rewrites": report.rewrites,
            "errors": report.errors,
            "migrated_toml": if dry_run { Some(new_toml.clone()) } else { None },
        });
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        for rewrite in &report.rewrites {
            println!("{} {}", "✓".green(), rewrite);
        }
        for err in &report.errors {
            println!("{} {}", "⚠".yellow(), err);
        }
        if report.rewrites.is_empty() && report.errors.is_empty() {
            println!("{}", "No routing migration needed.".green());
        }
    }

    if dry_run {
        if !json {
            println!("\n{}", "(dry-run) Resulting TOML:".cyan());
            println!("{}", new_toml);
            println!("Use without --dry-run to apply.");
        }
        return Ok(());
    }

    // True no-op (no rewrites, no errors): leave the file untouched and skip
    // the backup — there's nothing to migrate.
    if report.rewrites.is_empty() && report.errors.is_empty() {
        return Ok(());
    }

    // Write the migrated TOML, backing up the original first.
    if !no_backup {
        let backup_path = path.with_extension("toml.bak");
        std::fs::copy(&path, &backup_path)
            .with_context(|| format!("Failed to create backup at {}", backup_path.display()))?;
        if !json {
            println!("Backup: {}", backup_path.display());
        }
    }

    std::fs::write(&path, &new_toml)
        .with_context(|| format!("Failed to write migrated config: {}", path.display()))?;
    if !json {
        println!("Written: {}", path.display());
    }

    Ok(())
}
