// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Free helper types and functions extracted from `engine_manager::mod`.
//!
//! These carry no `&self` and are kept module-free of `EngineManager` so the
//! provenance / error / path-lexical / rate-limit / memory logic can be
//! unit-tested without standing up an `EngineManager` (whose `::new` needs
//! Enigo and so is `#[ignore]`d on Linux).

use super::*;

/// The chord-detection window (ms) currently in effect (#2486): the Learn
/// window (`chord_learn_timeout_ms`) while MIDI Learn is active, else the normal
/// window (`chord_timeout_ms`). Single source of truth so the daemon's
/// `EventProcessor` chord window (`events_device`) and the GUI-facing pattern
/// event's displayed window (`monitor_capture`) always agree — replacing the
/// hardcoded `EventProcessor::new()` 50ms and the hardcoded `Some(100)` pattern
/// timeout that disagreed with both.
pub(crate) fn active_chord_window_ms(
    learn_active: bool,
    chord_timeout_ms: u64,
    chord_learn_timeout_ms: u64,
) -> u64 {
    if learn_active {
        chord_learn_timeout_ms
    } else {
        chord_timeout_ms
    }
}

/// Build the [`EventTimings`] currently in effect from `advanced_settings`
/// (#2490). Chord uses the Learn-aware window via [`active_chord_window_ms`];
/// double-tap and hold come straight from config. Single source so the daemon
/// applies the same values at processor creation (`events_device`) and on reload
/// (`reload::apply_prepared`) — closing the gap where `hold_threshold_ms` /
/// `double_tap_timeout_ms` were set in config (the "Long Press" slider) but never
/// reached the `EventProcessor`.
pub(crate) fn event_timings_from_config(
    learn_active: bool,
    advanced: &conductor_core::config::types::AdvancedSettings,
) -> conductor_core::event_processor::EventTimings {
    conductor_core::event_processor::EventTimings {
        chord_timeout: std::time::Duration::from_millis(active_chord_window_ms(
            learn_active,
            advanced.chord_timeout_ms,
            advanced.chord_learn_timeout_ms,
        )),
        double_tap_timeout: std::time::Duration::from_millis(advanced.double_tap_timeout_ms),
        hold_threshold: std::time::Duration::from_millis(advanced.hold_threshold_ms),
        short_press_threshold: std::time::Duration::from_millis(advanced.short_press_ms),
    }
}

/// Console monitor rate limiter (fixed-window, per-second).
///
/// Up to `2 * max_per_second` events may pass at a second boundary (e.g., N events
/// at 1.999s and N events at 2.000s). This is acceptable for console monitoring.
///
/// Uses `lock()` (not `try_lock()`) since this mutex is uncontended.
/// Recovers from mutex poisoning to keep the daemon alive.
pub(crate) struct MonitorRateLimiter {
    max_per_second: u32,
    state: std::sync::Mutex<WindowState>,
}

#[derive(Debug, Clone, Copy)]
struct WindowState {
    start_sec: u64,
    count: u32,
}

impl MonitorRateLimiter {
    pub fn new(max_per_second: u32) -> Self {
        Self {
            max_per_second,
            state: std::sync::Mutex::new(WindowState {
                start_sec: 0,
                count: 0,
            }),
        }
    }

    /// Returns true if the event should be allowed, false if it should be dropped.
    pub fn allow(&self, timestamp_ms: u64) -> bool {
        if self.max_per_second == 0 {
            return true; // Unlimited
        }
        let now_sec = timestamp_ms / 1000;
        // Recover from poisoning to keep the daemon alive (council review).
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.start_sec == now_sec {
            if state.count >= self.max_per_second {
                return false; // Drop — rate limit exceeded
            }
            state.count += 1;
        } else {
            *state = WindowState {
                start_sec: now_sec,
                count: 1,
            };
        }
        true
    }
}

/// Get process resident memory (RSS) in bytes (R920: track_memory).
/// Returns None if unavailable on the current platform.
#[cfg(target_os = "macos")]
pub(crate) fn get_resident_memory_bytes() -> Option<u64> {
    use std::mem;
    let mut info: libc::mach_task_basic_info_data_t = unsafe { mem::zeroed() };
    let mut count = (mem::size_of::<libc::mach_task_basic_info_data_t>()
        / mem::size_of::<libc::natural_t>()) as libc::mach_msg_type_number_t;
    let kr = unsafe {
        libc::task_info(
            #[allow(deprecated)]
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as libc::task_info_t,
            &mut count,
        )
    };
    if kr == libc::KERN_SUCCESS {
        Some(info.resident_size)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn get_resident_memory_bytes() -> Option<u64> {
    use std::sync::OnceLock;
    static PAGE_SIZE: OnceLock<Option<u64>> = OnceLock::new();
    let page_size = *PAGE_SIZE.get_or_init(|| {
        let raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if raw <= 0 { None } else { Some(raw as u64) }
    });
    let page_size = page_size?;
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    rss_pages.checked_mul(page_size)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn get_resident_memory_bytes() -> Option<u64> {
    None
}

/// Reason a SysEx probe cannot be sent at the resolution step. Lets
/// `run_probe_device_identity` distinguish "no output mapping" (a
/// configuration concern) from "input is disconnected right now" (a
/// runtime concern) so callers see the right `ProbeOutcome`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProbeResolveError {
    /// Either the port isn't in the bindings list, the binding isn't
    /// linked to a configured `[[devices]]` identity, or the alias has
    /// no entry in the device-output map. Maps to
    /// `ProbeOutcome::NoPairedOutput`.
    NoPairedOutput,
    /// Binding exists with a paired output, but the input device is
    /// currently disconnected — sending the SysEx Identity Request
    /// would just time out (`NoReply`) while burning the per-port
    /// rate-limit budget. Maps to `ProbeOutcome::SendFailed` with a
    /// human-readable reason so callers can suggest a re-probe after
    /// reconnect.
    InputDisconnected,
}

/// Resolve an input port name → paired SysEx-probe output port name.
///
/// Pure helper extracted from `EngineManager::run_probe_device_identity`
/// so the resolution logic can be unit-tested without an `InputManager`,
/// `ArcSwap`, or tokio runtime. The error variant lets the caller pick
/// the right `ProbeOutcome` (NoPairedOutput vs SendFailed) — the helper
/// itself stays free of probe-protocol types.
pub(crate) fn resolve_probe_output_port(
    port_name: &str,
    bindings: &[(DeviceId, String, bool, bool)],
    device_output_map: &HashMap<String, String>,
) -> std::result::Result<String, ProbeResolveError> {
    // Step 1: find a *configured* binding regardless of connection
    // state. We need the alias either way so we can distinguish
    // "no mapping at all" from "mapping exists, device offline".
    let Some((alias, connected)) = bindings
        .iter()
        .find(|(_, p, _, is_configured)| *is_configured && p == port_name)
        .map(|(device_id, _, connected, _)| (device_id.as_str().to_string(), *connected))
    else {
        return Err(ProbeResolveError::NoPairedOutput);
    };

    // Step 2: the alias must have a paired output. If not, there's
    // nothing to send to regardless of connection state.
    let Some(output) = device_output_map.get(&alias).cloned() else {
        return Err(ProbeResolveError::NoPairedOutput);
    };

    // Step 3: configuration is fine — alias + paired output exist.
    // The only remaining gate is whether the input is actually
    // connected; without that the reply path can't observe anything.
    if !connected {
        return Err(ProbeResolveError::InputDisconnected);
    }
    Ok(output)
}

// ── ADR-034 §D2 / D4.C IPC mutation-surface helpers (#1901) ──────────────
//
// Shared by the SaveConfig / ReloadFromDisk / ImportConfig / ConfigDriftStatus
// handlers. Kept as free functions (no `&self`) so the provenance / error /
// path-lexical logic is unit-testable without standing up an `EngineManager`
// (whose `::new` needs Enigo and so is `#[ignore]`d on Linux).

/// Build an error [`IpcResponse`] from an [`IpcErrorCode`] + message.
pub(crate) fn ipc_err(id: String, code: IpcErrorCode, message: impl Into<String>) -> IpcResponse {
    IpcResponse {
        id,
        status: ResponseStatus::Error,
        data: None,
        error: Some(ErrorDetails {
            code: code.as_u16(),
            message: message.into(),
            details: None,
        }),
    }
}

/// Map an IPC caller's pinned trust band to the audit [`Initiator`] for the
/// `Provenance` recorded by a config mutation (ADR-034 §D6 provenance table).
///
/// `Untrusted` / unpinned peers default to `Cli` — the broadest legitimate
/// raw-IPC client. LLM-initiated config changes take the `ApplyPlan` path
/// (which stamps `Initiator::Llm`), never these raw mutation commands, so an
/// untrusted peer reaching here is treated as an anonymous CLI caller.
pub(crate) fn provenance_for(
    caller_ctx: &Option<crate::security::CallerContext>,
    source: conductor_core::config::Source,
) -> conductor_core::config::Provenance {
    use conductor_core::config::Initiator;
    let initiator = match caller_ctx.as_ref().map(|c| c.trust_level) {
        Some(crate::security::TrustLevel::GuiTrusted) => Initiator::Gui,
        Some(crate::security::TrustLevel::CliTrusted) => Initiator::Cli,
        _ => Initiator::Cli,
    };
    conductor_core::config::Provenance {
        initiator,
        source,
        peer: None,
    }
}

/// Map a [`MutateError`](crate::daemon::live_config::MutateError) to an IPC error
/// `(code, message)`. Per ADR-034 §D2.1 a stale base_generation surfaces as
/// the dedicated `StaleBaseGeneration = 5002` so each caller (GUI / LLM / CLI)
/// can branch on it; validation/compile failures map to
/// `ConfigValidationFailed`; everything else is an internal fault.
pub(crate) fn mutate_error_to_ipc(e: &crate::daemon::live_config::MutateError) -> (u16, String) {
    use crate::daemon::live_config::MutateError;
    match e {
        MutateError::StaleBaseGeneration { current, supplied } => (
            IpcErrorCode::StaleBaseGeneration.as_u16(),
            format!("stale base_generation: current={current}, supplied={supplied}"),
        ),
        MutateError::InvalidOp(msg) | MutateError::MutatorAborted(msg) => {
            (IpcErrorCode::ConfigValidationFailed.as_u16(), msg.clone())
        }
        MutateError::CompileFailed(err) => (
            IpcErrorCode::ConfigValidationFailed.as_u16(),
            format!("rule compilation failed: {err}"),
        ),
        // ADR-035 §D5: operator-actionable, not an internal fault.
        MutateError::RestartRequired(msg) => (IpcErrorCode::InvalidRequest.as_u16(), msg.clone()),
        // ADR-034 §D8.2: outbox full → fail-closed, operator drains + resumes.
        MutateError::AuditUnavailable(msg) => {
            (IpcErrorCode::AuditUnavailable.as_u16(), msg.clone())
        }
        other => (IpcErrorCode::InternalError.as_u16(), format!("{other}")),
    }
}

/// Lexical pre-checks for a caller-supplied `ReloadFromDisk` / `ImportConfig`
/// path. The path must be absolute, contain no `..` component, and end in
/// `.toml`.
///
/// This is the §D2 *lexical* layer only. The full §D2.2 safe-walk hardening
/// (per-component `openat`/`openat2` with `O_NOFOLLOW` + `RESOLVE_BENEATH`,
/// allowlist-root capture, inode-ownership `fstat`) is child #1902's scope —
/// these checks must NOT be mistaken for symlink-traversal protection.
pub(crate) fn lexical_config_path_ok(path: &std::path::Path) -> std::result::Result<(), String> {
    use std::path::Component;
    if !path.is_absolute() {
        return Err(format!("path must be absolute, got: {}", path.display()));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path must not contain `..`".to_string());
    }
    if path.extension().and_then(|e| e.to_str()) != Some("toml") {
        return Err("path must end in .toml".to_string());
    }
    Ok(())
}

/// Disposition of an incoming event with respect to the post-mapping route
/// stage (ADR-038 §2). Pump-local — NOT a config type.
///
/// Drives the one behavioural gate ADR-038 introduces: post-mapping routes fire
/// on `NoMatch` or `LetThrough`, and are skipped on `Consumed`. It is kept as an
/// explicit three-way enum (carrying the winning mapping's id) rather than a
/// collapsed boolean so the cause — "nothing matched" vs "a mapping consented to
/// let-through" vs "a mapping consumed the event" — survives into the dispatch
/// trace (R2 P1; trace fidelity wired in Slice 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteDisposition {
    /// The rule engine returned no match.
    NoMatch,
    /// A mapping matched and set `let_through = true`: fire the action AND let
    /// the event continue to the route stage. Carries the winning mapping's
    /// positional id (`ActionEnvelope::mapping_id`) for the trace.
    LetThrough(Option<usize>),
    /// A mapping matched and consumed the event (`let_through = false`, the
    /// pre-ADR-038 swallow). Carries the winning mapping's id for the trace.
    Consumed(Option<usize>),
}

impl RouteDisposition {
    /// Classify a matched envelope (or its absence) into a disposition.
    ///
    /// Derived purely from the envelope's `let_through` flag — never from
    /// whether the action dispatch succeeded — which is what guarantees the
    /// route stage proceeds independently of the (fire-and-forget, ADR-015)
    /// action: a slow action does not gate the route, and a backpressure-dropped
    /// action still lets the route fire.
    pub(crate) fn from_envelope(envelope: Option<&ActionEnvelope>) -> Self {
        match envelope {
            None => RouteDisposition::NoMatch,
            Some(e) if e.let_through => RouteDisposition::LetThrough(e.mapping_id),
            Some(e) => RouteDisposition::Consumed(e.mapping_id),
        }
    }

    /// Whether post-mapping routes should fire for this disposition: yes on
    /// `NoMatch` and `LetThrough`, no on `Consumed`.
    pub(crate) fn allows_route(&self) -> bool {
        matches!(
            self,
            RouteDisposition::NoMatch | RouteDisposition::LetThrough(_)
        )
    }

    /// Trace fields derived from the disposition (ADR-038 §2): the matched
    /// mapping id (if any) and whether the event was let through. Never
    /// reconstructed from a bool — this is the single source the dispatch trace
    /// reads, so `NoMatch` and `LetThrough` route entries stay distinguishable.
    pub(crate) fn trace_fields(&self) -> (Option<usize>, bool) {
        match self {
            RouteDisposition::NoMatch => (None, false),
            RouteDisposition::Consumed(id) => (*id, false),
            RouteDisposition::LetThrough(id) => (*id, true),
        }
    }
}

/// Resolved I/O fields for a single device, used when building `DevicePortStatus`.
pub(crate) struct DeviceIoResolution {
    pub(crate) direction: DeviceDirection,
    pub(crate) output_port_name: Option<String>,
    pub(crate) output_connected: bool,
    pub(crate) output_auto_paired: bool,
}

/// Serialise an `ExecutionResult` to JSON for IPC responses, with a
/// guaranteed wire-shape-compatible fallback if the primary
/// serialisation fails.
///
/// The naive `serde_json::to_value(&result).unwrap_or(json!({"error": ...}))`
/// fallback is dangerous: the `{"error": ...}` blob doesn't match any
/// `ExecutionResult` variant, so the GUI's wire-format helpers
/// (e.g. `extract_probe_outcome_from_execution_result` for the
/// probe path) silently misparse and surface confusing errors.
///
/// On primary serialisation failure (effectively unreachable —
/// `ExecutionResult` is derived `Serialize` over owned types — but
/// guarded defensively for future-proofing), build an
/// `ExecutionResult::Error { message }` and serialise THAT. The
/// fallback's serialisation is provably infallible (single owned
/// `String`), so the `.expect()` is justified.
#[cfg(feature = "llm-executor")]
pub(crate) fn execution_result_to_value(
    result: &crate::daemon::llm::executor::ExecutionResult,
) -> serde_json::Value {
    match serde_json::to_value(result) {
        Ok(value) => value,
        Err(e) => {
            let fallback = crate::daemon::llm::executor::ExecutionResult::Error {
                message: format!("Failed to serialise execution result: {}", e),
            };
            serde_json::to_value(&fallback)
                .expect("ExecutionResult::Error with an owned String must serialise")
        }
    }
}

/// Resolve device I/O fields (direction, output port, availability, auto-paired) for a single device.
/// Used by both `connect_multi_device` and `process_hot_plug_check` to avoid duplicating
/// the direction-upgrade and output-availability logic.
pub(crate) fn resolve_device_io(
    alias: &str,
    is_configured: bool,
    endpoints: &[conductor_core::config::types::EndpointConfig],
    output_map: &std::collections::HashMap<
        String,
        crate::daemon::output_resolver::OutputResolution,
    >,
) -> DeviceIoResolution {
    use conductor_core::config::types::ConnectorDirection;
    if !is_configured {
        return DeviceIoResolution {
            direction: DeviceDirection::default(),
            output_port_name: None,
            output_connected: false,
            output_auto_paired: false,
        };
    }
    // ADR-035 Slice 9.5: direction comes from the unified endpoint set, not
    // `config.devices`. Map the endpoint's `ConnectorDirection` onto the
    // status-facing `DeviceDirection`.
    let dir = endpoints
        .iter()
        .find(|e| e.alias == alias)
        .map(|e| match e.direction {
            ConnectorDirection::Input => DeviceDirection::Input,
            ConnectorDirection::Output => DeviceDirection::Output,
            ConnectorDirection::Bidirectional => DeviceDirection::Bidirectional,
        })
        .unwrap_or_default();
    let (opn, oap) = match output_map.get(alias) {
        Some(res) => (Some(res.port_name.clone()), res.auto_paired),
        None => (None, false),
    };
    // ADR-021 GAP-B3: Upgrade direction when auto-pairing discovered an output
    let dir = if dir == DeviceDirection::Input && oap {
        DeviceDirection::Bidirectional
    } else {
        dir
    };
    DeviceIoResolution {
        // output_connected = output was resolved (build_output_map only
        // returns ports currently in the available output list, so resolved ≡ available)
        output_connected: opn.is_some(),
        direction: dir,
        output_port_name: opn,
        output_auto_paired: oap,
    }
}

/// Convert a PathBuf to a UTF-8 string, returning an error with context if conversion fails.
/// Added v4.14.0 (LLM Council feedback v4.13.3)
pub(crate) fn pathbuf_to_str_or_err<'a>(
    path: &'a std::path::PathBuf,
    context: &str,
) -> Result<&'a str> {
    path.to_str().ok_or_else(|| DaemonError::InvalidPath {
        context: format!("{}: {:?}", context, path),
    })
}

/// Replace the contents of an existing `ConnectorRegistry` with a fresh
/// build from the given config. Used by `EngineManager::reload_config()`
/// to keep the registry in sync with config changes (ADR-031 § 3.4 P1B).
///
/// Per-connector runtime state (`bound_port`, `connected`, `metrics`)
/// is reset on rebuild — same trade-off `device_output_map` already
/// accepts on every reload. Carry-over of bound ports across reloads
/// is a future refinement.
pub(crate) async fn rebuild_connector_registry(
    registry: &Arc<RwLock<crate::connector_registry::ConnectorRegistry>>,
    endpoints: &[conductor_core::config::types::EndpointConfig],
) {
    let fresh = crate::connector_registry::ConnectorRegistry::from_config(endpoints);
    *registry.write().await = fresh;
}

/// Extract and validate a plugin name from an IPC request, returning an error response on failure.
#[allow(clippy::result_large_err)]
pub(crate) fn extract_validated_plugin_name(
    request: &IpcRequest,
    id: &str,
) -> std::result::Result<String, IpcResponse> {
    let name = request
        .args
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if let Err(e) = crate::daemon::types::validate_plugin_name(&name) {
        return Err(IpcResponse {
            id: id.to_string(),
            status: ResponseStatus::Error,
            data: None,
            error: Some(ErrorDetails {
                code: IpcErrorCode::InvalidRequest.as_u16(),
                message: e,
                details: None,
            }),
        });
    }
    Ok(name)
}

/// Convert Path to &str inside spawn_blocking (where we can't use pathbuf_to_str_or_err's lifetime)
pub(crate) fn cfg_path_str(path: &std::path::Path) -> Result<&str> {
    path.to_str().ok_or_else(|| DaemonError::InvalidPath {
        context: format!("config_path: {:?}", path),
    })
}

/// Resolve the startup mode index using the proper fallback chain.
/// Added for Phase 3 of Issue #321 - Mode Fallback Chain on Startup
///
/// Fallback chain: last_selected_mode → default_mode → mode index 0 → global mappings only
///
/// Thin wrapper over the canonical [`Config::resolve_startup_mode`] in
/// conductor-core (#1567) — the same code the mode-management integration tests
/// exercise, so they can't drift from production. Kept as a free fn so the
/// existing call sites and unit tests in this module are unchanged.
pub(crate) fn resolve_startup_mode(config: &Config) -> usize {
    config.resolve_startup_mode()
}

/// Structured top-level diff between two configs-as-JSON (#2414, ADR-034 §D4.D).
///
/// Returns the sorted set of top-level keys whose values differ between `live`
/// (the daemon's in-memory authority) and `target` (the on-disk drift source or
/// an imported file) — e.g. `["endpoints", "routes"]`. Config-shape-agnostic
/// (compares whatever serde keys exist on `Config`), so adding a field needs no
/// change here. The full `live`/`target` trees are returned alongside this by
/// `GetConfigDiff` so the GUI can render the detailed diff; this summary is what
/// #2284's drift banner names ("endpoints and routes changed").
pub(crate) fn config_changed_sections(
    live: &serde_json::Value,
    target: &serde_json::Value,
) -> Vec<String> {
    use std::collections::BTreeSet;
    let empty = serde_json::Map::new();
    let live_obj = live.as_object().unwrap_or(&empty);
    let target_obj = target.as_object().unwrap_or(&empty);
    // Union of keys so a section present on only one side still registers.
    let keys: BTreeSet<&String> = live_obj.keys().chain(target_obj.keys()).collect();
    keys.into_iter()
        .filter(|k| live_obj.get(*k) != target_obj.get(*k))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{active_chord_window_ms, event_timings_from_config};

    #[test]
    fn active_chord_window_picks_learn_vs_normal() {
        // #2486: the one window both the EventProcessor and the pattern-event
        // display read — Learn window when Learn is active, normal window
        // otherwise. (50/150 are the respective config defaults.)
        assert_eq!(active_chord_window_ms(false, 50, 150), 50);
        assert_eq!(active_chord_window_ms(true, 50, 150), 150);
        // Honours non-default config values (the user's normal=500 / learn=1000).
        assert_eq!(active_chord_window_ms(false, 500, 1000), 500);
        assert_eq!(active_chord_window_ms(true, 500, 1000), 1000);
    }

    #[test]
    fn event_timings_from_config_maps_every_knob() {
        // #2490: the full EventProcessor timing surface from config — chord is
        // Learn-aware, double-tap + hold come straight from advanced_settings
        // (previously ignored by the daemon).
        use conductor_core::config::types::AdvancedSettings;
        use std::time::Duration;
        let adv = AdvancedSettings {
            chord_timeout_ms: 60,
            chord_learn_timeout_ms: 600,
            double_tap_timeout_ms: 250,
            hold_threshold_ms: 1500,
            short_press_ms: 175,
            ..AdvancedSettings::default()
        };

        // Normal mode → chord = chord_timeout_ms; double-tap + hold + short-press from config.
        let t = event_timings_from_config(false, &adv);
        assert_eq!(t.chord_timeout, Duration::from_millis(60));
        assert_eq!(t.double_tap_timeout, Duration::from_millis(250));
        assert_eq!(t.hold_threshold, Duration::from_millis(1500));
        assert_eq!(t.short_press_threshold, Duration::from_millis(175));

        // Learn mode → chord = chord_learn_timeout_ms; the rest unchanged.
        let t = event_timings_from_config(true, &adv);
        assert_eq!(t.chord_timeout, Duration::from_millis(600));
        assert_eq!(t.double_tap_timeout, Duration::from_millis(250));
        assert_eq!(t.hold_threshold, Duration::from_millis(1500));
        assert_eq!(t.short_press_threshold, Duration::from_millis(175));
    }
}
