// Copyright 2025 Amiable
// SPDX-License-Identifier: MIT

//! Conductor Core Engine
//!
//! Pure Rust MIDI mapping engine with zero UI dependencies.
//!
//! This library provides the core functionality for processing MIDI events,
//! mapping them to actions, and executing those actions. It's designed to be
//! embedded in applications that need MIDI controller mapping capabilities.
//!
//! This crate is used by:
//! - **conductor-daemon**: Background service with config hot-reload (v1.0.0+)
//! - **conductor**: Legacy direct-run application (deprecated in v1.0.0)
//! - External applications needing MIDI mapping capabilities
//!
//! # Architecture
//!
//! The engine follows a three-stage processing pipeline:
//!
//! 1. **MIDI Input** → [`MidiEvent`] (raw MIDI bytes converted to structured events)
//! 2. **Event Processing** → [`ProcessedEvent`] (adds timing, velocity, chord detection)
//! 3. **Mapping & Execution** → [`Action`] (matches events to actions and executes them)
//!
//! # Quick Start
//!
//! ```rust,no_run
//! use conductor_core::{Config, MappingEngine, EventProcessor};
//!
//! // Load configuration
//! let config = Config::load("config.toml").expect("Failed to load config");
//!
//! // Create engine components
//! let mut event_processor = EventProcessor::new();
//! let mut mapping_engine = MappingEngine::new();
//!
//! // Process MIDI events (in your event loop)
//! // let midi_event = ...; // from your MIDI input
//! // let processed = event_processor.process(midi_event);
//! // let action = mapping_engine.map_event(&processed, &config);
//! // Action execution is handled by conductor-daemon's ActionExecutor
//! ```
//!
//! # Features
//!
//! ## Trigger Types
//!
//! - **Note**: Basic note on/off with optional velocity range
//! - **VelocityRange**: Different actions for soft/medium/hard presses
//! - **LongPress**: Hold detection with configurable duration
//! - **DoubleTap**: Quick double-tap detection
//! - **NoteChord**: Multiple notes pressed simultaneously
//! - **EncoderTurn**: Encoder rotation with direction
//! - **Aftertouch**: Pressure sensitivity
//! - **PitchBend**: Touch strip control
//! - **CC**: Control change messages
//!
//! ## Action Types
//!
//! - **Keystroke**: Keyboard shortcuts with modifiers
//! - **Text**: Type text strings
//! - **Launch**: Open applications
//! - **Shell**: Execute shell commands
//! - **VolumeControl**: System volume control
//! - **ModeChange**: Switch between mapping modes
//! - **Sequence**: Chain multiple actions
//! - **Delay**: Timing control
//! - **MouseClick**: Mouse simulation
//! - **Repeat**: Repeat an action N times
//! - **Conditional**: Conditional execution
//!
//! ## System Features
//!
//! - **Multi-mode operation**: Switch between different mapping sets
//! - **Global mappings**: Work across all modes
//! - **Device profiles**: Support for device-specific configurations
//! - **LED feedback**: RGB LED control via HID or MIDI
//! - **Zero UI dependencies**: Pure engine library
//!
//! # Examples
//!
//! See the `conductor-daemon` package for a complete CLI implementation.

#![allow(dead_code, unused_variables, unused_imports)]
// TODO: Re-enable missing_docs after adding documentation to all public items
#![allow(missing_docs)]

// Public modules
pub mod actions;
pub mod config;
pub mod control_state; // Physical control state store (ADR-025)
pub mod device;
pub mod device_intelligence; // SysEx identity, fingerprinting (ADR-022 Phase 5)
pub mod dispatch; // Action dispatch result types (v4.25.0 - ADR-009 Gap 1)
pub mod engine;
pub mod error;
pub mod event_processor;
pub mod event_types; // Type-safe event/pattern enums (v4.9.0 - ADR-004)
pub mod events;
pub mod execution_context; // Per-event evaluation context (ADR-025)
pub mod feedback;
pub mod gamepad_events; // Gamepad/HID input mapping (v3.0)
pub mod gamepad_filters; // Gamepad stream-quality filters (#599)
pub mod identity; // Device identity types (v4.19.0 - ADR-009)
pub mod mapping; // Public for advanced event processing
pub mod midi_output; // MIDI output management (v2.1)
pub mod osc_pattern; // OSC 1.0 address pattern matching (ADR-039-A Slice 2, #2325)
pub mod resolver; // Port resolver for multi-device binding (v4.19.0 - ADR-009)
pub mod rule_compiler; // Config → CompiledRuleSet compiler (v4.21.0 - ADR-009 Phase 3)
pub mod rule_set; // Lock-free compiled rule set (v4.21.0 - ADR-009 Phase 3)
pub mod transform;
pub mod velocity; // Velocity mapping calculations (v2.2) // MIDI transform pipeline (v4.25.0 - ADR-009 Gap 2)

// Private modules (implementation details)
pub mod logging;
mod midi_feedback;
pub mod mikro_leds; // Structured logging with tracing

// Re-exports for convenience

// Engine
pub use engine::ConductorEngine;

// Configuration
pub use config::preferences::{
    DaemonAnalytics, DaemonLogging, DaemonSettings, GuiPreferences, GuiSettings,
};
pub use config::{
    ActionConfig, Config, ConfigMeta, ConfigSource, DeviceDirection, InputMode, ListenMode,
    LoggingConfig, Mapping, Mode, Trigger, UserFilePolicy,
};

// Events
pub use event_processor::EventProcessor;
pub use event_types::{
    EventType, FiredActionInfo, FiredResult, FiredTriggerInfo, MappingCancelledPayload,
    MappingDroppedPayload, MappingFiredPayload, MappingMatchedPayload, PatternType,
    action_type_string, summarize_action,
};
pub use events::{EncoderDirection, InputEvent, MidiEvent, ProcessedEvent, VelocityLevel};
// ADR-039 cross-protocol substrate (#1758)
pub use events::{DMX_UNIVERSE_SIZE, DmxFrame, DmxInbound, OscInbound, ProtocolEvent};

// Actions (ActionExecutor moved to conductor-daemon in Phase 2 security refactor)
// Domain-specific types for platform-independent action representation
pub use actions::{
    Action, Condition, KeyCode, MidiMessageParams, MidiMessageType, ModifierKey, MouseButton,
    OscArg, VelocityCurve, VelocityMapping, VolumeOperation,
};

// Feedback
pub use feedback::{
    FeedbackManager, HidLedFeedback, LightingScheme, PadFeedback, create_feedback_manager,
};

// Device Profiles
pub use device::{DeviceProfile, PadPageMapping};

// Errors
pub use error::{ActionError, ConfigError, EngineError, FeedbackError, ProfileError};

// Mapping
pub use mapping::MappingEngine;

// MIDI Output (v2.1)
pub use midi_output::{MidiMessage, MidiOutputManager};

// Plugin System (v2.3)
pub mod plugin;

// Plugin ID validation (v5.4-alpha, ADR-027 D10c PR #1029
// round-2). Single source of truth for the plugin-id character
// set; available without `plugin-registry` so wasm_runtime
// (gated behind `plugin-wasm`) can use the same rules.
pub mod plugin_id;

// Plugin Registry (v2.4)
#[cfg(feature = "plugin-registry")]
pub mod plugin_registry;

// Security primitives (ADR-027 Phase 1A — D5 risk tiers, D3 capabilities,
// D1 peer-cred trait shapes).
pub mod security;
