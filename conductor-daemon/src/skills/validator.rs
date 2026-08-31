// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Agent Skills validation and management (ADR-007)
//!
//! This module provides validation, installation, and listing of Agent Skills
//! following the agentskills.io specification for LLM integration.
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
//! # Usage
//!
//! ```no_run
//! use conductor_daemon::skills::{validate_skill, SkillMetadata};
//!
//! let result = validate_skill("./skills/conductor-midi-mapping");
//! match result {
//!     Ok(skill) => println!("Valid skill: {}", skill.metadata.name),
//!     Err(errors) => {
//!         for error in errors {
//!             eprintln!("Error: {}", error);
//!         }
//!     }
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Maximum token count for skill metadata (Level 1)
/// Reserved for future use when implementing token budgets
#[allow(dead_code)]
const MAX_METADATA_TOKENS: usize = 200;

/// Maximum token count for full skill body (Level 2)
const MAX_BODY_TOKENS: usize = 6000;

/// Approximate tokens per character for estimation
const TOKENS_PER_CHAR: f64 = 0.25;

/// Errors that can occur during skill validation
#[derive(Debug, Error, Clone, PartialEq)]
pub enum SkillValidationError {
    #[error("SKILL.md not found at {0}")]
    SkillMdNotFound(PathBuf),

    #[error("Invalid YAML frontmatter: {0}")]
    InvalidFrontmatter(String),

    #[error("Missing required field: {0}")]
    MissingRequiredField(String),

    #[error("Skill name '{actual}' does not match directory name '{expected}'")]
    NameMismatch { expected: String, actual: String },

    #[error("Referenced file not found: {0}")]
    ReferenceNotFound(PathBuf),

    #[error("Skill body exceeds token limit: {actual} > {max}")]
    TokenLimitExceeded { actual: usize, max: usize },

    #[error("Invalid license format: {0}")]
    InvalidLicense(String),

    #[error("IO error: {0}")]
    IoError(String),

    #[error("Invalid tool pattern: {0}")]
    InvalidToolPattern(String),
}

/// Skill metadata from YAML frontmatter
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillMetadata {
    /// Skill name (must match directory name)
    #[serde(default)]
    pub name: String,

    /// Human-readable description
    #[serde(default)]
    pub description: String,

    /// SPDX license identifier
    #[serde(default = "default_license")]
    pub license: String,

    /// Compatibility requirements
    #[serde(default)]
    pub compatibility: Option<String>,

    /// Additional metadata fields
    #[serde(default)]
    pub metadata: HashMap<String, String>,

    /// Allowed tool patterns (e.g., "conductor:get_*, conductor:list_*")
    #[serde(default, rename = "allowed-tools")]
    pub allowed_tools: Option<String>,

    /// Trust level for the skill (bundled, user, remote)
    #[serde(default, rename = "trust-level")]
    pub trust_level: Option<String>,
}

fn default_license() -> String {
    "Apache-2.0".to_string()
}

impl SkillMetadata {
    /// Estimate token count for metadata (Level 1)
    pub fn estimate_tokens(&self) -> usize {
        let json = serde_json::to_string(self).unwrap_or_default();
        estimate_tokens(&json)
    }

    /// Check if this skill has restricted tool access
    pub fn has_tool_restrictions(&self) -> bool {
        self.allowed_tools.is_some()
    }

    /// Get the trust level, defaulting to "user" if not specified
    pub fn get_trust_level(&self) -> &str {
        self.trust_level.as_deref().unwrap_or("user")
    }
}

/// Validated skill with full content
#[derive(Debug, Clone)]
pub struct ValidatedSkill {
    /// Skill metadata from frontmatter
    pub metadata: SkillMetadata,

    /// Full skill body (instructions section)
    pub body: String,

    /// Path to the skill directory
    pub path: PathBuf,

    /// Referenced files that exist
    pub references: Vec<PathBuf>,

    /// Estimated token count for body
    pub body_tokens: usize,
}

/// Estimate token count for a string (rough approximation)
pub fn estimate_tokens(text: &str) -> usize {
    (text.len() as f64 * TOKENS_PER_CHAR).ceil() as usize
}

/// Extract YAML frontmatter from markdown content
fn extract_frontmatter(content: &str) -> Option<(&str, &str)> {
    let trimmed = content.trim_start();

    if !trimmed.starts_with("---") {
        return None;
    }

    // Find the end of frontmatter
    let after_first = &trimmed[3..];
    if let Some(end_idx) = after_first.find("\n---") {
        let frontmatter = &after_first[..end_idx].trim();
        let body = &after_first[end_idx + 4..].trim_start();
        Some((frontmatter, body))
    } else {
        None
    }
}

/// Parse skill metadata from YAML frontmatter — YAML structure +
/// required-field check only.
///
/// Public so callers (e.g. integration tests asserting that a shipped
/// SKILL.md has the right `name` / `description` / `allowed-tools`
/// fields) can run the same parsing the production validator does
/// without paying the cost — or, today, the false negative — of the
/// `allowed-tools` pattern check (`validate_tool_patterns`).
///
/// `validate_skill` is still the right call for full end-to-end
/// validation; this is the lower-level helper.
pub fn parse_skill_metadata(yaml: &str) -> Result<SkillMetadata, SkillValidationError> {
    let metadata: SkillMetadata = serde_yaml::from_str(yaml)
        .map_err(|e| SkillValidationError::InvalidFrontmatter(e.to_string()))?;

    if metadata.name.is_empty() {
        return Err(SkillValidationError::MissingRequiredField("name".into()));
    }

    if metadata.description.is_empty() {
        return Err(SkillValidationError::MissingRequiredField(
            "description".into(),
        ));
    }

    Ok(metadata)
}

/// Extract YAML frontmatter and parse it into [`SkillMetadata`] in one
/// step. Equivalent to `extract_frontmatter` + [`parse_skill_metadata`]
/// for callers that have the raw markdown content rather than the
/// pre-extracted YAML block.
pub fn parse_skill_md(content: &str) -> Result<(SkillMetadata, &str), SkillValidationError> {
    let (yaml, body) = extract_frontmatter(content)
        .ok_or_else(|| SkillValidationError::InvalidFrontmatter("missing `---` markers".into()))?;
    let metadata = parse_skill_metadata(yaml)?;
    Ok((metadata, body))
}

/// Internal: parse frontmatter + run all validations. Used by
/// [`validate_skill`]. Tests that only want the metadata should call
/// [`parse_skill_metadata`] (or [`parse_skill_md`]) directly.
fn parse_frontmatter(yaml: &str) -> Result<SkillMetadata, SkillValidationError> {
    let metadata = parse_skill_metadata(yaml)?;

    if let Some(ref patterns) = metadata.allowed_tools {
        validate_tool_patterns(patterns)?;
    }

    Ok(metadata)
}

/// Validate `allowed-tools` syntax (#1410).
///
/// Two grammars are accepted; the choice is structural, not a feature
/// flag:
///
/// - **Legacy `namespace:pattern, …`** — chosen when the value contains
///   a comma. The pattern is `<namespace>:<pattern>` with alphanumeric +
///   `_` segments and an optional `*` wildcard at the right side. This
///   is the documented form used by the existing test suite.
/// - **Claude Code space-separated** — chosen when there is no comma.
///   Tokens are split on whitespace and each must be a bare identifier
///   (`Bash`, `Read`), a `Name(args)` permission scope, or an
///   `mcp:<server>[/<tool>]` reference. Every SKILL.md shipped in this
///   repo uses this grammar; before the fix, `validate_skill` rejected
///   all of them.
///
/// The two grammars share the `*` global wildcard.
fn validate_tool_patterns(patterns: &str) -> Result<(), SkillValidationError> {
    let trimmed = patterns.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if trimmed.contains(',') {
        return validate_legacy_tool_patterns(trimmed);
    }
    validate_claude_tool_patterns(trimmed)
}

/// Legacy `namespace:pattern, …` strict validator (pre-#1410 behaviour).
fn validate_legacy_tool_patterns(patterns: &str) -> Result<(), SkillValidationError> {
    for pattern in patterns.split(',') {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        if pattern == "*" {
            continue;
        }

        if !pattern.contains(':') {
            return Err(SkillValidationError::InvalidToolPattern(format!(
                "Pattern '{}' must be in format 'namespace:pattern' (e.g., 'conductor:get_*')",
                pattern
            )));
        }

        let parts: Vec<&str> = pattern.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(SkillValidationError::InvalidToolPattern(format!(
                "Invalid pattern format: '{}'",
                pattern
            )));
        }

        let namespace = parts[0];
        let tool_pattern = parts[1];

        if !namespace.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(SkillValidationError::InvalidToolPattern(format!(
                "Invalid namespace '{}': must be alphanumeric",
                namespace
            )));
        }

        if !tool_pattern
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '*')
        {
            return Err(SkillValidationError::InvalidToolPattern(format!(
                "Invalid tool pattern '{}': must be alphanumeric with optional * wildcard",
                tool_pattern
            )));
        }
    }
    Ok(())
}

/// Claude space-separated grammar — bare names, `Name(args)`, `mcp:*`.
/// Tokens that look like the legacy `namespace:pattern` shape (colon,
/// non-`mcp` prefix) are routed back to the legacy validator so a
/// comma-less single pattern like `conductor:get_*` keeps working.
fn validate_claude_tool_patterns(patterns: &str) -> Result<(), SkillValidationError> {
    for token in patterns.split_whitespace() {
        if token == "*" {
            continue;
        }
        if looks_like_legacy_token(token) {
            validate_legacy_tool_patterns(token)?;
            continue;
        }
        validate_claude_token(token).map_err(SkillValidationError::InvalidToolPattern)?;
    }
    Ok(())
}

/// A token is "legacy `namespace:pattern`" only when the colon sits
/// **before** any opening paren and the prefix isn't the reserved
/// `mcp` namespace. This keeps `Bash(conductor:*)` (Claude — colon
/// is inside the paren scope) from being misread as a legacy form.
fn looks_like_legacy_token(token: &str) -> bool {
    let colon = match token.find(':') {
        Some(idx) => idx,
        None => return false,
    };
    if let Some(paren) = token.find('(')
        && paren < colon
    {
        return false;
    }
    let prefix = &token[..colon];
    prefix != "mcp" && !prefix.is_empty()
}

fn validate_claude_token(token: &str) -> Result<(), String> {
    // mcp:<server>[/<tool>]
    if let Some(rest) = token.strip_prefix("mcp:") {
        if rest.is_empty() {
            return Err(format!("'{token}' is missing the server name"));
        }
        let (server, tool_opt) = match rest.split_once('/') {
            Some((s, t)) => (s, Some(t)),
            None => (rest, None),
        };
        if !is_mcp_segment(server) {
            return Err(format!("Invalid mcp server name in '{token}'"));
        }
        if let Some(t) = tool_opt
            && !is_mcp_segment(t)
        {
            return Err(format!("Invalid mcp tool name in '{token}'"));
        }
        return Ok(());
    }

    // Name or Name(args)
    let bare = match token.split_once('(') {
        Some((name, rest)) => {
            if !rest.ends_with(')') {
                return Err(format!("Unbalanced parentheses in '{token}'"));
            }
            name
        }
        None => token,
    };
    if !is_identifier(bare) {
        return Err(format!(
            "'{token}' is not a valid tool name (letters, digits, underscore)"
        ));
    }
    Ok(())
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_mcp_segment(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Find reference links in skill body (Level 3 references)
fn find_reference_links(body: &str) -> Vec<String> {
    let mut references = Vec::new();

    // Look for markdown links to references/ directory
    // Pattern: [text](references/FILE.md)
    let re = regex::Regex::new(r"\[.*?\]\((references/[^)]+)\)").unwrap();
    for cap in re.captures_iter(body) {
        if let Some(path) = cap.get(1) {
            references.push(path.as_str().to_string());
        }
    }

    references
}

/// Validate a skill directory
///
/// # Arguments
///
/// * `skill_path` - Path to the skill directory (containing SKILL.md)
///
/// # Returns
///
/// * `Ok(ValidatedSkill)` - If the skill is valid
/// * `Err(Vec<SkillValidationError>)` - List of validation errors
pub fn validate_skill<P: AsRef<Path>>(
    skill_path: P,
) -> Result<ValidatedSkill, Vec<SkillValidationError>> {
    let path = skill_path.as_ref();
    let mut errors = Vec::new();

    // Check SKILL.md exists
    let skill_md_path = path.join("SKILL.md");
    if !skill_md_path.exists() {
        errors.push(SkillValidationError::SkillMdNotFound(skill_md_path.clone()));
        return Err(errors);
    }

    // Read SKILL.md content
    let content = match std::fs::read_to_string(&skill_md_path) {
        Ok(c) => c,
        Err(e) => {
            errors.push(SkillValidationError::IoError(e.to_string()));
            return Err(errors);
        }
    };

    // Extract frontmatter
    let (frontmatter, body) = match extract_frontmatter(&content) {
        Some((f, b)) => (f, b),
        None => {
            errors.push(SkillValidationError::InvalidFrontmatter(
                "No YAML frontmatter found (must start with ---)".into(),
            ));
            return Err(errors);
        }
    };

    // Parse frontmatter
    let metadata = match parse_frontmatter(frontmatter) {
        Ok(m) => m,
        Err(e) => {
            errors.push(e);
            return Err(errors);
        }
    };

    // Validate name matches directory
    let dir_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    if metadata.name != dir_name {
        errors.push(SkillValidationError::NameMismatch {
            expected: dir_name.to_string(),
            actual: metadata.name.clone(),
        });
    }

    // Validate license format (SPDX identifier)
    let valid_licenses = [
        "MIT",
        "Apache-2.0",
        "GPL-3.0",
        "BSD-3-Clause",
        "BSD-2-Clause",
        "ISC",
        "MPL-2.0",
        "LGPL-3.0",
        "Unlicense",
        "CC0-1.0",
    ];
    if !valid_licenses.contains(&metadata.license.as_str()) {
        errors.push(SkillValidationError::InvalidLicense(
            metadata.license.clone(),
        ));
    }

    // Check token limits
    let body_tokens = estimate_tokens(body);
    if body_tokens > MAX_BODY_TOKENS {
        errors.push(SkillValidationError::TokenLimitExceeded {
            actual: body_tokens,
            max: MAX_BODY_TOKENS,
        });
    }

    // Find and validate references
    let reference_links = find_reference_links(body);
    let mut valid_references = Vec::new();

    for ref_link in reference_links {
        let ref_path = path.join(&ref_link);
        if ref_path.exists() {
            valid_references.push(ref_path);
        } else {
            errors.push(SkillValidationError::ReferenceNotFound(ref_path));
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(ValidatedSkill {
        metadata,
        body: body.to_string(),
        path: path.to_path_buf(),
        references: valid_references,
        body_tokens,
    })
}

/// List all skills in a directory
///
/// # Arguments
///
/// * `skills_dir` - Path to directory containing skill directories
///
/// # Returns
///
/// List of (path, validation_result) tuples
pub fn list_skills<P: AsRef<Path>>(
    skills_dir: P,
) -> Vec<(PathBuf, Result<ValidatedSkill, Vec<SkillValidationError>>)> {
    let dir = skills_dir.as_ref();
    let mut skills = Vec::new();

    if !dir.exists() || !dir.is_dir() {
        return skills;
    }

    // Read directory entries
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return skills,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // Skip non-directories
        if !path.is_dir() {
            continue;
        }

        // Skip hidden directories
        if path
            .file_name()
            .map(|n| n.to_string_lossy().starts_with('.'))
            .unwrap_or(true)
        {
            continue;
        }

        // Check if it looks like a skill directory
        if path.join("SKILL.md").exists() {
            let result = validate_skill(&path);
            skills.push((path, result));
        }
    }

    // Sort by directory name
    skills.sort_by(|a, b| a.0.cmp(&b.0));

    skills
}

/// Get the default skills directory path
pub fn get_skills_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".conductor").join("skills"))
}

/// Install a skill from a path to the skills directory
///
/// # Arguments
///
/// * `source` - Source path (local directory or remote URL)
/// * `target_dir` - Target skills directory (defaults to ~/.conductor/skills)
///
/// # Returns
///
/// * `Ok(PathBuf)` - Path to installed skill
/// * `Err(String)` - Error message
pub fn install_skill<P: AsRef<Path>>(
    source: P,
    target_dir: Option<PathBuf>,
) -> Result<PathBuf, String> {
    let source_path = source.as_ref();

    // Validate source skill first
    let validated = validate_skill(source_path).map_err(|errors| {
        errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ")
    })?;

    // Determine target directory
    let target_dir = target_dir
        .or_else(get_skills_dir)
        .ok_or_else(|| "Could not determine skills directory".to_string())?;

    // Create target directory if needed
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| format!("Failed to create skills directory: {}", e))?;

    // Target path for this skill
    let skill_target = target_dir.join(&validated.metadata.name);

    // Check if already installed
    if skill_target.exists() {
        return Err(format!(
            "Skill '{}' already installed at {:?}",
            validated.metadata.name, skill_target
        ));
    }

    // Copy skill directory
    copy_dir_all(source_path, &skill_target).map_err(|e| format!("Failed to copy skill: {}", e))?;

    Ok(skill_target)
}

/// Recursively copy a directory
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

/// Test helpers for skills module (exposed for crate-internal testing)
#[cfg(test)]
pub(crate) mod test_helpers {
    use std::path::{Path, PathBuf};

    /// Create a valid skill directory for testing
    pub fn create_test_skill(dir: &Path, name: &str) -> PathBuf {
        create_test_skill_with_tools(dir, name, None)
    }

    /// Create a valid skill directory with allowed_tools
    pub fn create_test_skill_with_tools(
        dir: &Path,
        name: &str,
        allowed_tools: Option<&str>,
    ) -> PathBuf {
        create_test_skill_full(dir, name, allowed_tools, None)
    }

    /// Create a valid skill directory with allowed_tools and trust_level
    pub fn create_test_skill_full(
        dir: &Path,
        name: &str,
        allowed_tools: Option<&str>,
        trust_level: Option<&str>,
    ) -> PathBuf {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::create_dir_all(skill_dir.join("references")).unwrap();

        let tools_line = match allowed_tools {
            Some(tools) => format!("allowed-tools: \"{}\"\n", tools),
            None => String::new(),
        };

        let trust_line = match trust_level {
            Some(level) => format!("trust-level: {}\n", level),
            None => String::new(),
        };

        let skill_md = format!(
            r#"---
name: {name}
description: Test skill for validation
license: MIT
{tools_line}{trust_line}metadata:
  author: test
  version: "1.0.0"
---

# Test Skill

This is a test skill body.

See [TRIGGERS.md](references/TRIGGERS.md) for more info.
"#,
            name = name,
            tools_line = tools_line,
            trust_line = trust_line
        );

        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
        std::fs::write(
            skill_dir.join("references/TRIGGERS.md"),
            "# Triggers Reference",
        )
        .unwrap();

        skill_dir
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::{create_test_skill, create_test_skill_with_tools};
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_skill_md_frontmatter_valid() {
        let tmp = TempDir::new().unwrap();
        let skill_path = create_test_skill(tmp.path(), "test-skill");

        let result = validate_skill(&skill_path);
        assert!(result.is_ok(), "Expected valid skill: {:?}", result);

        let skill = result.unwrap();
        assert_eq!(skill.metadata.name, "test-skill");
        assert_eq!(skill.metadata.license, "MIT");
    }

    #[test]
    fn test_skill_md_name_matches_directory() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        // Create skill with mismatched name
        let skill_md = r#"---
name: wrong-name
description: Test skill with wrong name
license: MIT
---

# Wrong Name Skill
"#;
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();

        let result = validate_skill(&skill_dir);
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            SkillValidationError::NameMismatch {
                expected,
                actual
            } if expected == "my-skill" && actual == "wrong-name"
        )));
    }

    #[test]
    fn test_skill_references_exist() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("ref-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        // Create skill with reference to non-existent file
        let skill_md = r#"---
name: ref-skill
description: Test skill with missing reference
license: MIT
---

# Ref Skill

See [MISSING.md](references/MISSING.md) for details.
"#;
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();

        let result = validate_skill(&skill_dir);
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, SkillValidationError::ReferenceNotFound(_)))
        );
    }

    #[test]
    fn test_skill_progressive_disclosure_token_limits() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("large-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        // Create skill with very large body (exceeds token limit)
        let large_body = "x".repeat(30000); // ~7500 tokens at 0.25 tokens/char
        let skill_md = format!(
            r#"---
name: large-skill
description: Test skill with large body
license: MIT
---

# Large Skill

{body}
"#,
            body = large_body
        );
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();

        let result = validate_skill(&skill_dir);
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, SkillValidationError::TokenLimitExceeded { .. }))
        );
    }

    #[test]
    fn test_validate_invalid_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("bad-yaml");
        std::fs::create_dir_all(&skill_dir).unwrap();

        // Create skill with invalid YAML
        let skill_md = r#"---
name: [invalid yaml
description: missing bracket
---

# Bad YAML
"#;
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();

        let result = validate_skill(&skill_dir);
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, SkillValidationError::InvalidFrontmatter(_)))
        );
    }

    #[test]
    fn test_validate_missing_required_field() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("no-desc");
        std::fs::create_dir_all(&skill_dir).unwrap();

        // Create skill with missing description
        let skill_md = r#"---
name: no-desc
license: MIT
---

# No Description
"#;
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();

        let result = validate_skill(&skill_dir);
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            SkillValidationError::MissingRequiredField(field) if field == "description"
        )));
    }

    #[test]
    fn test_list_skills() {
        let tmp = TempDir::new().unwrap();
        create_test_skill(tmp.path(), "skill-a");
        create_test_skill(tmp.path(), "skill-b");

        // Create a non-skill directory
        std::fs::create_dir_all(tmp.path().join("not-a-skill")).unwrap();

        let skills = list_skills(tmp.path());
        assert_eq!(skills.len(), 2);

        // Check both are valid
        assert!(skills.iter().all(|(_, result)| result.is_ok()));

        // Check names
        let names: Vec<_> = skills
            .iter()
            .filter_map(|(_, r)| r.as_ref().ok().map(|s| s.metadata.name.clone()))
            .collect();
        assert!(names.contains(&"skill-a".to_string()));
        assert!(names.contains(&"skill-b".to_string()));
    }

    #[test]
    fn test_extract_frontmatter() {
        let content = r#"---
name: test
description: test desc
---

# Body content
"#;

        let (frontmatter, body) = extract_frontmatter(content).unwrap();
        assert!(frontmatter.contains("name: test"));
        assert!(body.contains("# Body content"));
    }

    #[test]
    fn test_estimate_tokens() {
        // Rough check: 100 chars should be ~25 tokens
        let text = "x".repeat(100);
        let tokens = estimate_tokens(&text);
        assert_eq!(tokens, 25);
    }

    #[test]
    fn test_install_skill() {
        let tmp_src = TempDir::new().unwrap();
        let tmp_dst = TempDir::new().unwrap();

        let skill_path = create_test_skill(tmp_src.path(), "install-test");

        let result = install_skill(&skill_path, Some(tmp_dst.path().to_path_buf()));
        assert!(result.is_ok());

        let installed_path = result.unwrap();
        assert!(installed_path.exists());
        assert!(installed_path.join("SKILL.md").exists());
    }

    #[test]
    fn test_install_skill_already_exists() {
        let tmp_src = TempDir::new().unwrap();
        let tmp_dst = TempDir::new().unwrap();

        let skill_path = create_test_skill(tmp_src.path(), "dupe-skill");
        create_test_skill(tmp_dst.path(), "dupe-skill"); // Already exists

        let result = install_skill(&skill_path, Some(tmp_dst.path().to_path_buf()));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already installed"));
    }

    #[test]
    fn test_skill_with_allowed_tools() {
        let tmp = TempDir::new().unwrap();
        let skill_path =
            create_test_skill_with_tools(tmp.path(), "tool-skill", Some("conductor:get_*"));

        let result = validate_skill(&skill_path);
        assert!(result.is_ok(), "Expected valid skill: {:?}", result);

        let skill = result.unwrap();
        assert!(skill.metadata.has_tool_restrictions());
        assert_eq!(
            skill.metadata.allowed_tools,
            Some("conductor:get_*".to_string())
        );
    }

    #[test]
    fn test_validate_tool_patterns() {
        // Legacy `namespace:pattern` grammar — selected when input contains
        // a comma. Stays strict for back-compat with documented syntax.
        assert!(validate_tool_patterns("conductor:get_*").is_ok());
        assert!(validate_tool_patterns("conductor:*").is_ok());
        assert!(validate_tool_patterns("conductor:get_status").is_ok());
        assert!(validate_tool_patterns("conductor:get_*, conductor:list_*").is_ok());
        assert!(validate_tool_patterns("*").is_ok());

        // Legacy strict rejections (input contains comma → strict mode).
        assert!(validate_tool_patterns("conductor:get-*, conductor:list_*").is_err());
        assert!(validate_tool_patterns("conductor:get @, conductor:list_*").is_err());

        // Comma-less single tokens that fail the legacy parser still fail
        // when both grammars would reject (reserved punctuation).
        assert!(validate_tool_patterns("conductor:get @").is_err());
    }

    #[test]
    fn test_skill_invalid_tool_pattern() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("bad-pattern");
        std::fs::create_dir_all(&skill_dir).unwrap();

        // Use a token containing a reserved character that neither grammar
        // (legacy `namespace:pattern` nor Claude space-separated) accepts.
        let skill_md = r#"---
name: bad-pattern
description: Test skill with invalid tool pattern
license: MIT
allowed-tools: "Bash @invalid"
---

# Bad Pattern Skill
"#;
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();

        let result = validate_skill(&skill_dir);
        assert!(result.is_err());

        let errors = result.unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, SkillValidationError::InvalidToolPattern(_)))
        );
    }

    // ==================== Claude-style grammar tests (#1410) ====================

    #[test]
    fn test_validate_claude_style_bare_names() {
        // Claude Code permits space-separated bare tool names; the shipped
        // `skills/` and `.claude/skills/` directories all use this form.
        assert!(validate_tool_patterns("Bash Read Write").is_ok());
        assert!(validate_tool_patterns("Grep Glob").is_ok());
    }

    #[test]
    fn test_validate_claude_style_with_parens() {
        // Claude permission scope inside parentheses (e.g. `Bash(conductor:*)`).
        assert!(validate_tool_patterns("Bash(conductor:*) Read Write").is_ok());
        assert!(validate_tool_patterns("Bash(npm:*) Bash(git:*)").is_ok());
    }

    #[test]
    fn test_validate_claude_style_mcp_server() {
        // MCP server references — hyphens in server names, optional /tool suffix.
        assert!(validate_tool_patterns("Bash Read mcp:session-memory").is_ok());
        assert!(
            validate_tool_patterns("Read mcp:llm-council/verify mcp:llm-council/audit").is_ok()
        );
    }

    #[test]
    fn test_validate_claude_style_invalid_token() {
        // Reserved punctuation rejected even under Claude grammar.
        assert!(validate_tool_patterns("Bash @invalid").is_err());
        assert!(validate_tool_patterns("Bash [bracket]").is_err());
    }

    #[test]
    fn test_validate_shipped_skills_claude_syntax() {
        // Per #1410: every SKILL.md shipped under `skills/` and
        // `.claude/skills/` must validate. The bug fix is the validator
        // accepting Claude's space-separated allowed-tools grammar.
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir
            .parent()
            .expect("conductor-daemon must have a parent (workspace root)");

        // Just spot-check two — one from each skill tree — so the test
        // pins the contract without coupling to every shipped skill.
        for rel in [
            "skills/conductor-signal-routing",
            ".claude/skills/session-init",
        ] {
            let skill_dir = repo_root.join(rel);
            // Skip when running outside the repo (e.g. packaged crate).
            if !skill_dir.join("SKILL.md").exists() {
                continue;
            }
            let result = validate_skill(&skill_dir);
            assert!(
                result.is_ok(),
                "validate_skill must accept shipped skill {rel}: {:?}",
                result.err()
            );
        }
    }
}
