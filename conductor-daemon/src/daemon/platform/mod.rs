// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Platform-specific OS integration for the daemon.
//!
//! Currently houses focused-window-title detection (ADR-040 §4.3) — the one
//! daemon subsystem that needs a real OS permission escalation (macOS
//! Accessibility). Kept under a `platform/` module so the unsafe, per-OS leaves
//! stay isolated behind portable trait seams.

pub mod window_title;
