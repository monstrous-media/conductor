// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! HardwareIO tier implementation for dangerous MIDI operations (P4-01)
//!
//! This module provides multi-step confirmation for hardware I/O operations
//! like SysEx messages that could potentially modify device firmware or settings.
//!
//! # Security Model
//!
//! HardwareIO operations require a two-phase confirmation:
//! 1. First call returns a confirmation token and risk assessment
//! 2. Second call with valid token executes the operation
//!
//! Tokens expire after 60 seconds to prevent stale confirmations.

mod confirmation;
mod sysex;

pub use confirmation::{
    ConfirmationManager, ConfirmationRequest, ConfirmationStatus, ConfirmationToken,
    HardwareIoError, MidiSendMessage,
};
pub use sysex::{SysExCategory, SysExValidation, SysExValidator};

#[cfg(test)]
mod tests;
