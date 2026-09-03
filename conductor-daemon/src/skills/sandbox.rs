// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Skill sandbox for user-provided skills
//!
//! This module provides security sandboxing for user-provided Agent Skills,
//! enforcing tool access restrictions based on the `allowed-tools` metadata.
//!
//! # Tool Access Patterns
//!
//! Patterns follow the format `namespace:tool_pattern`:
//!
//! - `conductor:*` - All tools in the conductor namespace
//! - `conductor:get_*` - Tools starting with "get_" in conductor namespace
//! - `conductor:get_status` - Specific tool only
//! - `*` - All tools (unrestricted, only for bundled skills)
//!
//! # Trust Levels
//!
//! Skills have three trust levels that affect sandboxing:
//!
//! - `bundled`: Shipped with Conductor, unrestricted access
//! - `user`: User-installed, restricted by allowed-tools
//! - `remote`: Fetched from network, most restricted

use super::validator::{SkillMetadata, ValidatedSkill};
use std::collections::HashSet;
use thiserror::Error;

/// Result type for sandbox operations
pub type SandboxResult<T> = Result<T, SandboxError>;

/// Errors that can occur during sandbox operations
#[derive(Debug, Error, Clone, PartialEq)]
pub enum SandboxError {
    #[error("Tool access denied: '{tool}' not in allowed patterns")]
    ToolAccessDenied { tool: String },

    #[error(
        "Tool access denied: '{tool}' requires tier '{required_tier}' but skill has max tier '{allowed_tier}'"
    )]
    TierAccessDenied {
        tool: String,
        required_tier: String,
        allowed_tier: String,
    },

    #[error("Invalid tool pattern: {0}")]
    InvalidPattern(String),

    #[error("Sandbox violation: {0}")]
    Violation(String),

    #[error("Skill has no tool restrictions defined")]
    NoRestrictions,
}

/// Result of a tool access check
#[derive(Debug, Clone, PartialEq)]
pub enum ToolAccessResult {
    /// Tool access is allowed
    Allowed,
    /// Tool access is allowed but should be logged
    AllowedWithLogging { reason: String },
    /// Tool access is denied
    Denied { reason: String },
}

impl ToolAccessResult {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed | Self::AllowedWithLogging { .. })
    }
}

/// Matching strategy for a `ToolPattern`. The legacy `namespace:pattern`
/// grammar (`Namespaced`) was originally the only one the validator
/// understood — but every SKILL.md shipped in this repo uses
/// Claude Code's space-separated grammar instead. The extra modes let
/// `ToolPattern` represent Claude tokens without breaking the legacy
/// matching semantics that existing tests rely on.
#[derive(Debug, Clone, PartialEq, Default)]
enum MatchMode {
    /// Legacy: `namespace_pattern` form. Uses `namespace`, `pattern`,
    /// `is_wildcard`, `is_prefix`, `is_global` on the parent struct.
    #[default]
    Namespaced,
    /// Claude bare tool name (`Bash`, `Read`). Matches the runtime
    /// tool name **exactly**; `pattern` holds the name.
    ExactName,
    /// Claude `mcp:<server>` reference. Matches any MCP tool routed
    /// through `<server>` — runtime tool names follow the
    /// `mcp__<server>__<tool>` shape (double underscores). `pattern` holds `<server>`.
    McpServer,
    /// Claude `mcp:<server>/<tool>` reference. Matches the single
    /// `mcp__<server>__<tool>` tool (double underscores). `pattern` holds `<server>/<tool>`.
    McpServerTool,
}

/// Parsed tool pattern for matching
#[derive(Debug, Clone, PartialEq)]
pub struct ToolPattern {
    /// Namespace (e.g., "conductor")
    pub namespace: String,
    /// Pattern to match tool names (e.g., "get_*" or "*")
    pub pattern: String,
    /// Whether this is a wildcard pattern
    pub is_wildcard: bool,
    /// Whether this is a prefix pattern (ends with *)
    pub is_prefix: bool,
    /// Whether this is the global "*" pattern
    pub is_global: bool,
    /// Which grammar produced this pattern; drives [`Self::matches`].
    /// `MatchMode::Namespaced` reproduces the legacy behaviour.
    mode: MatchMode,
}

impl ToolPattern {
    /// Parse a legacy `namespace:pattern` token (the comma-separated
    /// grammar). For Claude Code's space-separated grammar — bare names,
    /// `Name(args)`, `mcp:server`, `mcp:server/tool` — call
    /// [`Self::parse_claude_token`] instead. The two are kept separate
    /// so legacy callers cannot accidentally accept Claude tokens (which
    /// have looser punctuation rules), and so the documented strict
    /// rejections (`invalid`, `conductor:get-status`) still hold.
    pub fn parse(pattern_str: &str) -> Result<Self, SandboxError> {
        let pattern_str = pattern_str.trim();

        // Handle global wildcard
        if pattern_str == "*" {
            return Ok(Self {
                namespace: String::new(),
                pattern: "*".to_string(),
                is_wildcard: true,
                is_prefix: false,
                is_global: true,
                mode: MatchMode::Namespaced,
            });
        }

        // Split on colon
        let parts: Vec<&str> = pattern_str.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(SandboxError::InvalidPattern(format!(
                "Pattern must be in format 'namespace:pattern': '{}'",
                pattern_str
            )));
        }

        let namespace = parts[0].to_string();
        let pattern = parts[1].to_string();

        // Validate namespace
        if !namespace.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(SandboxError::InvalidPattern(format!(
                "Invalid namespace '{}': must be alphanumeric",
                namespace
            )));
        }

        // Validate pattern
        if !pattern
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '*')
        {
            return Err(SandboxError::InvalidPattern(format!(
                "Invalid pattern '{}': must be alphanumeric with optional * wildcard",
                pattern
            )));
        }

        let is_wildcard = pattern == "*";
        let is_prefix = pattern.ends_with('*') && !is_wildcard;

        Ok(Self {
            namespace,
            pattern,
            is_wildcard,
            is_prefix,
            is_global: false,
            mode: MatchMode::Namespaced,
        })
    }

    /// Parse a single Claude Code permission token.
    ///
    /// Claude's grammar covers four shapes — all shipped SKILL.md files
    /// in this repo use one of them:
    ///
    /// - bare identifier: `Bash`, `Read`, `Write`, `Grep`, `Glob`
    /// - identifier with permission-scope args: `Bash(conductor:*)`
    /// - MCP server: `mcp:<server>` (server name may contain `-`)
    /// - MCP server + tool: `mcp:<server>/<tool>`
    ///
    /// The parens on `Name(args)` are Claude's *shell-command* scope
    /// hint; this sandbox enforces tool-level access, not shell-command
    /// patterns, so the args are stripped at parse time and the bare
    /// name is what we match against.
    pub fn parse_claude_token(token: &str) -> Result<Self, SandboxError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(SandboxError::InvalidPattern("Empty token".to_string()));
        }
        if token == "*" {
            return Self::parse("*");
        }

        // mcp:<server> or mcp:<server>/<tool>
        if let Some(rest) = token.strip_prefix("mcp:") {
            if rest.is_empty() {
                return Err(SandboxError::InvalidPattern(
                    "mcp: prefix requires a server name".to_string(),
                ));
            }
            // server names may contain hyphens; tool segment is optional
            // and split on the first `/`.
            let (server, tool_opt) = match rest.split_once('/') {
                Some((s, t)) => (s, Some(t)),
                None => (rest, None),
            };
            if !is_valid_mcp_segment(server) {
                return Err(SandboxError::InvalidPattern(format!(
                    "Invalid mcp server name '{}'",
                    server
                )));
            }
            if let Some(t) = tool_opt {
                if !is_valid_mcp_segment(t) {
                    return Err(SandboxError::InvalidPattern(format!(
                        "Invalid mcp tool name '{}'",
                        t
                    )));
                }
                return Ok(Self {
                    namespace: "mcp".to_string(),
                    pattern: format!("{server}/{t}"),
                    is_wildcard: false,
                    is_prefix: false,
                    is_global: false,
                    mode: MatchMode::McpServerTool,
                });
            }
            return Ok(Self {
                namespace: "mcp".to_string(),
                pattern: server.to_string(),
                is_wildcard: false,
                is_prefix: false,
                is_global: false,
                mode: MatchMode::McpServer,
            });
        }

        // Strip optional permission-scope parens: `Name(args)` → `Name`.
        // Args are not validated for shell-pattern correctness because
        // the tool sandbox doesn't enforce shell-level restrictions.
        let bare = match token.split_once('(') {
            Some((name, rest)) => {
                if !rest.ends_with(')') {
                    return Err(SandboxError::InvalidPattern(format!(
                        "Unbalanced parentheses in '{}'",
                        token
                    )));
                }
                name
            }
            None => token,
        };
        if !is_valid_identifier(bare) {
            return Err(SandboxError::InvalidPattern(format!(
                "Invalid tool name '{}': must be an identifier (letters, digits, underscore)",
                bare
            )));
        }
        Ok(Self {
            namespace: String::new(),
            pattern: bare.to_string(),
            is_wildcard: false,
            is_prefix: false,
            is_global: false,
            mode: MatchMode::ExactName,
        })
    }

    /// Check if this pattern matches a tool name
    pub fn matches(&self, tool_name: &str) -> bool {
        // Global wildcard matches everything
        if self.is_global {
            return true;
        }

        match self.mode {
            MatchMode::ExactName => return tool_name == self.pattern,
            MatchMode::McpServer => {
                // Claude runtime MCP tools are named `mcp__<server>__<tool>`
                // (double underscores) — see `.claude/skills/session-init/
                // SKILL.md` invoking `mcp__session-memory__get_task_context`.
                let prefix = format!("mcp__{}__", self.pattern);
                return tool_name.starts_with(&prefix);
            }
            MatchMode::McpServerTool => {
                // pattern is `<server>/<tool>` — match `mcp__<server>__<tool>`.
                let expected = self
                    .pattern
                    .split_once('/')
                    .map(|(s, t)| format!("mcp__{s}__{t}"));
                return matches!(expected, Some(e) if e == tool_name);
            }
            MatchMode::Namespaced => {}
        }

        // Extract namespace from tool name (e.g., "conductor_get_status" -> "conductor")
        let tool_namespace = tool_name.split('_').next().unwrap_or("");

        // Check namespace match
        if tool_namespace != self.namespace {
            return false;
        }

        // Get the part after the namespace prefix
        let tool_suffix = if tool_name.starts_with(&format!("{}_", self.namespace)) {
            &tool_name[self.namespace.len() + 1..]
        } else {
            return false;
        };

        // Wildcard matches everything in namespace
        if self.is_wildcard {
            return true;
        }

        // Prefix pattern matches
        if self.is_prefix {
            let prefix = &self.pattern[..self.pattern.len() - 1];
            return tool_suffix.starts_with(prefix);
        }

        // Exact match
        tool_suffix == self.pattern
    }
}

/// Identifier characters per Claude bare-tool-name grammar: ASCII letters,
/// digits, and underscore. The leading character must be a letter.
fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// MCP server / tool name characters: identifier chars plus `-` (server
/// names like `llm-council` are common). Must start with a letter.
fn is_valid_mcp_segment(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A token is "legacy `namespace:pattern`" only when the colon sits
/// **before** any opening paren and the prefix isn't the reserved
/// `mcp` namespace. Mirrors the validator-side helper so both files
/// route the same token to the same parser.
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

/// Trust level for skills
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrustLevel {
    /// Most restricted - fetched from network
    Remote,
    /// User-installed, restricted by allowed-tools
    User,
    /// Shipped with Conductor, unrestricted
    Bundled,
}

impl TrustLevel {
    /// Parse trust level from string
    ///
    /// SECURITY: Unknown trust levels default to Remote (most restricted)
    /// to fail-closed. Valid values: "bundled", "user", "remote"
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "bundled" => Self::Bundled,
            "user" => Self::User,
            // Default to most restricted for unknown values (fail-closed)
            _ => Self::Remote,
        }
    }

    /// Parse a trust level from a SKILL-PROVIDED (self-declared) value.
    ///
    /// A skill's frontmatter is attacker-controlled, so it must never let the
    /// skill ESCALATE its own privilege: a skill declaring `trust-level:
    /// bundled` is capped to [`TrustLevel::User`]. `Bundled` (unrestricted,
    /// `*`-capable) trust must be assigned out-of-band by a trusted loader based
    /// on the skill's provenance — where it was loaded from — never from the
    /// skill's own metadata. Use [`SkillSandbox::from_skill_with_trust`] for
    /// that. `user` stays User; anything unknown stays Remote (fail-closed).
    pub fn from_self_declared(s: &str) -> Self {
        match Self::from_str(s) {
            Self::Bundled => Self::User,
            other => other,
        }
    }

    /// Get maximum allowed risk tier for this trust level
    pub fn max_risk_tier(&self) -> &'static str {
        match self {
            Self::Bundled => "HardwareIO", // Full access
            Self::User => "ConfigChange",  // No HardwareIO
            Self::Remote => "Stateful",    // No ConfigChange or HardwareIO
        }
    }
}

/// Configuration for sandbox behavior
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Whether to enforce tool restrictions
    pub enforce_restrictions: bool,
    /// Whether to log all tool access
    pub log_all_access: bool,
    /// Maximum risk tier allowed (ReadOnly, Stateful, ConfigChange, HardwareIO)
    pub max_risk_tier: String,
    /// Deny list of tools (always denied regardless of patterns)
    pub deny_list: HashSet<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enforce_restrictions: true,
            log_all_access: false,
            max_risk_tier: "ConfigChange".to_string(),
            deny_list: HashSet::new(),
        }
    }
}

impl SandboxConfig {
    /// Create config for bundled skills (unrestricted)
    pub fn for_bundled() -> Self {
        Self {
            enforce_restrictions: false,
            log_all_access: false,
            max_risk_tier: "HardwareIO".to_string(),
            deny_list: HashSet::new(),
        }
    }

    /// Create config for user skills (restricted)
    pub fn for_user() -> Self {
        Self {
            enforce_restrictions: true,
            log_all_access: true,
            max_risk_tier: "ConfigChange".to_string(),
            deny_list: HashSet::new(),
        }
    }

    /// Create config for remote skills (most restricted)
    pub fn for_remote() -> Self {
        let mut deny_list = HashSet::new();
        // Deny dangerous tools for remote skills
        deny_list.insert("conductor_send_sysex".to_string());
        deny_list.insert("conductor_device_reset".to_string());

        Self {
            enforce_restrictions: true,
            log_all_access: true,
            max_risk_tier: "Stateful".to_string(),
            deny_list,
        }
    }

    /// Create config from trust level
    pub fn from_trust_level(level: TrustLevel) -> Self {
        match level {
            TrustLevel::Bundled => Self::for_bundled(),
            TrustLevel::User => Self::for_user(),
            TrustLevel::Remote => Self::for_remote(),
        }
    }
}

/// Sandbox execution context for a skill
#[derive(Debug)]
pub struct SkillSandbox {
    /// Skill name for logging
    pub skill_name: String,
    /// Parsed tool patterns
    patterns: Vec<ToolPattern>,
    /// Sandbox configuration
    config: SandboxConfig,
    /// Trust level of the skill
    trust_level: TrustLevel,
    /// Tools that have been accessed
    accessed_tools: HashSet<String>,
    /// Access denied count
    denied_count: u32,
}

impl SkillSandbox {
    /// Create a sandbox from a validated skill.
    ///
    /// SECURITY: the trust level is derived from the skill's
    /// SELF-DECLARED `trust-level` via [`TrustLevel::from_self_declared`], which
    /// can never yield `Bundled`. A skill therefore cannot grant itself
    /// unrestricted / `*`-capable access by writing `trust-level: bundled` in
    /// its own frontmatter. For a skill whose `Bundled` provenance has been
    /// established by a trusted loader, use [`Self::from_skill_with_trust`].
    pub fn from_skill(skill: &ValidatedSkill) -> SandboxResult<Self> {
        let trust_level = TrustLevel::from_self_declared(skill.metadata.get_trust_level());
        Self::from_skill_with_trust(skill, trust_level)
    }

    /// Create a sandbox with an EXPLICIT trust level assigned by a trusted
    /// caller based on the skill's provenance.
    ///
    /// This is the ONLY path to a `Bundled` sandbox: the caller — not the
    /// skill's metadata — vouches for the trust level (e.g. the skill was
    /// loaded from the application's bundled-skills location). `from_skill`
    /// derives trust from self-declared metadata and so never reaches Bundled.
    pub fn from_skill_with_trust(
        skill: &ValidatedSkill,
        trust_level: TrustLevel,
    ) -> SandboxResult<Self> {
        let config = SandboxConfig::from_trust_level(trust_level);
        let patterns = Self::parse_patterns(&skill.metadata, trust_level)?;

        Ok(Self {
            skill_name: skill.metadata.name.clone(),
            patterns,
            config,
            trust_level,
            accessed_tools: HashSet::new(),
            denied_count: 0,
        })
    }

    /// Create a sandbox from a validated skill with a custom config.
    ///
    /// The custom config may only TIGHTEN the sandbox (e.g. add deny-list
    /// entries). Because trust here is self-declared (always capped below
    /// Bundled), restriction enforcement is forced ON regardless of the
    /// supplied config — otherwise a caller could hand an untrusted skill an
    /// unrestricted config (`SandboxConfig::for_bundled()`, where
    /// `enforce_restrictions == false`) and bypass pattern checks entirely.
    /// A genuinely-bundled skill must go through
    /// [`Self::from_skill_with_trust`] with `TrustLevel::Bundled`.
    pub fn from_skill_with_config(
        skill: &ValidatedSkill,
        mut config: SandboxConfig,
    ) -> SandboxResult<Self> {
        let trust_level = TrustLevel::from_self_declared(skill.metadata.get_trust_level());
        // Footgun guard: never let a self-declared skill run unrestricted.
        config.enforce_restrictions = true;
        let patterns = Self::parse_patterns(&skill.metadata, trust_level)?;

        Ok(Self {
            skill_name: skill.metadata.name.clone(),
            patterns,
            config,
            trust_level,
            accessed_tools: HashSet::new(),
            denied_count: 0,
        })
    }

    /// Create a sandbox from metadata only.
    ///
    /// Trust is derived from self-declared metadata and capped below Bundled.
    pub fn from_metadata(metadata: &SkillMetadata) -> SandboxResult<Self> {
        let trust_level = TrustLevel::from_self_declared(metadata.get_trust_level());
        let config = SandboxConfig::from_trust_level(trust_level);

        let patterns = Self::parse_patterns(metadata, trust_level)?;

        Ok(Self {
            skill_name: metadata.name.clone(),
            patterns,
            config,
            trust_level,
            accessed_tools: HashSet::new(),
            denied_count: 0,
        })
    }

    /// Parse tool patterns from metadata for a given trust level.
    ///
    /// SECURITY: Non-bundled skills without allowed_tools get NO access (fail-closed).
    /// Only bundled skills are allowed unrestricted access. The `trust_level` is
    /// supplied by the caller (NOT re-read from `metadata`) so a skill cannot
    /// self-declare `bundled` to unlock `*` / wildcard access.
    fn parse_patterns(
        metadata: &SkillMetadata,
        trust_level: TrustLevel,
    ) -> SandboxResult<Vec<ToolPattern>> {
        let allowed_tools = match &metadata.allowed_tools {
            Some(t) => t,
            None => {
                // SECURITY: Fail-closed for non-bundled skills
                // Only bundled skills get wildcard access when allowed_tools is missing
                match trust_level {
                    TrustLevel::Bundled => {
                        return Ok(vec![ToolPattern::parse("*")?]);
                    }
                    TrustLevel::User | TrustLevel::Remote => {
                        // Non-bundled skills without allowed_tools get no tool access
                        return Err(SandboxError::NoRestrictions);
                    }
                }
            }
        };

        // Grammar dispatch: commas select the legacy
        // `namespace:pattern, …` parser; otherwise we use the Claude
        // space-separated grammar that every shipped SKILL.md uses.
        let parsed: Vec<ToolPattern> = if allowed_tools.contains(',') {
            let mut out = Vec::new();
            for pattern_str in allowed_tools.split(',') {
                let pattern_str = pattern_str.trim();
                if !pattern_str.is_empty() {
                    out.push(ToolPattern::parse(pattern_str)?);
                }
            }
            out
        } else {
            let mut out = Vec::new();
            for token in allowed_tools.split_whitespace() {
                if token.is_empty() {
                    continue;
                }
                // Legacy `namespace:pattern` tokens (non-`mcp` prefix, colon
                // *before* any paren) still route to the strict parser, so
                // a comma-less single pattern like `conductor:get_*` works
                // and `Bash(conductor:*)` is not misread as legacy.
                let parsed = if looks_like_legacy_token(token) {
                    ToolPattern::parse(token)?
                } else {
                    ToolPattern::parse_claude_token(token)?
                };
                out.push(parsed);
            }
            out
        };

        let mut patterns = Vec::new();
        for pattern in parsed {
            // SECURITY: Non-bundled skills cannot use global wildcard '*'
            if pattern.is_global && !matches!(trust_level, TrustLevel::Bundled) {
                return Err(SandboxError::InvalidPattern(
                    "Global wildcard '*' not allowed for non-bundled skills".to_string(),
                ));
            }
            patterns.push(pattern);
        }

        if patterns.is_empty() {
            return Err(SandboxError::InvalidPattern(
                "No valid patterns found in allowed-tools".to_string(),
            ));
        }

        Ok(patterns)
    }

    /// Check patterns and deny list without recording access (internal use)
    ///
    /// SECURITY: This method does NOT mutate state. Use check_tool_access or
    /// check_tool_access_with_tier for the full check with state recording.
    fn check_patterns_only(&self, tool_name: &str) -> ToolAccessResult {
        // Check deny list first
        if self.config.deny_list.contains(tool_name) {
            return ToolAccessResult::Denied {
                reason: format!("Tool '{}' is in deny list", tool_name),
            };
        }

        // If restrictions not enforced, allow all
        if !self.config.enforce_restrictions {
            return ToolAccessResult::Allowed;
        }

        // Check against patterns
        for pattern in &self.patterns {
            if pattern.matches(tool_name) {
                if self.config.log_all_access {
                    return ToolAccessResult::AllowedWithLogging {
                        reason: format!(
                            "Skill '{}' accessed tool '{}'",
                            self.skill_name, tool_name
                        ),
                    };
                }
                return ToolAccessResult::Allowed;
            }
        }

        // No pattern matched
        ToolAccessResult::Denied {
            reason: format!(
                "Tool '{}' not allowed by skill '{}' patterns",
                tool_name, self.skill_name
            ),
        }
    }

    /// Record access result (after all checks pass)
    fn record_access_result(&mut self, tool_name: &str, allowed: bool) {
        if allowed {
            self.accessed_tools.insert(tool_name.to_string());
        } else {
            self.denied_count += 1;
        }
    }

    /// Check if a tool is allowed to be accessed
    pub fn check_tool_access(&mut self, tool_name: &str) -> ToolAccessResult {
        let result = self.check_patterns_only(tool_name);
        self.record_access_result(tool_name, result.is_allowed());
        result
    }

    /// Check if a tool with given risk tier is allowed
    ///
    /// SECURITY: This method checks BOTH pattern and tier restrictions before
    /// recording access. State is only mutated after ALL checks complete.
    pub fn check_tool_access_with_tier(
        &mut self,
        tool_name: &str,
        risk_tier: &str,
    ) -> ToolAccessResult {
        // First check pattern access (without recording)
        let pattern_result = self.check_patterns_only(tool_name);
        if !pattern_result.is_allowed() {
            self.record_access_result(tool_name, false);
            return pattern_result;
        }

        // Then check risk tier
        let tier_order = [
            "ReadOnly",
            "Stateful",
            "ConfigChange",
            "HardwareIO",
            "Privileged",
        ];
        let required_idx = tier_order.iter().position(|&t| t == risk_tier);
        let max_idx = tier_order
            .iter()
            .position(|&t| t == self.config.max_risk_tier);

        let final_result = match (required_idx, max_idx) {
            (Some(req), Some(max)) if req <= max => pattern_result,
            (Some(_), Some(_)) => ToolAccessResult::Denied {
                reason: format!(
                    "Tool '{}' requires tier '{}' but skill max tier is '{}'",
                    tool_name, risk_tier, self.config.max_risk_tier
                ),
            },
            // SECURITY: Fail-closed on unknown tiers
            (None, _) => ToolAccessResult::Denied {
                reason: format!(
                    "Tool '{}' has unknown risk tier '{}' - access denied",
                    tool_name, risk_tier
                ),
            },
            (_, None) => ToolAccessResult::Denied {
                reason: format!(
                    "Skill '{}' has unknown max tier '{}' - access denied",
                    self.skill_name, self.config.max_risk_tier
                ),
            },
        };

        // SECURITY: Only record access AFTER all checks complete
        self.record_access_result(tool_name, final_result.is_allowed());
        final_result
    }

    /// Get the set of tools that have been accessed
    pub fn accessed_tools(&self) -> &HashSet<String> {
        &self.accessed_tools
    }

    /// Get count of denied accesses
    pub fn denied_count(&self) -> u32 {
        self.denied_count
    }

    /// Get the trust level
    pub fn trust_level(&self) -> TrustLevel {
        self.trust_level
    }

    /// Check if this sandbox enforces restrictions
    pub fn enforces_restrictions(&self) -> bool {
        self.config.enforce_restrictions
    }

    /// Get summary of sandbox state
    pub fn summary(&self) -> String {
        format!(
            "Sandbox[skill={}, trust={:?}, accessed={}, denied={}]",
            self.skill_name,
            self.trust_level,
            self.accessed_tools.len(),
            self.denied_count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::validate_skill;
    use crate::skills::validator::test_helpers::{
        create_test_skill, create_test_skill_full, create_test_skill_with_tools,
    };
    use tempfile::TempDir;

    // ==================== ToolPattern Tests ====================

    #[test]
    fn test_pattern_parse_global() {
        let pattern = ToolPattern::parse("*").unwrap();
        assert!(pattern.is_global);
        assert!(pattern.matches("anything"));
        assert!(pattern.matches("conductor_get_status"));
    }

    #[test]
    fn test_pattern_parse_namespace_wildcard() {
        let pattern = ToolPattern::parse("conductor:*").unwrap();
        assert_eq!(pattern.namespace, "conductor");
        assert!(pattern.is_wildcard);
        assert!(!pattern.is_global);

        assert!(pattern.matches("conductor_get_status"));
        assert!(pattern.matches("conductor_create_mapping"));
        assert!(!pattern.matches("other_tool"));
    }

    #[test]
    fn test_pattern_parse_prefix() {
        let pattern = ToolPattern::parse("conductor:get_*").unwrap();
        assert_eq!(pattern.namespace, "conductor");
        assert!(pattern.is_prefix);
        assert!(!pattern.is_wildcard);

        assert!(pattern.matches("conductor_get_status"));
        assert!(pattern.matches("conductor_get_config"));
        assert!(!pattern.matches("conductor_list_devices"));
        assert!(!pattern.matches("conductor_create_mapping"));
    }

    #[test]
    fn test_pattern_parse_exact() {
        let pattern = ToolPattern::parse("conductor:get_status").unwrap();
        assert!(!pattern.is_prefix);
        assert!(!pattern.is_wildcard);

        assert!(pattern.matches("conductor_get_status"));
        assert!(!pattern.matches("conductor_get_config"));
    }

    #[test]
    fn test_pattern_parse_invalid() {
        assert!(ToolPattern::parse("invalid").is_err());
        assert!(ToolPattern::parse("conductor:get-status").is_err()); // hyphen
        assert!(ToolPattern::parse("conductor:get status").is_err()); // space
    }

    // ==================== Claude-style token tests ====================

    #[test]
    fn test_pattern_parse_claude_bare_name() {
        // Bare tool name from Claude grammar: matches the tool name exactly.
        let pattern = ToolPattern::parse_claude_token("Bash").unwrap();
        assert!(pattern.matches("Bash"));
        assert!(!pattern.matches("Read"));
        assert!(!pattern.matches("bash"));
    }

    #[test]
    fn test_pattern_parse_claude_tool_with_args() {
        // Parens carry Claude permission scope hints. We don't enforce
        // shell-command-level restrictions; the parens are stripped and
        // the bare tool name is what we match against.
        let pattern = ToolPattern::parse_claude_token("Bash(conductor:*)").unwrap();
        assert!(pattern.matches("Bash"));
    }

    #[test]
    fn test_pattern_parse_claude_mcp_server() {
        // `mcp:<server>` — matches Claude runtime MCP-server-namespaced
        // tools (`mcp__<server>__<tool>`, double underscores).
        let pattern = ToolPattern::parse_claude_token("mcp:session-memory").unwrap();
        assert!(pattern.matches("mcp__session-memory__get_task_context"));
        assert!(pattern.matches("mcp__session-memory__recall"));
        assert!(!pattern.matches("mcp__pixeltable-memory__recall"));
        assert!(!pattern.matches("Bash"));
    }

    #[test]
    fn test_pattern_parse_claude_mcp_server_with_subtool() {
        // `mcp:<server>/<tool>` — only matches the exact runtime tool.
        let pattern = ToolPattern::parse_claude_token("mcp:llm-council/verify").unwrap();
        assert!(pattern.matches("mcp__llm-council__verify"));
        assert!(!pattern.matches("mcp__llm-council__audit"));
    }

    #[test]
    fn test_pattern_parse_claude_invalid_token() {
        // Reserved punctuation rejected.
        assert!(ToolPattern::parse_claude_token("@invalid").is_err());
        assert!(ToolPattern::parse_claude_token("[bracket]").is_err());
        assert!(ToolPattern::parse_claude_token("").is_err());
    }

    #[test]
    fn test_sandbox_from_skill_with_claude_style_bare() {
        // The sandbox must build from Claude-style allowed-tools — the
        // core capability this grammar support unblocks.
        let tmp = TempDir::new().unwrap();
        let skill_path =
            create_test_skill_with_tools(tmp.path(), "claude-bare", Some("Bash Read Write"));
        let skill = validate_skill(&skill_path).expect("validator accepts Claude bare names");
        let mut sandbox =
            SkillSandbox::from_skill(&skill).expect("sandbox accepts Claude bare names");
        assert!(sandbox.check_tool_access("Bash").is_allowed());
        assert!(sandbox.check_tool_access("Read").is_allowed());
        assert!(
            !sandbox
                .check_tool_access("conductor_get_status")
                .is_allowed()
        );
    }

    #[test]
    fn test_sandbox_from_skill_with_claude_style_mcp() {
        let tmp = TempDir::new().unwrap();
        let skill_path = create_test_skill_with_tools(
            tmp.path(),
            "claude-mcp",
            Some("Bash Read mcp:session-memory"),
        );
        let skill = validate_skill(&skill_path).expect("validator accepts mcp: prefix");
        let mut sandbox = SkillSandbox::from_skill(&skill).expect("sandbox accepts mcp: prefix");
        assert!(sandbox.check_tool_access("Bash").is_allowed());
        // Claude runtime form: `mcp__<server>__<tool>` (double underscores).
        assert!(
            sandbox
                .check_tool_access("mcp__session-memory__get_task_context")
                .is_allowed()
        );
        assert!(
            !sandbox
                .check_tool_access("mcp__pixeltable-memory__recall")
                .is_allowed()
        );
    }

    // ==================== TrustLevel Tests ====================

    #[test]
    fn test_trust_level_ordering() {
        assert!(TrustLevel::Remote < TrustLevel::User);
        assert!(TrustLevel::User < TrustLevel::Bundled);
    }

    #[test]
    fn test_trust_level_max_tier() {
        assert_eq!(TrustLevel::Bundled.max_risk_tier(), "HardwareIO");
        assert_eq!(TrustLevel::User.max_risk_tier(), "ConfigChange");
        assert_eq!(TrustLevel::Remote.max_risk_tier(), "Stateful");
    }

    // ==================== SandboxConfig Tests ====================

    #[test]
    fn test_sandbox_config_bundled() {
        let config = SandboxConfig::for_bundled();
        assert!(!config.enforce_restrictions);
        assert_eq!(config.max_risk_tier, "HardwareIO");
    }

    #[test]
    fn test_sandbox_config_user() {
        let config = SandboxConfig::for_user();
        assert!(config.enforce_restrictions);
        assert!(config.log_all_access);
        assert_eq!(config.max_risk_tier, "ConfigChange");
    }

    #[test]
    fn test_sandbox_config_remote() {
        let config = SandboxConfig::for_remote();
        assert!(config.enforce_restrictions);
        assert!(config.deny_list.contains("conductor_send_sysex"));
        assert_eq!(config.max_risk_tier, "Stateful");
    }

    // ==================== SkillSandbox Tests ====================

    #[test]
    fn test_sandbox_unrestricted_skill() {
        // SECURITY FIX: Only bundled skills without allowed_tools get global access
        // User/remote skills without allowed_tools now fail-closed with NoRestrictions error
        let tmp = TempDir::new().unwrap();

        // Test 1: A skill whose Bundled provenance is asserted by the trusted
        // caller (NOT self-declared) gets global access. This must go
        // through `from_skill_with_trust`, not `from_skill`.
        let bundled_path =
            create_test_skill_full(tmp.path(), "bundled-skill", None, Some("bundled"));
        let bundled_skill = validate_skill(&bundled_path).unwrap();
        let mut bundled_sandbox =
            SkillSandbox::from_skill_with_trust(&bundled_skill, TrustLevel::Bundled).unwrap();

        assert!(
            bundled_sandbox
                .check_tool_access("conductor_get_status")
                .is_allowed()
        );
        assert!(
            bundled_sandbox
                .check_tool_access("conductor_create_mapping")
                .is_allowed()
        );
        assert!(bundled_sandbox.check_tool_access("any_tool").is_allowed());

        // Test 1b: the SAME skill, self-declaring `bundled` via
        // `from_skill`, is capped to User and so fails closed without
        // allowed_tools — it cannot self-elevate to global access.
        let result = SkillSandbox::from_skill(&bundled_skill);
        assert!(matches!(result, Err(SandboxError::NoRestrictions)));

        // Test 2: User skill without allowed_tools fails closed
        let user_path = create_test_skill(tmp.path(), "user-skill");
        let user_skill = validate_skill(&user_path).unwrap();
        let result = SkillSandbox::from_skill(&user_skill);
        assert!(matches!(result, Err(SandboxError::NoRestrictions)));

        // Test 3: Remote skill without allowed_tools also fails closed
        let remote_path = create_test_skill_full(tmp.path(), "remote-skill", None, Some("remote"));
        let remote_skill = validate_skill(&remote_path).unwrap();
        let result = SkillSandbox::from_skill(&remote_skill);
        assert!(matches!(result, Err(SandboxError::NoRestrictions)));
    }

    /// Regression test: a skill must not be able to
    /// grant itself `Bundled` trust (unrestricted / `*`-capable) by writing
    /// `trust-level: bundled` in its own frontmatter. Self-declared trust is
    /// derived via `from_self_declared` and capped to `User`; `Bundled` is only
    /// reachable through `from_skill_with_trust`, where a trusted caller vouches
    /// for provenance.
    #[test]
    fn test_skill_cannot_self_declare_bundled_trust() {
        let tmp = TempDir::new().unwrap();

        // A skill brazenly self-declaring bundled + a global `*` wildcard.
        let evil_path =
            create_test_skill_full(tmp.path(), "evil-wildcard", Some("*"), Some("bundled"));
        let evil_skill = validate_skill(&evil_path).unwrap();
        // The `*` is rejected because self-declared trust is capped below Bundled.
        assert!(
            matches!(
                SkillSandbox::from_skill(&evil_skill),
                Err(SandboxError::InvalidPattern(_))
            ),
            "a skill must not self-elevate to bundled to unlock '*'"
        );

        // With concrete tools, the derived trust is User — never Bundled.
        let path = create_test_skill_full(
            tmp.path(),
            "evil-claims-bundled",
            Some("conductor:get_*"),
            Some("bundled"),
        );
        let skill = validate_skill(&path).unwrap();
        let sandbox = SkillSandbox::from_skill(&skill).unwrap();
        assert_eq!(
            sandbox.trust_level(),
            TrustLevel::User,
            "self-declared 'bundled' must be capped to User"
        );

        // The provenance path still grants Bundled when the caller vouches.
        let bundled = SkillSandbox::from_skill_with_trust(&skill, TrustLevel::Bundled).unwrap();
        assert_eq!(bundled.trust_level(), TrustLevel::Bundled);

        // And the cap is at the parser level too.
        assert_eq!(TrustLevel::from_self_declared("bundled"), TrustLevel::User);
        assert_eq!(TrustLevel::from_self_declared("user"), TrustLevel::User);
        assert_eq!(TrustLevel::from_self_declared("remote"), TrustLevel::Remote);
        assert_eq!(
            TrustLevel::from_self_declared("nonsense"),
            TrustLevel::Remote
        );
    }

    /// Regression test: passing an
    /// unrestricted config (`for_bundled`, `enforce_restrictions == false`) to
    /// `from_skill_with_config` must NOT bypass pattern enforcement for a
    /// self-declared skill — enforcement is forced on.
    #[test]
    fn test_from_skill_with_config_cannot_grant_unrestricted_to_self_declared_skill() {
        let tmp = TempDir::new().unwrap();
        let path = create_test_skill_full(
            tmp.path(),
            "sneaky",
            Some("conductor:get_*"),
            Some("bundled"),
        );
        let skill = validate_skill(&path).unwrap();

        // Hand it the most permissive config on purpose.
        let mut sandbox =
            SkillSandbox::from_skill_with_config(&skill, SandboxConfig::for_bundled()).unwrap();

        assert!(
            sandbox.enforces_restrictions(),
            "a self-declared skill must stay restriction-enforcing even with an unrestricted config"
        );
        // Patterns are still enforced: in-pattern allowed, out-of-pattern denied.
        assert!(
            sandbox
                .check_tool_access("conductor_get_status")
                .is_allowed()
        );
        assert!(
            !sandbox
                .check_tool_access("conductor_send_sysex")
                .is_allowed(),
            "an unrestricted config must not let a self-declared skill reach tools outside its patterns"
        );
    }

    #[test]
    fn test_sandbox_global_wildcard_denied_for_non_bundled() {
        // SECURITY: Non-bundled skills cannot use global wildcard '*' in allowed-tools
        let tmp = TempDir::new().unwrap();

        // User skill requesting '*' should be rejected
        let user_path = create_test_skill_with_tools(tmp.path(), "user-wildcard", Some("*"));
        let user_skill = validate_skill(&user_path).unwrap();
        let result = SkillSandbox::from_skill(&user_skill);
        assert!(matches!(result, Err(SandboxError::InvalidPattern(_))));

        // Remote skill requesting '*' should also be rejected
        let remote_path =
            create_test_skill_full(tmp.path(), "remote-wildcard", Some("*"), Some("remote"));
        let remote_skill = validate_skill(&remote_path).unwrap();
        let result = SkillSandbox::from_skill(&remote_skill);
        assert!(matches!(result, Err(SandboxError::InvalidPattern(_))));

        // A skill self-declaring `bundled` does NOT get '*' via from_skill:
        // self-declared trust is capped below Bundled.
        let self_bundled_path = create_test_skill_full(
            tmp.path(),
            "self-bundled-wildcard",
            Some("*"),
            Some("bundled"),
        );
        let self_bundled_skill = validate_skill(&self_bundled_path).unwrap();
        let result = SkillSandbox::from_skill(&self_bundled_skill);
        assert!(matches!(result, Err(SandboxError::InvalidPattern(_))));

        // A genuinely-Bundled skill (provenance asserted by the caller) CAN use '*'.
        let result = SkillSandbox::from_skill_with_trust(&self_bundled_skill, TrustLevel::Bundled);
        assert!(result.is_ok());
    }

    #[test]
    fn test_sandbox_restricted_skill() {
        let tmp = TempDir::new().unwrap();
        let skill_path =
            create_test_skill_with_tools(tmp.path(), "readonly-skill", Some("conductor:get_*"));
        let skill = validate_skill(&skill_path).unwrap();

        let mut sandbox = SkillSandbox::from_skill(&skill).unwrap();

        // get_* should be allowed
        assert!(
            sandbox
                .check_tool_access("conductor_get_status")
                .is_allowed()
        );
        assert!(
            sandbox
                .check_tool_access("conductor_get_config")
                .is_allowed()
        );

        // create_mapping should be denied
        assert!(
            !sandbox
                .check_tool_access("conductor_create_mapping")
                .is_allowed()
        );
        assert!(
            !sandbox
                .check_tool_access("conductor_delete_mapping")
                .is_allowed()
        );
    }

    #[test]
    fn test_sandbox_multiple_patterns() {
        let tmp = TempDir::new().unwrap();
        let skill_path = create_test_skill_with_tools(
            tmp.path(),
            "multi-skill",
            Some("conductor:get_*, conductor:list_*"),
        );
        let skill = validate_skill(&skill_path).unwrap();

        let mut sandbox = SkillSandbox::from_skill(&skill).unwrap();

        assert!(
            sandbox
                .check_tool_access("conductor_get_status")
                .is_allowed()
        );
        assert!(
            sandbox
                .check_tool_access("conductor_list_devices")
                .is_allowed()
        );
        assert!(
            !sandbox
                .check_tool_access("conductor_create_mapping")
                .is_allowed()
        );
    }

    #[test]
    fn test_sandbox_exact_tool() {
        let tmp = TempDir::new().unwrap();
        let skill_path =
            create_test_skill_with_tools(tmp.path(), "exact-skill", Some("conductor:get_status"));
        let skill = validate_skill(&skill_path).unwrap();

        let mut sandbox = SkillSandbox::from_skill(&skill).unwrap();

        assert!(
            sandbox
                .check_tool_access("conductor_get_status")
                .is_allowed()
        );
        assert!(
            !sandbox
                .check_tool_access("conductor_get_config")
                .is_allowed()
        );
    }

    #[test]
    fn test_sandbox_deny_list() {
        let tmp = TempDir::new().unwrap();
        let skill_path =
            create_test_skill_with_tools(tmp.path(), "deny-skill", Some("conductor:*"));
        let skill = validate_skill(&skill_path).unwrap();

        let mut config = SandboxConfig::for_user();
        config.deny_list.insert("conductor_send_sysex".to_string());

        let mut sandbox = SkillSandbox::from_skill_with_config(&skill, config).unwrap();

        // Regular tools allowed
        assert!(
            sandbox
                .check_tool_access("conductor_get_status")
                .is_allowed()
        );

        // Denied tool blocked even though pattern matches
        assert!(
            !sandbox
                .check_tool_access("conductor_send_sysex")
                .is_allowed()
        );
    }

    #[test]
    fn test_sandbox_risk_tier_check() {
        let tmp = TempDir::new().unwrap();
        let skill_path =
            create_test_skill_with_tools(tmp.path(), "tier-skill", Some("conductor:*"));
        let skill = validate_skill(&skill_path).unwrap();

        // User config limits to ConfigChange
        let config = SandboxConfig::for_user();
        let mut sandbox = SkillSandbox::from_skill_with_config(&skill, config).unwrap();

        // ReadOnly and Stateful allowed
        assert!(
            sandbox
                .check_tool_access_with_tier("conductor_get_status", "ReadOnly")
                .is_allowed()
        );
        assert!(
            sandbox
                .check_tool_access_with_tier("conductor_start_midi_learn", "Stateful")
                .is_allowed()
        );
        assert!(
            sandbox
                .check_tool_access_with_tier("conductor_create_mapping", "ConfigChange")
                .is_allowed()
        );

        // HardwareIO denied for user trust level
        assert!(
            !sandbox
                .check_tool_access_with_tier("conductor_send_sysex", "HardwareIO")
                .is_allowed()
        );

        // SECURITY: Unknown tiers should be denied (fail-closed)
        assert!(
            !sandbox
                .check_tool_access_with_tier("some_tool", "UnknownTier")
                .is_allowed()
        );
    }

    #[test]
    fn test_sandbox_remote_restrictions() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("remote-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        let skill_md = r#"---
name: remote-skill
description: Test remote skill
license: MIT
allowed-tools: "conductor:*"
trust-level: remote
---

# Remote Skill
"#;
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();

        let skill = validate_skill(&skill_dir).unwrap();
        let mut sandbox = SkillSandbox::from_skill(&skill).unwrap();

        assert_eq!(sandbox.trust_level(), TrustLevel::Remote);

        // Sysex in deny list for remote
        assert!(
            !sandbox
                .check_tool_access("conductor_send_sysex")
                .is_allowed()
        );

        // ConfigChange tier blocked for remote
        assert!(
            !sandbox
                .check_tool_access_with_tier("conductor_create_mapping", "ConfigChange")
                .is_allowed()
        );

        // Stateful allowed for remote
        assert!(
            sandbox
                .check_tool_access_with_tier("conductor_start_midi_learn", "Stateful")
                .is_allowed()
        );
    }

    #[test]
    fn test_sandbox_tracks_access() {
        let tmp = TempDir::new().unwrap();
        let skill_path =
            create_test_skill_with_tools(tmp.path(), "track-skill", Some("conductor:*"));
        let skill = validate_skill(&skill_path).unwrap();

        let mut sandbox = SkillSandbox::from_skill(&skill).unwrap();

        sandbox.check_tool_access("conductor_get_status");
        sandbox.check_tool_access("conductor_get_config");
        sandbox.check_tool_access("other_tool"); // Will be denied

        assert!(sandbox.accessed_tools().contains("conductor_get_status"));
        assert!(sandbox.accessed_tools().contains("conductor_get_config"));
        assert!(!sandbox.accessed_tools().contains("other_tool"));
        assert_eq!(sandbox.denied_count(), 1);
    }

    #[test]
    fn test_sandbox_logging_result() {
        let tmp = TempDir::new().unwrap();
        let skill_path =
            create_test_skill_with_tools(tmp.path(), "log-skill", Some("conductor:get_*"));
        let skill = validate_skill(&skill_path).unwrap();

        let config = SandboxConfig::for_user();
        let mut sandbox = SkillSandbox::from_skill_with_config(&skill, config).unwrap();

        let result = sandbox.check_tool_access("conductor_get_status");

        // User skills should have logging
        assert!(matches!(
            result,
            ToolAccessResult::AllowedWithLogging { .. }
        ));
    }

    #[test]
    fn test_sandbox_summary() {
        let tmp = TempDir::new().unwrap();
        let skill_path =
            create_test_skill_with_tools(tmp.path(), "summary-skill", Some("conductor:get_*"));
        let skill = validate_skill(&skill_path).unwrap();

        let mut sandbox = SkillSandbox::from_skill(&skill).unwrap();
        sandbox.check_tool_access("conductor_get_status");
        sandbox.check_tool_access("conductor_create_mapping"); // denied

        let summary = sandbox.summary();
        assert!(summary.contains("summary-skill"));
        assert!(summary.contains("accessed=1"));
        assert!(summary.contains("denied=1"));
    }

    #[test]
    fn test_sandbox_bundled_unrestricted() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("bundled-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        let skill_md = r#"---
name: bundled-skill
description: Test bundled skill
license: MIT
trust-level: bundled
---

# Bundled Skill
"#;
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();

        let skill = validate_skill(&skill_dir).unwrap();
        // Bundled trust comes from the trusted caller (provenance), not
        // the skill's self-declared `trust-level`. `from_skill` would cap this
        // to User; the unrestricted bundled behaviour is exercised via
        // `from_skill_with_trust`.
        let mut sandbox = SkillSandbox::from_skill_with_trust(&skill, TrustLevel::Bundled).unwrap();

        assert_eq!(sandbox.trust_level(), TrustLevel::Bundled);
        assert!(!sandbox.enforces_restrictions());

        // All tools allowed for bundled
        assert!(
            sandbox
                .check_tool_access("conductor_send_sysex")
                .is_allowed()
        );
        assert!(
            sandbox
                .check_tool_access_with_tier("conductor_send_sysex", "HardwareIO")
                .is_allowed()
        );

        // The same skill self-declaring `bundled` via `from_skill` is
        // capped to User and does NOT get unrestricted access.
        let self_declared = SkillSandbox::from_skill(&skill);
        assert!(matches!(self_declared, Err(SandboxError::NoRestrictions)));
    }
}
