// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase A — listener manager + ordered edge (Slice A.6a).
//!
//! [`ListenerManager`] owns one [`ListenerEdge`] per enabled Input/Bidirectional
//! OSC/Art-Net endpoint. Each edge bundles that listener's [`AclFilter`] (A.3),
//! [`RateLimitEdge`] (A.4) and [`AuditRateLimiter`] (A.5), and runs the
//! per-packet checks in the order mandated by spec §4.2.1:
//!
//! > **ACL filter FIRST, then the rate-limit edge.**
//!
//! That order is a security property (Council R1 G2): if the rate limiter ran
//! first, an off-ACL attacker could drain a listener's rate-limit budget with
//! packets the ACL would have rejected anyway, starving legitimate senders.
//! [`ListenerEdge::admit`] therefore returns at the ACL stage for an off-ACL
//! source, never touching the rate buckets.
//!
//! Socket binding, the receive loop, and audit emission are wired in Slice A.6b;
//! this slice is the socket-free edge + manager so the ordering guarantee can be
//! proven in isolation.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use conductor_core::config::types::{Config, ConnectorDirection, ConnectorProtocol, EndpointKind};

use crate::listeners::{
    AclFilter, AclFilterError, AuditEventKind, AuditRateLimiter, RateLimitEdge,
};

/// Loopback-default ACL applied when a listener declares no explicit
/// `network_acl` (spec D1). Phase A binds loopback only, so a bare listener
/// should still admit loopback sources rather than deny-all (an empty
/// [`crate::listeners::AclFilter`] rejects everything).
const LOOPBACK_DEFAULT_ACL: &[&str] = &["127.0.0.0/8", "::1/128"];

/// Default per-protocol rate limits (spec §5 A.4): `(total_rps, per_sender_rps)`.
const OSC_DEFAULT_RATE: (u32, u32) = (1000, 200);
const ARTNET_DEFAULT_RATE: (u32, u32) = (100, 50);

/// Outcome of running a packet's source IP through a listener edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeDecision {
    /// Source passed the ACL and the rate limiter — hand to the parser.
    Accept,
    /// Source rejected by the ACL filter (the rate limiter was NOT consulted).
    RejectAcl,
    /// Source passed the ACL but is over a rate-limit bucket.
    RejectRateLimit,
}

/// The per-listener security edge: ACL → rate-limit (→ audit at emission time).
pub struct ListenerEdge {
    alias: String,
    protocol: ConnectorProtocol,
    host: IpAddr,
    port: u16,
    allow_sensitive_actions: bool,
    acl: AclFilter,
    rate: RateLimitEdge,
    audit: AuditRateLimiter,
}

impl ListenerEdge {
    /// Assemble an edge from its already-built primitives.
    pub fn new(
        alias: String,
        protocol: ConnectorProtocol,
        host: IpAddr,
        port: u16,
        allow_sensitive_actions: bool,
        acl: AclFilter,
        rate: RateLimitEdge,
    ) -> Self {
        Self {
            alias,
            protocol,
            host,
            port,
            allow_sensitive_actions,
            acl,
            rate,
            audit: AuditRateLimiter::new(),
        }
    }

    /// Run the edge in the mandated order (spec §4.2.1): ACL filter first, then
    /// the rate-limit edge. An off-ACL source returns [`EdgeDecision::RejectAcl`]
    /// *before* the rate limiter is consulted, so it never consumes a token.
    pub fn admit(&self, source: IpAddr) -> EdgeDecision {
        if !self.acl.check(source) {
            return EdgeDecision::RejectAcl;
        }
        match self.rate.check(source) {
            Ok(()) => EdgeDecision::Accept,
            Err(_) => EdgeDecision::RejectRateLimit,
        }
    }

    /// Whether an audit line for `(this listener, source, kind)` should be
    /// emitted now (dedup window, spec §4.4). Emission itself is wired in A.6b.
    pub fn should_audit(&self, source: IpAddr, kind: AuditEventKind) -> bool {
        self.audit.should_emit(&self.alias, source, kind)
    }

    /// Connector alias of this listener.
    pub fn alias(&self) -> &str {
        &self.alias
    }
    /// Protocol spoken by this listener (`Osc` or `ArtNet`).
    pub fn protocol(&self) -> ConnectorProtocol {
        self.protocol
    }
    /// Bound host (loopback in Phase A).
    pub fn host(&self) -> IpAddr {
        self.host
    }
    /// Bound UDP port.
    pub fn port(&self) -> u16 {
        self.port
    }
    /// D17 action-class gate flag for this listener (Slice A.6.6 reads this).
    pub fn allow_sensitive_actions(&self) -> bool {
        self.allow_sensitive_actions
    }
}

/// Error building a [`ListenerManager`] from config.
#[derive(Debug, thiserror::Error)]
pub enum ListenerError {
    /// A listener's ACL / sender-ACL config was rejected (D11 hardening, etc.).
    #[error("listener '{alias}': {source}")]
    Acl {
        alias: String,
        #[source]
        source: AclFilterError,
    },
    /// A listener `host` could not be parsed as an IP address.
    #[error("listener '{alias}': host '{host}' is not a valid IP address")]
    BadHost { alias: String, host: String },
}

/// Owns the security edge for every network listener declared in the config.
pub struct ListenerManager {
    edges: HashMap<String, ListenerEdge>,
}

impl ListenerManager {
    /// Build one [`ListenerEdge`] per enabled Input/Bidirectional OSC/Art-Net
    /// endpoint. Output-only and non-network (MIDI/HID) endpoints are skipped.
    /// A listener with no explicit `network_acl` gets the loopback-default ACL.
    pub fn from_config(config: &Config) -> Result<Self, ListenerError> {
        let mut edges = HashMap::new();

        for ep in &config.endpoints {
            if !ep.enabled {
                continue;
            }
            if !matches!(
                ep.direction,
                ConnectorDirection::Input | ConnectorDirection::Bidirectional
            ) {
                continue;
            }

            // Only OSC / Art-Net endpoints are network listeners.
            let (protocol, host_str, port, allow_broadcast, security) = match &ep.kind {
                EndpointKind::OscEndpoint {
                    host,
                    port,
                    security,
                } => (
                    ConnectorProtocol::Osc,
                    host.as_str(),
                    *port,
                    false,
                    security,
                ),
                EndpointKind::ArtNetEndpoint {
                    host,
                    port,
                    allow_broadcast,
                    security,
                    ..
                } => (
                    ConnectorProtocol::ArtNet,
                    host.as_str(),
                    *port,
                    *allow_broadcast,
                    security,
                ),
                EndpointKind::Matcher { .. } | EndpointKind::MidiVirtualPort { .. } => continue,
            };

            let host = parse_host(host_str).ok_or_else(|| ListenerError::BadHost {
                alias: ep.alias.clone(),
                host: host_str.to_string(),
            })?;

            // Loopback-default ACL (spec D1) when none is declared.
            let acl_entries: Vec<String> = if security.network_acl.is_empty() {
                LOOPBACK_DEFAULT_ACL.iter().map(|s| s.to_string()).collect()
            } else {
                security.network_acl.clone()
            };
            let acl = AclFilter::from_config(
                &acl_entries,
                &security.sender_acl,
                allow_broadcast,
                security.i_understand_amplification_risk,
            )
            .map_err(|source| ListenerError::Acl {
                alias: ep.alias.clone(),
                source,
            })?;

            let (default_total, default_per) = match protocol {
                ConnectorProtocol::ArtNet => ARTNET_DEFAULT_RATE,
                _ => OSC_DEFAULT_RATE,
            };
            let total = security.rate_limit_total.unwrap_or(default_total);
            let per_sender = security.rate_limit_per_sender.unwrap_or(default_per);
            let rate = match protocol {
                ConnectorProtocol::ArtNet => RateLimitEdge::for_artnet(total, per_sender),
                _ => RateLimitEdge::for_osc(total, per_sender),
            };

            edges.insert(
                ep.alias.clone(),
                ListenerEdge::new(
                    ep.alias.clone(),
                    protocol,
                    host,
                    port,
                    security.allow_sensitive_actions,
                    acl,
                    rate,
                ),
            );
        }

        Ok(Self { edges })
    }

    /// The edge for a listener alias, if one was built.
    pub fn edge(&self, alias: &str) -> Option<&ListenerEdge> {
        self.edges.get(alias)
    }

    /// Consume the manager, yielding the built edges for spawning (A.6b-3b).
    pub fn into_edges(self) -> impl Iterator<Item = ListenerEdge> {
        self.edges.into_values()
    }

    /// Listener aliases (for `GetListenerStatus` in Slice A.6b).
    pub fn aliases(&self) -> impl Iterator<Item = &str> {
        self.edges.keys().map(String::as_str)
    }

    /// Number of network listeners.
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Whether there are no network listeners.
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

/// Resolve a listener `host` string to an [`IpAddr`], accepting the `localhost`
/// alias. The A.2 validator has already proven Phase A hosts are loopback.
fn parse_host(host: &str) -> Option<IpAddr> {
    if host == "localhost" {
        return Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    host.parse().ok()
}
