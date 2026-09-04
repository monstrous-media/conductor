// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-042 Phase A — orphaned-listener detection.
//!
//! **Deviation from spec §5 A.6.1 (deliberate, documented):** the spec proposed
//! scanning the OS process table (`lsof -iUDP -c conductor` / `ss -lunp`) for
//! UDP sockets bound by a prior crashed daemon. That is fragile (output parsing,
//! per-OS differences), not CI-testable, and — crucially — **a UDP socket is
//! reclaimed by the kernel the instant its owning process dies**, so a socket
//! "left bound by a crashed daemon and not held by anyone now" does not exist.
//!
//! The real, detectable orphan signal is a **bind conflict**: a configured
//! listener port is already held by *another live process* — most likely a
//! second/stale conductor instance. We detect that at bind time
//! ([`is_orphaned_port`]): an `AddrInUse` error → emit `ListenerOrphanedAtStartup`
//! and log the operator hint. Phase A is **detection only** — we never force-kill
//! the holder; the listener is skipped and the rest of the daemon proceeds.

use std::io;

/// Operator hint logged (and recorded in the audit summary) when a listener
/// port is already in use at startup.
pub const ORPHAN_HINT: &str = "verify no other conductor instance is running";

/// `true` if a listener bind error indicates an orphaned / conflicting socket —
/// i.e. another process already holds the port (`AddrInUse`). Other bind errors
/// (permissions, invalid address) are genuine `NetworkListenerBindFailed`s, not
/// orphans.
pub fn is_orphaned_port(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::AddrInUse
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_in_use_is_an_orphan() {
        let err = io::Error::from(io::ErrorKind::AddrInUse);
        assert!(is_orphaned_port(&err));
    }

    #[test]
    fn other_errors_are_not_orphans() {
        assert!(!is_orphaned_port(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert!(!is_orphaned_port(&io::Error::from(
            io::ErrorKind::AddrNotAvailable
        )));
    }
}
