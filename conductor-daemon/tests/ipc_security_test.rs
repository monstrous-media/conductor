// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "test diagnostic output"
)]

//! Integration tests for IPC request-size security controls.
//!
//! #1517: previously these tests only compared locally-constructed string
//! lengths against a *duplicated* `MAX_REQUEST_SIZE` literal — they never sent
//! anything through the server's reader, so a removed / inverted / off-by-one
//! size check would not fail them, and they would keep passing if the real
//! limit changed.
//!
//! They now import the production `MAX_REQUEST_SIZE` (no duplicated literal) and
//! drive bytes through `read_bounded_line(.., MAX_REQUEST_SIZE)` — the exact
//! call the IPC server makes for every request (ipc.rs) — asserting the real
//! `ReadResult` (`Line` accepted vs `Overflow` rejected). That is the actual
//! size-limit enforcement.
//!
//! The subsequent `Overflow` → `IpcErrorCode::InvalidRequest` (1004) response
//! mapping lives inline in the server's per-connection socket loop (ipc.rs) and
//! would need a live `UnixStream` round-trip to exercise — out of scope here.
//! `error_code_invalid_request_is_1004` pins the error *code* the server uses.

use conductor_daemon::daemon::error::IpcErrorCode;
use conductor_daemon::daemon::ipc::MAX_REQUEST_SIZE;
use conductor_daemon::daemon::ipc_framing::{ReadResult, read_bounded_line};
use std::io::Cursor;
use tokio::io::BufReader;

/// Read `bytes` through the same bounded reader + limit the IPC server uses.
async fn read_through_server_limit(bytes: Vec<u8>) -> ReadResult {
    let mut reader = BufReader::new(Cursor::new(bytes));
    let mut buf = Vec::new();
    read_bounded_line(&mut reader, &mut buf, MAX_REQUEST_SIZE)
        .await
        .expect("bounded read should not error on an in-memory cursor")
}

#[tokio::test]
async fn below_limit_request_is_accepted() {
    // A normal newline-terminated request well under the limit is read as a Line.
    let line = b"{\"id\":\"x\",\"command\":\"PING\",\"args\":{}}\n".to_vec();
    assert!(line.len() < MAX_REQUEST_SIZE);
    assert!(
        matches!(read_through_server_limit(line).await, ReadResult::Line(_)),
        "a below-limit request must be accepted by the server reader"
    );
}

#[tokio::test]
async fn at_limit_request_is_accepted() {
    // Exactly MAX_REQUEST_SIZE bytes including the trailing newline: the
    // inclusive boundary must be accepted (guards against an off-by-one that
    // rejects valid max-size requests).
    let mut line = vec![b'x'; MAX_REQUEST_SIZE - 1];
    line.push(b'\n');
    assert_eq!(line.len(), MAX_REQUEST_SIZE);
    assert!(
        matches!(read_through_server_limit(line).await, ReadResult::Line(_)),
        "a request of exactly MAX_REQUEST_SIZE bytes must be accepted"
    );
}

#[tokio::test]
async fn over_limit_request_is_rejected() {
    // MAX_REQUEST_SIZE + 1 bytes with no newline: the server reader must report
    // Overflow (which the server maps to an InvalidRequest/1004 error response)
    // rather than buffering unbounded memory. A removed/inverted size check
    // would return Line here and fail this assertion.
    let oversized = vec![b'x'; MAX_REQUEST_SIZE + 1];
    assert!(
        matches!(
            read_through_server_limit(oversized).await,
            ReadResult::Overflow
        ),
        "an over-limit request must be rejected (Overflow) by the server reader"
    );
}

#[test]
fn error_code_invalid_request_is_1004() {
    // The status the server returns when the reader reports Overflow for an
    // over-limit request (see over_limit_request_is_rejected).
    let error_code = IpcErrorCode::InvalidRequest;
    assert_eq!(error_code.as_u16(), 1004);
    assert!(error_code.message().contains("Invalid request"));
}
