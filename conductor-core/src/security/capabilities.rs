// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! Capability vocabulary for ADR-027 D3 — sub-pieces (1/N) and (2/N).
//!
//! This module hosts:
//!
//! - **(1/N)** the closed-vocabulary [`Capability`] enum, supporting
//!   [`InterpreterFamily`] and [`PathScope`] enums, and the
//!   [`CapabilitySet`] alias.
//! - **(2/N)** the per-`ActionConfig`-variant mapping function
//!   [`capabilities_for_action`] (per spec §3.1, "Derive required
//!   capabilities from an `ActionConfig`"), implemented as an
//!   exhaustive match (no `_` arm) so adding a new variant fails
//!   compilation here until capabilities are declared deliberately.
//!
//! The runtime wrapper-resolution + interpreter-classification step
//! (per spec §3.2 — defeating the `/usr/bin/env python3 -c '…'` bypass
//! class, adding `InterpreterExec(family)` on top of `ShellExec`) lands
//! in (3/N). The Shell argv-form schema migration is tracked separately
//! as `#1037`, deliberately sequenced after Phase 1A per epic `#999`.
//!
//! D5's gate signature does not yet take `granted_capabilities:
//! CapabilitySet` — that gate-signature change lands together with D1
//! (peer-credential IPC auth) and the wiring sub-piece, to keep the
//! Phase 1A atomic-bundle invariant.

use crate::config::ActionConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

/// Closed capability vocabulary per ADR-027 spec §3.1.
///
/// Capabilities are **declared** by an action / tool's required permissions
/// and **granted** to a caller via [`CallerContext`]. The gate (D5) admits
/// the action only when the caller's granted set is a superset of the
/// action's required set. The flag flip that activates this enforcement
/// path is part of the Phase 1A bundle (D5 + D3 + D1 + wiring); until
/// then, the vocabulary is in tree but unused.
///
/// `#[non_exhaustive]` so adding new capabilities post-Phase-1A — e.g.
/// `OscOut`, `MouseSend`, `PluginExec` — does not break downstream
/// `match` arms.
///
/// **Variant constructors:**
///
/// - [`InterpreterExec`](Self::InterpreterExec) is keyed by
///   [`InterpreterFamily`] so the gate can grant `python` while denying
///   `bash`. The single biggest bypass of the v1.0 design was running
///   interpreters via wrapper binaries (`/usr/bin/env python3 -c '…'`);
///   the (3/N) wrapper-resolution + classification step (per spec §3.2)
///   walks the wrapper chain and classifies the *effective* binary
///   before the gate sees it.
/// - [`FsRead`](Self::FsRead) / [`FsWrite`](Self::FsWrite) are keyed by
///   [`PathScope`] so the gate can grant `PluginData` (a per-plugin
///   sandbox subdirectory) without granting `Home` or arbitrary
///   `Explicit(path)`.
///
/// [`CallerContext`]: https://github.com/monstrous-media/conductor/blob/main/conductor-daemon/src/security/gate.rs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Capability {
    /// Direct argv-form execution of a non-interpreter binary. Required by
    /// `Shell` actions whose resolved binary is not on the interpreter
    /// list — e.g. `/usr/bin/say`, `/usr/bin/afplay`, `/usr/bin/open`,
    /// `/bin/ls`. Note: `/usr/bin/osascript` is **not** an example here
    /// — it executes AppleScript via `-e`, so it is classified as
    /// `InterpreterExec(InterpreterFamily::Other)` once the (3/N)
    /// wrapper-resolution + classification step lands.
    ShellExec,

    /// Execution of an interpreter (python, bash, perl, ruby, node, sh,
    /// awk, lua, …). Strictly distinct from [`ShellExec`](Self::ShellExec)
    /// because interpreters can execute arbitrary code via `-c` / `-e`
    /// flags and bypass argv-array protections. Default: not granted.
    InterpreterExec(InterpreterFamily),

    /// Outbound network calls from a shell action (e.g. `curl`, `wget`,
    /// `nc`, `ssh`). Composed with [`ShellExec`](Self::ShellExec) — a
    /// shell action that hits the network requires both.
    ShellNetwork,

    /// Synthesised keyboard input. Required by `Keystroke` and `Text`
    /// actions.
    KeystrokeSend,

    /// Modifier-key combinations (`cmd+`, `ctrl+`, `alt+`). Composed
    /// with [`KeystrokeSend`](Self::KeystrokeSend) — a `Keystroke` with
    /// modifiers requires both. Separated because plain typing is a
    /// lower-blast-radius capability than e.g. `cmd+q` (quit anything).
    KeystrokeModifierCombo,

    /// Launching applications. Required by `Launch` actions and any
    /// action that resolves to `open -a` / `gtk-launch` / `xdg-open`.
    LaunchApp,

    /// Outbound MIDI traffic. Required by `SendMidi` and `MidiForward`
    /// actions.
    MidiOut,

    /// Outbound OSC (Open Sound Control) traffic over UDP. Required by
    /// `OscSend` actions. Distinct from [`MidiOut`](Self::MidiOut)
    /// because OSC traffic crosses the network stack (UDP packets to
    /// configurable host:port) and OSC clients on the LAN are a
    /// different blast radius from local MIDI ports.
    OscOut,

    /// Synthesised mouse input (button clicks, position-set). Required
    /// by `MouseClick` actions. Separated from
    /// [`KeystrokeSend`](Self::KeystrokeSend) because mouse and keyboard
    /// synthesis are independently observable in audit logs and a user
    /// might reasonably grant one without the other (e.g. accessibility
    /// macros that only need keyboard).
    MouseSend,

    /// Plugin invocation — WASM (sandboxed via wasmtime) or native
    /// (`.dylib` / `.so` / `.dll`). Required by `Plugin` actions. The
    /// per-plugin filesystem sandbox is a separate concern (ADR-027
    /// D10c, PR #1029, captured by [`FsRead`](Self::FsRead) /
    /// [`FsWrite`](Self::FsWrite) with [`PathScope::PluginData`]); this
    /// capability is the right *to invoke a plugin at all*.
    PluginExec,

    /// Read access to the active config (TOML, daemon state, plan
    /// previews). Required by MCP read-only tools.
    ConfigRead,

    /// Write access to the active config — Plan/Apply, mode mutations,
    /// mapping CRUD. Required by MCP `ConfigChange`-tier tools and by
    /// `ModeChange` actions that persist the active mode to disk.
    ConfigWrite,

    /// Read access to a path scope. Path scopes are coarse-grained
    /// (per-plugin sandbox vs `~` vs explicit) so granting plugin data
    /// access does not implicitly grant access to the user home
    /// directory.
    FsRead(PathScope),

    /// Write access to a path scope. Same scope vocabulary as
    /// [`FsRead`](Self::FsRead).
    FsWrite(PathScope),
}

/// Interpreter family classification (per spec §3.1).
///
/// Used by [`Capability::InterpreterExec`] so the gate can grant `python`
/// while denying `bash`. The resolution path that maps an effective
/// binary path to one of these variants — including wrapper-chain
/// unwinding for `env`, `xargs`, `sudo`, `nice`, etc. — lands in D3
/// (3/N) per spec §3.2.
///
/// `#[non_exhaustive]` so e.g. `Tcsh`, `Ksh`, `Mawk`, `RubyJit` can be
/// added later without breaking downstream `match` arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum InterpreterFamily {
    /// Python (CPython, PyPy, MicroPython — all treated identically).
    Python,
    /// Ruby (MRI, JRuby, TruffleRuby — all treated identically).
    Ruby,
    /// Perl 5 (and Perl 6 / Raku — both flagged the same way).
    Perl,
    /// Node.js / Deno / Bun — JavaScript runtimes that take `-e`.
    Node,
    /// GNU Bash.
    Bash,
    /// POSIX `sh` (typically dash on Debian, ash on Alpine).
    Sh,
    /// Z shell.
    Zsh,
    /// Friendly Interactive SHell.
    Fish,
    /// `awk` and `sed` — both take expressions on the command line and
    /// can read / write arbitrary files.
    AwkOrSed,
    /// Lua (5.x, LuaJIT).
    Lua,
    /// Tcl / `tclsh`.
    TclSh,
    /// PHP (`php -r`).
    Php,
    /// Anything classified as an interpreter via heuristic but not
    /// matching one of the named families. Forces the gate to use the
    /// most-restrictive policy when granting `Other`.
    Other,
}

/// Filesystem path scope per spec §3.1.
///
/// Scopes are coarse-grained on purpose: each variant carries a clear
/// blast-radius signal. Granting [`PluginData`](Self::PluginData) to an
/// action says "this can touch its own sandbox subdirectory and nothing
/// else"; granting [`Home`](Self::Home) says "this can touch anything in
/// `~`"; granting [`Explicit`](Self::Explicit) says "this can touch
/// exactly this absolute path".
///
/// `#[non_exhaustive]` so future scopes — `XdgConfig`, `XdgData`,
/// `Cwd`, `Tempdir` — slot in additively.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PathScope {
    /// Per-plugin sandbox subdirectory. ADR-027 D10c (#1029) wires the
    /// per-plugin filesystem scope; this variant names it for the
    /// capability vocabulary.
    PluginData,
    /// User home directory. Coarse but bounded — does **not** include
    /// system paths.
    Home,
    /// An explicit absolute path. The gate validates that the path is
    /// canonical and absolute before granting; relative / symlink-traversal
    /// paths are rejected.
    Explicit(PathBuf),
}

/// A set of capabilities, used for both action declarations and caller
/// grants. Type alias rather than a wrapper struct because the operations
/// the gate performs — superset checks, intersection, union for `Sequence`
/// actions — are exactly the standard [`HashSet`] API.
pub type CapabilitySet = HashSet<Capability>;

/// Derive the capabilities an [`ActionConfig`] requires (D3 spec §3.1).
///
/// **Schema-time** declaration only — the function inspects the config
/// shape, not runtime arguments. Two consequences:
///
/// 1. The Shell variant declares only [`Capability::ShellExec`]. Wrapper
///    resolution and interpreter classification (the `/usr/bin/env python3
///    -c '…'` bypass class) is a runtime step that lands in D3 (3/N) per
///    spec §3.2 and adds [`Capability::InterpreterExec`] on top.
/// 2. The function does **not** inspect file paths inside `Shell.command`
///    or `Plugin.params`, so it never declares [`Capability::FsRead`] /
///    [`Capability::FsWrite`]. That tracking lands in (3/N) once argv
///    parsing exists.
///
/// **Composition.** Recursive variants (`Sequence`, `Repeat`,
/// `Conditional`, `PcContextSwitch`, `CcContextSwitch`) return the
/// **union** of their inner actions' capability sets. The gate's
/// admission check then reduces to a single subset test against the
/// caller's granted set.
///
/// **No-op variant.** `Delay` is the only variant returning an empty
/// [`CapabilitySet`] — it's a control-flow primitive (sleep) with no
/// security-relevant side effects. `ModeChange` looked superficially
/// similar but actually persists `last_selected_mode` to the config
/// TOML via `EngineManager::persist_mode_change` (see
/// `conductor-daemon/src/daemon/engine_manager.rs:5877`), so it
/// requires [`Capability::ConfigWrite`].
///
/// **Exhaustive matching.** The implementation matches every
/// [`ActionConfig`] variant *without* a `_` arm, so adding a new variant
/// fails compilation here until the capability declaration is decided
/// deliberately. This is the spec §3.1 "totality" guarantee.
pub fn capabilities_for_action(action: &ActionConfig) -> CapabilitySet {
    use ActionConfig::*;

    match action {
        Keystroke { modifiers, .. } => {
            let mut set = CapabilitySet::new();
            set.insert(Capability::KeystrokeSend);
            if !modifiers.is_empty() {
                set.insert(Capability::KeystrokeModifierCombo);
            }
            set
        }
        Text { .. } => caps_singleton(Capability::KeystrokeSend),
        Launch { .. } => caps_singleton(Capability::LaunchApp),
        Shell { command, args, .. } => {
            // D3 3/N wiring (issue #1037 Phase 2): unwind wrappers,
            // classify the effective binary, and add
            // `InterpreterExec(family)` on top of the baseline
            // `ShellExec` when the resolved binary is a known
            // interpreter. Defeats the `/usr/bin/env python3 -c '…'`
            // bypass class — by the time the gate inspects this set,
            // both the wrapper-stripping and the interpreter
            // classification have already happened via the
            // `super::interpreters` module.
            let mut set = caps_singleton(Capability::ShellExec);
            if let Some(family) = resolved_interpreter_family(command, args.as_deref()) {
                set.insert(Capability::InterpreterExec(family));
            }
            set
        }
        MouseClick { .. } => caps_singleton(Capability::MouseSend),
        // VolumeControl spawns processes (osascript on macOS, pactl on
        // Linux). Cross-platform minimum is ShellExec; macOS-specific
        // InterpreterExec(Other) follows in (3/N).
        VolumeControl { .. } => caps_singleton(Capability::ShellExec),
        SendMidi { .. } | MidiForward { .. } => caps_singleton(Capability::MidiOut),
        OscSend { .. } => caps_singleton(Capability::OscOut),
        // OscForward re-sends an inbound OSC message to an OSC output endpoint
        // (ADR-039-A Slice 3, #2326) — same OscOut capability as OscSend.
        OscForward { .. } => caps_singleton(Capability::OscOut),
        // HidForward V1 forwards a gamepad event to a MIDI output (HidToMidi
        // only — HID→OSC/Art-Net stay route-only, rejected at config-load),
        // so it needs MidiOut, same as MidiForward (ADR-039-B #1762 step 4b).
        HidForward { .. } => caps_singleton(Capability::MidiOut),
        Plugin { .. } => caps_singleton(Capability::PluginExec),
        // ModeChange persists `last_selected_mode` to the config TOML
        // via `EngineManager::persist_mode_change` (see
        // `conductor-daemon/src/daemon/engine_manager.rs:5877`),
        // which patches the on-disk config and then atomically swaps
        // the `ArcSwap<Config>`. That's a config write — distinct
        // from the in-memory `ArcSwap<ModeState>` flip which is just
        // bookkeeping for the same persisted change.
        ModeChange { .. } => caps_singleton(Capability::ConfigWrite),

        // No-op variant — see function-level docs.
        Delay { .. } => CapabilitySet::new(),

        // Tap (ADR-038) only emits a templated message to the event
        // stream / trace — no keystroke, shell, MIDI, or network side
        // effect — so it requires no capability, like `Delay`.
        Tap { .. } => CapabilitySet::new(),

        // Recursive variants — union of inner action capabilities.
        Sequence { actions } => union_inner(actions.iter()),
        Repeat { action, .. } => capabilities_for_action(action),
        Conditional {
            then_action,
            else_action,
            ..
        } => {
            let mut set = capabilities_for_action(then_action);
            if let Some(else_action) = else_action {
                set.extend(capabilities_for_action(else_action));
            }
            set
        }
        PcContextSwitch {
            mappings, default, ..
        } => {
            let mut set = union_inner(mappings.values().map(|b| b.as_ref()));
            if let Some(default) = default {
                set.extend(capabilities_for_action(default));
            }
            set
        }
        CcContextSwitch {
            ranges, default, ..
        } => {
            let mut set = union_inner(ranges.iter().map(|r| r.action.as_ref()));
            if let Some(default) = default {
                set.extend(capabilities_for_action(default));
            }
            set
        }
    }
}

/// Build a single-element capability set without going through the
/// `[cap].into_iter().collect()` boilerplate at every leaf branch.
fn caps_singleton(cap: Capability) -> CapabilitySet {
    let mut set = CapabilitySet::new();
    set.insert(cap);
    set
}

/// Fold an iterator of inner actions into a single capability set —
/// equivalent to `actions.flat_map(capabilities_for_action).collect()`
/// but avoids a full intermediate `Vec` for the recursive composition.
fn union_inner<'a, I>(actions: I) -> CapabilitySet
where
    I: IntoIterator<Item = &'a ActionConfig>,
{
    let mut set = CapabilitySet::new();
    for action in actions {
        set.extend(capabilities_for_action(action));
    }
    set
}

/// Schema-time helper: given a Shell action's `command` + optional
/// argv-form `args`, return the resolved interpreter family (if any).
///
/// Wraps [`resolve_effective_executable`] + [`classify_interpreter`]
/// (from the `super::interpreters` module — 67 unit tests there cover
/// the wrapper-unwinding logic in detail) with the **fail-closed
/// convention** documented in that module: every [`ResolveError`]
/// variant collapses to `Some(InterpreterFamily::Other)`, the strictest
/// classification.
///
/// Returns `None` only when wrapper resolution succeeds AND the
/// resolved binary's basename is **not** on the interpreter list.
///
/// Legacy form (`args = None`): falls back to
/// `command.split_whitespace().next()` for argv[0] before delegating.
/// The quote-aware parser lives in `conductor-daemon::parse_command_line`
/// and can't be reached from `conductor-core`; configs with quoted
/// command wrappers like `"'/bin/sh' -c '…'"` degrade to "treat the
/// whole `command` as one token", which classifies harmlessly as
/// `NotInterpreter` and falls through to the metacharacter validator.
///
/// [`resolve_effective_executable`]: super::interpreters::resolve_effective_executable
/// [`classify_interpreter`]: super::interpreters::classify_interpreter
/// [`ResolveError`]: super::interpreters::ResolveError
pub fn resolved_interpreter_family(
    command: &str,
    args: Option<&[String]>,
) -> Option<InterpreterFamily> {
    use super::interpreters::{
        DEFAULT_MAX_WRAPPER_DEPTH, InterpreterClassification, classify_interpreter,
        resolve_effective_executable,
    };
    use std::path::Path;

    // Legacy form: derive argv[0] by simple whitespace split. Argv
    // form: command is already argv[0].
    let (program, arg_slice): (&str, Vec<String>) = match args {
        Some(a) => (command, a.to_vec()),
        None => {
            let mut iter = command.split_whitespace();
            let head = iter.next().unwrap_or("");
            let tail: Vec<String> = iter.map(String::from).collect();
            (head, tail)
        }
    };
    if program.is_empty() {
        return None;
    }
    match resolve_effective_executable(Path::new(program), &arg_slice, DEFAULT_MAX_WRAPPER_DEPTH) {
        Ok(resolved) => match classify_interpreter(&resolved.effective_path) {
            InterpreterClassification::NotInterpreter => None,
            InterpreterClassification::Interpreter(family) => Some(family),
        },
        // Fail-closed per the `ResolveError` convention. Anything we
        // can't statically resolve (depth blown, malformed argv,
        // missing program after a wrapper) is treated as the
        // strictest interpreter family so it can't smuggle past the
        // gate by being weird.
        Err(_) => Some(InterpreterFamily::Other),
    }
}

#[cfg(test)]
mod tests {
    //! Vocabulary-level tests. The `capabilities_for_action` mapping
    //! is tested end-to-end in
    //! `conductor-core/tests/capability_declared_actions_test.rs` —
    //! every leaf-variant mapping, all 5 recursive variants, and a
    //! handful of edge cases. These unit tests pin only the
    //! type-level shape (serde wire format, `HashSet` semantics,
    //! `Copy`/`Send + Sync` invariants).

    use super::*;

    #[test]
    fn hid_forward_requires_midi_out() {
        // ADR-039-B #1762 step 4b: HidForward V1 sends to a MIDI output, so it
        // needs MidiOut (same as MidiForward) regardless of transform fields.
        use crate::config::types::SignalTransform;
        let action = ActionConfig::HidForward {
            target: "synth".to_string(),
            transform: SignalTransform::HidToMidi {
                trigger_to_cc: std::collections::HashMap::new(),
                channel: 0,
            },
        };
        let caps = capabilities_for_action(&action);
        assert!(caps.contains(&Capability::MidiOut));
        assert_eq!(caps.len(), 1, "HidForward needs exactly MidiOut");
    }

    #[test]
    fn capability_serializes_unit_variant_as_pascal_case() {
        let json = serde_json::to_string(&Capability::ShellExec).unwrap();
        assert_eq!(json, "\"ShellExec\"");
    }

    #[test]
    fn capability_round_trips_through_json() {
        let cap = Capability::InterpreterExec(InterpreterFamily::Python);
        let serialised = serde_json::to_string(&cap).unwrap();
        let parsed: Capability = serde_json::from_str(&serialised).unwrap();
        assert_eq!(cap, parsed);
    }

    #[test]
    fn capability_round_trips_path_scope_explicit() {
        let cap = Capability::FsWrite(PathScope::Explicit(PathBuf::from("/tmp/conductor")));
        let serialised = serde_json::to_string(&cap).unwrap();
        let parsed: Capability = serde_json::from_str(&serialised).unwrap();
        assert_eq!(cap, parsed);
    }

    #[test]
    fn capability_set_is_a_hashset_of_capability() {
        // The gate performs superset / intersection / union over capability
        // sets; the alias must produce the standard HashSet API.
        let mut declared = CapabilitySet::new();
        declared.insert(Capability::ShellExec);
        declared.insert(Capability::ShellNetwork);

        let mut granted = CapabilitySet::new();
        granted.insert(Capability::ShellExec);
        granted.insert(Capability::ShellNetwork);
        granted.insert(Capability::KeystrokeSend);

        assert!(
            declared.is_subset(&granted),
            "all declared caps are granted"
        );
        assert!(!granted.is_subset(&declared), "granted has extra caps");
    }

    #[test]
    fn interpreter_family_is_copy_for_cheap_passing() {
        // Copy is structurally important — the gate evaluates per-action
        // capability sets in hot paths and passes families by value into
        // the (3/N) wrapper-resolution code. If a future variant breaks
        // Copy (e.g. by carrying a String name), this test fails at
        // build time and the trade-off is reviewed deliberately.
        fn assert_copy<T: Copy>() {}
        assert_copy::<InterpreterFamily>();
    }

    #[test]
    fn capability_types_are_send_sync_for_cross_thread_use() {
        // The gate types (and the Phase 1A audit / denial stream
        // consumers) cross tokio task boundaries, so the vocabulary
        // must be Send + Sync.
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<Capability>();
        assert_send_sync::<InterpreterFamily>();
        assert_send_sync::<PathScope>();
        assert_send_sync::<CapabilitySet>();
    }

    #[test]
    fn path_scope_explicit_round_trips_paths_with_spaces() {
        let cap = Capability::FsRead(PathScope::Explicit(PathBuf::from(
            "/Users/test/Library/Application Support/conductor/plugins",
        )));
        let serialised = serde_json::to_string(&cap).unwrap();
        let parsed: Capability = serde_json::from_str(&serialised).unwrap();
        assert_eq!(cap, parsed);
    }

    #[test]
    fn capability_distinguishes_interpreter_families() {
        // Python and Bash are both InterpreterExec but must hash and
        // compare distinctly — the gate grants per-family.
        let py = Capability::InterpreterExec(InterpreterFamily::Python);
        let bash = Capability::InterpreterExec(InterpreterFamily::Bash);
        assert_ne!(py, bash);

        let mut grants = CapabilitySet::new();
        grants.insert(py.clone());
        assert!(grants.contains(&py));
        assert!(!grants.contains(&bash));
    }
}
