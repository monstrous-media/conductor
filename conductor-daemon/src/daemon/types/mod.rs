// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Core types for daemon operations

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

/// Validate a plugin name from IPC input
///
/// Plugin names must be non-empty and contain only alphanumeric chars, hyphens, underscores, and dots.
/// This prevents directory traversal and other injection attacks.
pub(crate) fn validate_plugin_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Missing 'name' parameter".into());
    }
    if name == "." || name == ".." || name.contains("..") {
        return Err(format!(
            "Invalid plugin name '{}': path traversal not allowed",
            name
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(format!(
            "Invalid plugin name '{}': must contain only ASCII alphanumeric, hyphens, underscores, or dots",
            name
        ));
    }
    Ok(())
}

/// Validate a profile config path (Phase 1)
///
/// This function validates that the path is:
/// - Absolute
/// - Points to an existing file
/// - Has a .toml extension
/// - Can be canonicalized (resolves symlinks)
pub(crate) fn validate_profile_path(path_str: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path_str);
    // Must be absolute
    if !path.is_absolute() {
        return Err("Profile config path must be absolute".into());
    }
    // Canonicalize (resolves symlinks, validates existence)
    let canonical =
        std::fs::canonicalize(&path).map_err(|e| format!("Profile config path invalid: {}", e))?;
    // Must be a file
    if !canonical.is_file() {
        return Err("Profile config path is not a file".into());
    }
    // Must be .toml — case-insensitive ASCII, matching the startup path's
    // `active_profile_config_path` contract. A profile named e.g.
    // `studio.TOML` resolves correctly at boot but was previously rejected here
    // at runtime (IPC / MCP / app-detection profile switch), an api-contract
    // mismatch.
    let is_toml = canonical
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
    if !is_toml {
        return Err("Profile config must be a .toml file".into());
    }
    Ok(canonical)
}

mod commands;
mod ipc;
mod lifecycle;
mod monitoring;
mod state;
mod status;

pub use commands::*;
pub use ipc::*;
pub use lifecycle::*;
pub use monitoring::*;
pub use state::*;
pub use status::*;

#[cfg(test)]
mod tests;
