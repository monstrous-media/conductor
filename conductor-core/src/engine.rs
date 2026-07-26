// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Conductor Core Engine
//!
//! This module provides a minimal engine stub for conductor-core.
//! The full engine implementation is in conductor-daemon's EngineManager.
//!
//! This stub is retained for API compatibility and basic state management.

#![allow(dead_code, unused_variables)]

use crate::config::Config;
use crate::error::EngineError;

/// Main engine for MIDI mapping and processing
pub struct ConductorEngine {
    config: Config,
    running: bool,
}

impl ConductorEngine {
    /// Create a new engine with the given configuration
    pub fn new(config: Config) -> Result<Self, EngineError> {
        Ok(Self {
            config,
            running: false,
        })
    }

    /// Start the engine (connects to devices and begins processing)
    ///
    /// Note: Full device connection and event processing is handled by
    /// conductor-daemon's EngineManager. This stub manages running state only.
    pub fn start(&mut self) -> Result<(), EngineError> {
        if self.running {
            return Err(EngineError::AlreadyRunning);
        }
        self.running = true;
        Ok(())
    }

    /// Stop the engine
    ///
    /// Note: Full shutdown is handled by conductor-daemon's EngineManager.
    /// This stub manages running state only.
    pub fn stop(&mut self) -> Result<(), EngineError> {
        if !self.running {
            return Err(EngineError::NotRunning);
        }
        self.running = false;
        Ok(())
    }

    /// Check if engine is running
    pub fn is_running(&self) -> bool {
        self.running
    }
}
