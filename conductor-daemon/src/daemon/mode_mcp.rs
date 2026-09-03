// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-040 D4 §4.2 — shared execution for the mode-lock MCP tools.
//!
//! `conductor_set_mode` / `conductor_unlock_mode` / `conductor_mode_status` need
//! the live `EngineManager` lock state, which neither the MCP server (`mcp.rs`)
//! nor the LLM executor (`executor.rs`) holds directly — they only have the
//! daemon command channel. Both run in their own tasks (not the `command_rx`
//! select arm), so they can send a [`DaemonCommand`] and await the oneshot
//! reply without deadlocking. These helpers centralise that send/await so the
//! two call sites stay identical; each takes the command sender so they're
//! testable with a bare channel.

use super::types::DaemonCommand;
use crate::daemon::mcp_types::ToolCallResult;
#[cfg(feature = "llm-executor")]
use serde_json::Value;
#[cfg(any(test, feature = "llm-executor"))]
use serde_json::json;
use std::time::Duration;
use tokio::sync::mpsc;

/// Bound the wait so a stalled engine loop surfaces a clear error to the MCP
/// client instead of hanging. Mode mutations are near-instant.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// `conductor_set_mode { mode, lock=true }` — set the active mode, optionally
/// locking it against auto-switching (origin `Mcp`).
#[cfg(feature = "llm-executor")]
pub(crate) async fn set_mode(
    command_tx: &mpsc::Sender<DaemonCommand>,
    args: Option<&Value>,
) -> ToolCallResult {
    let Some(mode) = args.and_then(|a| a.get("mode")).and_then(|v| v.as_str()) else {
        return ToolCallResult::error("Missing required argument: mode");
    };
    // Locks by default (mirrors `conductorctl mode set`); pass `lock: false` to
    // switch without locking.
    let lock = args
        .and_then(|a| a.get("lock"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let (tx, rx) = tokio::sync::oneshot::channel();
    if command_tx
        .send(DaemonCommand::SetModeLocked {
            mode: mode.to_string(),
            lock,
            response_tx: tx,
        })
        .await
        .is_err()
    {
        return ToolCallResult::error(
            "Failed to dispatch set-mode command (daemon command channel closed)",
        );
    }
    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(Ok(()))) => {
            ToolCallResult::json(&json!({ "mode": mode, "locked": lock, "status": "set" }))
        }
        Ok(Ok(Err(msg))) => ToolCallResult::error(&format!("Failed to set mode: {msg}")),
        // Receiver got RecvError: the engine dropped the reply (shutting down) —
        // distinct from a timeout.
        Ok(Err(_)) => ToolCallResult::error("Daemon dropped the set-mode reply"),
        Err(_) => ToolCallResult::error("Timed out awaiting set-mode result"),
    }
}

/// `conductor_unlock_mode {}` — release the manual lock, resuming auto-switching.
#[cfg(feature = "llm-executor")]
pub(crate) async fn unlock_mode(command_tx: &mpsc::Sender<DaemonCommand>) -> ToolCallResult {
    let (tx, rx) = tokio::sync::oneshot::channel();
    if command_tx
        .send(DaemonCommand::ReleaseModeLock { response_tx: tx })
        .await
        .is_err()
    {
        return ToolCallResult::error(
            "Failed to dispatch unlock command (daemon command channel closed)",
        );
    }
    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(was_locked)) => ToolCallResult::json(&json!({ "unlocked": was_locked })),
        Ok(Err(_)) => ToolCallResult::error("Daemon dropped the unlock reply"),
        Err(_) => ToolCallResult::error("Timed out awaiting unlock result"),
    }
}

/// `conductor_mode_status {}` — report the active mode + lock state.
pub(crate) async fn mode_status(command_tx: &mpsc::Sender<DaemonCommand>) -> ToolCallResult {
    let (tx, rx) = tokio::sync::oneshot::channel();
    if command_tx
        .send(DaemonCommand::QueryModeStatus { response_tx: tx })
        .await
        .is_err()
    {
        return ToolCallResult::error(
            "Failed to dispatch mode-status query (daemon command channel closed)",
        );
    }
    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(status)) => ToolCallResult::json(&status),
        Ok(Err(_)) => ToolCallResult::error("Daemon dropped the mode-status reply"),
        Err(_) => ToolCallResult::error("Timed out awaiting mode status"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn a fake engine that receives one command and replies via its
    /// oneshot, so each helper's send→await→format path is testable with a
    /// bare channel (no EngineManager). Returns the join handle so the caller
    /// can await it and surface any assertion panic from the handler — a
    /// detached task would swallow it.
    fn channel_with_fake_engine<F>(
        handler: F,
    ) -> (mpsc::Sender<DaemonCommand>, tokio::task::JoinHandle<()>)
    where
        F: FnOnce(DaemonCommand) + Send + 'static,
    {
        let (tx, mut rx) = mpsc::channel::<DaemonCommand>(4);
        let handle = tokio::spawn(async move {
            let cmd = rx.recv().await.expect("fake engine received no command");
            handler(cmd);
        });
        (tx, handle)
    }

    #[cfg(feature = "llm-executor")]
    #[tokio::test]
    async fn set_mode_success_replies_set() {
        let (tx, h) = channel_with_fake_engine(|cmd| match cmd {
            DaemonCommand::SetModeLocked {
                mode,
                lock,
                response_tx,
            } => {
                assert_eq!(mode, "Edit");
                assert!(lock, "lock defaults to true");
                let _ = response_tx.send(Ok(()));
            }
            other => panic!("expected SetModeLocked, got {other:?}"),
        });
        let res = set_mode(&tx, Some(&json!({ "mode": "Edit" }))).await;
        h.await.unwrap();
        assert!(res.is_error.is_none(), "{res:?}");
    }

    #[cfg(feature = "llm-executor")]
    #[tokio::test]
    async fn set_mode_unknown_replies_error() {
        let (tx, h) = channel_with_fake_engine(|cmd| match cmd {
            DaemonCommand::SetModeLocked { response_tx, .. } => {
                let _ = response_tx.send(Err("unknown mode 'Ghost'".to_string()));
            }
            other => panic!("expected SetModeLocked, got {other:?}"),
        });
        let res = set_mode(&tx, Some(&json!({ "mode": "Ghost" }))).await;
        h.await.unwrap();
        assert!(res.is_error.is_some());
    }

    #[cfg(feature = "llm-executor")]
    #[tokio::test]
    async fn set_mode_lock_false_is_forwarded() {
        let (tx, h) = channel_with_fake_engine(|cmd| match cmd {
            DaemonCommand::SetModeLocked {
                lock, response_tx, ..
            } => {
                assert!(!lock, "lock:false forwarded");
                let _ = response_tx.send(Ok(()));
            }
            other => panic!("expected SetModeLocked, got {other:?}"),
        });
        let res = set_mode(&tx, Some(&json!({ "mode": "Edit", "lock": false }))).await;
        h.await.unwrap();
        assert!(res.is_error.is_none());
    }

    #[cfg(feature = "llm-executor")]
    #[tokio::test]
    async fn set_mode_missing_arg_errors_without_dispatch() {
        let (tx, _rx) = mpsc::channel::<DaemonCommand>(4);
        let res = set_mode(&tx, Some(&json!({}))).await;
        assert!(res.is_error.is_some());
    }

    #[cfg(feature = "llm-executor")]
    #[tokio::test]
    async fn unlock_replies_was_locked() {
        let (tx, h) = channel_with_fake_engine(|cmd| match cmd {
            DaemonCommand::ReleaseModeLock { response_tx } => {
                let _ = response_tx.send(true);
            }
            other => panic!("expected ReleaseModeLock, got {other:?}"),
        });
        let res = unlock_mode(&tx).await;
        h.await.unwrap();
        assert!(res.is_error.is_none());
    }

    #[tokio::test]
    async fn status_replies_json() {
        let (tx, h) = channel_with_fake_engine(|cmd| match cmd {
            DaemonCommand::QueryModeStatus { response_tx } => {
                let _ = response_tx.send(json!({ "mode": "Mix", "locked": false }));
            }
            other => panic!("expected QueryModeStatus, got {other:?}"),
        });
        let res = mode_status(&tx).await;
        h.await.unwrap();
        assert!(res.is_error.is_none());
    }
}
