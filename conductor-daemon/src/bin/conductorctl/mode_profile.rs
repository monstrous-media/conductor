// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Mode, profile, LED, and plugin subcommand handlers.

use super::*;

/// `conductorctl mode set <name> [--no-lock]` (ADR-040 D4 §4.2).
pub(crate) async fn handle_mode_set(
    client: &mut IpcClient,
    name: &str,
    no_lock: bool,
    json: bool,
) -> Result<()> {
    let lock = !no_lock;
    let response = client
        .send_command(
            IpcCommand::SetMode,
            serde_json::json!({ "mode": name, "lock": lock }),
        )
        .await
        .context("Failed to set mode")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    }
    // Validate even in --json mode so daemon errors exit non-zero.
    check_ipc_response(&response, "set mode")?;
    if json {
        return Ok(());
    }
    if lock {
        println!("Mode set to {} {}", name.green(), "(locked)".dimmed());
    } else {
        println!(
            "Mode set to {} {}",
            name.green(),
            "(auto-switch active)".dimmed()
        );
    }
    Ok(())
}

/// `conductorctl mode unlock` (ADR-040 D4 §4.2).
pub(crate) async fn handle_mode_unlock(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::UnlockMode, Value::Null)
        .await
        .context("Failed to unlock mode")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    }
    check_ipc_response(&response, "unlock mode")?;
    if json {
        return Ok(());
    }
    let was_locked = response
        .data
        .as_ref()
        .and_then(|d| d.get("unlocked"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if was_locked {
        println!("{}", "Mode unlocked — auto-switching resumed".green());
    } else {
        println!("{}", "No mode lock was held".yellow());
    }
    Ok(())
}

/// `conductorctl mode status` (ADR-040 D4 §4.2).
pub(crate) async fn handle_mode_status(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::ModeStatus, Value::Null)
        .await
        .context("Failed to get mode status")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    }
    check_ipc_response(&response, "get mode status")?;
    if json {
        return Ok(());
    }
    if let Some(data) = &response.data {
        let mode = data.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
        let locked = data
            .get("locked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        println!("{}", "Mode Status".bold().cyan());
        println!("{}", "─".repeat(50));
        println!("Active: {}", mode.green());
        if locked {
            let origin = data
                .get("lock_origin")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            println!("Lock:   {} (origin: {})", "locked".yellow(), origin);
        } else {
            println!("Lock:   {}", "unlocked (auto-switch active)".dimmed());
        }
    }
    Ok(())
}

pub(crate) async fn handle_profile_status(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::GetActiveProfile, Value::Null)
        .await
        .context("Failed to get active profile")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    // Check for daemon errors first
    if matches!(response.status, conductor_daemon::ResponseStatus::Error) {
        let msg = response
            .error
            .map(|e| e.message)
            .unwrap_or_else(|| "Unknown error".to_string());
        bail!("Failed to get profile status: {}", msg);
    }

    if let Some(data) = &response.data {
        if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
            println!("{}", "Active Profile".bold().cyan());
            println!("{}", "─".repeat(50));
            println!("Name:   {}", name.green());
            if let Some(path) = data.get("config_path").and_then(|v| v.as_str()) {
                println!("Config: {}", path);
            }
        } else {
            println!("{}", "No active profile".yellow());
        }
    } else {
        println!("{}", "No active profile".yellow());
    }

    Ok(())
}

pub(crate) async fn handle_profile_switch(
    client: &mut IpcClient,
    path: &Path,
    json: bool,
) -> Result<()> {
    // Validate path is a .toml file
    if !path.is_file() {
        bail!("Profile config not found or not a file: {}", path.display());
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());
    if ext.as_deref() != Some("toml") {
        bail!("Profile config must be a .toml file: {}", path.display());
    }

    let canonical = path
        .canonicalize()
        .with_context(|| format!("Failed to resolve path: {}", path.display()))?;

    let args = serde_json::json!({ "config_path": canonical.to_string_lossy() });

    let response = client
        .send_command(IpcCommand::SwitchProfile, args)
        .await
        .context("Failed to switch profile")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        match response.status {
            conductor_daemon::ResponseStatus::Success => {
                println!(
                    "{} Switched to profile: {}",
                    "✓".green(),
                    canonical.display()
                );
            }
            conductor_daemon::ResponseStatus::Error => {
                let msg = response
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "Unknown error".to_string());
                bail!("Failed to switch profile: {}", msg);
            }
        }
    }

    Ok(())
}

pub(crate) fn handle_profile_validate(path: &Path, json: bool) -> Result<()> {
    if !path.exists() {
        bail!("Profile config not found: {}", path.display());
    }

    let path_str = path
        .to_str()
        .context("Profile config path must be valid UTF-8")?;
    let config =
        Config::load(path_str).map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;

    let report = conductor_core::config::validator::validate_config(&config);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if report.errors.is_empty() {
            println!("{}", "✓ Profile config is valid".green().bold());
        } else {
            println!(
                "{} ({} error(s)):",
                "✗ Profile config has errors".red().bold(),
                report.errors.len()
            );
            for f in &report.errors {
                println!("  {} {}: {}", "✗".red(), f.path, f.message);
            }
        }

        if !report.warnings.is_empty() {
            println!(
                "\n{} ({}):",
                "Warnings".yellow().bold(),
                report.warnings.len()
            );
            for f in &report.warnings {
                println!("  {} {}: {}", "!".yellow(), f.path, f.message);
            }
        }
    }

    if !report.is_valid() {
        bail!("Validation failed with {} error(s)", report.errors.len());
    }

    Ok(())
}

pub(crate) fn handle_profile_list(dir: &Option<PathBuf>, json: bool) -> Result<()> {
    let profile_dir = match dir {
        Some(d) => d.clone(),
        None => dirs::config_dir()
            .context("Could not determine config directory")?
            .join("conductor")
            .join("profiles"),
    };

    if !profile_dir.exists() {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "directory": profile_dir.display().to_string(),
                    "profiles": []
                }))?
            );
        } else {
            println!(
                "{} Directory not found: {}",
                "!".yellow(),
                profile_dir.display()
            );
        }
        return Ok(());
    }

    let mut profiles: Vec<(String, u64)> = Vec::new();

    for entry in std::fs::read_dir(&profile_dir)
        .with_context(|| format!("Failed to read directory: {}", profile_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            profiles.push((name, size));
        }
    }

    profiles.sort_by(|a, b| a.0.cmp(&b.0));

    if json {
        let items: Vec<serde_json::Value> = profiles
            .iter()
            .map(|(name, size)| serde_json::json!({ "name": name, "size": size }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "directory": profile_dir.display().to_string(),
                "profiles": items
            }))?
        );
    } else {
        println!("{}", "Profile Configs".bold().cyan());
        println!("Directory: {}", profile_dir.display());
        println!("{}", "─".repeat(50));

        if profiles.is_empty() {
            println!("{}", "  No .toml files found".yellow());
        } else {
            for (name, size) in &profiles {
                println!("  {} ({} bytes)", name, size);
            }
            println!("\n{} profile(s) found.", profiles.len());
        }
    }

    Ok(())
}

/// Resolve a profile name or path to a concrete path.
///
/// If the input contains `/` or ends with `.toml`, treat as a path.
/// Otherwise, resolve as `~/.config/conductor/profiles/<name>.toml`.
/// Validate a profile name to prevent path traversal and invalid filenames.
pub(crate) fn validate_profile_name(name: &str) -> Result<()> {
    if name.is_empty() || name.trim().is_empty() {
        bail!("Profile name cannot be empty");
    }
    if name.len() > 64 {
        bail!("Profile name too long (max 64 characters)");
    }
    if name.contains(['/', '\\', '\0']) || name.contains("..") {
        bail!("Profile name contains invalid characters");
    }
    // Only allow alphanumeric, hyphens, underscores, spaces
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ' ')
    {
        bail!(
            "Profile name may only contain alphanumeric characters, hyphens, underscores, and spaces"
        );
    }
    Ok(())
}

pub(crate) fn resolve_profile_path(name_or_path: &str) -> Result<PathBuf> {
    if name_or_path.contains('/') || name_or_path.ends_with(".toml") {
        let path = PathBuf::from(name_or_path);
        // For explicit paths, validate the file exists or parent dir exists
        // The caller (switch/delete) will do further validation
        Ok(path)
    } else {
        validate_profile_name(name_or_path)?;
        let profile_dir = dirs::config_dir()
            .context("Could not determine config directory")?
            .join("conductor")
            .join("profiles");
        Ok(profile_dir.join(format!("{}.toml", name_or_path)))
    }
}

pub(crate) fn handle_profile_create(name: &str, apps: &[String], json: bool) -> Result<()> {
    validate_profile_name(name)?;

    let profile_dir = dirs::config_dir()
        .context("Could not determine config directory")?
        .join("conductor")
        .join("profiles");

    std::fs::create_dir_all(&profile_dir).with_context(|| {
        format!(
            "Failed to create profiles directory: {}",
            profile_dir.display()
        )
    })?;

    let profile_path = profile_dir.join(format!("{}.toml", name));

    // Build a minimal config — one mode per app, or a single Default mode
    let modes: Vec<conductor_core::Mode> = if apps.is_empty() {
        vec![conductor_core::Mode {
            name: "Default".to_string(),
            color: Some("blue".to_string()),
            mappings: vec![],
        }]
    } else {
        apps.iter()
            .map(|app| conductor_core::Mode {
                name: app.clone(),
                color: None,
                mappings: vec![],
            })
            .collect()
    };

    let mut config = Config::default_config();
    config.modes = modes;
    config.global_mappings = vec![];

    let toml_str = toml::to_string_pretty(&config).context("Failed to serialize profile config")?;

    // Add a header comment
    let content = format!(
        "# Conductor profile: {}\n# Created by conductorctl\n\n{}",
        name, toml_str
    );

    // Atomic create — fails if file already exists (no TOCTOU race)
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&profile_path)
        .with_context(|| {
            if profile_path.exists() {
                format!(
                    "Profile '{}' already exists: {}",
                    name,
                    profile_path.display()
                )
            } else {
                format!("Failed to create profile: {}", profile_path.display())
            }
        })?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("Failed to write profile: {}", profile_path.display()))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "created",
                "name": name,
                "path": profile_path.display().to_string(),
                "apps": apps,
            }))?
        );
    } else {
        println!(
            "{} Created profile '{}': {}",
            "✓".green(),
            name.bold(),
            profile_path.display()
        );
        if !apps.is_empty() {
            println!("  Modes: {}", apps.join(", "));
        }
    }

    Ok(())
}

pub(crate) fn handle_profile_delete(name: &str, force: bool, json: bool) -> Result<()> {
    // resolve_profile_path validates names internally
    let profile_path = resolve_profile_path(name)?;

    if !profile_path.exists() {
        bail!("Profile '{}' not found: {}", name, profile_path.display());
    }

    if !force {
        // In non-interactive / CI contexts, require --force
        if !std::io::stdin().is_terminal() {
            bail!(
                "Refusing to delete '{}' without --force in non-interactive mode",
                name
            );
        }
        eprint!("Delete profile '{}'? [y/N] ", name);
        use std::io::BufRead;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    std::fs::remove_file(&profile_path)
        .with_context(|| format!("Failed to delete profile: {}", profile_path.display()))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "deleted",
                "name": name,
                "path": profile_path.display().to_string(),
            }))?
        );
    } else {
        println!(
            "{} Deleted profile '{}': {}",
            "✓".green(),
            name.bold(),
            profile_path.display()
        );
    }

    Ok(())
}

pub(crate) async fn handle_led(
    client: &mut IpcClient,
    action: &LedAction,
    json: bool,
) -> Result<()> {
    match action {
        LedAction::Status => {
            let response = client
                .send_command(IpcCommand::GetLedStatus, serde_json::json!({}))
                .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&response.data.unwrap_or(serde_json::json!({})))?
                );
            } else if let Some(data) = &response.data {
                println!("{}", "LED Status".cyan().bold());
                println!("  Enabled:      {}", data["enabled"]);
                println!("  Brightness:   {}", data["brightness"]);
                println!("  Scheme:       {}", data["scheme"]);
                println!("  Idle timeout: {}s", data["idle_timeout_secs"]);
            } else {
                println!("{}", "No LED configuration found".yellow());
            }
        }
        LedAction::Scheme { name } => {
            let response = client
                .send_command(
                    IpcCommand::SetLedScheme,
                    serde_json::json!({ "scheme": name }),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            // JSON mode must change output shape, not success
            // semantics — print the response (above), then return Err on a
            // daemon-error response so automation gets a non-zero exit.
            check_ipc_response(&response, "Set LED scheme")?;
            if !json {
                println!("{} LED scheme set to '{}'", "✓".green(), name);
            }
        }
        LedAction::Brightness { level } => {
            if *level > 127 {
                bail!("Brightness must be 0-127, got {}", level);
            }
            let response = client
                .send_command(
                    IpcCommand::SetLedBrightness,
                    serde_json::json!({ "brightness": level }),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            check_ipc_response(&response, "Set LED brightness")?;
            if !json {
                println!("{} LED brightness set to {}", "✓".green(), level);
            }
        }
        LedAction::Off => {
            let response = client
                .send_command(
                    IpcCommand::SetLedScheme,
                    serde_json::json!({ "scheme": "off" }),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            check_ipc_response(&response, "Turn off LEDs")?;
            if !json {
                println!("{} LEDs turned off", "✓".green());
            }
        }
    }
    Ok(())
}

pub(crate) async fn handle_plugin(
    client: &mut IpcClient,
    action: &PluginAction,
    json: bool,
) -> Result<()> {
    match action {
        PluginAction::List => {
            let response = client
                .send_command(IpcCommand::ListPlugins, serde_json::json!({}))
                .await?;
            if matches!(response.status, conductor_daemon::ResponseStatus::Error) {
                let msg = response
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "Unknown error".to_string());
                bail!("Failed to list plugins: {}", msg);
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&response.data.unwrap_or(serde_json::json!({})))?
                );
            } else if let Some(data) = &response.data {
                println!("{}", "Plugins".cyan().bold());
                if let Some(available) = data["available"].as_array() {
                    println!("  Available: {}", available.len());
                    for p in available {
                        println!("    - {}", p.as_str().unwrap_or("?"));
                    }
                }
                if let Some(loaded) = data["loaded"].as_array() {
                    println!("  Loaded: {}", loaded.len());
                    for p in loaded {
                        println!("    - {}", p.as_str().unwrap_or("?"));
                    }
                }
            } else {
                println!("{}", "No plugin data available".yellow());
            }
        }
        PluginAction::Info { name } => {
            let response = client
                .send_command(
                    IpcCommand::GetPluginInfo,
                    serde_json::json!({ "name": name }),
                )
                .await?;
            if matches!(response.status, conductor_daemon::ResponseStatus::Error) {
                let msg = response
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "Unknown error".to_string());
                bail!("Plugin '{}': {}", name, msg);
            }
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&response.data.unwrap_or(serde_json::json!({})))?
                );
            } else if let Some(data) = &response.data {
                println!("{} {}", "Plugin:".cyan().bold(), name);
                if let Some(v) = data.get("version") {
                    println!("  Version:     {}", v);
                }
                if let Some(d) = data.get("description") {
                    println!("  Description: {}", d);
                }
                if let Some(a) = data.get("author") {
                    println!("  Author:      {}", a);
                }
                if let Some(t) = data.get("type") {
                    println!("  Type:        {}", t);
                }
            } else {
                bail!("Plugin '{}' not found", name);
            }
        }
        PluginAction::Enable { name } => {
            let response = client
                .send_command(
                    IpcCommand::EnablePlugin,
                    serde_json::json!({ "name": name }),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            check_ipc_response(&response, "Enable plugin")?;
            if !json {
                println!("{} Plugin '{}' enabled", "✓".green(), name);
            }
        }
        PluginAction::Disable { name } => {
            let response = client
                .send_command(
                    IpcCommand::DisablePlugin,
                    serde_json::json!({ "name": name }),
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            check_ipc_response(&response, "Disable plugin")?;
            if !json {
                println!("{} Plugin '{}' disabled", "✓".green(), name);
            }
        }
    }
    Ok(())
}
