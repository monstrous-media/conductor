// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Test helper for ADR-027 D1 `still_pinned()` lifecycle.
//!
//! Connects to the unix socket whose path is passed as `argv[1]`,
//! writes a single byte so the parent's `accept()` returns with a
//! ready connection, then blocks in `pause()` until the parent
//! delivers a signal. This gives the test a real peer process whose
//! `(pid, exe)` can be pinned and then killed, exercising the
//! kernel-handle liveness check.

use std::io::Write;
use std::os::unix::net::UnixStream;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: d1-peer-test-helper <socket-path>");
    let mut stream = UnixStream::connect(&path).expect("connect to socket");
    stream.write_all(b"x").expect("write greeting byte");
    stream.flush().ok();
    // Block forever. The parent will SIGKILL us; pause() returns -1
    // when interrupted, but we won't reach the next line in the
    // SIGKILL case anyway because the kernel terminates the process.
    unsafe {
        libc::pause();
    }
}
