// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Conductor daemon control CLI
//!
//! Command-line interface for controlling the Conductor daemon.

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use colored::Colorize;
use conductor_core::Config;
// ADR-042 Phase B-early listener-approval CLI is Unix-only (the approval
// registry / keychain hardened-file APIs are `#[cfg(unix)]`); conductorctl
// itself talks over a Unix domain socket, so this is consistent.
#[cfg(unix)]
use conductor_core::security::keychain::select_keychain;
#[cfg(unix)]
use conductor_daemon::security::ApprovingSurface;
#[cfg(unix)]
use conductor_daemon::security::approval_admin;
use conductor_daemon::{IpcClient, IpcCommand, get_socket_path};
use serde_json::Value;
use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

mod audit_config;
mod cli;
mod daemon_ops;
mod diagnostics;
mod events;
#[cfg(unix)]
mod listener_security;
mod mode_profile;
mod service_mgmt;

use audit_config::*;
use cli::*;
use daemon_ops::*;
use diagnostics::*;
use events::*;
#[cfg(unix)]
use listener_security::*;
use mode_profile::*;
use service_mgmt::*;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging if verbose
    if cli.verbose {
        tracing_subscriber::fmt()
            .with_env_filter("conductor_daemon=debug")
            .init();
    }

    match execute_command(&cli).await {
        Ok(()) => Ok(()),
        Err(e) => {
            if !cli.json {
                eprintln!("{} {}", "Error:".red().bold(), e);
            } else {
                let error_json = serde_json::json!({
                    "status": "error",
                    "error": e.to_string()
                });
                println!("{}", serde_json::to_string_pretty(&error_json)?);
            }
            std::process::exit(1);
        }
    }
}

async fn execute_command(cli: &Cli) -> Result<()> {
    // Service management commands don't need IPC client
    match &cli.command {
        Commands::Install {
            install_binary,
            force,
        } => {
            return handle_install(*install_binary, *force, cli.json);
        }
        Commands::Uninstall {
            remove_binary,
            remove_logs,
        } => {
            return handle_uninstall(*remove_binary, *remove_logs, cli.json);
        }
        Commands::Start { wait } => {
            return handle_start(*wait, cli.json).await;
        }
        Commands::Stop { force } => {
            return handle_stop_service(*force, cli.json).await;
        }
        Commands::Restart { wait } => {
            return handle_restart(*wait, cli.json).await;
        }
        Commands::Enable => {
            return handle_enable(cli.json);
        }
        Commands::Disable => {
            return handle_disable(cli.json);
        }
        Commands::ServiceStatus => {
            return handle_service_status(cli.json);
        }
        Commands::MigrateConfig {
            config,
            no_backup,
            routing,
            dry_run,
        } => {
            if *routing {
                return handle_migrate_routing(config, *dry_run, *no_backup, cli.json);
            }
            // ADR-035: the legacy [[bindings]]/[[connectors]] and [device]
            // formats were removed entirely, so the identity/[device] migration
            // paths are gone. Only the ADR-036 routing migration remains.
            bail!(
                "migrate-config now supports only --routing (legacy [[bindings]]/[[connectors]]/[device] \
                 have been removed; author [[endpoints]] directly)."
            );
        }
        Commands::ValidateSchema { config } => {
            return handle_validate_schema(config, cli.json);
        }
        Commands::Permissions {
            check,
            open_input_monitoring,
        } => {
            // The handler is async because it tries
            // IPC first (to ask the running daemon for its real grant
            // state) before falling back to a local probe. The
            // existing dispatch above handles non-IPC commands
            // synchronously, so we await here directly and return.
            return handle_permissions(*check, *open_input_monitoring, cli.json).await;
        }
        Commands::Profile { action } => match action {
            ProfileAction::Validate { path } => {
                return handle_profile_validate(path, cli.json);
            }
            ProfileAction::List { dir } => {
                return handle_profile_list(dir, cli.json);
            }
            ProfileAction::Create { name, app } => {
                return handle_profile_create(name, app, cli.json);
            }
            ProfileAction::Delete { name, force } => {
                return handle_profile_delete(name, *force, cli.json);
            }
            _ => {} // Status and Switch need IPC
        },
        Commands::Mcp { action } => match action {
            McpAction::Register {
                name,
                exe_path,
                tier,
            } => {
                return handle_mcp_register(name, exe_path, *tier, cli.json);
            }
            McpAction::List => {
                return handle_mcp_list(cli.json);
            }
            McpAction::Revoke { exe_path } => {
                return handle_mcp_revoke(exe_path, cli.json);
            }
        },
        Commands::Llm { action } => match action {
            LlmAction::Budgets { action } => match action {
                BudgetsAction::Show { config, json } => {
                    return handle_llm_budgets_show(config, *json || cli.json);
                }
            },
        },
        #[cfg(unix)]
        Commands::Listener { action } => match action {
            ListenerAction::List { config } => {
                return handle_listener_list(config, cli.json, false);
            }
            ListenerAction::Status { config } => {
                return handle_listener_list(config, cli.json, true);
            }
            ListenerAction::Approve { alias, config } => {
                return handle_listener_approve(alias, config, cli.json);
            }
            ListenerAction::Deny { alias, config } => {
                return handle_listener_deny(alias, config, cli.json);
            }
        },
        #[cfg(unix)]
        Commands::Security { action } => match action {
            SecurityAction::Status => {
                return handle_security_status(cli.json);
            }
            SecurityAction::RotateHmac => {
                return handle_security_rotate_hmac(cli.json);
            }
        },
        _ => {} // Fall through to IPC commands
    }

    // IPC commands require connection to daemon
    let socket_path = get_socket_path()
        .context("Failed to determine IPC socket path")?
        .to_string_lossy()
        .to_string();

    let mut client = IpcClient::new(socket_path)
        .await
        .context("Failed to connect to daemon. Is the daemon running?")?;

    // Execute IPC command
    match &cli.command {
        Commands::Status => handle_status(&mut client, cli.json).await?,
        Commands::Reload => handle_reload(&mut client, cli.json).await?,
        Commands::Shutdown => handle_shutdown(&mut client, cli.json).await?,
        Commands::Validate { config } => handle_validate(&mut client, config, cli.json).await?,
        Commands::Ping => handle_ping(&mut client, cli.json).await?,
        Commands::ListDevices => handle_list_devices(&mut client, cli.json).await?,
        Commands::Bindings {
            alias,
            unbound_only,
        } => handle_bindings(&mut client, cli.json, alias.clone(), *unbound_only).await?,
        Commands::SetDevice { port } => handle_set_device(&mut client, *port, cli.json).await?,
        Commands::GetDevice => handle_get_device(&mut client, cli.json).await?,
        Commands::RollbackConfig => handle_rollback_config(&mut client, cli.json).await?,
        Commands::RollbackConfigForce { reason } => {
            handle_rollback_config_force(&mut client, reason, cli.json).await?
        }
        Commands::Config { action } => match action {
            ConfigAction::Drift => handle_config_drift(&mut client, cli.json).await?,
            ConfigAction::MarkKnownGood => {
                handle_config_mark_known_good(&mut client, cli.json).await?
            }
            ConfigAction::Reload { path } => {
                handle_config_reload(&mut client, path.as_deref(), cli.json).await?
            }
            ConfigAction::Import { path } => {
                handle_config_import(&mut client, path, cli.json).await?
            }
            ConfigAction::Save {
                path,
                base_generation,
            } => {
                handle_config_save(&mut client, path.as_deref(), *base_generation, cli.json).await?
            }
        },
        Commands::Profile { action } => match action {
            ProfileAction::Status => handle_profile_status(&mut client, cli.json).await?,
            ProfileAction::Switch { name_or_path } => {
                let path = resolve_profile_path(name_or_path)?;
                handle_profile_switch(&mut client, &path, cli.json).await?
            }
            ProfileAction::Validate { .. }
            | ProfileAction::List { .. }
            | ProfileAction::Create { .. }
            | ProfileAction::Delete { .. } => {
                // Already handled before IPC connection — should not reach here
                bail!("Internal error: local profile command reached IPC handler")
            }
        },
        Commands::Mode { action } => match action {
            ModeAction::Set { name, no_lock } => {
                handle_mode_set(&mut client, name, *no_lock, cli.json).await?
            }
            ModeAction::Unlock => handle_mode_unlock(&mut client, cli.json).await?,
            ModeAction::Status => handle_mode_status(&mut client, cli.json).await?,
        },
        Commands::Led { action } => handle_led(&mut client, action, cli.json).await?,
        Commands::Plugin { action } => handle_plugin(&mut client, action, cli.json).await?,
        Commands::Events {
            follow,
            event_type,
            channel,
            note_min,
            note_max,
            device,
            since,
            debounce,
            filter: named_filter,
            format,
            limit,
            output,
            duration,
            profiling,
        } => {
            // Build filter from CLI args
            // CLI uses 1-16 for channels (human-friendly), internal uses 0-15
            let since_ms = match since.as_deref() {
                Some(s) => {
                    let secs = parse_duration_str(Some(s)).ok_or_else(|| {
                        anyhow!("Invalid --since value '{}'. Use e.g. 30s, 5m, 1h", s)
                    })?;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis() as u64;
                    Some(now.saturating_sub(secs.saturating_mul(1000)))
                }
                None => None,
            };

            // Named filter from config (R914) — merges with any CLI flags
            let mut filter = conductor_daemon::EventFilter {
                event_type: event_type.clone(),
                channel: channel.map(|ch| ch.saturating_sub(1)),
                note_min: *note_min,
                note_max: *note_max,
                device_id: device.clone(),
                since_ms,
            };
            if let Some(name) = named_filter {
                let config = load_config_for_filter()?;
                let named = config
                    .event_console
                    .as_ref()
                    .and_then(|ec| ec.filters.get(name.as_str()))
                    .ok_or_else(|| {
                        anyhow!(
                            "Named filter '{}' not found in [event_console.filters] config",
                            name
                        )
                    })?;
                // Named filter values fill in any fields not already set by CLI flags
                if filter.event_type.is_none() {
                    filter.event_type = named.event_type.clone();
                }
                if filter.channel.is_none() {
                    filter.channel = named.channel;
                }
                if filter.note_min.is_none() {
                    filter.note_min = named.note_min;
                }
                if filter.note_max.is_none() {
                    filter.note_max = named.note_max;
                }
                if filter.device_id.is_none() {
                    filter.device_id = named.device_id.clone();
                }
            }
            handle_events(
                &mut client,
                *follow,
                filter,
                format,
                *limit,
                cli.json,
                output.clone(),
                duration.clone(),
                *debounce,
                *profiling,
            )
            .await?
        }
        Commands::PlaybackEvents {
            file,
            speed,
            format,
            no_delay,
        } => {
            handle_playback_events(file, *speed, format, *no_delay).await?;
        }
        Commands::Audit { action } => match action {
            AuditAction::Tail { follow, last } => {
                handle_audit(&mut client, false, *follow, *last, cli.json).await?
            }
            AuditAction::Denied { follow, last } => {
                handle_audit(&mut client, true, *follow, *last, cli.json).await?
            }
            AuditAction::Resume => handle_audit_resume(&mut client, cli.json).await?,
        },
        _ => unreachable!("Service commands handled above"),
    }

    Ok(())
}

#[cfg(test)]
mod tests;
