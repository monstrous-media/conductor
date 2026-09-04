// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Bounded in-memory ring buffer of route-dispatch decisions
//! (ADR-036 D5 / ADR-036 §8 / Slice 9).
//!
//! Each time the event pump dispatches an event to one or more route
//! destinations, it records a [`DispatchTraceEntry`] here. The
//! `conductor_get_dispatch_trace` MCP tool reads the last N entries so an
//! LLM (or a developer) can answer "what did the router just do?" without
//! attaching a debugger. The buffer is in-memory only and cleared on
//! daemon restart by design — it is an observability aid, not durable
//! state.
//!
//! Capacity is fixed at [`DISPATCH_TRACE_CAPACITY`]; the oldest entry is
//! evicted when full (FIFO). Only events that actually routed somewhere
//! are recorded — the overwhelming majority of events match the rule
//! engine and never reach a route, so recording those too would flood
//! the ring with empty-destination noise and evict the useful entries.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default number of dispatch-decision entries retained in memory.
/// Per ADR-036 §8 ("Ring buffer holds last 1,000 traces in memory").
/// Overridable at runtime via `advanced_settings.trace_buffer_size`
/// (spec §10 Open Item #3) — see [`DispatchTraceRing::with_capacity`].
pub const DISPATCH_TRACE_CAPACITY: usize = 1000;

/// One recorded route-dispatch decision: an input event that was routed
/// to one or more destination connectors. All routes are post-mapping
/// (ADR-036 Phase 3 removed the `pre_mapping` phase).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DispatchTraceEntry {
    /// Unix-epoch milliseconds when the decision was recorded.
    pub timestamp_ms: u128,
    /// Source device id (binding alias) the event arrived on.
    pub device_id: String,
    /// Active mode at dispatch time.
    pub active_mode: String,
    /// Human-readable summary of the source MIDI event (status + data).
    pub event: String,
    /// Destination connector aliases the event was dispatched to.
    pub destinations: Vec<String>,
    /// ADR-038 §2: the positional id of the mapping that matched, or `None`
    /// when nothing matched (`RouteDisposition::NoMatch`). Populated from the
    /// pump's `RouteDisposition`, never reconstructed from a bool, so the trace
    /// names which mapping let the event through.
    #[serde(default)]
    pub matched_mapping: Option<usize>,
    /// ADR-038 §2: `true` when this route fired because the matched mapping set
    /// `let_through = true` (`RouteDisposition::LetThrough`); `false` when it
    /// fired because nothing matched (`NoMatch`). Routes never fire for a
    /// consuming mapping, so a recorded entry is always one of those two.
    #[serde(default)]
    pub let_through: bool,
}

/// Thread-safe, fixed-capacity FIFO ring of [`DispatchTraceEntry`].
///
/// Held behind an `Arc` and shared between the dispatch path (writer, in
/// `EngineManager::dispatch_route_outputs`) and the MCP executor (reader,
/// in `conductor_get_dispatch_trace`). Uses a `std::sync::Mutex` with a
/// tiny critical section (a `push_back` / a bounded clone) — never held
/// across an `.await`.
#[derive(Debug)]
pub struct DispatchTraceRing {
    inner: Mutex<VecDeque<DispatchTraceEntry>>,
    /// Maximum entries retained before the oldest is evicted. Set from
    /// `advanced_settings.trace_buffer_size` in production; defaults to
    /// [`DISPATCH_TRACE_CAPACITY`].
    capacity: usize,
}

impl Default for DispatchTraceRing {
    fn default() -> Self {
        Self::with_capacity(DISPATCH_TRACE_CAPACITY)
    }
}

impl DispatchTraceRing {
    /// Construct a ring with an explicit capacity (spec §10 Open Item #3).
    /// `capacity` is clamped to at least 1 — a zero-capacity ring could
    /// never retain a trace. Config validation already rejects `0`
    /// (`advanced_settings.trace_buffer_size`); the clamp is defence in
    /// depth for any non-validated caller.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            capacity: capacity.max(1),
        }
    }

    /// Record one dispatch decision, evicting the oldest entry if the
    /// ring is at [`Self::capacity`].
    pub fn record(&self, entry: DispatchTraceEntry) {
        let mut buf = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if buf.len() >= self.capacity {
            buf.pop_front();
        }
        buf.push_back(entry);
    }

    /// Return the most recent `n` entries (oldest first, newest last).
    /// `n` larger than the buffer simply returns everything; callers
    /// (e.g. the MCP tool) cap `n` to a sane request size.
    pub fn last(&self, n: usize) -> Vec<DispatchTraceEntry> {
        let buf = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let start = buf.len().saturating_sub(n);
        buf.iter().skip(start).cloned().collect()
    }

    /// Current number of buffered entries.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// True when no entries are buffered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Current Unix-epoch milliseconds, saturating to 0 before the epoch.
pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Summarise a raw MIDI message for a trace entry, e.g.
/// `"NoteOn ch0 note36 vel100"` / `"CC ch9 cc7 val100"` /
/// `"System 0xF8"`. Best-effort; never panics on short/odd input.
pub fn summarize_midi(raw: &[u8]) -> String {
    let Some(&status) = raw.first() else {
        return "<empty>".to_string();
    };
    if status >= 0xF0 {
        return format!("System {status:#04x}");
    }
    let channel = status & 0x0F;
    let d1 = raw.get(1).copied();
    let d2 = raw.get(2).copied();
    match status & 0xF0 {
        0x80 => format!("NoteOff ch{channel} note{} vel{}", opt(d1), opt(d2)),
        0x90 => {
            if d2 == Some(0) {
                format!("NoteOff ch{channel} note{} (vel0)", opt(d1))
            } else {
                format!("NoteOn ch{channel} note{} vel{}", opt(d1), opt(d2))
            }
        }
        0xA0 => format!("PolyAftertouch ch{channel} note{} val{}", opt(d1), opt(d2)),
        0xB0 => format!("CC ch{channel} cc{} val{}", opt(d1), opt(d2)),
        0xC0 => format!("ProgramChange ch{channel} prog{}", opt(d1)),
        0xD0 => format!("Aftertouch ch{channel} val{}", opt(d1)),
        0xE0 => format!("PitchBend ch{channel} lsb{} msb{}", opt(d1), opt(d2)),
        other => format!("Unknown {other:#04x} ch{channel}"),
    }
}

fn opt(b: Option<u8>) -> String {
    b.map(|v| v.to_string()).unwrap_or_else(|| "?".to_string())
}

/// Primary env var gating per-event stderr trace logging (ADR-037 D4 /
/// spec §8): `CONDUCTOR_TRACE_LOG=1`.
const TRACE_LOG_ENV: &str = "CONDUCTOR_TRACE_LOG";
/// Shorthand alias for [`TRACE_LOG_ENV`]; either enables logging.
const TRACE_LOG_ENV_ALIAS: &str = "CONDUCTOR_TRACE";

/// Pure predicate: does an env-var *value* enable trace logging?
///
/// Enabled for the truthy tokens `1` / `true` / `yes` / `on`
/// (case-insensitive, surrounding whitespace ignored). Everything else —
/// unset (`None`), empty, `0`, `false`, `no`, `off`, or any unrecognised
/// string — leaves it disabled. Kept separate from [`trace_log_enabled`]
/// so the truth table is unit-testable without mutating process env.
pub fn env_enables_trace_log(val: Option<&str>) -> bool {
    matches!(
        val.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Whether per-event stderr trace logging is enabled for this process.
///
/// Reads `CONDUCTOR_TRACE_LOG` (or the `CONDUCTOR_TRACE` alias) **once**
/// and caches the verdict: the gate is fixed at first call, so the hot
/// dispatch path pays only a single relaxed atomic load per event when
/// the var is unset (the common case). Set the env var before starting
/// the daemon.
pub fn trace_log_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let primary = std::env::var(TRACE_LOG_ENV).ok();
        let alias = std::env::var(TRACE_LOG_ENV_ALIAS).ok();
        env_enables_trace_log(primary.as_deref()) || env_enables_trace_log(alias.as_deref())
    })
}

/// Serialise one trace entry as a single newline-terminated JSON line to
/// `w`. Factored out of [`emit_stderr_trace`] so the wire shape is
/// unit-testable against an in-memory buffer.
fn write_trace_line<W: Write>(w: &mut W, entry: &DispatchTraceEntry) -> io::Result<()> {
    serde_json::to_writer(&mut *w, entry)?;
    w.write_all(b"\n")
}

/// Emit one structured-JSON trace line for `entry` to stderr, but only
/// when [`trace_log_enabled`] (ADR-037 D4 / spec §8). Off by default:
/// when the env gate is unset this is a single atomic load and an early
/// return, so there is no per-event overhead in the common case.
/// Best-effort — a write error (e.g. closed stderr) is swallowed;
/// tracing must never disrupt dispatch.
pub fn emit_stderr_trace(entry: &DispatchTraceEntry) {
    if !trace_log_enabled() {
        return;
    }
    let _ = write_trace_line(&mut io::stderr().lock(), entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(event: &str, dests: &[&str]) -> DispatchTraceEntry {
        DispatchTraceEntry {
            timestamp_ms: now_ms(),
            device_id: "pads".into(),
            active_mode: "Default".into(),
            event: event.into(),
            destinations: dests.iter().map(|s| s.to_string()).collect(),
            matched_mapping: None,
            let_through: false,
        }
    }

    #[test]
    fn record_and_last_return_newest_at_end() {
        let ring = DispatchTraceRing::default();
        assert!(ring.is_empty());
        ring.record(entry("e1", &["a"]));
        ring.record(entry("e2", &["b"]));
        ring.record(entry("e3", &["c"]));
        assert_eq!(ring.len(), 3);

        let last2 = ring.last(2);
        assert_eq!(last2.len(), 2);
        assert_eq!(last2[0].event, "e2");
        assert_eq!(last2[1].event, "e3", "newest entry is last");
    }

    #[test]
    fn last_more_than_buffered_returns_all() {
        let ring = DispatchTraceRing::default();
        ring.record(entry("only", &["a"]));
        assert_eq!(ring.last(256).len(), 1);
    }

    #[test]
    fn ring_evicts_oldest_at_capacity() {
        let ring = DispatchTraceRing::default();
        for i in 0..(DISPATCH_TRACE_CAPACITY + 50) {
            ring.record(entry(&format!("e{i}"), &["a"]));
        }
        assert_eq!(ring.len(), DISPATCH_TRACE_CAPACITY, "capped at capacity");
        // The 50 oldest were evicted; the first surviving entry is e50.
        let all = ring.last(DISPATCH_TRACE_CAPACITY);
        assert_eq!(all.first().unwrap().event, "e50");
        assert_eq!(
            all.last().unwrap().event,
            format!("e{}", DISPATCH_TRACE_CAPACITY + 49)
        );
    }

    #[test]
    fn with_capacity_evicts_at_configured_capacity() {
        let ring = DispatchTraceRing::with_capacity(3);
        for i in 0..5 {
            ring.record(entry(&format!("e{i}"), &["a"]));
        }
        assert_eq!(
            ring.len(),
            3,
            "capped at the configured capacity, not the default"
        );
        let all = ring.last(3);
        assert_eq!(all.first().unwrap().event, "e2", "two oldest evicted");
        assert_eq!(all.last().unwrap().event, "e4", "newest retained");
    }

    #[test]
    fn with_capacity_clamps_zero_to_one() {
        // Validation rejects 0; the ring clamps defensively so a stray
        // 0 can still retain the latest entry rather than nothing.
        let ring = DispatchTraceRing::with_capacity(0);
        ring.record(entry("first", &["a"]));
        ring.record(entry("second", &["a"]));
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.last(1)[0].event, "second");
    }

    #[test]
    fn default_uses_dispatch_trace_capacity() {
        let ring = DispatchTraceRing::default();
        for i in 0..(DISPATCH_TRACE_CAPACITY + 10) {
            ring.record(entry(&format!("e{i}"), &["a"]));
        }
        assert_eq!(ring.len(), DISPATCH_TRACE_CAPACITY);
    }

    #[test]
    fn env_gate_enables_only_truthy_tokens() {
        // Truthy: 1 / true / yes / on, case- and whitespace-insensitive.
        for v in [
            "1", "true", "TRUE", "True", "yes", "YES", "on", "ON", " 1 ", "  true  ",
        ] {
            assert!(env_enables_trace_log(Some(v)), "{v:?} should enable");
        }
        // Falsy / unrecognised: stays disabled (no surprise opt-in).
        for v in [
            "0", "false", "FALSE", "no", "off", "", "  ", "2", "enabled", "y", "garbage",
        ] {
            assert!(!env_enables_trace_log(Some(v)), "{v:?} should NOT enable");
        }
        assert!(!env_enables_trace_log(None), "unset should NOT enable");
    }

    #[test]
    fn write_trace_line_emits_one_json_line_with_fields() {
        let e = entry("NoteOn ch0 note36 vel100", &["absynth", "reaktor"]);
        let mut buf = Vec::new();
        write_trace_line(&mut buf, &e).unwrap();
        let s = String::from_utf8(buf).unwrap();

        // Exactly one newline-terminated line — one trace per event.
        assert!(s.ends_with('\n'), "must be newline-terminated");
        assert_eq!(s.matches('\n').count(), 1, "exactly one line");
        assert!(!s.trim_end().contains('\n'), "no embedded newline");

        // Valid JSON carrying the entry fields.
        let v: serde_json::Value = serde_json::from_str(s.trim_end()).unwrap();
        assert_eq!(v["event"], "NoteOn ch0 note36 vel100");
        assert_eq!(v["device_id"], "pads");
        assert_eq!(v["active_mode"], "Default");
        assert_eq!(v["destinations"][0], "absynth");
        assert_eq!(v["destinations"][1], "reaktor");
        assert!(v["timestamp_ms"].is_number(), "timestamp_ms is numeric");
    }

    #[test]
    fn summarize_midi_formats_common_messages() {
        assert_eq!(summarize_midi(&[0x90, 36, 100]), "NoteOn ch0 note36 vel100");
        assert_eq!(summarize_midi(&[0x90, 36, 0]), "NoteOff ch0 note36 (vel0)");
        assert_eq!(summarize_midi(&[0x80, 36, 0]), "NoteOff ch0 note36 vel0");
        assert_eq!(summarize_midi(&[0xB9, 7, 100]), "CC ch9 cc7 val100");
        assert_eq!(summarize_midi(&[0xF8]), "System 0xf8");
        assert_eq!(summarize_midi(&[]), "<empty>");
    }
}
