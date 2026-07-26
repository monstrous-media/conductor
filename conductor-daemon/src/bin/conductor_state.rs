// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! conductor-state — dump the daemon's physical control-state store.
//!
//! Diagnostic CLI that connects to a running daemon, pulls the current
//! `PhysicalControlStateStore` snapshot via the `conductor_get_control_state`
//! MCP tool, and pretty-prints it. Complements the ADR-025 Phase 3.B
//! `via context` annotation in the GUI Events panel — lets users inspect
//! what state the daemon actually sees from the terminal.
//!
//! Read-only; mutation lives in `conductor_reset_control_state` (MCP).

// CLI binary: legitimate println!/eprintln! to stdout/stderr.
// See docs/epic-loop/rust-coverage.md Path A.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use colored::Colorize;
use conductor_daemon::{IpcClient, IpcCommand, ResponseStatus, get_socket_path};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(name = "conductor-state")]
#[command(about = "Dump the daemon's physical control-state store (ADR-025)", long_about = None)]
#[command(version)]
struct Cli {
    /// Filter to a single device alias
    #[arg(short, long)]
    device: Option<String>,

    /// Emit raw JSON instead of human-readable output
    #[arg(short, long)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let socket_path = get_socket_path()
        .context("Failed to determine IPC socket path")?
        .to_string_lossy()
        .to_string();
    let mut client = IpcClient::new(socket_path)
        .await
        .context("Failed to connect to daemon. Is the daemon running?")?;

    let mut args = json!({ "tool_name": "conductor_get_control_state" });
    if let Some(ref device) = cli.device {
        args["arguments"] = json!({ "device": device });
    }

    let response = client
        .send_command(IpcCommand::ExecuteMcpTool, args)
        .await
        .context("IPC call failed")?;

    // Daemon-level errors arrive in `response.error`, not `response.data`.
    // Surface code/message/details before attempting to unwrap the payload.
    if matches!(response.status, ResponseStatus::Error) {
        let (code, msg, details) = response
            .error
            .map(|e| (e.code, e.message, e.details))
            .unwrap_or_else(|| (0, "(no error details from daemon)".to_string(), None));
        match details {
            Some(d) => bail!("Daemon error [{code}]: {msg} — {d}"),
            None => bail!("Daemon error [{code}]: {msg}"),
        }
    }

    let data = response
        .data
        .ok_or_else(|| anyhow!("Daemon returned success with no data"))?;

    let body = extract_tool_json(&data)?;
    render(&body, cli.device.as_deref(), cli.json)
}

/// Pull the inner JSON payload out of the daemon's `ExecutionResult` envelope.
///
/// `ExecuteMcpTool` responds with a tagged `ExecutionResult`. Variants we
/// can hit from a ReadOnly tool:
///   - `Success { result }` — happy path; parse the inner `ToolCallResult`.
///   - `Logged { result, log_entry }` — shouldn't reach here but handled
///     symmetrically (Stateful tier wraps the same `result` shape).
///   - `Error { message }` — surface the message.
///   - `RateLimited { retry_after_secs, .. }` — surface retry hint.
///   - `PlanCreated` / `HardwareIoConfirmation` — not possible for ReadOnly,
///     but we name them explicitly so future variant additions fail loudly
///     rather than falling into a generic "missing text content" error.
///
/// Also accepts a bare `ToolCallResult` (`{content, isError}`) so
/// fixture-driven tests don't need to synthesise the full envelope.
fn extract_tool_json(data: &Value) -> Result<Value> {
    match data.get("type").and_then(|v| v.as_str()) {
        Some("Success") | Some("Logged") => {
            let result = data.get("result").ok_or_else(|| {
                anyhow!("ExecutionResult::{{Success,Logged}} missing 'result' field")
            })?;
            extract_from_tool_call_result(result)
        }
        Some("Error") => {
            let msg = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("(no message)");
            bail!("Tool reported error: {msg}")
        }
        Some("RateLimited") => {
            let retry = data
                .get("retry_after_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            bail!("Rate limited — retry in {retry}s")
        }
        Some(other) => bail!("Unexpected ExecutionResult variant for ReadOnly tool: {other}"),
        // No tagged envelope → treat as bare ToolCallResult (test fixtures).
        None => extract_from_tool_call_result(data),
    }
}

fn extract_from_tool_call_result(result: &Value) -> Result<Value> {
    if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
        let msg = first_text(result).unwrap_or("(no error message)");
        bail!("Tool reported error: {msg}");
    }
    let text = first_text(result).ok_or_else(|| {
        anyhow!(
            "ToolCallResult missing text content (got envelope: {})",
            result
        )
    })?;
    serde_json::from_str(text)
        .with_context(|| format!("Failed to parse tool payload as JSON: {text}"))
}

fn first_text(data: &Value) -> Option<&str> {
    data.get("content")?
        .as_array()?
        .iter()
        .find_map(|c| c.get("text").and_then(|v| v.as_str()))
}

fn render(body: &Value, filter: Option<&str>, as_json: bool) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(body)?);
        return Ok(());
    }

    let entries = body
        .get("entries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("Response missing 'entries' array"))?;

    let title = match filter {
        Some(d) => format!("Control state — device: {d}"),
        None => "Control state — all devices".to_string(),
    };
    println!("{}", title.bold().cyan());
    println!("{}", "─".repeat(56));

    if entries.is_empty() {
        let msg = match filter {
            Some(d) => format!(
                "(no entries for device '{d}' — check the alias, or re-stomp / re-press to observe state)"
            ),
            None => "(store is empty — re-stomp / re-press to observe state)".to_string(),
        };
        println!("{}", msg.dimmed());
        return Ok(());
    }

    // Group by device so output stays readable when multiple devices are active.
    let mut by_device: std::collections::BTreeMap<&str, Vec<&Value>> = Default::default();
    for e in entries {
        let device = e.get("device").and_then(|v| v.as_str()).unwrap_or("?");
        by_device.entry(device).or_default().push(e);
    }

    for (device, device_entries) in &by_device {
        println!("{} {}", "▸".cyan(), device.bold());
        for e in device_entries {
            println!("    {}", format_entry(e));
        }
    }

    println!();
    println!(
        "{} {} entr{}",
        "•".dimmed(),
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" }
    );
    Ok(())
}

/// Render one `{kind, device, channel, value, [cc|note]}` entry as a single line.
/// Kept pure so it's testable without an IPC round-trip.
fn format_entry(e: &Value) -> String {
    let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
    let channel = e.get("channel").and_then(|v| v.as_i64()).unwrap_or(-1);
    let value = e.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
    // Channels are stored 0-indexed internally; present 1-indexed to match MIDI spec.
    let ch_display = if channel < 0 {
        "?".to_string()
    } else {
        (channel + 1).to_string()
    };

    match kind {
        "program_change" => format!("ch{ch_display}  PC  {value}"),
        "control_change" => {
            let cc = e.get("cc").and_then(|v| v.as_i64()).unwrap_or(-1);
            format!("ch{ch_display}  CC  {cc:>3} = {value}")
        }
        "note_held" => {
            let note = e.get("note").and_then(|v| v.as_i64()).unwrap_or(-1);
            format!("ch{ch_display}  note {note:>3}  (velocity {value})")
        }
        "poly_aftertouch" => {
            let note = e.get("note").and_then(|v| v.as_i64()).unwrap_or(-1);
            format!("ch{ch_display}  poly-at note {note}  = {value}")
        }
        "channel_aftertouch" => format!("ch{ch_display}  channel-at  = {value}"),
        "pitch_bend" => format!("ch{ch_display}  pitch-bend  = {value}"),
        other => format!("ch{ch_display}  {other:<18} {value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tool_json_pulls_inner_payload() {
        let envelope = json!({
            "content": [{"type": "text", "text": "{\"entries\":[],\"total\":0}"}],
        });
        let body = extract_tool_json(&envelope).unwrap();
        assert_eq!(body["total"], 0);
    }

    #[test]
    fn extract_tool_json_reports_error_flag() {
        let envelope = json!({
            "content": [{"type": "text", "text": "Store not available"}],
            "isError": true,
        });
        let err = extract_tool_json(&envelope).unwrap_err().to_string();
        assert!(err.contains("Store not available"), "got: {err}");
    }

    /// The daemon wraps the `ToolCallResult` in an `ExecutionResult::Success`
    /// tagged variant: `{type: "Success", result: {...}}`. Make sure we look
    /// through that wrapper (regression for missed envelope unwrap).
    #[test]
    fn extract_tool_json_unwraps_execution_result_envelope() {
        let envelope = json!({
            "type": "Success",
            "result": {
                "content": [{"type": "text", "text": "{\"entries\":[],\"total\":0}"}],
            }
        });
        let body = extract_tool_json(&envelope).unwrap();
        assert_eq!(body["total"], 0);
    }

    #[test]
    fn extract_tool_json_reports_error_through_envelope() {
        let envelope = json!({
            "type": "Success",
            "result": {
                "content": [{"type": "text", "text": "kaboom"}],
                "isError": true,
            }
        });
        let err = extract_tool_json(&envelope).unwrap_err().to_string();
        assert!(err.contains("kaboom"), "got: {err}");
    }

    /// `ExecutionResult::Error { message }` — the ReadOnly tool failed
    /// before a ToolCallResult was produced (e.g., internal daemon error).
    /// Surface the message instead of complaining about missing content.
    #[test]
    fn extract_tool_json_handles_execution_error_variant() {
        let envelope = json!({
            "type": "Error",
            "message": "control-state store not initialised",
        });
        let err = extract_tool_json(&envelope).unwrap_err().to_string();
        assert!(
            err.contains("control-state store not initialised"),
            "got: {err}"
        );
    }

    /// `ExecutionResult::RateLimited` — readonly tier can still be rate-
    /// limited before execution. Surface the retry hint.
    #[test]
    fn extract_tool_json_handles_rate_limited_variant() {
        let envelope = json!({
            "type": "RateLimited",
            "tier": "ReadOnly",
            "current": 100,
            "limit": 60,
            "retry_after_secs": 5,
        });
        let err = extract_tool_json(&envelope).unwrap_err().to_string();
        assert!(err.contains("Rate limited"), "got: {err}");
        assert!(err.contains("5s"), "got: {err}");
    }

    /// Unknown future variant → clear error rather than "missing text".
    #[test]
    fn extract_tool_json_rejects_unknown_variant() {
        let envelope = json!({
            "type": "SomeFutureVariant",
        });
        let err = extract_tool_json(&envelope).unwrap_err().to_string();
        assert!(err.contains("SomeFutureVariant"), "got: {err}");
    }

    #[test]
    fn format_entry_program_change() {
        let e = json!({"kind": "program_change", "device": "fcb1010", "channel": 0, "value": 12});
        // Channel stored 0-indexed → displayed as 1-indexed.
        assert_eq!(format_entry(&e), "ch1  PC  12");
    }

    #[test]
    fn format_entry_control_change() {
        let e = json!({"kind": "control_change", "device": "fcb1010", "channel": 0, "cc": 7, "value": 50});
        assert_eq!(format_entry(&e), "ch1  CC    7 = 50");
    }

    #[test]
    fn format_entry_note_held() {
        let e =
            json!({"kind": "note_held", "device": "pad", "channel": 9, "note": 36, "value": 100});
        assert_eq!(format_entry(&e), "ch10  note  36  (velocity 100)");
    }

    #[test]
    fn format_entry_unknown_kind_falls_back() {
        let e = json!({"kind": "mystery", "channel": 0, "value": 7});
        assert_eq!(format_entry(&e), "ch1  mystery            7");
    }
}
