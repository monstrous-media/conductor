// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Bounded line-framing for the IPC server (#1313).
//!
//! Replaces `BufReader::read_line` with a helper that aborts as soon
//! as `max_bytes` is exceeded — BEFORE waiting for a newline or EOF.
//!
//! ## Why
//!
//! `read_line` reads bytes into the buffer until a newline or EOF is
//! seen. It honours no size cap. The IPC server documents a 1 MB
//! per-request cap to prevent memory exhaustion, but the check fires
//! only AFTER `read_line` returns. A malicious local peer can send
//! `MAX_REQUEST_SIZE + N` bytes with NO newline, keep the connection
//! open, and grow the daemon's per-connection buffer past 1 MB —
//! up to the 300 s IPC_IDLE_TIMEOUT or until the OS runs out of
//! memory. Clawpatch finding #1313, severity HIGH.
//!
//! ## What
//!
//! [`read_bounded_line`] reads into `buf` until any of:
//!
//! - A `\n` byte is found → returns `ReadResult::Line(n)` where `n`
//!   is the byte count INCLUDING the newline.
//! - EOF is reached with no bytes in `buf` → returns
//!   `ReadResult::Eof`.
//! - EOF is reached with partial bytes (no newline) → returns
//!   `ReadResult::Line(n)` so the caller can parse whatever arrived.
//!   Matches existing IPC semantics where `read_line` returned the
//!   partial bytes on EOF.
//! - `buf.len()` would exceed `max_bytes + 1` → returns
//!   `ReadResult::Overflow`. The caller MUST close the connection
//!   (the remaining bytes on the wire aren't drained — that's the
//!   point; an overflowed frame is by definition malformed).
//!
//! The `+1` on the cap is the byte that triggered the check —
//! semantically "we saw one byte past the cap" so a peer can't
//! sneak `max_bytes` of pre-newline content past the limit.
//!
//! ## Implementation note
//!
//! Uses the underlying `AsyncBufRead::fill_buf` / `consume` primitive
//! directly rather than wrapping the reader in a `Take` adapter +
//! nested `BufReader`. Nested buffering would discard any
//! pre-buffered bytes when the inner `BufReader` is dropped between
//! calls, breaking sequential reads from the same underlying stream.

use tokio::io::AsyncBufRead;
use tokio::io::AsyncBufReadExt as _; // `.fill_buf().await` lives on the Ext trait

/// Outcome of a single bounded read of one IPC frame.
#[derive(Debug)]
pub enum ReadResult {
    /// A full line was read; `n` bytes (including any trailing
    /// newline) were appended to `buf` during this call.
    Line(usize),
    /// Reader returned EOF with zero new bytes accumulated and `buf`
    /// was empty at call time — clean client disconnect.
    Eof,
    /// `buf.len()` reached `max_bytes + 1` before any newline was
    /// seen. Caller must close the connection (the peer is either
    /// buggy or hostile; subsequent bytes on the wire are
    /// unrecoverable).
    Overflow,
}

/// Read one bounded line into `buf` from `reader`.
///
/// See the module docs for the contract. Bytes are APPENDED to
/// `buf`; the caller is responsible for clearing between frames.
///
/// `max_bytes` is the **inclusive** cap on bytes-per-line (including
/// any trailing newline). A frame of exactly `max_bytes` ending in
/// `\n` succeeds with `Line(max_bytes)`. The first byte that would
/// push the buffer past `max_bytes` triggers `Overflow`.
pub async fn read_bounded_line<R>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<ReadResult>
where
    R: AsyncBufRead + Unpin,
{
    let start_len = buf.len();

    loop {
        // `fill_buf` borrows `reader` immutably; we extend_from_slice
        // (which copies) so the borrow ends before the subsequent
        // mutable `consume` call.
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            // EOF.
            let read_now = buf.len() - start_len;
            if read_now == 0 {
                return Ok(ReadResult::Eof);
            }
            // Partial line ended by EOF — return what arrived; matches
            // the legacy `read_line` semantic.
            return Ok(ReadResult::Line(read_now));
        }

        let already_read = buf.len() - start_len;
        let remaining_cap = max_bytes.saturating_sub(already_read);
        // Council R1: use saturating_add for all `cap + 1` arithmetic
        // so a pathological `max_bytes = usize::MAX` can't panic.
        // The +1 is the "trigger byte" — the first byte that would
        // push us over the inclusive cap.
        let cap_plus_one = remaining_cap.saturating_add(1);

        // Search for newline in the chunk we just got.
        let newline_pos = chunk.iter().position(|&b| b == b'\n');

        match newline_pos {
            Some(pos) if pos < cap_plus_one => {
                // Newline fits within the inclusive cap. Total bytes
                // after append = already_read + pos + 1; with the
                // guard `pos < cap_plus_one` this is at most
                // max_bytes + 1 (equality only at the strict
                // boundary, which we check below).
                let take = pos + 1;
                buf.extend_from_slice(&chunk[..take]);
                reader.consume(take);
                let total_read = buf.len() - start_len;
                if total_read > max_bytes {
                    // Boundary: the newline pushed us over by exactly
                    // one byte. Treat as Overflow per the inclusive
                    // cap contract. Council R3: truncate buf back to
                    // start_len so the caller doesn't see leaked
                    // bytes if they reuse the buffer.
                    buf.truncate(start_len);
                    return Ok(ReadResult::Overflow);
                }
                return Ok(ReadResult::Line(total_read));
            }
            _ => {
                // No newline in chunk, OR newline is beyond our
                // remaining cap. Take what we can without exceeding
                // the cap; if even that takes the whole chunk and
                // leaves remaining_cap intact, we just need more
                // bytes — loop.
                //
                // Council R2: the previous `safe_take == chunk.len()
                // && safe_take < remaining_cap + 1` second clause was
                // redundant. `safe_take = remaining_cap.min(chunk.len())`
                // means `safe_take == chunk.len()` ⇒ `chunk.len() ≤
                // remaining_cap` ⇒ `safe_take ≤ remaining_cap <
                // cap_plus_one`. So the AND was always true when the
                // first part was true.
                let safe_take = remaining_cap.min(chunk.len());
                if safe_take == chunk.len() {
                    // Whole chunk fits within remaining_cap; absorb
                    // and loop for more bytes.
                    buf.extend_from_slice(chunk);
                    reader.consume(safe_take);
                    continue;
                }
                // Chunk extends past cap. Record up to the (cap+1)-th
                // byte to trigger Overflow, then truncate per
                // Council R3.
                let trigger_take = cap_plus_one.min(chunk.len());
                buf.extend_from_slice(&chunk[..trigger_take]);
                reader.consume(trigger_take);
                buf.truncate(start_len);
                return Ok(ReadResult::Overflow);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader;

    /// Sanity: `read_bounded_line` correctly returns Line(n) for a
    /// normal newline-terminated frame. Mirrors the integration
    /// test but kept here so the module is self-checking.
    #[tokio::test]
    async fn line_with_newline() {
        let mut r = BufReader::new(Cursor::new(b"hi\n".to_vec()));
        let mut buf = Vec::new();
        let result = read_bounded_line(&mut r, &mut buf, 1024).await.unwrap();
        assert!(matches!(result, ReadResult::Line(3)));
        assert_eq!(buf, b"hi\n");
    }

    /// Sanity: empty reader returns Eof.
    #[tokio::test]
    async fn empty_returns_eof() {
        let mut r = BufReader::new(Cursor::new(Vec::<u8>::new()));
        let mut buf = Vec::new();
        let result = read_bounded_line(&mut r, &mut buf, 1024).await.unwrap();
        assert!(matches!(result, ReadResult::Eof));
    }
}
