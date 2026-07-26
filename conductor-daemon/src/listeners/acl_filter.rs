// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-042 Phase A — ACL filter at the listener edge (Slice A.3).
//!
//! An [`AclFilter`] is the per-packet source-IP gate for a network listener.
//! It wraps a compiled [`NetworkAcl`] (CIDR allow-list) plus an optional
//! `sender_acl` (individual sender IPs). [`AclFilter::check`] runs at the
//! listener edge **before** the rate limiter (spec §4.2.1):
//!
//! 1. the source must be inside the CIDR `network_acl`; then
//! 2. if `sender_acl` is non-empty, the source must also be one of its IPs.
//!
//! An empty `sender_acl` means "no per-sender narrowing" — the CIDR allow-list
//! is the only gate. Both checks normalize IPv4-mapped IPv6 (`::ffff:a.b.c.d`)
//! to its v4 form so a dual-stack socket can't bypass an IPv4 entry — the same
//! invariant [`NetworkAcl::contains`] enforces.

use std::net::IpAddr;

use conductor_core::security::{AclError, NetworkAcl};

/// Source-IP allow-list gate for a network listener.
#[derive(Debug, Clone)]
pub struct AclFilter {
    network_acl: NetworkAcl,
    /// Explicit sender IPs (already normalized). Empty = no per-sender gate.
    sender_acl: Vec<IpAddr>,
}

/// Error building an [`AclFilter`] from config strings.
#[derive(Debug, thiserror::Error)]
pub enum AclFilterError {
    /// The `network_acl` failed to parse / was rejected by D11 hardening.
    #[error(transparent)]
    NetworkAcl(#[from] AclError),
    /// A `sender_acl` entry is not a valid IP address.
    #[error("invalid sender_acl IP: {0}")]
    InvalidSenderIp(String),
}

impl AclFilter {
    /// Build from an already-compiled [`NetworkAcl`] and a list of explicit
    /// sender IPs (empty = no per-sender narrowing).
    pub fn new(network_acl: NetworkAcl, sender_acl: Vec<IpAddr>) -> Self {
        let sender_acl = sender_acl.iter().map(|ip| normalize_mapped(*ip)).collect();
        Self {
            network_acl,
            sender_acl,
        }
    }

    /// Build from raw config strings: parse `network_acl` through
    /// [`NetworkAcl::parse`] (applying the D11 hardening / amplification
    /// budget) and parse each `sender_acl` entry as an [`IpAddr`].
    pub fn from_config(
        network_acl: &[String],
        sender_acl: &[String],
        allow_broadcast: bool,
        risk_acknowledged: bool,
    ) -> Result<Self, AclFilterError> {
        let (acl, _warnings) = NetworkAcl::parse(network_acl, allow_broadcast, risk_acknowledged)?;
        let senders = sender_acl
            .iter()
            .map(|s| {
                s.trim()
                    .parse::<IpAddr>()
                    .map_err(|_| AclFilterError::InvalidSenderIp(s.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(acl, senders))
    }

    /// `true` if `source` is permitted: inside the CIDR `network_acl` and —
    /// when `sender_acl` is populated — one of its explicit IPs.
    pub fn check(&self, source: IpAddr) -> bool {
        if !self.network_acl.contains(&source) {
            return false;
        }
        if self.sender_acl.is_empty() {
            return true;
        }
        let norm = normalize_mapped(source);
        self.sender_acl.contains(&norm)
    }

    /// `true` if a per-sender allow-list is configured (narrows `network_acl`).
    pub fn has_sender_acl(&self) -> bool {
        !self.sender_acl.is_empty()
    }
}

/// Normalize an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) to its v4 form;
/// other addresses pass through unchanged. Delegates to the shared edge helper
/// so all stages (ACL / rate-limit / audit) normalize identically.
fn normalize_mapped(addr: IpAddr) -> IpAddr {
    crate::listeners::normalize_mapped_ip(addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acl(entries: &[&str]) -> NetworkAcl {
        NetworkAcl::parse(
            &entries.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            false,
            false,
        )
        .expect("parse")
        .0
    }

    #[test]
    fn empty_sender_acl_uses_cidr_only() {
        let f = AclFilter::new(acl(&["10.0.0.0/8"]), vec![]);
        assert!(!f.has_sender_acl());
        assert!(f.check("10.1.2.3".parse().unwrap()));
        assert!(!f.check("192.168.0.1".parse().unwrap()));
    }

    #[test]
    fn sender_acl_narrows_cidr() {
        let f = AclFilter::new(acl(&["10.0.0.0/8"]), vec!["10.0.0.5".parse().unwrap()]);
        assert!(f.has_sender_acl());
        assert!(f.check("10.0.0.5".parse().unwrap()));
        assert!(!f.check("10.0.0.6".parse().unwrap()));
    }
}
