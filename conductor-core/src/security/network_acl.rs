// Copyright 2026 Amiable
// SPDX-License-Identifier: MIT

//! ADR-042 Phase A — network ACL primitive (Slice A.1).
//!
//! A [`NetworkAcl`] is a compiled set of CIDR entries that gates which source
//! IPs a network listener will accept. It is a pure, std/`ipnet`-only value
//! type: parse once at config-load, then [`NetworkAcl::contains`] per packet at
//! the listener edge (before the rate limiter — spec §4.2.1).
//!
//! Hardened defaults (ADR-042 D11), enforced at [`NetworkAcl::parse`]:
//!
//! - `0.0.0.0/0` and `::/0` are rejected outright ("trust the whole internet"
//!   is never the answer).
//! - When `allow_broadcast` is set without `i_understand_amplification_risk`,
//!   the **aggregate** host count across all entries is checked against a `/24`
//!   (IPv4) / `/64` (IPv6) budget. Aggregate — not per-entry — so an operator
//!   (or an attacker editing config) cannot shard a broad range into smaller
//!   blocks to slip under a per-entry check (Council #1912).
//! - IPv6 link-local (`fe80::/10`) entries emit a deprecation warning.
//!
//! [`NetworkAcl::contains`] normalizes IPv4-mapped IPv6 (`::ffff:a.b.c.d`) to
//! its v4 form before matching, so a dual-stack socket delivering a mapped peer
//! cannot bypass an IPv4 ACL entry.

use std::net::{IpAddr, Ipv6Addr};
use std::str::FromStr;

use ipnet::IpNet;

/// A compiled allow-list of CIDR entries.
#[derive(Debug, Clone, Default)]
pub struct NetworkAcl {
    entries: Vec<IpNet>,
}

/// IPv4 aggregate budget: a `/24` worth of addresses (256). Aggregate host
/// count strictly greater than this requires the amplification acknowledgement.
const V4_AGGREGATE_MAX: u128 = 256;
/// IPv6 aggregate budget: a `/64` worth of addresses.
const V6_AGGREGATE_MAX: u128 = 1u128 << 64;

/// Hard rejection at parse time. (Link-local is a *warning*, see [`AclWarning`].)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AclError {
    #[error("invalid CIDR: {0}")]
    InvalidCidr(String),
    #[error("CIDR 0.0.0.0/0 (entire IPv4 internet) is rejected")]
    AnyV4Rejected,
    #[error("CIDR ::/0 (entire IPv6 internet) is rejected")]
    AnyV6Rejected,
    #[error(
        "allow_broadcast with an IPv4 ACL spanning {0} addresses exceeds a /24 ({V4_AGGREGATE_MAX}); \
         set i_understand_amplification_risk to override"
    )]
    AmplificationRiskV4(u128),
    #[error(
        "allow_broadcast with an IPv6 ACL spanning ~2^{0} addresses exceeds a /64; \
         set i_understand_amplification_risk to override"
    )]
    AmplificationRiskV6(u32),
}

/// Non-fatal parse warnings (the ACL still loads).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclWarning {
    /// An `fe80::/10` link-local entry — accessible from the whole L2 segment.
    Ipv6LinkLocal(String),
}

impl NetworkAcl {
    /// Parse CIDR strings into a compiled ACL, applying the D11 hardened
    /// defaults. Returns the ACL plus any non-fatal warnings.
    pub fn parse(
        raw: &[String],
        allow_broadcast: bool,
        risk_acknowledged: bool,
    ) -> Result<(Self, Vec<AclWarning>), AclError> {
        let mut entries = Vec::with_capacity(raw.len());
        let mut warnings = Vec::new();

        for s in raw {
            let cidr = IpNet::from_str(s.trim()).map_err(|_| AclError::InvalidCidr(s.clone()))?;

            // D11: reject the whole-internet CIDRs outright.
            match cidr {
                IpNet::V4(v4) if v4.prefix_len() == 0 => return Err(AclError::AnyV4Rejected),
                IpNet::V6(v6) if v6.prefix_len() == 0 => return Err(AclError::AnyV6Rejected),
                _ => {}
            }

            // D1: IPv6 link-local is reachable from the whole segment — warn.
            if let IpNet::V6(v6) = cidr
                && is_v6_link_local(v6.network())
            {
                warnings.push(AclWarning::Ipv6LinkLocal(s.clone()));
            }

            entries.push(cidr);
        }

        // D11 (R6): AGGREGATE amplification budget — sum host counts per family
        // so sharding a broad range into smaller blocks can't evade the check.
        if allow_broadcast && !risk_acknowledged {
            let mut v4_total: u128 = 0;
            let mut v6_total: u128 = 0;
            for e in &entries {
                match e {
                    IpNet::V4(v4) => {
                        v4_total = v4_total.saturating_add(1u128 << (32 - v4.prefix_len()));
                    }
                    IpNet::V6(v6) => {
                        // Saturate: a single small-prefix v6 entry dwarfs the budget.
                        let bits = 128 - v6.prefix_len() as u32;
                        v6_total =
                            v6_total.saturating_add(1u128.checked_shl(bits).unwrap_or(u128::MAX));
                    }
                }
            }
            if v4_total > V4_AGGREGATE_MAX {
                return Err(AclError::AmplificationRiskV4(v4_total));
            }
            if v6_total > V6_AGGREGATE_MAX {
                // Report the log2 of the span for a readable message.
                return Err(AclError::AmplificationRiskV6(
                    128 - smallest_v6_prefix(&entries),
                ));
            }
        }

        Ok((Self { entries }, warnings))
    }

    /// `true` if `addr` is permitted by the ACL. IPv4-mapped IPv6
    /// (`::ffff:a.b.c.d`) is normalized to its v4 form before matching so a
    /// dual-stack socket can't bypass an IPv4 entry.
    pub fn contains(&self, addr: &IpAddr) -> bool {
        let norm = match addr {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(*addr),
            v4 => *v4,
        };
        self.entries.iter().any(|e| e.contains(&norm))
    }

    /// `true` if the ACL has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of CIDR entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// ADR-042 §4.2 (G3) — IPv6 dual-stack loopback scoping. Recognises all
    /// three loopback forms so a dual-stack socket can't slip a loopback peer
    /// past a naive `127.`-prefix check: `127.0.0.0/8` (v4), `::1` (v6), and
    /// the IPv4-mapped loopback `::ffff:127.0.0.0/104`.
    pub fn is_loopback_address(addr: &IpAddr) -> bool {
        match addr {
            IpAddr::V4(v4) => v4.is_loopback(),
            IpAddr::V6(v6) => {
                v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
            }
        }
    }

    /// `true` iff every entry's network address is a loopback address. The
    /// A.2 validator uses this for the "is this listener loopback-only?" check;
    /// an empty ACL is **not** loopback-only (vacuously-true would let an
    /// unconfigured listener masquerade as safe).
    pub fn is_loopback_only(&self) -> bool {
        !self.entries.is_empty()
            && self
                .entries
                .iter()
                .all(|e| Self::is_loopback_address(&e.network()))
    }

    /// Phase B-late hybrid-trust helper: is every entry of `self` a subset of
    /// some entry of `other`?
    pub fn is_strict_subset_of(&self, other: &NetworkAcl) -> bool {
        !self.entries.is_empty()
            && self
                .entries
                .iter()
                .all(|a| other.entries.iter().any(|b| cidr_is_subset_of(*a, *b)))
    }
}

/// `fe80::/10` link-local detection — `Ipv6Addr::is_unicast_link_local` is
/// unstable, so check the top 10 bits directly.
fn is_v6_link_local(a: Ipv6Addr) -> bool {
    let o = a.octets();
    o[0] == 0xfe && (o[1] & 0xc0) == 0x80
}

/// The smallest prefix length among IPv6 entries (the broadest entry), for the
/// amplification error message.
fn smallest_v6_prefix(entries: &[IpNet]) -> u32 {
    entries
        .iter()
        .filter_map(|e| match e {
            IpNet::V6(v6) => Some(v6.prefix_len() as u32),
            _ => None,
        })
        .min()
        .unwrap_or(128)
}

fn cidr_is_subset_of(a: IpNet, b: IpNet) -> bool {
    match (a, b) {
        (IpNet::V4(a4), IpNet::V4(b4)) => b4.contains(&a4),
        (IpNet::V6(a6), IpNet::V6(b6)) => b6.contains(&a6),
        _ => false,
    }
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
    fn rejects_any_v4_and_v6() {
        let any4 = NetworkAcl::parse(&["0.0.0.0/0".into()], false, false);
        assert_eq!(any4.unwrap_err(), AclError::AnyV4Rejected);
        let any6 = NetworkAcl::parse(&["::/0".into()], false, false);
        assert_eq!(any6.unwrap_err(), AclError::AnyV6Rejected);
    }

    #[test]
    fn rejects_invalid_cidr() {
        assert!(matches!(
            NetworkAcl::parse(&["not-a-cidr".into()], false, false),
            Err(AclError::InvalidCidr(_))
        ));
    }

    #[test]
    fn contains_matches_v4_and_v6() {
        let a = acl(&["192.168.1.0/24", "10.0.0.0/8", "2001:db8::/32"]);
        assert!(a.contains(&"192.168.1.50".parse().unwrap()));
        assert!(a.contains(&"10.5.6.7".parse().unwrap()));
        assert!(a.contains(&"2001:db8::1".parse().unwrap()));
        assert!(!a.contains(&"8.8.8.8".parse().unwrap()));
        assert!(!a.contains(&"192.168.2.1".parse().unwrap()));
    }

    #[test]
    fn ipv4_mapped_ipv6_is_normalized() {
        // A dual-stack socket may deliver an IPv4 peer as ::ffff:a.b.c.d;
        // it must match the IPv4 ACL entry.
        let a = acl(&["192.168.1.0/24"]);
        assert!(a.contains(&"::ffff:192.168.1.50".parse().unwrap()));
        assert!(!a.contains(&"::ffff:8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn aggregate_amplification_blocks_broad_with_broadcast() {
        // /24 worth (256) is the inclusive max.
        assert!(NetworkAcl::parse(&["10.0.0.0/24".into()], true, false).is_ok());
        // a /23 (512) exceeds it.
        assert!(matches!(
            NetworkAcl::parse(&["10.0.0.0/23".into()], true, false),
            Err(AclError::AmplificationRiskV4(512))
        ));
        // SHARD BYPASS CLOSED: a /23 split into two /24s still aggregates to 512.
        assert!(matches!(
            NetworkAcl::parse(&["10.0.0.0/24".into(), "10.0.1.0/24".into()], true, false),
            Err(AclError::AmplificationRiskV4(512))
        ));
        // two /25s == a /24 (256) — at the inclusive boundary, allowed.
        assert!(
            NetworkAcl::parse(&["10.0.0.0/25".into(), "10.0.0.128/25".into()], true, false).is_ok()
        );
    }

    #[test]
    fn amplification_check_skipped_without_broadcast_or_with_ack() {
        // No broadcast → no amplification check even for a broad range.
        assert!(NetworkAcl::parse(&["10.0.0.0/8".into()], false, false).is_ok());
        // Broadcast + acknowledged → allowed.
        assert!(NetworkAcl::parse(&["10.0.0.0/8".into()], true, true).is_ok());
    }

    #[test]
    fn ipv6_aggregate_amplification() {
        // A /48 (broader than /64) with broadcast and no ack → blocked.
        assert!(matches!(
            NetworkAcl::parse(&["2001:db8::/48".into()], true, false),
            Err(AclError::AmplificationRiskV6(_))
        ));
        // A /64 is the inclusive max → allowed.
        assert!(NetworkAcl::parse(&["2001:db8::/64".into()], true, false).is_ok());
    }

    #[test]
    fn ipv6_link_local_warns_but_loads() {
        let (a, warnings) = NetworkAcl::parse(&["fe80::/64".into()], false, false).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(
            warnings,
            vec![AclWarning::Ipv6LinkLocal("fe80::/64".into())]
        );
    }

    #[test]
    fn is_loopback_address_covers_all_three_forms() {
        assert!(NetworkAcl::is_loopback_address(
            &"127.0.0.1".parse().unwrap()
        ));
        // anywhere in 127.0.0.0/8
        assert!(NetworkAcl::is_loopback_address(
            &"127.5.6.7".parse().unwrap()
        ));
        assert!(NetworkAcl::is_loopback_address(&"::1".parse().unwrap()));
        // IPv4-mapped loopback
        assert!(NetworkAcl::is_loopback_address(
            &"::ffff:127.0.0.1".parse().unwrap()
        ));
        // non-loopback
        assert!(!NetworkAcl::is_loopback_address(
            &"192.168.1.1".parse().unwrap()
        ));
        assert!(!NetworkAcl::is_loopback_address(&"::".parse().unwrap()));
        assert!(!NetworkAcl::is_loopback_address(
            &"::ffff:8.8.8.8".parse().unwrap()
        ));
    }

    #[test]
    fn is_loopback_only() {
        assert!(acl(&["127.0.0.0/8"]).is_loopback_only());
        assert!(acl(&["::1/128"]).is_loopback_only());
        assert!(acl(&["::ffff:127.0.0.0/104"]).is_loopback_only());
        assert!(acl(&["127.0.0.0/8", "::1/128"]).is_loopback_only());
        // a single non-loopback entry breaks it
        assert!(!acl(&["127.0.0.0/8", "192.168.1.0/24"]).is_loopback_only());
        // empty ACL is NOT loopback-only (no vacuous truth)
        assert!(!NetworkAcl::default().is_loopback_only());
    }

    #[test]
    fn strict_subset() {
        let inner = acl(&["192.168.1.0/25"]);
        let outer = acl(&["192.168.1.0/24"]);
        assert!(inner.is_strict_subset_of(&outer));
        assert!(!outer.is_strict_subset_of(&inner));
        // multi-entry: every entry must be covered
        let multi = acl(&["192.168.1.0/25", "10.0.0.0/16"]);
        let trusted = acl(&["192.168.1.0/24", "10.0.0.0/8"]);
        assert!(multi.is_strict_subset_of(&trusted));
        // empty ACL is not a subset of anything
        assert!(!NetworkAcl::default().is_strict_subset_of(&outer));
    }
}
