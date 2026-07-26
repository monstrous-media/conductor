// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Regression tests for the IPC bounded-read helper (#1313).
//!
//! The bug: `BufReader::read_line(&mut String)` buffers until a newline
//! or EOF before returning. A client that sends `MAX_REQUEST_SIZE + N`
//! bytes WITHOUT a newline can grow the daemon's per-connection `String`
//! buffer past the documented 1 MB cap, since the size check fires
//! AFTER `read_line` returns. With the 300 s IPC_IDLE_TIMEOUT, a
//! malicious local client can sustain that growth → OOM.
//!
//! The fix: replace `read_line` with a bounded helper that aborts as
//! soon as `max_bytes` is exceeded, BEFORE waiting for a newline.

use conductor_daemon::daemon::ipc_framing::{ReadResult, read_bounded_line};
use std::io::Cursor;
use tokio::io::BufReader;

/// Under-limit single line: reader returns Line(n) with the full line
/// including the trailing newline. Bytes in buf match input.
#[tokio::test]
async fn under_limit_line_returns_line_outcome() {
    let input = b"hello world\n".to_vec();
    let cursor = Cursor::new(input.clone());
    let mut reader = BufReader::new(cursor);
    let mut buf = Vec::new();

    let result = read_bounded_line(&mut reader, &mut buf, 1024).await;

    match result {
        Ok(ReadResult::Line(n)) => {
            assert_eq!(n, input.len(), "byte count should match input");
            assert_eq!(buf, input, "buffer should contain the line");
        }
        other => panic!("expected Line, got {other:?}"),
    }
}

/// Empty reader (immediate EOF): returns Eof, buf untouched.
#[tokio::test]
async fn empty_reader_returns_eof() {
    let cursor: Cursor<Vec<u8>> = Cursor::new(Vec::new());
    let mut reader = BufReader::new(cursor);
    let mut buf = Vec::new();

    let result = read_bounded_line(&mut reader, &mut buf, 1024).await;

    assert!(matches!(result, Ok(ReadResult::Eof)), "got {result:?}");
    assert!(buf.is_empty(), "buf should be untouched on EOF");
}

/// Partial line + EOF (no newline): returns Line with the bytes the
/// client did send. Matches the documented IPC contract — clients
/// MAY close mid-line and the daemon parses whatever arrived.
#[tokio::test]
async fn partial_line_eof_returns_line_with_partial_bytes() {
    let input = b"partial".to_vec(); // no newline
    let cursor = Cursor::new(input.clone());
    let mut reader = BufReader::new(cursor);
    let mut buf = Vec::new();

    let result = read_bounded_line(&mut reader, &mut buf, 1024).await;

    match result {
        Ok(ReadResult::Line(n)) => {
            assert_eq!(n, input.len());
            assert_eq!(buf, input);
        }
        other => panic!("expected Line for partial-then-EOF, got {other:?}"),
    }
}

/// THE REGRESSION TEST for #1313. Feed `max_bytes + 1` bytes WITHOUT
/// a newline. The helper MUST return `Overflow` and the buffer MUST
/// NOT grow past `max_bytes + 1` (the +1 is the byte that triggered
/// the cap-check).
///
/// Without the fix, `read_line` would happily buffer the entire
/// `max_bytes + 1` AND keep reading until newline or EOF — meaning a
/// client streaming 10 MB without a newline grows the buffer to 10 MB.
#[tokio::test]
async fn over_limit_no_newline_returns_overflow_with_bounded_buffer() {
    const MAX: usize = 1024;
    let oversized: Vec<u8> = std::iter::repeat_n(b'x', MAX + 5000).collect();
    let cursor = Cursor::new(oversized);
    let mut reader = BufReader::new(cursor);
    let mut buf = Vec::new();

    let result = read_bounded_line(&mut reader, &mut buf, MAX).await;

    assert!(
        matches!(result, Ok(ReadResult::Overflow)),
        "expected Overflow for >MAX bytes without newline, got {result:?}"
    );
    assert!(
        buf.len() <= MAX + 1,
        "buffer must be bounded to MAX+1 ({} bytes); grew to {} bytes",
        MAX + 1,
        buf.len()
    );
}

/// THE STREAMING REGRESSION TEST for #1313 / #1495. The finite-`Cursor` test
/// above can't actually prove the helper stops *before* EOF: a `Cursor` hits
/// EOF immediately, so a buggy "read to EOF, then truncate and report
/// Overflow" implementation would pass it. Here the writer streams `MAX + 1`
/// bytes (no newline) and then **stays open** — the read half never sees EOF —
/// so the helper must return `Overflow` from the cap check alone. If it waited
/// for a newline/EOF (the original hang/OOM behaviour) it would block forever
/// and the timeout would fire, failing the test.
#[tokio::test]
async fn over_limit_without_eof_returns_overflow_before_eof() {
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    const MAX: usize = 1024;

    // duplex gives a connected reader/writer pair. Keep `writer` alive for the
    // whole test so the read half never observes EOF.
    let (mut writer, server) = tokio::io::duplex(MAX * 4);
    let payload: Vec<u8> = std::iter::repeat_n(b'x', MAX + 1).collect();
    writer.write_all(&payload).await.expect("write payload");
    // Deliberately do NOT drop/shutdown `writer` here.

    let mut reader = BufReader::new(server);
    let mut buf = Vec::new();

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        read_bounded_line(&mut reader, &mut buf, MAX),
    )
    .await
    .expect(
        "read_bounded_line hung waiting for newline/EOF instead of returning \
         Overflow at the cap (the #1313 streaming/OOM vector)",
    );

    assert!(
        matches!(outcome, Ok(ReadResult::Overflow)),
        "expected Overflow without EOF, got {outcome:?}"
    );
    assert!(
        buf.len() <= MAX + 1,
        "buffer must be bounded to MAX+1; grew to {} bytes",
        buf.len()
    );

    // Hold the writer open until after the assertions so the call above could
    // never have completed via EOF.
    drop(writer);
}

/// Exactly-at-limit line WITH trailing newline: must succeed (boundary
/// test for off-by-one in the cap check). MAX is the inclusive cap.
#[tokio::test]
async fn exactly_at_limit_with_newline_returns_line() {
    const MAX: usize = 1024;
    // MAX bytes total, last byte is newline.
    let mut input: Vec<u8> = std::iter::repeat_n(b'x', MAX - 1).collect();
    input.push(b'\n');
    assert_eq!(input.len(), MAX);

    let cursor = Cursor::new(input.clone());
    let mut reader = BufReader::new(cursor);
    let mut buf = Vec::new();

    let result = read_bounded_line(&mut reader, &mut buf, MAX).await;

    match result {
        Ok(ReadResult::Line(n)) => {
            assert_eq!(n, MAX, "expected exactly MAX bytes");
            assert_eq!(buf, input);
        }
        other => panic!("expected Line at boundary, got {other:?}"),
    }
}

/// Council R1 fix: on Overflow, the helper MUST truncate `buf` back
/// to its length at call time (no garbage bytes left behind). A
/// caller that mistakenly re-uses `buf` after Overflow without
/// clearing should NOT see leaked overflow bytes mixed into the
/// next frame.
#[tokio::test]
async fn overflow_truncates_buf_to_starting_length() {
    const MAX: usize = 64;
    let oversized: Vec<u8> = std::iter::repeat_n(b'x', MAX + 100).collect();
    let cursor = Cursor::new(oversized);
    let mut reader = BufReader::new(cursor);

    // Pre-load buf with some prefix bytes; helper must NOT touch
    // those, and on Overflow must restore buf to exactly this state.
    let mut buf = b"PREFIX:".to_vec();
    let prefix_len = buf.len();

    let result = read_bounded_line(&mut reader, &mut buf, MAX).await;

    assert!(
        matches!(result, Ok(ReadResult::Overflow)),
        "expected Overflow, got {result:?}"
    );
    assert_eq!(
        buf.len(),
        prefix_len,
        "buf must be truncated back to starting length on Overflow; \
         had {} bytes of prefix + leaked overflow bytes",
        buf.len() - prefix_len
    );
    assert_eq!(buf, b"PREFIX:", "prefix must survive Overflow untouched");
}

/// Multiple lines from the same reader: after a Line outcome, the
/// caller can clear the buffer and read the NEXT line successfully.
/// Verifies the helper doesn't lose bytes across iterations.
#[tokio::test]
async fn two_lines_can_be_read_sequentially() {
    let input = b"first line\nsecond line\n".to_vec();
    let cursor = Cursor::new(input);
    let mut reader = BufReader::new(cursor);
    let mut buf = Vec::new();

    let first = read_bounded_line(&mut reader, &mut buf, 1024).await;
    assert!(matches!(first, Ok(ReadResult::Line(11))), "got {first:?}");
    assert_eq!(buf, b"first line\n");

    buf.clear();
    let second = read_bounded_line(&mut reader, &mut buf, 1024).await;
    assert!(matches!(second, Ok(ReadResult::Line(12))), "got {second:?}");
    assert_eq!(buf, b"second line\n");
}
