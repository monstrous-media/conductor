// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-027 **D17** — egress allowlist for network-capable tools.
//!
//! A compromised LLM provider (or a malicious plugin) can exfiltrate data via
//! tools that make outbound network requests by encoding payload into the
//! query/path of a request to an attacker-controlled domain. D17 enforces a
//! server-side allowlist at the egress boundary, *before* any request leaves
//! the host.
//!
//! This module is the **pure policy core**: it decides whether a request to a
//! given host (and, after resolution, to a given set of IPs) is permitted. It
//! has no I/O, no DNS, and no feature coupling, so it is exhaustively
//! unit-testable. The call sites (currently the plugin registry fetch in
//! [`crate::plugin_registry`]; future `web_search` / `fetch_url` tools) are
//! responsible for: extracting the host from the URL, resolving it, calling
//! [`EgressPolicy::check_host`] *and* [`EgressPolicy::check_resolved_ips`], and
//! emitting the `EgressBlocked` audit event on a [`EgressDecision::Block`].
//!
//! ## Council R1 P0s addressed here
//!
//! - **DNS-rebinding defense** — [`EgressPolicy::check_resolved_ips`] rejects
//!   any request whose host resolves to a loopback / private / link-local /
//!   unspecified address, with the cloud metadata service
//!   (`169.254.169.254`) covered by the link-local check. An allowlisted
//!   domain that resolves to an internal IP is still blocked.
//! - **Immutable runtime config** — an [`EgressPolicy`] is *compiled once* from
//!   config (typically at startup) and is immutable thereafter; there is no API
//!   to broaden it at runtime. Dynamic broadening would be a privilege
//!   escalation vector.
//!
//! The remaining P0 — **HTTP redirect handling** — is a property of the HTTP
//! client at the call site (disable auto-redirects, or re-check every hop); it
//! cannot be enforced in this pure core. Call sites MUST configure their client
//! accordingly; see [`crate::plugin_registry`].

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};

use serde::{Deserialize, Serialize};

/// Enforcement mode for the egress allowlist (`allowlist_mode` in config).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressMode {
    /// Deny everything not on the allowlist. The safe default.
    #[default]
    Strict,
    /// Log violations but allow the request. For migration / observation.
    Warn,
    /// Disable enforcement entirely. **Development only** — never ship enabled.
    Off,
}

/// `[security.egress]` configuration block.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EgressConfig {
    /// Global enforcement mode.
    #[serde(default, rename = "allowlist_mode")]
    pub mode: EgressMode,
    /// Domains permitted for every network-capable tool. Entries may be exact
    /// (`example.com`) or wildcard (`*.example.com`, matching subdomains).
    #[serde(default)]
    pub allowlist_domains: Vec<String>,
    /// Per-tool overrides. When a tool name is present here, its
    /// `allowlist_domains` **replace** (not extend) the global list for that
    /// tool — least privilege: a tool gets exactly what it declares.
    #[serde(default)]
    pub tools: HashMap<String, ToolEgressConfig>,
}

/// Per-tool egress override (`[security.egress.tools.<name>]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolEgressConfig {
    /// Domains permitted for this specific tool (replaces the global list).
    #[serde(default)]
    pub allowlist_domains: Vec<String>,
}

/// File-only `[security]` configuration.
///
/// Deliberately **NOT** a field on the mutable [`crate::config::Config`]
/// struct: ADR-027 D3/D17 require security-elevating settings (egress
/// allowlist, future network flags) to be settable *only* via the on-disk
/// config file — never through the GUI / MCP surfaces that serialize and
/// round-trip `Config`. Keeping `[security]` off that struct enforces
/// "file-only" *by construction*: those surfaces have no typed handle to set
/// it. The daemon parses this separately (via [`SecurityConfig::from_toml_str`])
/// when constructing the egress policy.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SecurityConfig {
    /// Egress allowlist for network-capable tools (`[security.egress]`).
    #[serde(default)]
    pub egress: EgressConfig,
    /// ADR-027 D6 multi-dimensional LLM budget (`[security.llm]`). File-only
    /// for the same reason as `egress`: lowering a budget must not be reachable
    /// from the GUI / MCP surfaces a compromised LLM can drive.
    #[serde(default)]
    pub llm: crate::security::llm_budget::LlmBudgetConfig,
}

impl SecurityConfig {
    /// Parse just the `[security]` table out of a full config-file TOML string.
    /// All other tables are ignored, so this is safe to call on the raw config
    /// text. A missing `[security]` table yields the default (Strict mode,
    /// empty allowlist).
    pub fn from_toml_str(toml_src: &str) -> Result<Self, toml::de::Error> {
        /// Only deserializes the `[security]` table; unknown tables are ignored
        /// by serde's default behaviour.
        #[derive(Deserialize, Default)]
        struct SecurityTable {
            #[serde(default)]
            security: SecurityConfig,
        }
        toml::from_str::<SecurityTable>(toml_src).map(|t| t.security)
    }
}

/// The outcome of an egress check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressDecision {
    /// The request is permitted with no concern.
    Allow,
    /// The request is permitted, but it WOULD have been blocked under `Strict`
    /// mode — the caller MUST log `reason` (this is how `Warn` mode actually
    /// warns; returning a bare `Allow` would make `Warn` silently permissive).
    AllowWithWarning { reason: String },
    /// The request must not proceed. `reason` is suitable for the audit log.
    Block { reason: String },
}

impl EgressDecision {
    /// `true` if the request must not proceed.
    pub fn is_blocked(&self) -> bool {
        matches!(self, EgressDecision::Block { .. })
    }

    /// The warning/block reason, if any (for `AllowWithWarning` or `Block`).
    pub fn reason(&self) -> Option<&str> {
        match self {
            EgressDecision::Allow => None,
            EgressDecision::AllowWithWarning { reason } | EgressDecision::Block { reason } => {
                Some(reason)
            }
        }
    }
}

/// A single compiled allowlist rule.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DomainRule {
    /// Matches exactly this host (case-insensitive).
    Exact(String),
    /// `*.suffix` — matches any strict subdomain of `suffix` (not the apex).
    Wildcard(String),
}

impl DomainRule {
    fn parse(raw: &str) -> Option<Self> {
        // Strip the wildcard prefix on the RAW string first, then normalize the
        // suffix — so the suffix goes through the same IDNA/Punycode folding as
        // a resolved host (`*.` would otherwise make `domain_to_ascii` reject
        // the whole rule and fall back to un-normalized text).
        let trimmed = raw.trim().trim_end_matches('.');
        if let Some(suffix) = trimmed.strip_prefix("*.") {
            let s = normalize_host(suffix);
            (!s.is_empty()).then_some(DomainRule::Wildcard(s))
        } else {
            let s = normalize_host(trimmed);
            (!s.is_empty()).then_some(DomainRule::Exact(s))
        }
    }

    fn matches(&self, host: &str) -> bool {
        match self {
            DomainRule::Exact(d) => host == d,
            // `*.example.com` matches `a.example.com` and `a.b.example.com`,
            // but NOT the apex `example.com`.
            DomainRule::Wildcard(suffix) => host
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1),
        }
    }
}

/// Compiled, immutable egress policy. Build once from [`EgressConfig`]; there is
/// deliberately no method to broaden it after construction.
#[derive(Debug, Clone)]
pub struct EgressPolicy {
    mode: EgressMode,
    global: Vec<DomainRule>,
    per_tool: HashMap<String, Vec<DomainRule>>,
}

impl EgressPolicy {
    /// Compile a policy from config. Unparseable allowlist entries are dropped
    /// (a malformed rule must never silently widen the allowlist).
    pub fn from_config(cfg: &EgressConfig) -> Self {
        let compile = |domains: &[String]| -> Vec<DomainRule> {
            domains
                .iter()
                .filter_map(|d| DomainRule::parse(d))
                .collect()
        };
        EgressPolicy {
            mode: cfg.mode,
            global: compile(&cfg.allowlist_domains),
            per_tool: cfg
                .tools
                .iter()
                .map(|(name, t)| (name.clone(), compile(&t.allowlist_domains)))
                .collect(),
        }
    }

    /// The active enforcement mode.
    pub fn mode(&self) -> EgressMode {
        self.mode
    }

    /// Decide whether `tool` may make a request to `host` (the URL's host
    /// component, pre-resolution). A `host:port` is accepted (the port is
    /// stripped before matching). In `Warn` mode a non-allowlisted host yields
    /// [`EgressDecision::AllowWithWarning`] (the caller MUST log it); in `Off`
    /// mode everything is allowed.
    ///
    /// The effective allowlist is the per-tool list if the tool has an override,
    /// otherwise the global list (least privilege — an override *replaces*).
    pub fn check_host(&self, tool: &str, host: &str) -> EgressDecision {
        if self.mode == EgressMode::Off {
            return EgressDecision::Allow;
        }
        // Defensive: a caller may pass `host:port` (or a bracketed IPv6
        // literal). Strip the port so the allowlist match is host-only.
        let host = normalize_host(strip_port(host));
        if host.is_empty() {
            return EgressDecision::Block {
                reason: format!("tool '{tool}': request has no host component"),
            };
        }
        let rules = self.per_tool.get(tool).unwrap_or(&self.global);
        let allowed = rules.iter().any(|r| r.matches(&host));
        if allowed {
            EgressDecision::Allow
        } else if self.mode == EgressMode::Warn {
            // Warn must be observable: surface a reason the caller logs.
            EgressDecision::AllowWithWarning {
                reason: format!("tool '{tool}': host '{host}' not on egress allowlist (warn mode)"),
            }
        } else {
            EgressDecision::Block {
                reason: format!("tool '{tool}': host '{host}' not on egress allowlist"),
            }
        }
    }

    /// DNS-rebinding defense: reject if ANY resolved address is internal
    /// (loopback / private / link-local incl. the cloud metadata service /
    /// unspecified / multicast). An internal address is ALWAYS a hard `Block`
    /// (except in `Off`): a public domain rebinding to an internal IP is an
    /// attack, not a policy preference, so `Warn` does not soften it — `Warn`
    /// only softens the *domain allowlist* (see [`check_host`]).
    ///
    /// **Empty `ips` fails CLOSED in `Strict`** (Council #1912): an empty set
    /// means the caller could not resolve the host, so we cannot verify it is
    /// not internal. Allowing would be fail-OPEN — and combined with the
    /// resolve→connect TOCTOU (reqwest may resolve when our lookup didn't), it
    /// would let an unverified host through. `Strict` blocks; `Warn` permits
    /// with a warning; `Off` allows.
    ///
    /// [`check_host`]: EgressPolicy::check_host
    pub fn check_resolved_ips(&self, tool: &str, host: &str, ips: &[IpAddr]) -> EgressDecision {
        if self.mode == EgressMode::Off {
            return EgressDecision::Allow;
        }
        if ips.is_empty() {
            let reason = format!(
                "tool '{tool}': host '{host}' did not resolve to any address; \
                 cannot verify it is not internal"
            );
            return match self.mode {
                EgressMode::Strict => EgressDecision::Block { reason },
                EgressMode::Warn => EgressDecision::AllowWithWarning { reason },
                EgressMode::Off => EgressDecision::Allow, // handled above; exhaustiveness
            };
        }
        for ip in ips {
            if is_internal_ip(*ip) {
                return EgressDecision::Block {
                    reason: format!(
                        "tool '{tool}': host '{host}' resolved to internal address {ip} \
                         (DNS-rebinding / SSRF guard)"
                    ),
                };
            }
        }
        EgressDecision::Allow
    }
}

/// Normalize a host (or the bare-domain part of a rule) to a canonical ASCII
/// form for comparison. Applies IDNA/Punycode folding so that `café.com`,
/// `CAFÉ.COM`, and `xn--caf-dma.com` all compare equal (Council #1912). On
/// invalid input (IPv6 literal, malformed name, a `*` that slipped through),
/// falls back to ASCII-lowercase — which won't match a valid allowlist entry,
/// so `Strict` still blocks (fail-safe).
fn normalize_host(s: &str) -> String {
    let trimmed = s.trim().trim_end_matches('.');
    idna::domain_to_ascii(trimmed).unwrap_or_else(|_| trimmed.to_ascii_lowercase())
}

/// Strip a `:port` suffix (and IPv6 brackets) from a host, leaving the bare
/// host for allowlist matching. Handles `example.com:443`, `[::1]:443`/`[::1]`,
/// and leaves bare IPv6 literals (multiple colons, no brackets) untouched.
fn strip_port(host: &str) -> &str {
    let host = host.trim();
    // Bracketed IPv6 literal: `[::1]` or `[::1]:443` → `::1`.
    if let Some(rest) = host.strip_prefix('[') {
        return match rest.find(']') {
            Some(end) => &rest[..end],
            None => host, // malformed; leave as-is
        };
    }
    // `domain:port` / `ipv4:port` — exactly one colon, numeric port. A bare
    // IPv6 literal has ≥2 colons, so we leave those alone.
    if host.matches(':').count() == 1
        && let Some((h, port)) = host.rsplit_once(':')
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
    {
        return h;
    }
    host
}

/// `true` if `ip` is in a range that must never be reachable via an allowlisted
/// public domain. Covers loopback, RFC1918 private, link-local (incl.
/// `169.254.169.254` cloud metadata), unspecified, and IPv6 ULA / link-local /
/// IPv4-mapped equivalents.
pub fn is_internal_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local() // 169.254.0.0/16 — includes 169.254.169.254
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast() // 224.0.0.0/4 — SSRF vector (Council #1912)
                || o[0] == 0 // 0.0.0.0/8 "this network"
                || (o[0] & 0xf0) == 0xf0 // 240.0.0.0/4 reserved / future-use (incl. 255.*)
                || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15 benchmarking (RFC 2544)
                // 100.64.0.0/10 carrier-grade NAT (shared address space, RFC 6598)
                || (o[0] == 100 && (o[1] & 0xc0) == 0x40)
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped (::ffff:a.b.c.d) AND IPv4-compatible (::a.b.c.d,
            // deprecated) — `to_ipv4` covers both. Re-check as IPv4 so a
            // dual-stack / embedded form can't smuggle an internal address.
            if let Some(v4) = v6.to_ipv4() {
                return is_internal_ip(IpAddr::V4(v4));
            }
            // IPv6 transition mechanisms embed an IPv4 address inside a
            // public-looking IPv6 (Council #1912 round 2 — SSRF evasion):
            //   6to4   2002::/16          → embedded v4 in segments[1..=2]
            //   NAT64  64:ff9b::/96 (WKP) → embedded v4 in segments[6..=7]
            if let Some(embedded) = embedded_ipv4(v6)
                && is_internal_ip(IpAddr::V4(embedded))
            {
                return true;
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast() // ff00::/8 — SSRF vector (Council #1912)
                || is_ipv6_unique_local(v6)   // fc00::/7
                || is_ipv6_link_local(v6) // fe80::/10
        }
    }
}

/// Extract the IPv4 address embedded by an IPv6 transition mechanism (6to4 or
/// the NAT64 well-known prefix), if `v6` uses one. Used so an internal IPv4
/// embedded in a public-looking IPv6 can't bypass the SSRF guard.
fn embedded_ipv4(v6: Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let s = v6.segments();
    let v4_from = |hi: u16, lo: u16| {
        std::net::Ipv4Addr::new(
            (hi >> 8) as u8,
            (hi & 0xff) as u8,
            (lo >> 8) as u8,
            (lo & 0xff) as u8,
        )
    };
    // 6to4: 2002:WWXX:YYZZ::/48 → embedded W.X.Y.Z
    if s[0] == 0x2002 {
        return Some(v4_from(s[1], s[2]));
    }
    // NAT64 well-known prefix 64:ff9b::/96 → embedded v4 in the low 32 bits
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
        return Some(v4_from(s[6], s[7]));
    }
    None
}

/// fc00::/7 (unique local addresses) — `Ipv6Addr::is_unique_local` is unstable,
/// so check the top 7 bits directly.
fn is_ipv6_unique_local(v6: Ipv6Addr) -> bool {
    (v6.octets()[0] & 0xfe) == 0xfc
}

/// fe80::/10 (link-local) — `Ipv6Addr::is_unicast_link_local` is unstable, so
/// check the top 10 bits directly.
fn is_ipv6_link_local(v6: Ipv6Addr) -> bool {
    let [a, b, ..] = v6.octets();
    a == 0xfe && (b & 0xc0) == 0x80
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn policy(mode: EgressMode, global: &[&str]) -> EgressPolicy {
        EgressPolicy::from_config(&EgressConfig {
            mode,
            allowlist_domains: global.iter().map(|s| s.to_string()).collect(),
            tools: HashMap::new(),
        })
    }

    // ---- domain allowlist matching -------------------------------------

    #[test]
    fn strict_allows_exact_match() {
        let p = policy(EgressMode::Strict, &["github.com", "docs.anthropic.com"]);
        assert_eq!(
            p.check_host("web_search", "github.com"),
            EgressDecision::Allow
        );
        assert_eq!(
            p.check_host("web_search", "docs.anthropic.com"),
            EgressDecision::Allow
        );
    }

    #[test]
    fn strict_blocks_non_allowlisted() {
        let p = policy(EgressMode::Strict, &["github.com"]);
        assert!(p.check_host("web_search", "evil.example.com").is_blocked());
    }

    #[test]
    fn match_is_case_and_trailing_dot_insensitive() {
        let p = policy(EgressMode::Strict, &["GitHub.com"]);
        assert_eq!(p.check_host("t", "github.com."), EgressDecision::Allow);
        assert_eq!(p.check_host("t", "GITHUB.COM"), EgressDecision::Allow);
    }

    #[test]
    fn wildcard_matches_subdomains_not_apex() {
        let p = policy(EgressMode::Strict, &["*.example.com"]);
        assert_eq!(p.check_host("t", "a.example.com"), EgressDecision::Allow);
        assert_eq!(p.check_host("t", "a.b.example.com"), EgressDecision::Allow);
        // apex is NOT matched by a wildcard
        assert!(p.check_host("t", "example.com").is_blocked());
        // suffix-confusion must not match
        assert!(p.check_host("t", "notexample.com").is_blocked());
        assert!(p.check_host("t", "evilexample.com").is_blocked());
    }

    #[test]
    fn empty_host_is_blocked_in_strict() {
        let p = policy(EgressMode::Strict, &["github.com"]);
        assert!(p.check_host("t", "").is_blocked());
        assert!(p.check_host("t", "   ").is_blocked());
    }

    // ---- modes ---------------------------------------------------------

    #[test]
    fn warn_allows_non_listed_but_warns() {
        let p = policy(EgressMode::Warn, &["github.com"]);
        // Allowlisted host: clean allow.
        assert_eq!(p.check_host("t", "github.com"), EgressDecision::Allow);
        // Non-allowlisted host under warn: allowed BUT surfaced for logging
        // (not a silent Allow — that was the Council #1912 finding).
        let d = p.check_host("t", "evil.example.com");
        assert!(!d.is_blocked());
        assert!(matches!(d, EgressDecision::AllowWithWarning { .. }));
        assert!(d.reason().unwrap().contains("evil.example.com"));
    }

    #[test]
    fn check_host_strips_port() {
        let p = policy(EgressMode::Strict, &["github.com"]);
        assert_eq!(p.check_host("t", "github.com:443"), EgressDecision::Allow);
        assert_eq!(p.check_host("t", "github.com:8080"), EgressDecision::Allow);
        // bracketed IPv6 literal with port strips to the bare address
        assert_eq!(strip_port("[::1]:443"), "::1");
        assert_eq!(strip_port("[2606:4700::1111]"), "2606:4700::1111");
        // bare IPv6 (no brackets) is left intact (can't disambiguate port)
        assert_eq!(strip_port("2606:4700::1111"), "2606:4700::1111");
    }

    #[test]
    fn empty_ip_set_fails_closed_in_strict() {
        // Council #1912: a host that did not resolve must NOT silently pass the
        // rebinding check in Strict (fail-open → SSRF via resolve/connect TOCTOU).
        let strict = policy(EgressMode::Strict, &["github.com"]);
        assert!(
            strict
                .check_resolved_ips("t", "github.com", &[])
                .is_blocked()
        );
        // Warn permits-with-warning; Off allows.
        let warn = policy(EgressMode::Warn, &["github.com"]);
        assert!(matches!(
            warn.check_resolved_ips("t", "github.com", &[]),
            EgressDecision::AllowWithWarning { .. }
        ));
        let off = policy(EgressMode::Off, &[]);
        assert_eq!(off.check_resolved_ips("t", "h", &[]), EgressDecision::Allow);
    }

    #[test]
    fn internal_ip_blocks_even_in_warn() {
        // Warn softens the domain allowlist, NOT the SSRF/rebinding defense.
        let warn = policy(EgressMode::Warn, &["github.com"]);
        assert!(
            warn.check_resolved_ips(
                "t",
                "github.com",
                &[IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))]
            )
            .is_blocked()
        );
    }

    #[test]
    fn off_allows_everything_including_internal_ips() {
        let p = policy(EgressMode::Off, &[]);
        assert_eq!(p.check_host("t", "anything.example"), EgressDecision::Allow);
        assert_eq!(
            p.check_resolved_ips("t", "h", &[IpAddr::V4(Ipv4Addr::LOCALHOST)]),
            EgressDecision::Allow
        );
    }

    // ---- per-tool override (replaces, not extends) ---------------------

    #[test]
    fn per_tool_override_replaces_global() {
        let mut tools = HashMap::new();
        tools.insert(
            "web_search".to_string(),
            ToolEgressConfig {
                allowlist_domains: vec!["wikipedia.org".to_string()],
            },
        );
        let p = EgressPolicy::from_config(&EgressConfig {
            mode: EgressMode::Strict,
            allowlist_domains: vec!["github.com".to_string()],
            tools,
        });
        // web_search uses ONLY its override list
        assert_eq!(
            p.check_host("web_search", "wikipedia.org"),
            EgressDecision::Allow
        );
        assert!(p.check_host("web_search", "github.com").is_blocked());
        // a tool without an override falls back to the global list
        assert_eq!(
            p.check_host("fetch_url", "github.com"),
            EgressDecision::Allow
        );
        assert!(p.check_host("fetch_url", "wikipedia.org").is_blocked());
    }

    // ---- DNS-rebinding / internal-IP defense --------------------------

    #[test]
    fn resolved_internal_ip_is_blocked_even_if_host_allowlisted() {
        let p = policy(EgressMode::Strict, &["github.com"]);
        // host passes the allowlist...
        assert_eq!(p.check_host("t", "github.com"), EgressDecision::Allow);
        // ...but resolving to loopback is an attack.
        assert!(
            p.check_resolved_ips(
                "t",
                "github.com",
                &[IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))]
            )
            .is_blocked()
        );
    }

    #[test]
    fn cloud_metadata_ip_is_blocked() {
        let p = policy(EgressMode::Strict, &["github.com"]);
        assert!(
            p.check_resolved_ips("t", "h", &[IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))])
                .is_blocked()
        );
    }

    #[test]
    fn rebinding_blocked_when_any_ip_internal() {
        let p = policy(EgressMode::Strict, &["github.com"]);
        let ips = [
            IpAddr::V4(Ipv4Addr::new(140, 82, 121, 4)), // public
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),     // private — poisons the set
        ];
        assert!(p.check_resolved_ips("t", "h", &ips).is_blocked());
    }

    #[test]
    fn public_ip_is_allowed() {
        let p = policy(EgressMode::Strict, &["github.com"]);
        assert_eq!(
            p.check_resolved_ips("t", "h", &[IpAddr::V4(Ipv4Addr::new(140, 82, 121, 4))]),
            EgressDecision::Allow
        );
    }

    #[test]
    fn internal_ip_classification() {
        let internal = [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254", // metadata
            "100.64.0.1",      // CGNAT
            "0.0.0.0",
            "0.1.2.3",         // 0.0.0.0/8 "this network"
            "224.0.0.1",       // multicast (SSRF vector)
            "239.255.255.250", // multicast (SSDP)
            "240.0.0.1",       // 240.0.0.0/4 reserved
            "198.18.0.1",      // benchmarking
            "198.19.255.1",    // benchmarking (upper half of /15)
        ];
        for s in internal {
            assert!(is_internal_ip(s.parse().unwrap()), "{s} should be internal");
        }
        let public = ["8.8.8.8", "140.82.121.4", "1.1.1.1", "198.20.0.1"];
        for s in public {
            assert!(!is_internal_ip(s.parse().unwrap()), "{s} should be public");
        }
    }

    #[test]
    fn internal_ipv6_classification() {
        // loopback, ULA, link-local, IPv4-mapped-loopback
        assert!(is_internal_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_internal_ip("fc00::1".parse().unwrap()));
        assert!(is_internal_ip("fe80::1".parse().unwrap()));
        assert!(is_internal_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_internal_ip("::ffff:10.0.0.1".parse().unwrap()));
        assert!(is_internal_ip("ff02::1".parse().unwrap())); // v6 multicast (SSRF vector)
        // IPv4-compatible (deprecated) ::a.b.c.d embedding loopback
        assert!(is_internal_ip("::127.0.0.1".parse().unwrap()));
        // 6to4 (2002::/16) embedding internal v4 (Council #1912 r2)
        assert!(is_internal_ip("2002:7f00:0001::".parse().unwrap())); // 127.0.0.1
        assert!(is_internal_ip("2002:0a00:0001::".parse().unwrap())); // 10.0.0.1
        // NAT64 well-known prefix embedding internal v4
        assert!(is_internal_ip("64:ff9b::7f00:1".parse().unwrap())); // 127.0.0.1
        assert!(is_internal_ip("64:ff9b::a00:1".parse().unwrap())); // 10.0.0.1
        // public v6, and 6to4 embedding a PUBLIC v4 (must NOT over-block)
        assert!(!is_internal_ip("2606:4700:4700::1111".parse().unwrap()));
        assert!(!is_internal_ip("2002:0808:0808::".parse().unwrap())); // 6to4 for 8.8.8.8
        assert!(!is_internal_ip("64:ff9b::808:808".parse().unwrap())); // NAT64 for 8.8.8.8
    }

    #[test]
    fn idna_punycode_host_matches_unicode_allowlist() {
        // A Unicode allowlist entry matches its punycode / case-folded host.
        let p = policy(EgressMode::Strict, &["café.com"]);
        assert_eq!(p.check_host("t", "xn--caf-dma.com"), EgressDecision::Allow);
        assert_eq!(p.check_host("t", "CAFÉ.com"), EgressDecision::Allow);
        assert!(p.check_host("t", "evil.com").is_blocked());
        // Wildcard with a Unicode label folds consistently.
        let pw = policy(EgressMode::Strict, &["*.café.com"]);
        assert_eq!(
            pw.check_host("t", "a.xn--caf-dma.com"),
            EgressDecision::Allow
        );
        assert!(pw.check_host("t", "café.com").is_blocked()); // apex not matched by wildcard
    }

    #[test]
    fn strip_port_malformed_brackets_fail_safe() {
        assert_eq!(strip_port("[::1]:443"), "::1");
        assert_eq!(strip_port("[::1"), "[::1"); // unterminated — left as-is
        assert_eq!(strip_port("[]"), ""); // empty brackets
        let p = policy(EgressMode::Strict, &["github.com"]);
        // malformed hosts never match a real allowlist entry → blocked in strict
        assert!(p.check_host("t", "[::1").is_blocked());
        assert!(p.check_host("t", "[]").is_blocked());
    }

    // ---- file-only SecurityConfig parse (option c) --------------------

    #[test]
    fn security_config_parses_only_the_security_table() {
        let toml_src = r#"
            # unrelated tables that must be ignored
            [device]
            name = "Mikro"

            [[modes]]
            name = "Default"

            [security.egress]
            allowlist_mode = "strict"
            allowlist_domains = ["github.com", "*.anthropic.com"]

            [security.egress.tools.web_search]
            allowlist_domains = ["wikipedia.org"]
        "#;
        let sec = SecurityConfig::from_toml_str(toml_src).expect("parse");
        assert_eq!(sec.egress.mode, EgressMode::Strict);
        assert_eq!(
            sec.egress.allowlist_domains,
            vec!["github.com", "*.anthropic.com"]
        );
        let policy = EgressPolicy::from_config(&sec.egress);
        assert_eq!(
            policy.check_host("web_search", "wikipedia.org"),
            EgressDecision::Allow
        );
        assert!(policy.check_host("web_search", "github.com").is_blocked());
    }

    #[test]
    fn security_config_absent_table_is_default_strict() {
        let sec = SecurityConfig::from_toml_str("[[modes]]\nname = \"Default\"\n").expect("parse");
        assert_eq!(sec.egress.mode, EgressMode::Strict);
        assert!(sec.egress.allowlist_domains.is_empty());
    }

    #[test]
    fn malformed_allowlist_entries_dropped_not_widening() {
        // "*." with no suffix, and empty entries, must not create a match-all.
        let p = policy(EgressMode::Strict, &["*.", "", "   "]);
        assert!(p.check_host("t", "anything.com").is_blocked());
    }
}
