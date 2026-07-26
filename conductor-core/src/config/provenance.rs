// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Mutation provenance per ADR-034 §D6.
//!
//! Every config mutation that flows through `LiveConfig::mutate()`
//! (ADR-034 §D1) carries a [`Provenance`] tagging *who* initiated
//! ([`Initiator`]), *what was applied* ([`Source`]), and the
//! authenticated peer identity ([`PeerIdentity`]) when one exists.
//!
//! The split between `Initiator` and `Source` is deliberate: a single
//! initiator (e.g. CLI) can perform multiple source-distinct ops
//! (in-memory edit, disk import, rollback). Conflating these two
//! dimensions in a flat enum (R3 had this) made audit queries weaker.
//!
//! `PeerIdentity::Linux::exe_path` is **audit display only** — never
//! an authorisation input (R5-M8 fix per Opus R6 review). Authorization
//! decisions use platform-specific peer-trust mechanisms (macOS Team ID
//! via `SecCode`; Linux peer credentials via `SO_PEERCRED`).

use crate::config::ConfigRevision;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Mutation provenance: initiator × source × peer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Provenance {
    /// Who triggered the mutation (Daemon, GUI, CLI, LLM, System).
    pub initiator: Initiator,
    /// What was applied (in-memory edit, disk import, rollback, etc.).
    pub source: Source,
    /// Authenticated peer identity, when one exists (None for
    /// `Initiator::Daemon` and `Initiator::System`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<PeerIdentity>,
}

/// Who triggered the mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Initiator {
    /// Internal daemon work — startup, hot-plug, scheduled task.
    Daemon,
    /// A GUI peer (Tauri app).
    Gui,
    /// A `conductorctl` invocation.
    Cli,
    /// LLM via the Plan/Apply path.
    ///
    /// `plan_id` is `Uuid::to_string()` at construction (the `uuid`
    /// crate is a daemon-side dep; core stays string-typed).
    Llm {
        provider: String,
        model: String,
        plan_id: String,
    },
    /// OS-triggered (signal handler, etc.).
    System,
}

/// What was applied.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Source {
    /// `ApplyPlan`, `SaveConfig`, in-memory editor changes.
    InMemoryEdit,
    /// `ReloadFromDisk` / `ImportConfig` — pre-validated bytes from a
    /// disk file. `path` is informational only (audit display), never
    /// an authorisation input. `revision` is the [`ConfigRevision`]
    /// of the canonical bytes that were imported.
    DiskImport {
        path: PathBuf,
        revision: ConfigRevision,
    },
    /// CAS-checked `RollbackConfig` — loads `live.toml.known_good`.
    Rollback,
    /// Break-glass non-CAS `RollbackConfigForce`. `reason` is required
    /// (non-empty, enforced at the IPC framer) so audit operators can
    /// understand why CAS was bypassed.
    RollbackForced { reason: String },
    /// Schema-version migration via `conductorctl config migrate`.
    Migration { from_schema: u32, to_schema: u32 },
    /// First-boot init from built-in defaults.
    DaemonDefault,
    /// Startup load from `live.toml` or `live.toml.known_good`.
    /// `revision` is the [`ConfigRevision`] of the canonical bytes of
    /// the file actually loaded.
    DaemonStartup {
        source_file: PathBuf,
        revision: ConfigRevision,
    },
    /// `MarkKnownGood` — operator explicitly promoted the current
    /// snapshot's revision to `known_good_revision`.
    OperatorConfirmation,
}

/// Platform-specific peer identity captured at IPC connection time.
///
/// **`exe_path` is audit display only — never an authorisation input.**
/// On Linux, `/proc/<pid>/exe` resolves to the binary at exec time and
/// may now be deleted, renamed, or replaced. Authorisation decisions
/// MUST use the platform's peer-trust mechanism (ADR-027 §D1):
/// `SO_PEERCRED` + `SecCode` on macOS, `SO_PEERCRED` on Linux.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "platform")]
pub enum PeerIdentity {
    Macos {
        team_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bundle_id: Option<String>,
        pid: u32,
    },
    Linux {
        uid: u32,
        exe_path: PathBuf,
        pid: u32,
    },
    /// Reserved; Conductor does not ship on Windows today.
    Windows { sid: String, pid: u32 },
}
