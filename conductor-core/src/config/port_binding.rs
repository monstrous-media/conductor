// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Direction classification for I/O endpoints (ADR-021).
//!
//! `DeviceDirection` classifies an endpoint as Input, Output, or Bidirectional.
//! The legacy `DevicePortBinding` / `DeviceIdentityConfig` types were removed in
//! ADR-035 (unified `[[endpoints]]`); direction now lives on `EndpointConfig`.

use serde::{Deserialize, Serialize};

/// Direction classification for an I/O endpoint (ADR-021 D3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceDirection {
    /// Endpoint only receives input (default)
    #[default]
    Input,
    /// Endpoint only sends output (e.g., LED controller, external synth)
    Output,
    /// Endpoint has both input and output ports (e.g., Maschine Mikro with pads + LEDs)
    Bidirectional,
}
