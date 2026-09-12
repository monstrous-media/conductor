// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Core daemon handlers: status, reload, shutdown, validate, ping, bindings, devices.

use super::*;

pub(crate) async fn handle_status(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::Status, Value::Null)
        .await
        .context("Failed to get daemon status")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        // Pretty print status
        if let Some(data) = response.data {
            println!("{}", "Conductor Daemon Status".bold().cyan());
            println!("{}", "─".repeat(50));

            if let Some(state) = data.get("state").and_then(|v| v.as_str()) {
                let state_colored = match state {
                    "Running" => state.green(),
                    "Reloading" => state.yellow(),
                    "Degraded" => state.red(),
                    _ => state.normal(),
                };
                println!("State:           {}", state_colored);
            }

            if let Some(mode) = data.get("current_mode").and_then(|v| v.as_str()) {
                println!("Current Mode:    {}", mode.cyan());
            }

            if let Some(config_path) = data.get("config_path").and_then(|v| v.as_str()) {
                println!("Config:          {}", config_path);
            }

            if let Some(uptime) = data.get("uptime_secs").and_then(|v| v.as_u64()) {
                println!("Uptime:          {}", format_duration(uptime));
            }

            if let Some(events) = data.get("events_processed").and_then(|v| v.as_u64()) {
                println!("Events:          {}", format_number(events));
            }

            if let Some(reloads) = data.get("config_reloads").and_then(|v| v.as_u64()) {
                println!("Config Reloads:  {}", reloads);
            }

            // Reload statistics
            if let Some(reload_stats) = data.get("reload_stats") {
                println!("\n{}", "Reload Performance".bold());
                println!("{}", "─".repeat(50));

                if let Some(last) = reload_stats.get("last_reload_ms").and_then(|v| v.as_u64()) {
                    println!("Last Reload:     {} ms", last);
                }
                if let Some(avg) = reload_stats.get("avg_reload_ms").and_then(|v| v.as_u64()) {
                    let grade = if avg < 20 {
                        "A".green()
                    } else if avg < 50 {
                        "B".yellow()
                    } else {
                        "C".red()
                    };
                    println!("Average:         {} ms (grade: {})", avg, grade);
                }
                if let Some(fastest) = reload_stats
                    .get("fastest_reload_ms")
                    .and_then(|v| v.as_u64())
                {
                    println!("Fastest:         {} ms", fastest);
                }
                if let Some(slowest) = reload_stats
                    .get("slowest_reload_ms")
                    .and_then(|v| v.as_u64())
                {
                    println!("Slowest:         {} ms", slowest);
                }
            }

            println!();
        }
    }

    Ok(())
}

pub(crate) async fn handle_reload(client: &mut IpcClient, json: bool) -> Result<()> {
    if !json {
        println!("Reloading configuration...");
    }

    let response = client
        .send_command(IpcCommand::Reload, Value::Null)
        .await
        .context("Failed to reload configuration")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = response.data {
        println!("{}", "✓ Configuration reloaded successfully".green());

        if let Some(duration) = data.get("reload_duration_ms").and_then(|v| v.as_u64()) {
            let grade = data
                .get("performance_grade")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let grade_colored = match grade {
                "A" => grade.green(),
                "B" => grade.yellow(),
                _ => grade.red(),
            };
            println!("Duration:  {} ms (grade: {})", duration, grade_colored);
        }

        if let Some(modes) = data.get("modes_loaded").and_then(|v| v.as_u64()) {
            println!("Modes:     {}", modes);
        }

        if let Some(mappings) = data.get("mappings_loaded").and_then(|v| v.as_u64()) {
            println!("Mappings:  {}", mappings);
        }
    }

    Ok(())
}

pub(crate) async fn handle_shutdown(client: &mut IpcClient, json: bool) -> Result<()> {
    if !json {
        println!("Stopping daemon...");
    }

    let response = client
        .send_command(IpcCommand::Stop, Value::Null)
        .await
        .context("Failed to stop daemon")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("{}", "✓ Daemon stopped successfully".green());
    }

    Ok(())
}

pub(crate) async fn handle_validate(
    client: &mut IpcClient,
    config_path: &Option<PathBuf>,
    json: bool,
) -> Result<()> {
    let args = if let Some(path) = config_path {
        serde_json::json!({ "path": path })
    } else {
        Value::Null
    };

    let response = client
        .send_command(IpcCommand::ValidateConfig, args)
        .await
        .context("Failed to validate configuration")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = response.data
        && let Some(valid) = data.get("valid").and_then(|v| v.as_bool())
    {
        if valid {
            println!("{}", "✓ Configuration is valid".green());

            if let Some(modes) = data.get("modes").and_then(|v| v.as_u64()) {
                println!("Modes:    {}", modes);
            }

            if let Some(mappings) = data.get("mappings").and_then(|v| v.as_u64()) {
                println!("Mappings: {}", mappings);
            }
        } else {
            println!("{}", "✗ Configuration is invalid".red());
        }
    }

    Ok(())
}

pub(crate) async fn handle_ping(client: &mut IpcClient, json: bool) -> Result<()> {
    let start = std::time::Instant::now();
    let response = client
        .send_command(IpcCommand::Ping, Value::Null)
        .await
        .context("Failed to ping daemon")?;

    let latency = start.elapsed();

    if json {
        let mut resp_json = serde_json::to_value(&response)?;
        if let Some(obj) = resp_json.as_object_mut() {
            obj.insert(
                "latency_ms".to_string(),
                serde_json::json!(latency.as_millis()),
            );
        }
        println!("{}", serde_json::to_string_pretty(&resp_json)?);
    } else {
        println!(
            "{} ({:.2} ms)",
            "✓ Daemon is responding".green(),
            latency.as_secs_f64() * 1000.0
        );
    }

    Ok(())
}

/// Print resolved bindings derived from the daemon's Status payload.
///
/// The data is already exposed via `IpcCommand::Status` →
/// `device_status.devices` — each entry's `is_configured` flag distinguishes
/// `[[bindings]]`-resolved ports from opportunistic listen-mode ports. This
/// handler just filters and pretty-prints; no daemon-side change required.
pub(crate) async fn handle_bindings(
    client: &mut IpcClient,
    json: bool,
    alias_filter: Option<String>,
    unbound_only: bool,
) -> Result<()> {
    let response = client
        .send_command(IpcCommand::Status, Value::Null)
        .await
        .context("Failed to get daemon status for bindings")?;

    let data = match response.data {
        Some(d) => d,
        None => {
            anyhow::bail!("Daemon Status returned no payload");
        }
    };

    let devices = data
        .get("device_status")
        .and_then(|d| d.get("devices"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let filtered: Vec<&Value> = filter_bindings(&devices, alias_filter.as_deref(), unbound_only);

    if json {
        // Keep both counts so consumers using `--alias` /
        // `--unbound-only` can validate against either the response set
        // (`bindings_count`) or the daemon's full enumeration
        // (`total_ports`). Misleading consumers with a single ambiguous
        // count was the original bug Copilot caught.
        let out = serde_json::json!({
            "listen_mode": data.get("listen_mode"),
            "total_ports": devices.len(),
            "bindings_count": filtered.len(),
            "bindings": filtered,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("{}", "Conductor Daemon Bindings".bold().cyan());
    println!("{}", "─".repeat(50));

    if let Some(mode) = data.get("listen_mode").and_then(|v| v.as_str()) {
        println!("Listen mode:    {}", mode);
    }

    // Single-pass partition replaces the two-filter idiom.
    let (configured, opportunistic): (Vec<&Value>, Vec<&Value>) =
        filtered.iter().copied().partition(|d| {
            d.get("is_configured")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        });

    println!(
        "Total ports:    {} ({} configured, {} opportunistic)",
        filtered.len(),
        configured.len(),
        opportunistic.len()
    );

    if !configured.is_empty() && !unbound_only {
        println!("\n{}", "Configured (resolved via [[bindings]])".bold());
        for dev in &configured {
            print_binding_row(dev);
        }
    }

    if !opportunistic.is_empty() {
        println!(
            "\n{}",
            "Opportunistic (listen_mode=All, no [[bindings]] match)".bold()
        );
        for dev in &opportunistic {
            print_binding_row(dev);
        }
    }

    if filtered.is_empty() {
        if alias_filter.is_some() {
            println!("\nNo binding matches the given --alias filter.");
        } else if unbound_only {
            println!("\nNo opportunistic ports.");
        } else {
            println!("\nNo bindings reported by the daemon.");
        }
    }

    println!();
    Ok(())
}

/// A pure filter — separated from `handle_bindings` so it's exercisable
/// by unit tests without spinning up an IPC client. `alias_filter` matches
/// the device's `device_id` field **exactly** — for configured ports this is
/// the `[[bindings]]` alias, but for opportunistic ports the daemon prefixes
/// the port name with `raw:` (e.g. `raw:IAC Driver Bus 1`), so the caller
/// needs the full string. `unbound_only` keeps only devices where
/// `is_configured` is `false`.
pub(crate) fn filter_bindings<'a>(
    devices: &'a [Value],
    alias_filter: Option<&str>,
    unbound_only: bool,
) -> Vec<&'a Value> {
    devices
        .iter()
        .filter(|dev| {
            if let Some(a) = alias_filter {
                let id = dev.get("device_id").and_then(|v| v.as_str()).unwrap_or("");
                if id != a {
                    return false;
                }
            }
            if unbound_only {
                let is_configured = dev
                    .get("is_configured")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_configured {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Pretty-print one row of the bindings table.
///
/// Output handles missing optional fields (output_port_name / direction)
/// gracefully so callers don't panic on daemon payload changes.
pub(crate) fn print_binding_row(dev: &Value) {
    let id = dev.get("device_id").and_then(|v| v.as_str()).unwrap_or("?");
    let port = dev.get("port_name").and_then(|v| v.as_str()).unwrap_or("?");
    let connected = dev
        .get("connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let direction = dev
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("Input");
    let output_port = dev.get("output_port_name").and_then(|v| v.as_str());
    let auto_paired = dev
        .get("output_auto_paired")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let dot = if connected {
        "●".green()
    } else {
        "○".dimmed()
    };
    let mut suffix = direction.to_string();
    if let Some(out) = output_port {
        suffix.push_str(&format!(", output={}", out));
        if auto_paired {
            suffix.push_str(" [auto-paired]");
        }
    }
    // Widened device_id column from 12 to 24 chars to fit
    // `raw:<port_name>` IDs without breaking the port-column alignment.
    // Real-world examples like `raw:IAC Driver Bus 1` are 20+ chars.
    println!("  {} {:<24} {:<32} ← {}", dot, id, port, suffix);
}

pub(crate) async fn handle_list_devices(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::ListDevices, Value::Null)
        .await
        .context("Failed to list input devices")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = response.data {
        println!("{}", "Available Input Devices".bold().cyan());
        println!("{}", "─".repeat(60));

        // Display MIDI devices
        if let Some(midi_devices) = data.get("midi_devices").and_then(|v| v.as_array()) {
            println!("\n{}", "MIDI Devices:".green().bold());
            if midi_devices.is_empty() {
                println!("  No MIDI devices found");
            } else {
                for device in midi_devices {
                    let port = device
                        .get("port_index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let name = device
                        .get("port_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let connected = device
                        .get("connected")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);

                    let status = if connected {
                        " (connected)".green()
                    } else {
                        "".normal()
                    };

                    println!("  [{}] {}{}", port, name, status);
                }
            }
        }

        // Display HID/gamepad devices
        if let Some(hid_devices) = data.get("hid_devices").and_then(|v| v.as_array()) {
            println!(
                "\n{}",
                "HID Devices (Gamepads/Controllers):".yellow().bold()
            );
            if hid_devices.is_empty() {
                println!("  No HID devices found");
            } else {
                for device in hid_devices {
                    let index = device.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                    let name = device
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let uuid = device.get("uuid").and_then(|v| v.as_str()).unwrap_or("");

                    println!("  [{}] {} (UUID: {})", index, name, uuid);
                }
            }
        }

        println!();
    }

    Ok(())
}

pub(crate) async fn handle_set_device(
    client: &mut IpcClient,
    port: usize,
    json: bool,
) -> Result<()> {
    let args = serde_json::json!({ "port": port });

    let response = client
        .send_command(IpcCommand::SetDevice, args)
        .await
        .context("Failed to set MIDI device")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = response.data {
        if let Some(message) = data.get("message").and_then(|v| v.as_str()) {
            println!("{}", message.green());
        } else {
            println!(
                "{}",
                format!("✓ Switched to device at port {}", port).green()
            );
        }
    }

    Ok(())
}

pub(crate) async fn handle_get_device(client: &mut IpcClient, json: bool) -> Result<()> {
    let response = client
        .send_command(IpcCommand::GetDevice, Value::Null)
        .await
        .context("Failed to get current MIDI device")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if let Some(data) = response.data
        && let Some(device) = data.get("device")
    {
        println!("{}", "Current MIDI Device".bold().cyan());
        println!("{}", "─".repeat(50));

        let connected = device
            .get("connected")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let status = if connected {
            "Connected".green()
        } else {
            "Disconnected".red()
        };

        println!("Status:     {}", status);

        if let Some(name) = device.get("name").and_then(|v| v.as_str()) {
            println!("Name:       {}", name);
        }

        if let Some(port) = device.get("port").and_then(|v| v.as_u64()) {
            println!("Port:       {}", port);
        }

        if let Some(last_event) = device.get("last_event_at").and_then(|v| v.as_u64()) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let secs_ago = now.saturating_sub(last_event);
            println!("Last Event: {} ago", format_duration(secs_ago));
        }

        println!();
    }

    Ok(())
}

/// Format duration in seconds to human-readable string
pub(crate) fn format_duration(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if days > 0 {
        format!("{}d {}h {}m {}s", days, hours, minutes, seconds)
    } else if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

/// Format large numbers with comma separators
pub(crate) fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let mut count = 0;

    for c in s.chars().rev() {
        if count == 3 {
            result.push(',');
            count = 0;
        }
        result.push(c);
        count += 1;
    }

    result.chars().rev().collect()
}

// ============================================================================
// Service Management Implementation
// ============================================================================
