// ADR-042 Phase A — Slice A.6.1: orphaned-listener detection.
//
// A UDP socket is reclaimed by the kernel when its owning process dies, so the
// detectable "orphan" is a configured listener port already held by another
// LIVE process (most likely a second/stale conductor instance) — surfaced as an
// AddrInUse bind error. is_orphaned_port() classifies that so the daemon emits
// ListenerOrphanedAtStartup (detection only — the listener is skipped, the
// holder is never force-killed).
//
// Acceptance (issue #1898, spec §5 A.6.1):
//   cargo test --package conductor-daemon --test startup_cleanup_test

use std::net::UdpSocket;

use conductor_daemon::listeners::startup_cleanup::is_orphaned_port;

#[test]
fn port_already_in_use_is_classified_as_orphan() {
    // Hold a loopback UDP port, then attempt a second default bind on it.
    let held = UdpSocket::bind("127.0.0.1:0").expect("bind first socket");
    let port = held.local_addr().unwrap().port();

    let err =
        UdpSocket::bind(("127.0.0.1", port)).expect_err("second bind on the same port must fail");
    assert!(
        is_orphaned_port(&err),
        "a port-in-use bind error is an orphan/conflict signal, got: {:?}",
        err.kind()
    );
}

#[test]
fn free_port_binds_without_orphan() {
    // A fresh ephemeral port binds cleanly — no orphan.
    let sock = UdpSocket::bind("127.0.0.1:0").expect("bind clean");
    assert!(sock.local_addr().unwrap().port() != 0);
}
