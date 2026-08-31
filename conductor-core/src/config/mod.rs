// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Configuration module for Conductor
//!
//! This module defines and manages MIDI mapping configurations. It's organized into:
//!
//! - **types**: Data structures representing configuration (Config, Mode, Mapping, Trigger, ActionConfig)
//! - **loader**: Loading, saving, and validating configuration files
//!
//! # Example
//!
//! ```rust,no_run
//! use conductor_core::Config;
//!
//! // Load configuration from a file (creates default if not found)
//! let config = Config::load("config.toml")?;
//!
//! // Validate the configuration
//! config.validate()?;
//!
//! // Save to a new location
//! config.save("backup.toml")?;
//!
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod canonical; // ADR-034 §D5 — deterministic TOML for ConfigRevision hashing
pub mod compile; // Parse-time lowering (ADR-025 Phase 2.E)
pub mod control_state_analyzer; // ADR-025 Phase 3.F — PC-tuple inventory
pub mod feedback_loops; // #2398 — config-load MIDI feedback-loop detection (epic #2395)
pub mod loader;
pub mod midi_channel; // #845 Phase 1 — 1-indexed channel serde helpers
pub mod port_binding;
pub mod preferences;
pub mod protocol;
pub mod provenance; // ADR-034 §D6 — mutation provenance tagging
pub mod revision; // ADR-034 §D5 — content-addressed ConfigRevision
pub mod types;
pub mod u8_string_map; // #1356 — TOML-compatible serde for HashMap<u8, _> fields
pub mod validation;

/// Backward-compatible re-export — prefer `validation` directly.
pub mod validator {
    pub use super::validation::*;
}

// Re-export types for convenience
pub use canonical::{CanonicalError, serialise as canonical_serialise};
pub use port_binding::DeviceDirection;
pub use provenance::{Initiator, PeerIdentity, Provenance, Source};
pub use revision::ConfigRevision;
pub use types::{
    ActionConfig, AdvancedSettings, Config, ConfigMeta, ConfigSource, HidLedConfig, InputMode,
    LedConfig, ListenMode, LoggingConfig, Mapping, MidiMessageType, Mode, ResolvedHidLedConfig,
    RgbColor, Trigger, UserFilePolicy, VelocityColorMap, VelocityRange,
};
