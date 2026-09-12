// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Event monitoring, capture/export, and playback.

use super::*;

/// Handle `conductorctl events` — real-time event monitoring
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_events(
    client: &mut IpcClient,
    follow: bool,
    filter: conductor_daemon::EventFilter,
    format: &str,
    limit: usize,
    json_output: bool,
    output: Option<PathBuf>,
    duration: Option<String>,
    debounce_ms: Option<u64>,
    _profiling: bool,
) -> Result<()> {
    // Validate --duration BEFORE starting the monitor, so a typo like
    // `--duration nope` fails immediately instead of silently capturing the 2s
    // default. Omitted → 2s default (used only by the snapshot/export branch).
    let capture_secs = resolve_capture_secs(duration.as_deref())?;

    // Start event monitoring
    client
        .send_command(IpcCommand::StartEventMonitor, Value::Null)
        .await
        .context("Failed to start event monitor")?;

    // Set up Ctrl+C handler to stop monitoring
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag_clone = Arc::clone(&stop_flag);
    let _ = ctrlc::set_handler(move || {
        stop_flag_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    if !json_output && !follow {
        let unit = if capture_secs == 1 {
            "second"
        } else {
            "seconds"
        };
        println!(
            "{}",
            format!("Monitoring events for {capture_secs} {unit}...").bold()
        );
    } else if !json_output {
        println!("{}", "Monitoring events (Ctrl+C to stop)...".bold().cyan());
        println!("{}", "─".repeat(70));
    }

    let mut all_events: Vec<serde_json::Value> = Vec::new();

    // Wrap event collection in a block so StopEventMonitor is always sent
    let result: Result<()> = async {
        if follow {
            // Follow mode: poll continuously
            let mut last_displayed_ms: u64 = 0;
            loop {
                if stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }

                let response = client
                    .send_command(IpcCommand::GetMonitorEvents, Value::Null)
                    .await
                    .context("Failed to get monitor events")?;

                if let Some(events) = response
                    .data
                    .as_ref()
                    .and_then(|d| d.get("events"))
                    .and_then(|v| v.as_array())
                {
                    for event in events {
                        if !event_passes_filter(event, &filter) {
                            continue;
                        }
                        // Debounce: skip events too close together
                        if let Some(db) = debounce_ms {
                            let ts = event
                                .get("timestamp_ms")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            if ts > 0
                                && last_displayed_ms > 0
                                && ts.saturating_sub(last_displayed_ms) < db
                            {
                                continue;
                            }
                            if ts > 0 {
                                last_displayed_ms = ts;
                            }
                        }
                        if format == "json" {
                            println!("{}", serde_json::to_string(event).unwrap_or_default());
                        } else {
                            print_event_text(event);
                        }
                    }
                }

                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        } else {
            // Snapshot/export mode — capture window validated up front.
            tokio::time::sleep(Duration::from_secs(capture_secs)).await;

            let response = client
                .send_command(IpcCommand::GetMonitorEvents, Value::Null)
                .await
                .context("Failed to get monitor events")?;

            if let Some(events) = response
                .data
                .as_ref()
                .and_then(|d| d.get("events"))
                .and_then(|v| v.as_array())
            {
                let mut count = 0;
                for event in events {
                    // --limit bounds terminal display only; a file export (--output)
                    // must capture the whole window, otherwise a high-rate burst is
                    // silently truncated to the default 50 events.
                    if output.is_none() && count >= limit {
                        break;
                    }
                    if !event_passes_filter(event, &filter) {
                        continue;
                    }
                    if json_output || output.is_some() {
                        all_events.push(event.clone());
                    } else if format == "json" {
                        println!("{}", serde_json::to_string(event).unwrap_or_default());
                    } else {
                        print_event_text(event);
                    }
                    count += 1;
                }

                // Export to file if --output specified
                if let Some(ref output_path) = output {
                    export_events(&all_events, output_path)?;
                    println!(
                        "{} {} event(s) exported to {}",
                        "✓".green(),
                        all_events.len(),
                        output_path.display()
                    );
                } else if json_output {
                    println!("{}", serde_json::to_string_pretty(&all_events)?);
                } else if count == 0 {
                    println!("{}", "No events captured.".yellow());
                } else {
                    println!("\n{} event(s) captured.", count);
                }
            }
        }
        Ok(())
    }
    .await;

    // Always stop event monitoring, even on error
    let _ = client
        .send_command(IpcCommand::StopEventMonitor, Value::Null)
        .await;

    result
}

/// Check if a JSON event passes the filter. Returns false (skip) if:
/// - The event can't be deserialized and filters are active
/// - The event doesn't match the filter criteria
pub(crate) fn event_passes_filter(
    event: &serde_json::Value,
    filter: &conductor_daemon::EventFilter,
) -> bool {
    let has_active_filter = filter.event_type.is_some()
        || filter.channel.is_some()
        || filter.note_min.is_some()
        || filter.note_max.is_some()
        || filter.device_id.is_some()
        || filter.since_ms.is_some();

    match serde_json::from_value::<conductor_daemon::MonitorEvent>(event.clone()) {
        Ok(me) => filter.matches(&me),
        Err(_) => !has_active_filter, // Skip undeserializable events when filters are active
    }
}

/// Load config from default path for named filter lookup (R914)
pub(crate) fn load_config_for_filter() -> Result<Config> {
    let config_path = dirs::config_dir()
        .map(|d| d.join("conductor").join("config.toml"))
        .ok_or_else(|| anyhow!("Could not determine config directory"))?;
    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read config from {}", config_path.display()))?;
    let config: Config =
        toml::from_str(&content).context("Failed to parse config for named filter lookup")?;
    Ok(config)
}

// ── ADR-042 Phase B-early: network-listener approval CLI ──────────────────

/// Parse a duration string like "10s", "1m", "30" into seconds
pub(crate) fn parse_duration_str(s: Option<&str>) -> Option<u64> {
    let s = s?;
    let s = s.trim();
    if let Some(num) = s.strip_suffix('s') {
        num.parse().ok()
    } else if let Some(num) = s.strip_suffix('m') {
        num.parse::<u64>().ok().map(|m| m * 60)
    } else if let Some(num) = s.strip_suffix('h') {
        num.parse::<u64>().ok().map(|h| h * 3600)
    } else {
        s.parse().ok()
    }
}

/// Resolve the snapshot/export capture window (seconds) from `--duration`.
///
/// An OMITTED duration uses the 2-second default, but an explicit
/// unparsable value (e.g. `--duration 10seconds`) is an ERROR — matching the
/// `--since` behaviour — rather than silently falling back to 2 seconds and
/// producing an incomplete export without warning.
pub(crate) fn resolve_capture_secs(duration: Option<&str>) -> Result<u64> {
    match duration {
        None => Ok(2),
        Some(s) => parse_duration_str(Some(s))
            .ok_or_else(|| anyhow!("Invalid --duration value '{}'. Use e.g. 30s, 5m, 1h", s)),
    }
}

/// Export events to file (JSON or CSV based on extension)
/// CSV-safe string escaping: wraps in quotes if value contains comma, quote, or newline
pub(crate) fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Standard CSV column order for event export (shared between CLI and GUI)
pub(crate) const CSV_HEADER: &str =
    "timestamp_ms,event_type,device_id,channel,note,velocity,cc,value,button,axis";

pub(crate) fn format_event_csv_row(event: &serde_json::Value) -> String {
    let str_field =
        |key: &str| -> String { csv_escape(event.get(key).and_then(|v| v.as_str()).unwrap_or("")) };
    let num_field = |key: &str| -> String {
        event
            .get(key)
            .and_then(|v| v.as_u64())
            .map_or(String::new(), |v| v.to_string())
    };
    format!(
        "{},{},{},{},{},{},{},{},{},{}",
        num_field("timestamp_ms"),
        str_field("event_type"),
        str_field("device_id"),
        num_field("channel"),
        num_field("note"),
        num_field("velocity"),
        num_field("cc"),
        num_field("value"),
        num_field("button"),
        num_field("axis"),
    )
}

/// Export events to file (JSON or CSV based on extension)
pub(crate) fn export_events(events: &[serde_json::Value], path: &Path) -> Result<()> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("json");

    match ext {
        "csv" => {
            let mut wtr = std::io::BufWriter::new(
                std::fs::File::create(path)
                    .with_context(|| format!("Failed to create {}", path.display()))?,
            );
            use std::io::Write;
            writeln!(wtr, "{}", CSV_HEADER)?;
            for event in events {
                writeln!(wtr, "{}", format_event_csv_row(event))?;
            }
            Ok(())
        }
        _ => {
            // Default to JSON
            let json = serde_json::to_string_pretty(events)?;
            std::fs::write(path, json)
                .with_context(|| format!("Failed to write {}", path.display()))?;
            Ok(())
        }
    }
}

/// Format a monitor event as human-readable text
pub(crate) fn print_event_text(event: &serde_json::Value) {
    let ts = event
        .get("timestamp_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // Convert timestamp to HH:MM:SS.mmm
    let secs = (ts / 1000) % 86400;
    let ms = ts % 1000;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;

    let et = event
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let dev = event
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let et_upper = et.to_uppercase();
    let et_display = format!("{:<20}", et_upper);

    let mut details = String::new();

    if let Some(note) = event.get("note").and_then(|v| v.as_u64()) {
        details.push_str(&format!("note:{:<4}", note));
    }
    if let Some(vel) = event.get("velocity").and_then(|v| v.as_u64()) {
        details.push_str(&format!("vel:{:<4}", vel));
    }
    if let Some(cc) = event.get("cc").and_then(|v| v.as_u64()) {
        details.push_str(&format!("cc:{:<4}", cc));
    }
    if let Some(val) = event.get("value").and_then(|v| v.as_u64()) {
        details.push_str(&format!("val:{:<4}", val));
    }
    if let Some(btn) = event.get("button").and_then(|v| v.as_u64()) {
        details.push_str(&format!("btn:{:<4}", btn));
    }
    if let Some(axis) = event.get("axis").and_then(|v| v.as_u64()) {
        details.push_str(&format!("axis:{:<4}", axis));
    }
    if let Some(ch) = event.get("channel").and_then(|v| v.as_u64())
        && ch > 0
    {
        details.push_str(&format!("ch:{:<3}", ch));
    }

    let dev_str = if dev.is_empty() {
        String::new()
    } else {
        format!("dev:{}", dev)
    };

    // Profiling data (R921)
    let mut prof_str = String::new();
    if let Some(proc_us) = event.get("processing_us").and_then(|v| v.as_u64()) {
        prof_str.push_str(&format!("proc:{}us ", proc_us));
    }
    if let Some(mem) = event.get("memory_bytes").and_then(|v| v.as_u64()) {
        let mb = mem as f64 / (1024.0 * 1024.0);
        prof_str.push_str(&format!("mem:{:.1}MB ", mb));
    }

    let prof_display = if prof_str.is_empty() {
        String::new()
    } else {
        format!(" {}", prof_str.trim())
    };

    println!(
        "[{:02}:{:02}:{:02}.{:03}] {} {} {}{}",
        h, m, s, ms, et_display, details, dev_str, prof_display
    );
}

/// Replay recorded events from a JSON or CSV file (R910)
pub(crate) async fn handle_playback_events(
    file: &Path,
    speed: f64,
    format: &str,
    no_delay: bool,
) -> Result<()> {
    use std::io::BufRead;

    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("json");
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("Failed to read {}", file.display()))?;

    let events: Vec<serde_json::Value> = match ext {
        "csv" => {
            let reader = std::io::BufReader::new(content.as_bytes());
            let mut events = Vec::new();
            let mut lines = reader.lines();
            // Skip header
            let header = lines.next().transpose()?.unwrap_or_default();
            let fields: Vec<&str> = header.split(',').collect();

            for line in lines {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let values: Vec<&str> = line.split(',').collect();
                let mut event = serde_json::Map::new();
                for (i, field) in fields.iter().enumerate() {
                    if let Some(val) = values.get(i) {
                        let val = val.trim();
                        if val.is_empty() {
                            continue;
                        }
                        // Try parsing as number first
                        if let Ok(n) = val.parse::<u64>() {
                            event.insert(field.to_string(), serde_json::Value::from(n));
                        } else {
                            event.insert(
                                field.to_string(),
                                serde_json::Value::from(val.to_string()),
                            );
                        }
                    }
                }
                events.push(serde_json::Value::Object(event));
            }
            events
        }
        _ => {
            // JSON — could be array or newline-delimited
            if content.trim_start().starts_with('[') {
                serde_json::from_str(&content)
                    .with_context(|| "Failed to parse JSON events array")?
            } else {
                content
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(serde_json::from_str)
                    .collect::<Result<Vec<_>, _>>()
                    .with_context(|| "Failed to parse JSON lines")?
            }
        }
    };

    if events.is_empty() {
        println!("No events found in {}", file.display());
        return Ok(());
    }

    let speed = if speed <= 0.0 { 1.0 } else { speed };

    println!(
        "{}",
        format!(
            "Replaying {} events from {} (speed: {:.1}x)...",
            events.len(),
            file.display(),
            speed
        )
        .bold()
        .cyan()
    );

    let mut prev_ts: Option<u64> = None;
    for event in &events {
        // Timing delay between events
        if !no_delay && let Some(prev) = prev_ts {
            let ts = event
                .get("timestamp_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(prev);
            let delta = ts.saturating_sub(prev);
            if delta > 0 {
                let delay_ms = (delta as f64 / speed) as u64;
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }

        // Only update prev_ts if this event has a timestamp, to avoid
        // erasing the time gap for subsequent events.
        if let Some(ts) = event.get("timestamp_ms").and_then(|v| v.as_u64()) {
            prev_ts = Some(ts);
        }

        match format {
            "json" => println!("{}", serde_json::to_string(event)?),
            _ => print_event_text(event),
        }
    }

    println!(
        "{}",
        format!("Playback complete — {} events replayed.", events.len())
            .bold()
            .green()
    );

    Ok(())
}
