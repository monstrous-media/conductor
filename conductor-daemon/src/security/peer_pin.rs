// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Peer credential pinning (ADR-027 D1, spec §4.1).
//!
//! ## What this is
//!
//! When a peer connects to the daemon's Unix socket, we want to know:
//!
//! 1. **Who** they are — their uid and pid (the latter is captured
//!    on both Linux via `SO_PEERCRED` and macOS via the separate
//!    `getsockopt(LOCAL_PEERPID)` call).
//! 2. **What** binary they are running — the path to the executable
//!    image at the moment of connection.
//! 3. **Whether** they are still alive when a follow-up MCP tool
//!    call arrives mid-session and we need to re-verify before
//!    granting elevated capabilities.
//!
//! [`PinnedPeer`] captures all three. The gate's [`CallerContext`]
//! (D5 2/N) eventually carries an `Arc<PinnedPeer>` so every gate
//! decision is bound to the verified peer identity.
//!
//! ## Sub-piece scope
//!
//! D1 ships in three sub-pieces:
//!
//! - **(1/N)** (landed pre-migration): the credential-snapshot scaffold —
//!   `SO_PEERCRED` (Linux) and `getpeereid` + `LOCAL_PEERPID`
//!   (macOS) for uid/pid, `/proc/<pid>/exe` or `proc_pidpath` for
//!   the binary path. `still_pinned()` was a stub returning `true`.
//! - **(2/N)** (landed pre-migration): the kernel-handle liveness check —
//!   `pidfd_open` (Linux) + `LOCAL_PEERTOKEN` audit-token
//!   comparison (macOS). [`PinnedPeer::still_pinned`] is race-free
//!   with respect to the peer's exit.
//! - **(3/N)** (this module's current state): peer trust
//!   classification via [`classify_peer`] —
//!   `SecCodeCopyGuestWithAttributes` on macOS for Apple Team ID
//!   extraction (cross-checked against [`CONDUCTOR_TEAM_IDS`]), exe
//!   basename + uid match on Linux. Maps a [`PinnedPeer`] to the
//!   gate's existing [`crate::security::TrustLevel`] enum
//!   (`GuiTrusted` / `CliTrusted` / `Untrusted`) — the wiring
//!   sub-piece (still pending) plumbs the result through
//!   `CallerContext` so D5's decision table can consume it.
//!
//! Raw `libc` rather than `std::os::unix::net::UnixStream::peer_cred`
//! because the stdlib method is gated behind the unstable
//! `peer_credentials_unix_socket` feature (rust-lang/rust#42839)
//! and isn't available on stable.
//!
//! ## (2/N) liveness primitives
//!
//! - **Linux**: [`libc::syscall`] with `SYS_pidfd_open` returns a
//!   kernel-managed file descriptor that becomes readable
//!   (`POLLIN`) once the target process has exited. Stored in
//!   [`PinnedPeer`]; [`still_pinned`] runs a non-blocking
//!   [`libc::poll`] and returns `false` if `POLLIN` is set. Race
//!   safety: the `pidfd` pins the kernel-side process descriptor at
//!   `pin_linux` time, so even if the original PID is recycled to
//!   a different process before `still_pinned` is called, the
//!   `pidfd` continues to refer to the *original* (now-exited)
//!   instance, so `POLLIN` reliably fires on the original process's
//!   exit and never on the unrelated successor's lifecycle.
//! - **macOS**: [`getsockopt`] with `SOL_LOCAL` + `LOCAL_PEERTOKEN`
//!   returns the peer's `audit_token_t` (32 bytes / 8 `u32`s) — a
//!   process-lifetime identity used here strictly as a liveness
//!   primitive (see "What (2/N) explicitly does NOT defend
//!   against" below: the audit token's pid/uid/asid/auid fields
//!   are inherited across same-uid `execve`, so it does **not**
//!   detect a mid-session binary swap). We dup the connection FD
//!   and store both the FD and the initial token.
//!   [`still_pinned`] re-fetches the token via the dup'd FD and
//!   byte-compares; mismatch or fetch failure → `false`. Race
//!   safety: when the original peer process exits, the kernel
//!   tears down its end of the connection so the re-fetch fails
//!   (or returns a different token if the socket-state record is
//!   somehow recycled to a different process before our dup is
//!   closed). Either signal flips `still_pinned` to `false`,
//!   which is what callers need to refuse PID-recycling races on
//!   the (much rarer than Linux pidfd races) macOS Unix-socket
//!   path.
//!
//! ## What (2/N) liveness explicitly does NOT defend against
//!
//! Liveness ≠ identity. The kernel handle defends against
//! pid-recycling races but **does not** detect mid-session `execve`
//! into a different binary — Linux `pidfd` survives `execve`, and
//! the macOS `audit_token_t`'s pid/uid/asid/auid fields are
//! inherited across same-uid exec. (3/N)'s [`classify_peer`]
//! handles the *static* trust question (is this binary trusted at
//! pin time?) but it doesn't re-evaluate trust mid-session either
//! — a peer that execs after the initial classification keeps the
//! original [`TrustLevel`]. Closing the mid-session-exec gap
//! requires a follow-up sub-piece that re-classifies on every
//! gate enforcement; not yet planned. The gate's `shadow_mode =
//! true` default keeps everything dormant until the wiring
//! sub-piece flips that flag — see [`crate::security`] for the
//! atomic-bundle rationale.
//!
//! [`getsockopt`]: libc::getsockopt
//! [`libc::poll`]: libc::poll
//! [`libc::syscall`]: libc::syscall
//! [`TrustLevel`]: crate::security::TrustLevel

use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::PathBuf;

// `UnixStream::peer_cred` is gated behind the unstable
// `peer_credentials_unix_socket` feature (rust-lang/rust#42839). We
// call `getsockopt`/`getpeereid` directly so this module compiles
// on stable.

/// macOS socket-option constant for `LOCAL_PEERTOKEN`. Defined in
/// `<sys/un.h>` as `0x006`. The `libc` crate doesn't currently
/// expose this constant by name; declaring it locally keeps the
/// module self-contained and avoids a transitive dependency for a
/// single integer.
#[cfg(target_os = "macos")]
const LOCAL_PEERTOKEN: libc::c_int = 0x006;

/// macOS `kSecCSSigningInformation` flag for
/// [`SecCodeCopySigningInformation`] (Security.framework
/// `<Security/SecCode.h>`). Bit-mask value `0x2`; instructs the
/// API to include the signing certificates **and Team
/// Identifier** in the returned info dictionary. Without this
/// flag (e.g. when called with `kSecCSDefaultFlags = 0`) the
/// returned dict omits `teamid` entirely, so any consumer
/// looking up `teamid` always sees `None` and the caller would
/// silently always-deny.
///
/// **The v5.6.0-alpha install-test caught this exact regression**
/// — `verify_conductor_team_id` was passing `0` and every
/// Apple-Team-ID-signed binary classified as `Untrusted`. The
/// fix is to pass this flag.
///
/// This module redeclares the constant rather than depending on
/// `security-framework-sys` because Apple's value is a single
/// well-defined integer with a stable contract; pulling in a
/// crate just for one constant would inflate the dependency
/// surface for no benefit.
#[cfg(target_os = "macos")]
const SEC_CS_SIGNING_INFORMATION: u32 = 0x2;

/// macOS `audit_token_t` from `<bsm/audit.h>`:
/// `typedef struct { unsigned int val[8]; } audit_token_t;`. 32
/// bytes, no padding, byte-comparable.
#[cfg(target_os = "macos")]
type AuditToken = [u32; 8];

/// Apple Developer ID Team Identifier for Conductor (matches the
/// `TeamIdentifier` field shown by `codesign -dv` on a notarised
/// build — see `Conductor.app/Contents/MacOS/conductor`'s
/// signature). Hard-coded so the daemon refuses to grant
/// `GuiTrusted` / `CliTrusted` to a same-name binary signed
/// under any other Team ID, and so the value travels with the
/// binary rather than living in a config file the user could
/// edit.
///
/// # Why a list rather than a single value
///
/// Apple does not convert an Individual developer account into an
/// Organization one — you enrol separately and receive a **new Team
/// ID**. Conductor currently ships under an Individual account, and
/// an Organization move (blocked on company formation + a D&B
/// D-U-N-S registration) is expected later.
///
/// If this were a scalar, that migration would be a hard break: a
/// daemon signed by the old team would reject a GUI signed by the
/// new one and vice versa, with no build able to trust both. As a
/// list, the migration ships as a transition release that accepts
/// the old and new team, after which the old entry is dropped.
///
/// Keep this list SHORT and drop stale entries once the transition
/// completes — every entry is a binary that can claim trust.
#[cfg(target_os = "macos")]
const CONDUCTOR_TEAM_IDS: &[&str] = &[
    // Individual account (Christopher Joseph). Current signing team.
    "38H355VKB5",
];

/// Identity of a peer process pinned at connection accept time.
///
/// Captures the peer's uid, pid, initial executable path, and a
/// platform-specific kernel handle ([`OwnedFd`] holding a `pidfd`
/// on Linux; an audit-token snapshot plus a dup of the connection
/// FD on macOS) so [`Self::still_pinned`] can run a race-free
/// liveness check.
///
/// `#[non_exhaustive]` so 3/N can add fields (e.g. a verified
/// `team_id: Option<String>` after code-signature classification)
/// without breaking downstream construction. Use
/// [`Self::from_stream`] to construct.
#[derive(Debug)]
#[non_exhaustive]
pub struct PinnedPeer {
    /// Effective uid of the connecting process.
    pub uid: u32,

    /// Process id of the connecting process at connect time. The
    /// kernel-handle field below makes this race-free with respect
    /// to PID recycling: if the original process exits, the handle
    /// reports `false` from [`Self::still_pinned`] regardless of
    /// what later inhabits this PID.
    pub pid: u32,

    /// Absolute path to the executable image of the peer **at the
    /// moment of connection**. Mid-session `execve` is not detected
    /// by the (2/N) liveness check — that's 3/N's
    /// signature-classification job. Callers reasoning about
    /// trust should not assume this path remains accurate beyond
    /// the `from_stream` call.
    pub initial_exe: PathBuf,

    /// Linux: `pidfd` returned by `pidfd_open(pid, 0)`. Becomes
    /// `POLLIN`-readable when the original process exits. Held as
    /// [`OwnedFd`] so it's closed automatically on drop.
    #[cfg(target_os = "linux")]
    pidfd: OwnedFd,

    /// macOS: snapshot of the peer's `audit_token_t` taken at
    /// connection time via `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)`.
    /// [`Self::still_pinned`] re-fetches and byte-compares to
    /// detect process exit / audit-identity change.
    #[cfg(target_os = "macos")]
    initial_audit_token: AuditToken,

    /// macOS: dup of the original connection FD, retained so
    /// [`Self::still_pinned`] can re-issue
    /// `getsockopt(LOCAL_PEERTOKEN)` against the same kernel
    /// socket-state record even after the IPC server has dropped
    /// its `UnixStream` wrapper (either
    /// [`std::os::unix::net::UnixStream`] or
    /// `tokio::net::UnixStream` — `from_stream` is generic over
    /// `&impl AsFd` so both work).
    #[cfg(target_os = "macos")]
    conn_fd: OwnedFd,
}

impl PinnedPeer {
    /// Pin the peer of an accepted Unix socket connection.
    ///
    /// Platform dispatch:
    /// - Linux: [`pin_linux`]
    /// - macOS: [`pin_macos`]
    /// - Other Unix: [`PeerAuthError::UnsupportedPlatform`]
    pub fn from_stream<F: AsFd + ?Sized>(stream: &F) -> Result<Self, PeerAuthError> {
        // Each cfg block is the function's tail expression for its
        // target — only one is compiled on each platform.
        //
        // Generic over `AsFd` so both `std::os::unix::net::UnixStream`
        // and `tokio::net::UnixStream` (used by the daemon's IPC
        // accept loop) can be passed directly without a `ManuallyDrop`
        // ownership-bridging hack. `BorrowedFd` from `AsFd::as_fd()`
        // implements `AsRawFd`, so the platform pin functions still
        // get the raw fd they need internally.
        #[cfg(target_os = "linux")]
        {
            pin_linux(stream)
        }
        #[cfg(target_os = "macos")]
        {
            pin_macos(stream)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = stream;
            Err(PeerAuthError::UnsupportedPlatform)
        }
    }

    /// Returns `true` if the originally-pinned peer process is
    /// still running.
    ///
    /// **Race safety:** uses a kernel handle pinned at
    /// [`Self::from_stream`] time, so the answer is not affected
    /// by PID recycling.
    ///
    /// **What this does NOT detect:**
    /// - Mid-session `execve` into a different binary while the
    ///   pinned process keeps running. Defending against that is
    ///   3/N's signature-classification job.
    /// - Trust-level questions (is this binary signed? whose Team
    ///   ID? is the executable in an Apple-blessed location?). Also
    ///   3/N.
    ///
    /// The implementation fails closed: any error fetching the
    /// kernel-handle state returns `false`, on the principle that
    /// gate decisions made under uncertainty should refuse rather
    /// than admit.
    pub fn still_pinned(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            still_pinned_linux(&self.pidfd)
        }
        #[cfg(target_os = "macos")]
        {
            still_pinned_macos(&self.conn_fd, &self.initial_audit_token)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }
}

/// Classify a [`PinnedPeer`] into the gate's [`TrustLevel`] enum
/// per ADR-027 D1 (3/N) — the input axis to D5's per-tier
/// decision table.
///
/// ## Decision rules
///
/// 1. **Same-uid gate** — the peer's `uid` must equal the daemon's
///    effective uid. The IPC socket is mode 0600 / 0700 so a
///    different-uid peer can't connect in practice; this is
///    belt-and-braces defense, but it also keeps the classifier
///    honest if the socket permissions ever drift.
/// 2. **Exe-basename match** — the peer's `initial_exe` filename
///    must match one of conductor's trusted binary names:
///    - `conductor-gui` → [`TrustLevel::GuiTrusted`]
///    - `conductor` / `conductorctl` → [`TrustLevel::CliTrusted`]
///    - anything else → [`TrustLevel::Untrusted`]
/// 3. **Signature verification** (macOS only) — if rules (1) and
///    (2) elevate the peer to `GuiTrusted` / `CliTrusted`, the
///    macOS path additionally checks that the peer's code
///    signature carries Conductor's Apple Team ID
///    (`38H355VKB5`). A mismatch demotes to `Untrusted` rather
///    than admitting a same-name unsigned binary that happens to
///    sit at a trusted path. Linux has no equivalent in-tree
///    signature anchor — distro packaging and exe-path
///    permissions are the install-time verification — so the
///    Linux classifier stops at rules (1) and (2).
///
/// ## Why "Untrusted" is the safe default
///
/// Mis-classifying a foreign binary as `GuiTrusted` would let any
/// same-uid process inherit the gate decisions intended for the
/// signed GUI — unacceptable. The classifier therefore fails
/// closed at every step: any check that doesn't unambiguously
/// prove trust returns `Untrusted`.
pub fn classify_peer(peer: &PinnedPeer) -> crate::security::TrustLevel {
    use crate::security::TrustLevel;

    // Rule 1: same-uid gate.
    let self_uid = unsafe { libc::geteuid() };
    if peer.uid != self_uid {
        return TrustLevel::Untrusted;
    }

    // Rule 2: exe-basename match.
    let basic_trust = match peer.initial_exe.file_name().and_then(|s| s.to_str()) {
        Some("conductor-gui") => TrustLevel::GuiTrusted,
        Some("conductor") | Some("conductorctl") => TrustLevel::CliTrusted,
        _ => TrustLevel::Untrusted,
    };

    // Rule 3: macOS-only signature verification.
    #[cfg(target_os = "macos")]
    {
        if basic_trust == TrustLevel::Untrusted {
            return TrustLevel::Untrusted;
        }
        // The exe basename matched a trusted name — now require
        // a valid Conductor-Team-ID signature before admitting.
        if !verify_conductor_team_id(&peer.initial_audit_token) {
            return TrustLevel::Untrusted;
        }
    }

    basic_trust
}

/// Why peer pinning failed.
///
/// `#[non_exhaustive]` so 3/N can add `SignatureCheckFailed` /
/// `TeamIdMismatch` variants without breaking downstream `match`
/// arms.
#[derive(Debug)]
#[non_exhaustive]
pub enum PeerAuthError {
    /// `getsockopt(SO_PEERCRED)` (Linux) or `getpeereid` (macOS)
    /// returned an I/O error — the socket is not a Unix domain
    /// socket, the peer has disconnected, the kernel lacks support,
    /// or `getsockopt` reported a `len` that doesn't match the
    /// expected struct size (which would leave fields zero-init'd
    /// and cause the peer to be mis-pinned as uid 0 / pid 0; the
    /// resolver fails closed in that case).
    PeerCredFailed(io::Error),

    /// `getsockopt(LOCAL_PEERPID)` failed (macOS only). Either the
    /// kernel doesn't support it (very old macOS) or the connection
    /// is in a state where pid lookup isn't possible.
    #[cfg(target_os = "macos")]
    PeerPidFailed(io::Error),

    /// Could not resolve the executable path for the peer's pid.
    /// On Linux: `/proc/<pid>/exe` doesn't exist or isn't a symlink
    /// (peer already exited, or `/proc` not mounted).
    /// On macOS: `proc_pidpath` returned an error.
    ExePathResolutionFailed(io::Error),

    /// `pidfd_open(pid, 0)` failed (Linux only). The most common
    /// cause is the kernel being older than Linux 5.3 (`pidfd_open`
    /// was added in 5.3). Less commonly: the peer process exited
    /// between `getsockopt(SO_PEERCRED)` and `pidfd_open`, in which
    /// case the kernel returns `ESRCH`. This is a fail-closed
    /// situation: without a `pidfd` we have no race-free liveness
    /// primitive and must reject the peer rather than fall back to
    /// the racy pid-only check.
    #[cfg(target_os = "linux")]
    PidfdOpenFailed(io::Error),

    /// `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)` failed (macOS only)
    /// — either the kernel returned an error, or the response was
    /// shorter than the expected `audit_token_t` size (32 bytes).
    /// Like [`Self::PidfdOpenFailed`], this is treated as
    /// fail-closed: without a pinned audit token we have no
    /// race-free way to test peer liveness later.
    #[cfg(target_os = "macos")]
    AuditTokenFailed(io::Error),

    /// Could not duplicate the connection file descriptor for
    /// later re-querying (macOS only). Without a private dup we
    /// can't outlive the IPC server's stream wrapper, so this is
    /// a fail-closed condition.
    #[cfg(target_os = "macos")]
    ConnectionFdDupFailed(io::Error),

    /// The platform is neither Linux nor macOS. Conductor's daemon
    /// is currently macOS-and-Linux only, but this variant exists
    /// so future ports surface a structured error rather than a
    /// silent "all peers are uid 0" fallback.
    UnsupportedPlatform,
}

impl std::fmt::Display for PeerAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PeerCredFailed(e) => write!(f, "peer credential extraction failed: {}", e),
            #[cfg(target_os = "macos")]
            Self::PeerPidFailed(e) => write!(f, "LOCAL_PEERPID getsockopt failed: {}", e),
            Self::ExePathResolutionFailed(e) => {
                write!(f, "could not resolve peer executable path: {}", e)
            }
            #[cfg(target_os = "linux")]
            Self::PidfdOpenFailed(e) => {
                // Don't enumerate causes — kernel < 5.3 and a peer
                // exit racing between getsockopt and pidfd_open are
                // common but not exhaustive (LSM / seccomp / cgroup
                // policy / namespace restrictions can also produce
                // failures). The wrapped `io::Error` carries the
                // actual errno for diagnosis.
                write!(f, "pidfd_open failed: {}", e)
            }
            #[cfg(target_os = "macos")]
            Self::AuditTokenFailed(e) => {
                write!(f, "LOCAL_PEERTOKEN getsockopt failed: {}", e)
            }
            #[cfg(target_os = "macos")]
            Self::ConnectionFdDupFailed(e) => {
                write!(f, "could not duplicate connection FD for liveness check: {}", e)
            }
            Self::UnsupportedPlatform => f.write_str(
                "peer pinning is not implemented for this platform (Conductor supports Linux and macOS only)",
            ),
        }
    }
}

impl std::error::Error for PeerAuthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PeerCredFailed(e) | Self::ExePathResolutionFailed(e) => Some(e),
            #[cfg(target_os = "macos")]
            Self::PeerPidFailed(e) => Some(e),
            #[cfg(target_os = "linux")]
            Self::PidfdOpenFailed(e) => Some(e),
            #[cfg(target_os = "macos")]
            Self::AuditTokenFailed(e) | Self::ConnectionFdDupFailed(e) => Some(e),
            _ => None,
        }
    }
}

/// Linux peer pinning via `SO_PEERCRED` + `pidfd_open` +
/// `/proc/<pid>/exe`.
///
/// `pidfd_open` is called **immediately** after `SO_PEERCRED` (no
/// allocations or other syscalls in between) so the kernel-side
/// process descriptor pins the peer before any TOCTOU window opens.
/// Only after the `pidfd` is in hand do we resolve
/// `/proc/<pid>/exe`; if the peer execs into a different binary
/// after that point, the (2/N) liveness check still reports the
/// original process is alive (which is correct — exec doesn't
/// change pid) and 3/N's signature classification is the right
/// place to detect the binary swap.
#[cfg(target_os = "linux")]
pub fn pin_linux<F: AsFd + ?Sized>(stream: &F) -> Result<PinnedPeer, PeerAuthError> {
    let mut ucred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let fd = stream.as_fd().as_raw_fd();
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut ucred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(PeerAuthError::PeerCredFailed(io::Error::last_os_error()));
    }
    // getsockopt updates `len` with the bytes actually written. If the
    // kernel returned fewer bytes than `sizeof(ucred)`, the trailing
    // fields are still zero from the `mem::zeroed()` init — so a
    // partial fill would mis-pin the peer as uid 0 / pid 0. Fail
    // closed on size mismatch.
    if (len as usize) != std::mem::size_of::<libc::ucred>() {
        return Err(PeerAuthError::PeerCredFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "SO_PEERCRED returned {} bytes, expected {}",
                len,
                std::mem::size_of::<libc::ucred>()
            ),
        )));
    }
    // pid 0 is the kernel idle / scheduler — userland processes
    // connecting over a Unix socket cannot have pid 0. Treat it as
    // an unexpected kernel response and fail closed rather than
    // proceeding to read `/proc/0/exe`.
    if ucred.pid <= 0 {
        return Err(PeerAuthError::PeerCredFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("SO_PEERCRED returned non-positive pid {}", ucred.pid),
        )));
    }
    // Capture the kernel-handle FIRST, before any other syscall —
    // this is the TOCTOU close. After `pidfd_open` returns, the
    // pidfd refers to the original process descriptor regardless
    // of what later happens to its PID.
    let pidfd = pidfd_open(ucred.pid)?;
    let initial_exe = std::fs::read_link(format!("/proc/{}/exe", ucred.pid))
        .map_err(PeerAuthError::ExePathResolutionFailed)?;
    Ok(PinnedPeer {
        uid: ucred.uid,
        pid: ucred.pid as u32,
        initial_exe,
        pidfd,
    })
}

/// Open a `pidfd` for the given pid via the `SYS_pidfd_open`
/// syscall. Returns an [`OwnedFd`] so the descriptor is closed
/// automatically on drop.
///
/// We invoke the syscall directly rather than using
/// `libc::pidfd_open` because the latter wasn't exposed by the
/// `libc` crate until 0.2.140-ish, and the workspace pin is just
/// `"0.2"`. The syscall constant `SYS_pidfd_open` has been in
/// `libc` since 0.2.96 (kernel 5.3 era) so we're well within the
/// MSRV window.
#[cfg(target_os = "linux")]
fn pidfd_open(pid: libc::pid_t) -> Result<OwnedFd, PeerAuthError> {
    // Flags = 0 for a non-thread, non-PIDFD_NONBLOCK pidfd. We
    // poll non-blockingly via `poll(timeout=0)` instead of
    // PIDFD_NONBLOCK, which is sufficient for our liveness check
    // and avoids a second kernel-version dependency (PIDFD_NONBLOCK
    // landed in 5.10).
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
    if raw < 0 {
        return Err(PeerAuthError::PidfdOpenFailed(io::Error::last_os_error()));
    }
    // `syscall()` returns `c_long`. The fd fits in `c_int` by
    // construction (pidfds are small ints) but cast explicitly
    // and reject any value that doesn't round-trip — paranoia
    // against a libc that sign-extends differently than expected.
    let fd_int: libc::c_int = raw as libc::c_int;
    if fd_int as libc::c_long != raw {
        // SAFETY: rc was a valid fd; close it before bailing.
        unsafe {
            libc::close(fd_int);
        }
        return Err(PeerAuthError::PidfdOpenFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pidfd_open returned out-of-range fd {}", raw),
        )));
    }
    // SAFETY: pidfd_open returned a valid fd that the kernel has
    // transferred ownership of to us. Wrapping it in OwnedFd
    // ensures it's closed on drop.
    Ok(unsafe { OwnedFd::from_raw_fd(fd_int) })
}

/// Linux liveness check. Polls the `pidfd` non-blockingly: when
/// the original process has exited, the pidfd becomes
/// `POLLIN`-readable; while the process is alive the pidfd has
/// no ready events.
///
/// Returns `false` on:
/// - `POLLIN` set (process exited)
/// - `POLLERR` / `POLLHUP` / `POLLNVAL` set (kernel reports an
///   abnormal pidfd state — fail closed rather than admit)
/// - `poll()` syscall failure (e.g. EINTR loop exhausted)
///
/// Returns `true` only on the explicit "no events ready" signal.
#[cfg(target_os = "linux")]
fn still_pinned_linux(pidfd: &OwnedFd) -> bool {
    let mut pfd = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // Retry on EINTR — the kernel can interrupt poll() if a
    // signal is delivered; that's not an exit signal for the
    // pinned peer. Bound the retry count so a pathological signal
    // storm can't spin forever.
    for _ in 0..4 {
        // Clear `revents` before every call. POSIX guarantees the
        // kernel writes `revents` only on success (rc > 0); on
        // rc == 0 (timeout) and rc < 0 (error, including EINTR)
        // the field is undefined. If we left a stale POLLIN /
        // POLLERR bit from a previous EINTR-retried iteration,
        // the abnormal-state check below could fail closed
        // spuriously. Resetting to 0 each iteration keeps the
        // check honest.
        pfd.revents = 0;
        let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
        if rc < 0 {
            let errno = io::Error::last_os_error();
            if errno.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return false;
        }
        // Process exited — pidfd became readable.
        if (pfd.revents & libc::POLLIN) != 0 {
            return false;
        }
        // Any abnormal state on the fd = fail closed.
        let abnormal = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
        if (pfd.revents & abnormal) != 0 {
            return false;
        }
        // rc == 0 with no revents bits set: nothing ready, peer
        // is alive.
        return true;
    }
    // Exceeded EINTR retry budget; conservatively report dead.
    false
}

/// macOS peer pinning via `getpeereid` (uid) + `LOCAL_PEERPID`
/// (pid) + `LOCAL_PEERTOKEN` (audit token) + `proc_pidpath` (exe
/// path).
///
/// `LOCAL_PEERTOKEN` is fetched **immediately** after the basic
/// credential queries and **before** any allocating syscall, so
/// the audit token represents the peer's identity at the earliest
/// point we can pin it. The connection FD is dup'd into the
/// returned [`PinnedPeer`] so the (2/N) liveness check can re-issue
/// `getsockopt` against the same kernel socket-state record after
/// the IPC server's stream wrapper has been moved or dropped.
#[cfg(target_os = "macos")]
pub fn pin_macos<F: AsFd + ?Sized>(stream: &F) -> Result<PinnedPeer, PeerAuthError> {
    let fd = stream.as_fd().as_raw_fd();
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    if rc != 0 {
        return Err(PeerAuthError::PeerCredFailed(io::Error::last_os_error()));
    }
    let pid = pid_from_local_peerpid(stream)?;
    // Audit token before exe-path resolution — closes the TOCTOU
    // window between credential capture and policy enforcement.
    let initial_audit_token = fetch_audit_token(fd)?;
    // Dup the connection FD so we can outlive the caller's
    // UnixStream wrapper (std or tokio). `try_clone_to_owned()`
    // performs `dup` under the hood and returns an [`OwnedFd`]
    // tied to our lifetime.
    let conn_fd = stream
        .as_fd()
        .try_clone_to_owned()
        .map_err(PeerAuthError::ConnectionFdDupFailed)?;
    let initial_exe = exe_path_from_pidpath(pid)?;
    Ok(PinnedPeer {
        uid,
        pid,
        initial_exe,
        initial_audit_token,
        conn_fd,
    })
}

/// Fetch the peer's `audit_token_t` via
/// `getsockopt(SOL_LOCAL, LOCAL_PEERTOKEN)`. The kernel writes
/// 32 bytes into the buffer (8 × `u32`); we validate the returned
/// length matches and fail closed otherwise (a short response
/// would leave trailing bytes zero-init and produce a spurious
/// "match" against any other partial read).
#[cfg(target_os = "macos")]
fn fetch_audit_token(fd: libc::c_int) -> Result<AuditToken, PeerAuthError> {
    let mut token: AuditToken = [0; 8];
    let mut len = std::mem::size_of::<AuditToken>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            LOCAL_PEERTOKEN,
            token.as_mut_ptr() as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(PeerAuthError::AuditTokenFailed(io::Error::last_os_error()));
    }
    if (len as usize) != std::mem::size_of::<AuditToken>() {
        return Err(PeerAuthError::AuditTokenFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "LOCAL_PEERTOKEN returned {} bytes, expected {}",
                len,
                std::mem::size_of::<AuditToken>()
            ),
        )));
    }
    Ok(token)
}

/// macOS liveness check. Re-fetches the audit token via the dup'd
/// connection FD and byte-compares it against the snapshot taken
/// at `pin_macos` time.
///
/// Returns `false` if:
/// - The re-fetch fails (the kernel reports an error — typically
///   because the peer's end of the socket has been torn down by
///   the kernel after the peer process exited).
/// - The re-fetch succeeds but the token differs. This is **not**
///   an `execve` signal — the audit token's pid/uid/asid/auid
///   fields are inherited across same-uid `execve`, so a
///   re-exec'd process returns the *same* token (3/N's
///   signature classification is the right place to detect a
///   mid-session binary swap). A mismatch instead means an
///   unexpected peer-identity change at the kernel level
///   (e.g. socket-state record recycled to a different process,
///   or a kernel/library-version anomaly) — fail closed rather
///   than guess.
///
/// Returns `true` only on a successful fetch with byte-identical
/// token.
#[cfg(target_os = "macos")]
fn still_pinned_macos(conn_fd: &OwnedFd, expected: &AuditToken) -> bool {
    match fetch_audit_token(conn_fd.as_raw_fd()) {
        Ok(now) => now == *expected,
        Err(_) => false,
    }
}

/// #1125 — extracted signing info, abstracted behind a lookup trait
/// so tests can drive `verify_conductor_team_id_with` with a mock
/// that asserts the flags argument and returns a controlled
/// `teamid`. Only the fields the verifier actually consults are
/// modelled — keep this struct tight.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct SigningInfo {
    /// Team Identifier from the signing dictionary (`teamid` key in
    /// `SecCodeCopySigningInformation`'s returned CFDictionary).
    /// `None` if the dict didn't carry that key (ad-hoc signing,
    /// wrong `flags` argument, or signing dict absent entirely).
    team_id: Option<String>,
}

/// #1125 — seam between `verify_conductor_team_id` and the SecCode
/// FFI. Production wires through to `SecCodeCopyGuestWithAttributes`
/// followed by `SecCodeCopySigningInformation`; tests inject a mock
/// that asserts the `flags` argument is `SEC_CS_SIGNING_INFORMATION`
/// (the v5.6.0-alpha regression captured in #1122 was passing `0`,
/// which silently omitted `teamid` from the returned dict — every
/// Apple-Team-ID-signed binary then classified as `Untrusted`).
///
/// Pure trait — no state. Production impl is the zero-sized
/// `RealSecCodeLookup`; test impl is a local struct that asserts
/// `flags` then returns a fixture `SigningInfo`.
#[cfg(target_os = "macos")]
trait SigningInfoLookup {
    /// Look up signing information for the process identified by
    /// `audit_token`. `flags` MUST be forwarded to
    /// `SecCodeCopySigningInformation`. Production callers MUST pass
    /// `SEC_CS_SIGNING_INFORMATION = 0x2` — anything else produces a
    /// signing dict that omits `teamid`, which is the regression
    /// path #1125 exists to lock down.
    ///
    /// Returns `None` if the lookup fails for any reason
    /// (non-zero `OSStatus`, null pointer, dict without `teamid`).
    fn lookup(&self, audit_token: &AuditToken, flags: u32) -> Option<SigningInfo>;
}

/// Production `SigningInfoLookup` — the actual SecCode dance that
/// was previously inlined in `verify_conductor_team_id`. Zero-sized.
#[cfg(target_os = "macos")]
struct RealSecCodeLookup;

/// Verify that the peer process identified by `audit_token` carries
/// a valid Apple code signature whose Team Identifier matches
/// one of [`CONDUCTOR_TEAM_IDS`].
///
/// The Apple-recommended way to associate a `SecCodeRef` with a
/// specific running process is `SecCodeCopyGuestWithAttributes`
/// passing the `kSecGuestAttributeAudit` attribute (literal value
/// `"audit"`) keyed to the peer's `audit_token` as a `CFData`. The
/// audit token is preferable to a bare `pid` because it pins the
/// process+exec instance even across pid recycling. `security-framework` 3.x
/// doesn't expose this entry point on its `SecCode` wrapper, so
/// we bind it (and `SecCodeCopySigningInformation`) raw and use
/// `core-foundation`'s `TCFType` family for memory management.
///
/// Returns `false` for any error condition: a non-zero `OSStatus`
/// from `SecCodeCopyGuestWithAttributes` (process not found,
/// permission denied, etc.), missing or non-matching team
/// identifier in the signing dictionary, or absent `teamid` key
/// (e.g. ad-hoc-signed dev builds — by design, the `make
/// dev-build` ad-hoc-codesigned binaries do **not** carry a
/// Team ID and therefore correctly demote to `Untrusted` here).
///
/// #1125: thin wrapper around `verify_conductor_team_id_with`,
/// which takes a `SigningInfoLookup` so tests can exercise the
/// positive Team-ID-match path with a controlled mock.
#[cfg(target_os = "macos")]
fn verify_conductor_team_id(audit_token: &AuditToken) -> bool {
    verify_conductor_team_id_with(audit_token, &RealSecCodeLookup)
}

/// #1125 — testable core of `verify_conductor_team_id`. The
/// comparison logic lives here so it can be exercised with a mock
/// `SigningInfoLookup` that asserts the flags arg matches
/// `SEC_CS_SIGNING_INFORMATION`. Always forwards
/// `SEC_CS_SIGNING_INFORMATION` to the lookup — DO NOT replace
/// with `0` (that's the v5.6.0-alpha regression #1122 captured).
#[cfg(target_os = "macos")]
fn verify_conductor_team_id_with<L: SigningInfoLookup>(
    audit_token: &AuditToken,
    lookup: &L,
) -> bool {
    match lookup.lookup(audit_token, SEC_CS_SIGNING_INFORMATION) {
        Some(SigningInfo {
            team_id: Some(team_id),
        }) => CONDUCTOR_TEAM_IDS.contains(&team_id.as_str()),
        _ => false,
    }
}

#[cfg(target_os = "macos")]
impl SigningInfoLookup for RealSecCodeLookup {
    fn lookup(&self, audit_token: &AuditToken, flags: u32) -> Option<SigningInfo> {
        use core_foundation::base::{CFType, TCFType};
        use core_foundation::data::CFData;
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::string::CFString;

        // SAFETY: the audit token is `[u32; 8]` = 32 bytes with no
        // padding. Reinterpreting as `[u8; 32]` is sound. We hold the
        // borrow for the lifetime of the slice we hand to `CFData`.
        let token_bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(
                audit_token.as_ptr() as *const u8,
                std::mem::size_of::<AuditToken>(),
            )
        };
        let token_data = CFData::from_buffer(token_bytes);
        // `kSecGuestAttributeAudit` from `<Security/SecCode.h>` is the
        // literal CFStringRef value `"audit"`. Apple's `security-framework`
        // crate doesn't re-export the constant, so we declare the
        // literal here. **Do not write `"audit-token"`** — that's the
        // key for `getsockopt(LOCAL_PEERTOKEN)`'s contract description,
        // not the SecCode guest-attribute API. Using the wrong key
        // makes `SecCodeCopyGuestWithAttributes` always fail (returning
        // -67051 / `errSecCSNoSuchCode`) and `verify_conductor_team_id`
        // would silently always-deny.
        let key = CFString::from_static_string("audit");

        // The key/value types collapse to (CFTypeRef, CFTypeRef) in
        // the underlying CFDictionaryCreate; declaring as
        // `CFDictionary<CFType, CFType>` avoids a generic-inference
        // ambiguity Rust hits when both halves are the same crate-
        // local type.
        let attrs: CFDictionary<CFType, CFType> =
            CFDictionary::from_CFType_pairs(&[(key.as_CFType(), token_data.as_CFType())]);

        // Step 1: SecCodeCopyGuestWithAttributes(host=NULL, attrs, 0,
        // &out). NULL host means "look up against the dynamic root";
        // the attrs dict's `audit` attribute names which running
        // process we want by audit_token.
        let mut sec_code: SecCodeRef = std::ptr::null();
        let status = unsafe {
            SecCodeCopyGuestWithAttributes(
                std::ptr::null(),
                attrs.as_concrete_TypeRef() as *const _,
                0,
                &mut sec_code,
            )
        };
        if status != 0 || sec_code.is_null() {
            return None;
        }

        // Step 2: SecCodeCopySigningInformation. Returns a CFDictionary
        // of signing attributes; we want the `kSecCodeInfoTeamIdentifier`
        // key (which resolves to the literal string "teamid" in the
        // returned dict). The `flags` arg is forwarded from the caller
        // — production callers MUST pass `SEC_CS_SIGNING_INFORMATION`
        // so the dict actually contains the team identifier (see the
        // module-level constant's docstring for the v5.6.0-alpha
        // install-test history that motivated calling this out).
        let mut info_ref: CFDictionaryRef = std::ptr::null();
        let status = unsafe { SecCodeCopySigningInformation(sec_code, flags, &mut info_ref) };
        // Always release the SecCode handle once we've finished with
        // it — `SecCodeCopySigningInformation` doesn't retain it for us.
        let result: Option<SigningInfo> = if status != 0 || info_ref.is_null() {
            None
        } else {
            // SAFETY: `info_ref` was created with a Copy semantic
            // (signing-information returns a +1 retained dict that we
            // own), so wrap_under_create_rule is correct here. The
            // `as *const _` coerces our locally-declared
            // `*const c_void` to core-foundation's opaque
            // `*const __CFDictionary`; the underlying pointer value is
            // identical, only the type-system distinction matters.
            let info: CFDictionary<CFString, CFType> =
                unsafe { CFDictionary::wrap_under_create_rule(info_ref as *const _) };
            let team_id_key = CFString::from_static_string("teamid");
            let team_id = match info.find(&team_id_key) {
                Some(value) => value.downcast::<CFString>().map(|s| s.to_string()),
                None => None,
            };
            Some(SigningInfo { team_id })
        };

        // SAFETY: sec_code came from SecCodeCopyGuestWithAttributes
        // with a Copy-rule semantic (the `Copy` in the function name
        // means "we own a reference now"), so `CFRelease` is the right
        // disposal. `sec_code` is already `*const c_void` (via the
        // `SecCodeRef` type alias), so no cast is needed.
        unsafe { CFRelease(sec_code) };

        result
    }
}

/// Raw bindings to the Security.framework + CoreFoundation symbols
/// that `verify_conductor_team_id` needs.
///
/// `security-framework` 3.x doesn't expose
/// `SecCodeCopyGuestWithAttributes` on its `SecCode` wrapper, and
/// `SecCodeCopySigningInformation` is reachable through
/// `SecStaticCode` only — neither covers our "look up running
/// process by audit token" use case directly. The opaque
/// `SecCodeRef` / `CFDictionaryRef` types are imported as raw
/// `*const c_void`-shaped pointers so we don't have to bring in
/// the security-framework-sys crate just for these declarations.
#[cfg(target_os = "macos")]
type SecCodeRef = *const std::ffi::c_void;
#[cfg(target_os = "macos")]
type CFDictionaryRef = *const std::ffi::c_void;
#[cfg(target_os = "macos")]
type OSStatus = i32;

#[cfg(target_os = "macos")]
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCodeCopyGuestWithAttributes(
        host: SecCodeRef,
        attrs: CFDictionaryRef,
        flags: u32,
        guest: *mut SecCodeRef,
    ) -> OSStatus;

    fn SecCodeCopySigningInformation(
        code: SecCodeRef,
        flags: u32,
        info: *mut CFDictionaryRef,
    ) -> OSStatus;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}

/// Fetch the peer pid via `getsockopt(SOL_LOCAL, LOCAL_PEERPID)`.
/// macOS-specific because `LOCAL_PEERCRED` doesn't include pid;
/// `LOCAL_PEERPID` is a separate option that returns just the pid.
#[cfg(target_os = "macos")]
fn pid_from_local_peerpid<F: AsFd + ?Sized>(stream: &F) -> Result<u32, PeerAuthError> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let fd = stream.as_fd().as_raw_fd();
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(PeerAuthError::PeerPidFailed(io::Error::last_os_error()));
    }
    // getsockopt updates `len` with the bytes actually written. A
    // shorter kernel response leaves `pid` at 0 (its zero-init);
    // proceeding would resolve `proc_pidpath(0)` which returns the
    // kernel idle task. Fail closed on size mismatch.
    if (len as usize) != std::mem::size_of::<libc::pid_t>() {
        return Err(PeerAuthError::PeerPidFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "LOCAL_PEERPID returned {} bytes, expected {}",
                len,
                std::mem::size_of::<libc::pid_t>()
            ),
        )));
    }
    if pid <= 0 {
        return Err(PeerAuthError::PeerPidFailed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("LOCAL_PEERPID returned non-positive pid {}", pid),
        )));
    }
    Ok(pid as u32)
}

/// Resolve `/proc/<pid>/exe`-equivalent on macOS via `proc_pidpath`.
#[cfg(target_os = "macos")]
fn exe_path_from_pidpath(pid: u32) -> Result<PathBuf, PeerAuthError> {
    // PROC_PIDPATHINFO_MAXSIZE is 4 * MAXPATHLEN = 4096 on macOS.
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * 1024;
    let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
    let len = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as u32,
        )
    };
    if len <= 0 {
        return Err(PeerAuthError::ExePathResolutionFailed(
            io::Error::last_os_error(),
        ));
    }
    // `proc_pidpath` returns the path length **excluding** the
    // trailing NUL — so `buf[..len]` is, in the documented contract,
    // exactly the path bytes. Defensive coding: truncate at the
    // first NUL anyway. POSIX paths cannot contain interior NULs
    // (any NUL we find is the terminator); a `PathBuf` containing
    // an embedded NUL would later fail `Path::exists()` with
    // `InvalidInput` and produce a confusing runtime error.
    let written = (len as usize).min(buf.len());
    let nul_terminated = buf[..written]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(written);
    // Use `OsStr::from_bytes` rather than `from_utf8` — POSIX paths
    // can legitimately contain non-UTF-8 byte sequences, and
    // rejecting them would surface as a spurious
    // `ExePathResolutionFailed` for valid binaries (matches the
    // approach in `pin_linux`, where `std::fs::read_link` returns
    // a `PathBuf` directly without forcing UTF-8 validation).
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    Ok(PathBuf::from(OsStr::from_bytes(&buf[..nul_terminated])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream as StdUnixStream};
    use tempfile::TempDir;

    /// Spin up a unix-socket listener on a temp path, connect from
    /// the same process, and pin both ends. The peer's uid and pid
    /// should match the current process; the initial_exe should
    /// match the test binary.
    fn pin_self_connection() -> Result<PinnedPeer, PeerAuthError> {
        let tmp = TempDir::new().expect("tempdir");
        let sock = tmp.path().join("d1-test.sock");
        let listener = UnixListener::bind(&sock).expect("bind");
        // Connect from same process.
        let _client = StdUnixStream::connect(&sock).expect("connect");
        let (server_side, _) = listener.accept().expect("accept");
        PinnedPeer::from_stream(&server_side)
    }

    #[test]
    fn pinned_peer_has_self_uid() {
        let pinned = pin_self_connection().expect("pin should succeed");
        // Peer is the same process — uid must match self.
        let self_uid = unsafe { libc::geteuid() };
        assert_eq!(pinned.uid, self_uid);
    }

    #[test]
    fn pinned_peer_has_self_pid() {
        let pinned = pin_self_connection().expect("pin should succeed");
        // Peer is the same process — pid must be self.
        let self_pid = unsafe { libc::getpid() } as u32;
        assert_eq!(pinned.pid, self_pid);
    }

    #[test]
    fn pinned_peer_initial_exe_is_test_binary() {
        let pinned = pin_self_connection().expect("pin should succeed");
        // initial_exe must resolve to a real path. We don't assert
        // the exact path because cargo's test runner binary path
        // varies, but it must be absolute AND point to an existing
        // file (or symlink target). The peer is the test process
        // itself; its binary is by definition open and readable.
        assert!(
            pinned.initial_exe.is_absolute(),
            "initial_exe should be absolute, got {}",
            pinned.initial_exe.display()
        );
        assert!(
            pinned.initial_exe.exists(),
            "initial_exe should point to an existing file, got {}",
            pinned.initial_exe.display()
        );
    }

    #[test]
    fn still_pinned_returns_true_for_live_self_connection() {
        // The peer is the test process itself — alive throughout.
        // Set up the listener/client/server inline (rather than
        // via the pin_self_connection helper) so all socket
        // descriptors stay alive through the still_pinned() call;
        // on macOS the liveness check re-issues
        // `getsockopt(LOCAL_PEERTOKEN)` against a dup of the
        // server-side connection, and the kernel correctly fails
        // that if either endpoint has been torn down. Holding the
        // FDs in scope until *after* the assertion exercises the
        // genuine live-peer path.
        //
        // The negative case (peer exits → still_pinned() returns
        // `false`) lives in the integration test
        // `tests/peer_pin_2n_lifecycle.rs` because killing the
        // current process to test it would take the test runner
        // with us.
        let tmp = TempDir::new().expect("tempdir");
        let sock = tmp.path().join("d1-live.sock");
        let listener = UnixListener::bind(&sock).expect("bind");
        let _client = StdUnixStream::connect(&sock).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        let pinned = PinnedPeer::from_stream(&server).expect("pin should succeed");
        assert!(
            pinned.still_pinned(),
            "still_pinned() must return true for the live self-process"
        );
        // Explicitly bind the FD-holders until after the assertion —
        // dropping `_client`, `server`, or `listener` early would
        // tear down the connection state and would invalidate the
        // macOS liveness check (the kernel-handle test below
        // depends on the connection record staying live, which is
        // exactly the property being asserted).
        drop(server);
        drop(_client);
        drop(listener);
        drop(tmp);
    }

    #[test]
    fn pinned_peer_is_send_for_cross_thread_use() {
        // The IPC server passes PinnedPeer across tokio task
        // boundaries; if a future field breaks Send, this fails
        // at build time.
        fn assert_send<T: Send>() {}
        assert_send::<PinnedPeer>();
        assert_send::<PeerAuthError>();
    }

    #[test]
    fn peer_auth_error_displays_with_source() {
        // Smoke test for Display impl — the audit / denial stream
        // (D13a) renders these errors, so the format must not
        // panic on any variant.
        let err = PeerAuthError::PeerCredFailed(io::Error::other("test"));
        let msg = err.to_string();
        assert!(msg.contains("peer credential"), "got: {}", msg);
        assert!(msg.contains("test"), "expected source error in message");
    }

    // ---- classify_peer unit tests (D1 3/N) -------------------------
    //
    // These tests construct synthetic `PinnedPeer` values via direct
    // struct-literal construction (allowed within the same crate
    // even with `#[non_exhaustive]`) so they can drive `classify_peer`
    // through specific (uid, basename) combinations that
    // `pin_self_connection`'s real-process flow can't reproduce.
    //
    // The macOS Security.framework verification path is exercised
    // negatively: synthetic audit tokens don't match Conductor's
    // Team ID, so any case that would have classified as
    // `GuiTrusted` / `CliTrusted` on Linux correctly demotes to
    // `Untrusted` on macOS. The positive macOS case (signed
    // conductor-gui at the trusted Team ID) requires an actually-
    // signed binary at install time and is exercised by the
    // install-test cycle, not by `cargo test`.

    /// Open `/dev/null` and return an [`OwnedFd`] suitable for
    /// stuffing into the platform-specific kernel-handle field of
    /// a synthetic `PinnedPeer`. We never `poll()` this fd — it's a
    /// placeholder so the struct is constructible and `Drop` works
    /// without aborting on a zero raw fd.
    fn dev_null_fd() -> OwnedFd {
        std::fs::File::open("/dev/null")
            .expect("open /dev/null")
            .into()
    }

    /// Build a synthetic [`PinnedPeer`] with the given uid and exe.
    /// The pid is the test process pid (so live-peer assumptions in
    /// any future classifier extension hold); the kernel-handle
    /// field is a placeholder pointing at `/dev/null`.
    fn synthetic_peer(uid: u32, exe: PathBuf) -> PinnedPeer {
        PinnedPeer {
            uid,
            pid: unsafe { libc::getpid() } as u32,
            initial_exe: exe,
            #[cfg(target_os = "linux")]
            pidfd: dev_null_fd(),
            #[cfg(target_os = "macos")]
            initial_audit_token: [0; 8],
            #[cfg(target_os = "macos")]
            conn_fd: dev_null_fd(),
        }
    }

    #[test]
    fn classify_peer_returns_untrusted_for_different_uid() {
        // Different uid from the daemon's effective uid — should
        // never be admitted regardless of basename or signature
        // status. Use uid 0 (root) if we're not root, or 1 (daemon)
        // if we are; either way this is a different uid from a
        // non-root test runner.
        let self_uid = unsafe { libc::geteuid() };
        let other_uid = if self_uid == 0 { 1 } else { 0 };
        let peer = synthetic_peer(other_uid, PathBuf::from("/bin/conductor-gui"));
        assert_eq!(
            classify_peer(&peer),
            crate::security::TrustLevel::Untrusted,
            "different-uid peer must classify as Untrusted regardless of basename"
        );
    }

    #[test]
    fn classify_peer_returns_untrusted_for_unknown_basename() {
        // Same-uid but a binary name not on the trusted list.
        let peer = synthetic_peer(
            unsafe { libc::geteuid() },
            PathBuf::from("/usr/bin/python3"),
        );
        assert_eq!(
            classify_peer(&peer),
            crate::security::TrustLevel::Untrusted,
            "non-conductor basename must classify as Untrusted"
        );
    }

    #[test]
    fn classify_peer_handles_conductor_gui_basename() {
        // Same-uid + conductor-gui basename. Linux: GuiTrusted
        // (basename + uid suffice). macOS: Untrusted because the
        // synthetic audit token (all zeros) cannot satisfy the
        // CONDUCTOR_TEAM_IDS signature check — fail closed.
        let peer = synthetic_peer(
            unsafe { libc::geteuid() },
            PathBuf::from("/Applications/Conductor.app/Contents/MacOS/conductor-gui"),
        );
        let trust = classify_peer(&peer);
        #[cfg(target_os = "linux")]
        assert_eq!(
            trust,
            crate::security::TrustLevel::GuiTrusted,
            "Linux: same-uid + conductor-gui basename → GuiTrusted"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            trust,
            crate::security::TrustLevel::Untrusted,
            "macOS without Conductor Team ID signature must demote to Untrusted (got {:?})",
            trust
        );
    }

    #[test]
    fn classify_peer_handles_conductor_basename() {
        let peer = synthetic_peer(
            unsafe { libc::geteuid() },
            PathBuf::from("/usr/local/bin/conductor"),
        );
        let trust = classify_peer(&peer);
        #[cfg(target_os = "linux")]
        assert_eq!(trust, crate::security::TrustLevel::CliTrusted);
        #[cfg(target_os = "macos")]
        assert_eq!(trust, crate::security::TrustLevel::Untrusted);
    }

    #[test]
    fn classify_peer_handles_conductorctl_basename() {
        let peer = synthetic_peer(
            unsafe { libc::geteuid() },
            PathBuf::from("/usr/local/bin/conductorctl"),
        );
        let trust = classify_peer(&peer);
        #[cfg(target_os = "linux")]
        assert_eq!(trust, crate::security::TrustLevel::CliTrusted);
        #[cfg(target_os = "macos")]
        assert_eq!(trust, crate::security::TrustLevel::Untrusted);
    }

    /// #1125: positive Team-ID-match test. Exercises
    /// `verify_conductor_team_id_with` against a mock
    /// `SigningInfoLookup` that (a) ASSERTS the caller passes
    /// `SEC_CS_SIGNING_INFORMATION = 0x2` (catching the #1122
    /// regression class directly — passing `0` would silently omit
    /// `teamid` from the returned dict in production), and (b)
    /// returns a `SigningInfo` whose `team_id` matches
    /// `CONDUCTOR_TEAM_IDS`. Pre-#1125 no test in the Phase 1A suite
    /// exercised this path — the v5.6.0-alpha → v5.6.1-alpha
    /// install-test pause was the only signal that ever fired.
    #[cfg(target_os = "macos")]
    #[test]
    fn verify_conductor_team_id_returns_true_for_matching_team_id() {
        struct MockLookup;
        impl SigningInfoLookup for MockLookup {
            fn lookup(&self, _: &AuditToken, flags: u32) -> Option<SigningInfo> {
                assert_eq!(
                    flags, SEC_CS_SIGNING_INFORMATION,
                    "verify_conductor_team_id MUST forward \
                     SEC_CS_SIGNING_INFORMATION (= 0x{:x}) to \
                     SecCodeCopySigningInformation. Passing any other \
                     value (notably 0) silently omits the `teamid` \
                     key from the returned signing dict — that is the \
                     v5.6.0-alpha regression captured in #1122 and the \
                     gap this test (#1125) exists to lock down. \
                     Got flags = 0x{:x}.",
                    SEC_CS_SIGNING_INFORMATION, flags,
                );
                Some(SigningInfo {
                    team_id: Some(CONDUCTOR_TEAM_IDS[0].to_string()),
                })
            }
        }

        let token: AuditToken = [0; 8];
        assert!(
            verify_conductor_team_id_with(&token, &MockLookup),
            "verify_conductor_team_id_with MUST return true when the \
             lookup returns SigningInfo in CONDUCTOR_TEAM_IDS"
        );
    }

    /// #1125 negative coverage. The lookup returns a `SigningInfo`
    /// whose `team_id` is a non-matching string (e.g. a different
    /// Apple Team ID, like a third-party developer's). Verifier
    /// MUST return false.
    #[cfg(target_os = "macos")]
    #[test]
    fn verify_conductor_team_id_returns_false_for_non_matching_team_id() {
        struct MockLookup;
        impl SigningInfoLookup for MockLookup {
            fn lookup(&self, _: &AuditToken, _: u32) -> Option<SigningInfo> {
                Some(SigningInfo {
                    team_id: Some("FAKETEAMID".to_string()),
                })
            }
        }
        let token: AuditToken = [0; 8];
        assert!(!verify_conductor_team_id_with(&token, &MockLookup));
    }

    /// The allowlist is the entire surface of the IPC trust anchor, and a
    /// botched edit to it fails silently in the dangerous direction: a
    /// malformed entry simply never matches, so every peer degrades to
    /// `Untrusted` and the GUI stops talking to the daemon — at runtime, on
    /// a user's Mac, with no CI signal. An empty list does the same.
    ///
    /// Apple Team IDs are exactly 10 uppercase alphanumerics, so assert the
    /// shape here rather than discovering it post-release.
    #[cfg(target_os = "macos")]
    #[test]
    fn conductor_team_ids_are_wellformed() {
        assert!(
            !CONDUCTOR_TEAM_IDS.is_empty(),
            "empty allowlist fails every peer closed — no binary could ever be trusted"
        );
        for id in CONDUCTOR_TEAM_IDS {
            assert_eq!(
                id.len(),
                10,
                "Apple Team IDs are exactly 10 characters; got {id:?}"
            );
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
                "Apple Team IDs are uppercase alphanumeric; got {id:?}"
            );
        }
        let mut seen = CONDUCTOR_TEAM_IDS.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            CONDUCTOR_TEAM_IDS.len(),
            "duplicate Team ID entries — likely a botched transition edit"
        );
    }

    /// #1125 negative coverage. The lookup succeeds but the signing
    /// dict carries no `teamid` key (e.g. ad-hoc-signed dev builds
    /// — by design, `make dev-build` produces binaries without a
    /// Team ID). Verifier MUST return false.
    #[cfg(target_os = "macos")]
    #[test]
    fn verify_conductor_team_id_returns_false_for_missing_team_id() {
        struct MockLookup;
        impl SigningInfoLookup for MockLookup {
            fn lookup(&self, _: &AuditToken, _: u32) -> Option<SigningInfo> {
                Some(SigningInfo { team_id: None })
            }
        }
        let token: AuditToken = [0; 8];
        assert!(!verify_conductor_team_id_with(&token, &MockLookup));
    }

    /// #1125 negative coverage. The lookup itself fails (e.g.
    /// `SecCodeCopyGuestWithAttributes` returned non-zero — process
    /// already exited, permission denied, etc.). Verifier MUST
    /// return false.
    #[cfg(target_os = "macos")]
    #[test]
    fn verify_conductor_team_id_returns_false_when_lookup_fails() {
        struct MockLookup;
        impl SigningInfoLookup for MockLookup {
            fn lookup(&self, _: &AuditToken, _: u32) -> Option<SigningInfo> {
                None
            }
        }
        let token: AuditToken = [0; 8];
        assert!(!verify_conductor_team_id_with(&token, &MockLookup));
    }
}
