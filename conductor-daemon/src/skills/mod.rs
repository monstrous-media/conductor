// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Agent Skills validation, management, and sandboxing (ADR-007)
//!
//! This module provides:
//! - Validation, installation, and listing of Agent Skills
//! - Sandboxing for user-provided skills with tool access restrictions
//!
//! # Skill Directory Structure
//!
//! ```text
//! skill-name/
//! ├── SKILL.md              # Required: instructions + YAML frontmatter
//! └── references/           # Optional: additional reference documents
//!     ├── TRIGGERS.md
//!     └── ACTIONS.md
//! ```
//!
//! # Tool Access Patterns
//!
//! Skills can restrict tool access via the `allowed-tools` frontmatter field:
//!
//! ```yaml
//! allowed-tools: "conductor:get_*, conductor:list_*"
//! ```
//!
//! Pattern syntax:
//! - `conductor:*` - All Conductor tools
//! - `conductor:get_*` - All tools starting with "conductor_get_"
//! - `conductor:create_mapping` - Specific tool only
//! - `*` - All tools (use with caution)

mod sandbox;
mod validator;

pub use sandbox::{
    SandboxConfig, SandboxError, SandboxResult, SkillSandbox, ToolAccessResult, ToolPattern,
};
pub use validator::{
    SkillMetadata, SkillValidationError, ValidatedSkill, get_skills_dir, install_skill,
    list_skills, parse_skill_md, parse_skill_metadata, validate_skill,
};
