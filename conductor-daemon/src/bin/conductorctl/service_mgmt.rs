// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Service management (launchd/systemd), migrate-config, and service status.

use super::*;

/// Service configuration constants
mod service {
    use std::path::PathBuf;

    pub const SERVICE_LABEL: &str = "media.monstrous.conductor";
    pub const DAEMON_BINARY_NAME: &str = "conductor";

    pub fn get_plist_path() -> PathBuf {
        dirs::home_dir()
            .expect("Could not determine home directory")
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", SERVICE_LABEL))
    }

    pub fn get_binary_install_path() -> PathBuf {
        PathBuf::from("/usr/local/bin").join(DAEMON_BINARY_NAME)
    }

    pub fn get_log_dir() -> PathBuf {
        dirs::home_dir()
            .expect("Could not determine home directory")
            .join("Library/Logs")
    }

    pub fn get_template_plist_path() -> Option<PathBuf> {
        // Try to find the template plist in the source tree
        let cargo_manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
        let template_path = PathBuf::from(cargo_manifest_dir)
            .join("launchd")
            .join(format!("{}.plist", SERVICE_LABEL));

        if template_path.exists() {
            Some(template_path)
        } else {
            None
        }
    }
}

/// Check if running on macOS
pub(crate) fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

/// Check if service is installed
pub(crate) fn is_service_installed() -> bool {
    service::get_plist_path().exists()
}

/// Check if daemon is running via IPC ping
pub(crate) async fn is_daemon_running() -> bool {
    match get_socket_path() {
        Ok(socket_path) => {
            if let Ok(mut client) = IpcClient::new(socket_path.to_string_lossy().to_string()).await
            {
                client
                    .send_command(IpcCommand::Ping, Value::Null)
                    .await
                    .is_ok()
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// Install Conductor as a LaunchAgent service
pub(crate) fn handle_install(install_binary: bool, force: bool, json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service installation is currently only supported on macOS");
    }

    let plist_path = service::get_plist_path();

    // Check if already installed
    if is_service_installed() && !force {
        if json {
            let result = serde_json::json!({
                "status": "error",
                "error": "Service already installed. Use --force to reinstall."
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            eprintln!(
                "{}",
                "Service already installed. Use --force to reinstall.".yellow()
            );
        }
        return Ok(());
    }

    if !json {
        println!("{}", "Installing Conductor service...".bold());
    }

    // Step 1: Install binary if requested
    let binary_path = if install_binary {
        install_daemon_binary(json)?
    } else {
        // Try to find the binary in common locations
        find_daemon_binary()?
    };

    // Step 2: Create LaunchAgents directory
    let launch_agents_dir = plist_path
        .parent()
        .ok_or_else(|| anyhow!("Invalid plist path"))?;

    if !launch_agents_dir.exists() {
        std::fs::create_dir_all(launch_agents_dir)
            .context("Failed to create LaunchAgents directory")?;

        if !json {
            println!("  {} Created LaunchAgents directory", "✓".green());
        }
    }

    // Step 3: Create log directory
    let log_dir = service::get_log_dir();
    if !log_dir.exists() {
        std::fs::create_dir_all(&log_dir).context("Failed to create log directory")?;
    }

    if !json {
        println!(
            "  {} Created log directory: {}",
            "✓".green(),
            log_dir.display()
        );
    }

    // Step 4: Generate plist from template
    let plist_content = generate_plist(&binary_path)?;

    // Step 5: Write plist file
    std::fs::write(&plist_path, plist_content).context("Failed to write plist file")?;

    if !json {
        println!(
            "  {} Installed service plist: {}",
            "✓".green(),
            plist_path.display()
        );
    }

    // Step 6: Load the service (this enables it)
    let output = std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to load service: {}", stderr);
    }

    if json {
        let result = serde_json::json!({
            "status": "success",
            "plist_path": plist_path,
            "binary_path": binary_path,
            "enabled": true
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "\n{}",
            "✓ Conductor service installed successfully".green().bold()
        );
        println!("  Binary:  {}", binary_path.display());
        println!("  Plist:   {}", plist_path.display());
        println!("  Status:  {}", "Enabled (will start on login)".green());
        println!("\nUse 'conductorctl start' to start the service now.");
    }

    Ok(())
}

/// Install daemon binary to /usr/local/bin
pub(crate) fn install_daemon_binary(json: bool) -> Result<PathBuf> {
    // Find the source binary (prefer release, fallback to debug)
    let cargo_target_dir =
        std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".to_string());

    let source_binary = [
        PathBuf::from(&cargo_target_dir).join("release").join(service::DAEMON_BINARY_NAME),
        PathBuf::from(&cargo_target_dir).join("debug").join(service::DAEMON_BINARY_NAME),
    ]
    .into_iter()
    .find(|p| p.exists())
    .ok_or_else(|| anyhow!(
        "Could not find daemon binary. Build it first with 'cargo build --release --bin conductor'"
    ))?;

    let dest_binary = service::get_binary_install_path();

    // Copy binary
    std::fs::copy(&source_binary, &dest_binary)
        .context("Failed to copy binary. You may need sudo permissions.")?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&dest_binary, perms)
            .context("Failed to set binary permissions")?;
    }

    if !json {
        println!(
            "  {} Installed binary: {}",
            "✓".green(),
            dest_binary.display()
        );
    }

    Ok(dest_binary)
}

/// Find daemon binary in common locations.
///
/// Returns an absolute path (canonicalized) so it works correctly in the
/// launchd plist regardless of WorkingDirectory setting.
pub(crate) fn find_daemon_binary() -> Result<PathBuf> {
    let candidates = vec![
        service::get_binary_install_path(),
        PathBuf::from("target/release").join(service::DAEMON_BINARY_NAME),
        PathBuf::from("target/debug").join(service::DAEMON_BINARY_NAME),
    ];

    candidates.into_iter()
        .find(|p| p.exists())
        .map(|p| p.canonicalize().unwrap_or(p))
        .ok_or_else(|| anyhow!(
            "Could not find daemon binary. Use --install-binary to install it, or build it with 'cargo build --release --bin conductor'"
        ))
}

/// Generate plist content from template
pub(crate) fn generate_plist(binary_path: &Path) -> Result<String> {
    let username = std::env::var("USER").context("Could not determine current username")?;

    // Load template or use embedded default
    let template = if let Some(template_path) = service::get_template_plist_path() {
        std::fs::read_to_string(&template_path).context("Failed to read plist template")?
    } else {
        // Embedded template
        include_str!("../../../../conductor-daemon/launchd/media.monstrous.conductor.plist")
            .to_string()
    };

    // Replace placeholders
    let plist = template
        .replace("/usr/local/bin/conductor", &binary_path.to_string_lossy())
        .replace("/Users/USERNAME", &format!("/Users/{}", username));

    Ok(plist)
}

/// Uninstall Conductor service
pub(crate) fn handle_uninstall(remove_binary: bool, remove_logs: bool, json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    if !is_service_installed() {
        if json {
            let result = serde_json::json!({
                "status": "error",
                "error": "Service not installed"
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            eprintln!("{}", "Service not installed".yellow());
        }
        return Ok(());
    }

    if !json {
        println!("{}", "Uninstalling Conductor service...".bold());
    }

    let plist_path = service::get_plist_path();

    // Step 1: Unload service
    let output = std::process::Command::new("launchctl")
        .args(["unload", "-w", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Don't fail if already unloaded
        if !stderr.contains("Could not find specified service") {
            eprintln!("Warning: {}", stderr);
        }
    }

    if !json {
        println!("  {} Stopped service", "✓".green());
    }

    // Step 2: Remove plist
    std::fs::remove_file(&plist_path).context("Failed to remove plist file")?;

    if !json {
        println!("  {} Removed plist: {}", "✓".green(), plist_path.display());
    }

    // Step 3: Remove binary if requested
    if remove_binary {
        let binary_path = service::get_binary_install_path();
        if binary_path.exists() {
            std::fs::remove_file(&binary_path)
                .context("Failed to remove binary. You may need sudo permissions.")?;

            if !json {
                println!(
                    "  {} Removed binary: {}",
                    "✓".green(),
                    binary_path.display()
                );
            }
        }
    }

    // Step 4: Remove logs if requested
    if remove_logs {
        let log_dir = service::get_log_dir();
        for log_file in ["conductor.log", "conductor.error.log"] {
            let log_path = log_dir.join(log_file);
            if log_path.exists() {
                std::fs::remove_file(&log_path).context("Failed to remove log file")?;
            }
        }

        if !json {
            println!("  {} Removed log files", "✓".green());
        }
    }

    if json {
        let result = serde_json::json!({
            "status": "success",
            "removed_plist": true,
            "removed_binary": remove_binary,
            "removed_logs": remove_logs
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "\n{}",
            "✓ Conductor service uninstalled successfully"
                .green()
                .bold()
        );
    }

    Ok(())
}

/// Start the daemon service
pub(crate) async fn handle_start(wait_secs: u64, json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    if !is_service_installed() {
        bail!("Service not installed. Run 'conductorctl install' first.");
    }

    // Check if already running
    if is_daemon_running().await {
        if json {
            let result = serde_json::json!({
                "status": "already_running"
            });
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("{}", "Daemon is already running".yellow());
        }
        return Ok(());
    }

    if !json {
        println!("Starting Conductor service...");
    }

    let plist_path = service::get_plist_path();

    // Load the service
    let output = std::process::Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "already loaded" errors
        if !stderr.contains("Already loaded") {
            bail!("Failed to start service: {}", stderr);
        }
    }

    // Wait for daemon to be ready
    if wait_secs > 0 {
        if !json {
            print!("Waiting for daemon to be ready");
            std::io::Write::flush(&mut std::io::stdout())?;
        }

        let start = std::time::Instant::now();
        let timeout = Duration::from_secs(wait_secs);

        while start.elapsed() < timeout {
            tokio::time::sleep(Duration::from_millis(500)).await;

            if is_daemon_running().await {
                if !json {
                    println!(" {}", "✓".green());
                }
                break;
            }

            if !json {
                print!(".");
                std::io::Write::flush(&mut std::io::stdout())?;
            }
        }

        if !is_daemon_running().await {
            if !json {
                println!(" {}", "✗".red());
            }
            bail!(
                "Daemon did not start within {} seconds. Check logs for errors.",
                wait_secs
            );
        }
    }

    if json {
        let result = serde_json::json!({
            "status": "started",
            "ready": is_daemon_running().await
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}", "✓ Service started successfully".green().bold());
    }

    Ok(())
}

/// Stop the daemon service
pub(crate) async fn handle_stop_service(force: bool, json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    if !is_service_installed() {
        bail!("Service not installed");
    }

    if !json {
        println!("Stopping Conductor service...");
    }

    // Try graceful shutdown first unless force is specified
    if !force && is_daemon_running().await {
        if !json {
            println!("  Attempting graceful shutdown via IPC...");
        }

        if let Ok(socket_path) = get_socket_path()
            && let Ok(mut client) = IpcClient::new(socket_path.to_string_lossy().to_string()).await
        {
            let _ = client.send_command(IpcCommand::Stop, Value::Null).await;

            // Wait briefly for graceful shutdown
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    // Unload the service
    let plist_path = service::get_plist_path();
    let output = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "not loaded" errors
        if !stderr.contains("Could not find specified service") {
            eprintln!("Warning: {}", stderr);
        }
    }

    // Verify stopped
    if is_daemon_running().await {
        bail!("Service may still be running. Check 'ps aux | grep conductor'");
    }

    if json {
        let result = serde_json::json!({
            "status": "stopped"
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}", "✓ Service stopped successfully".green().bold());
    }

    Ok(())
}

/// Restart the daemon service
pub(crate) async fn handle_restart(wait_secs: u64, json: bool) -> Result<()> {
    if !json {
        println!("{}", "Restarting Conductor service...".bold());
    }

    // Stop
    handle_stop_service(false, json).await?;

    // Wait a moment
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Start
    handle_start(wait_secs, json).await?;

    Ok(())
}

/// Enable auto-start on login
pub(crate) fn handle_enable(json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    if !is_service_installed() {
        bail!("Service not installed. Run 'conductorctl install' first.");
    }

    let plist_path = service::get_plist_path();

    // Load with -w flag enables auto-start
    let output = std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "already loaded" errors
        if !stderr.contains("Already loaded") {
            bail!("Failed to enable service: {}", stderr);
        }
    }

    if json {
        let result = serde_json::json!({
            "status": "enabled"
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "{}",
            "✓ Service enabled (will start on login)".green().bold()
        );
    }

    Ok(())
}

/// Disable auto-start on login
pub(crate) fn handle_disable(json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    if !is_service_installed() {
        bail!("Service not installed");
    }

    let plist_path = service::get_plist_path();

    // Unload with -w flag disables auto-start
    let output = std::process::Command::new("launchctl")
        .args(["unload", "-w", &plist_path.to_string_lossy()])
        .output()
        .context("Failed to execute launchctl")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "not loaded" errors
        if !stderr.contains("Could not find specified service") {
            eprintln!("Warning: {}", stderr);
        }
    }

    if json {
        let result = serde_json::json!({
            "status": "disabled"
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "{}",
            "✓ Service disabled (will not start on login)"
                .green()
                .bold()
        );
    }

    Ok(())
}

/// Show service installation and running status
pub(crate) fn handle_service_status(json: bool) -> Result<()> {
    if !is_macos() {
        bail!("Service management is currently only supported on macOS");
    }

    let installed = is_service_installed();
    let plist_path = service::get_plist_path();
    let binary_path = service::get_binary_install_path();
    let binary_exists = binary_path.exists();

    // Check if service is loaded with launchctl
    let list_output = std::process::Command::new("launchctl")
        .args(["list", service::SERVICE_LABEL])
        .output()
        .context("Failed to execute launchctl")?;

    let loaded = list_output.status.success();

    if json {
        let result = serde_json::json!({
            "installed": installed,
            "plist_path": plist_path,
            "binary_exists": binary_exists,
            "binary_path": binary_path,
            "loaded": loaded,
            "service_label": service::SERVICE_LABEL
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}", "Conductor Service Status".bold().cyan());
        println!("{}", "─".repeat(50));

        let status = if installed && loaded {
            "Installed and Loaded".green()
        } else if installed {
            "Installed but Not Loaded".yellow()
        } else {
            "Not Installed".red()
        };

        println!("Status:          {}", status);
        println!("Service Label:   {}", service::SERVICE_LABEL);
        println!(
            "Plist:           {} {}",
            plist_path.display(),
            if installed {
                "✓".green()
            } else {
                "✗".red()
            }
        );
        println!(
            "Binary:          {} {}",
            binary_path.display(),
            if binary_exists {
                "✓".green()
            } else {
                "✗".red()
            }
        );

        if loaded {
            println!("\n{}", "Service is loaded (enabled)".green());
        } else if installed {
            println!(
                "\n{}",
                "Service is not loaded. Use 'conductorctl enable' or 'conductorctl start'."
                    .yellow()
            );
        } else {
            println!(
                "\n{}",
                "Service is not installed. Use 'conductorctl install'.".yellow()
            );
        }
    }

    Ok(())
}
