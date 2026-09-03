// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use crate::MidiEvent;
use crate::actions::Action;
use crate::config::{Config, Mapping, Trigger};
use crate::event_processor::ProcessedEvent;
use crate::events::VelocityLevel;
use std::collections::HashMap;
use tracing::{debug, trace, warn};

/// Classify velocity using configurable thresholds
/// This function respects user-defined soft_max and medium_max boundaries
/// instead of the hard-coded 40/80 defaults in EventProcessor.
pub(crate) fn classify_velocity(velocity: u8, soft_max: u8, medium_max: u8) -> VelocityLevel {
    if velocity <= soft_max {
        VelocityLevel::Soft
    } else if velocity <= medium_max {
        VelocityLevel::Medium
    } else {
        VelocityLevel::Hard
    }
}

pub struct MappingEngine {
    mode_mappings: HashMap<u8, Vec<CompiledMapping>>,
    global_mappings: Vec<CompiledMapping>,
}

impl Default for MappingEngine {
    fn default() -> Self {
        Self::new()
    }
}

struct CompiledMapping {
    trigger: CompiledTrigger,
    action: Action,
    description: Option<String>,
    /// Device filter: only match events from this device alias (ADR-009)
    device: Option<String>,
}

/// Compiled trigger representation used by both MappingEngine and CompiledRuleSet
/// (ADR-009 Phase 3: extracted for reuse by rule_set module)
#[derive(Debug, Clone)]
pub(crate) enum CompiledTrigger {
    Note {
        note: u8,
        velocity_min: u8,
        channel: Option<u8>,
    },
    CC {
        cc: u8,
        value_min: u8,
        channel: Option<u8>,
    },
    NoteChord {
        notes: Vec<u8>,
        channel: Option<u8>,
    },
    // ADR-002 Gap Fixes
    DoubleTap {
        note: u8,
        channel: Option<u8>,
    },
    LongPress {
        note: u8,
        duration_ms: u64,
        channel: Option<u8>,
    },
    Aftertouch {
        pressure_min: u8,
        channel: Option<u8>,
    },
    /// Polyphonic aftertouch — per-note pressure.
    PolyAftertouch {
        note: u8,
        pressure_min: u8,
        channel: Option<u8>,
    },
    PitchBend {
        value_min: Option<u16>,
        value_max: Option<u16>,
        channel: Option<u8>,
    },
    VelocityRange {
        note: u8,
        soft_max: u8,
        medium_max: u8,
        channel: Option<u8>,
    },
    EncoderTurn {
        cc: u8,
        direction: Option<String>,
        channel: Option<u8>,
    },
    // ADR-025 Phase 1
    ProgramChange {
        pc: Option<u8>,
        channel: Option<u8>,
    },
    // Gamepad triggers
    GamepadButton {
        button: u8,
        velocity_min: u8,
    },
    GamepadButtonChord {
        buttons: Vec<u8>,
    },
    GamepadAnalogStick {
        axis: u8,
        direction: Option<String>,
    },
    GamepadTrigger {
        trigger: u8,
        threshold: u8,
    },
    // OSC triggers (ADR-039-A). The pattern is compiled at
    // config load (`OscPattern::compile`) so per-event matching is pure
    // glob evaluation — invalid patterns are rejected by the validator and,
    // defensively, compile to a never-matching trigger here.
    OscMessage {
        address: String,
    },
    OscAddressPattern {
        pattern: crate::osc_pattern::OscPattern,
    },
    OscArgRange {
        arg_index: usize,
        min: f32,
        max: f32,
    },
}

/// Compile a config Trigger into a CompiledTrigger
/// (ADR-009 Phase 3: extracted from MappingEngine for reuse by rule_compiler)
pub(crate) fn compile_trigger(trigger: &Trigger) -> CompiledTrigger {
    match trigger {
        Trigger::Note {
            note,
            velocity_min,
            channel,
            ..
        } => CompiledTrigger::Note {
            note: *note,
            velocity_min: velocity_min.unwrap_or(1),
            channel: *channel,
        },
        Trigger::CC {
            cc,
            value_min,
            channel,
            ..
        } => CompiledTrigger::CC {
            cc: *cc,
            value_min: value_min.unwrap_or(0),
            channel: *channel,
        },
        Trigger::NoteChord { notes, channel, .. } => CompiledTrigger::NoteChord {
            notes: notes.clone(),
            channel: *channel,
        },
        // Gamepad triggers
        Trigger::GamepadButton {
            button,
            velocity_min,
            ..
        } => CompiledTrigger::GamepadButton {
            button: *button,
            velocity_min: velocity_min.unwrap_or(1),
        },
        Trigger::GamepadButtonChord { buttons, .. } => CompiledTrigger::GamepadButtonChord {
            buttons: buttons.clone(),
        },
        Trigger::GamepadAnalogStick {
            axis, direction, ..
        } => CompiledTrigger::GamepadAnalogStick {
            axis: *axis,
            direction: direction.clone(),
        },
        Trigger::GamepadTrigger {
            trigger, threshold, ..
        } => CompiledTrigger::GamepadTrigger {
            trigger: *trigger,
            threshold: threshold.unwrap_or(0),
        },
        // ADR-002 Gap Fixes
        Trigger::DoubleTap { note, channel, .. } => CompiledTrigger::DoubleTap {
            note: *note,
            channel: *channel,
        },
        Trigger::LongPress {
            note,
            duration_ms,
            channel,
            ..
        } => CompiledTrigger::LongPress {
            note: *note,
            duration_ms: duration_ms.unwrap_or(2000),
            channel: *channel,
        },
        Trigger::Aftertouch {
            pressure_min,
            channel,
            ..
        } => CompiledTrigger::Aftertouch {
            pressure_min: pressure_min.unwrap_or(0),
            channel: *channel,
        },
        Trigger::PolyAftertouch {
            note,
            pressure_min,
            channel,
            ..
        } => CompiledTrigger::PolyAftertouch {
            note: *note,
            pressure_min: pressure_min.unwrap_or(0),
            channel: *channel,
        },
        Trigger::PitchBend {
            value_min,
            value_max,
            channel,
            ..
        } => CompiledTrigger::PitchBend {
            value_min: *value_min,
            value_max: *value_max,
            channel: *channel,
        },
        Trigger::VelocityRange {
            note,
            soft_max,
            medium_max,
            channel,
            ..
        } => CompiledTrigger::VelocityRange {
            note: *note,
            soft_max: soft_max.unwrap_or(40),
            medium_max: medium_max.unwrap_or(80),
            channel: *channel,
        },
        Trigger::EncoderTurn {
            cc,
            direction,
            channel,
            ..
        } => CompiledTrigger::EncoderTurn {
            cc: *cc,
            direction: direction.clone(),
            channel: *channel,
        },
        Trigger::ProgramChange { pc, channel, .. } => CompiledTrigger::ProgramChange {
            pc: *pc,
            channel: *channel,
        },
        // OSC triggers (ADR-039-A)
        Trigger::OscMessage { address, .. } => CompiledTrigger::OscMessage {
            address: address.clone(),
        },
        Trigger::OscAddressPattern { pattern, .. } => CompiledTrigger::OscAddressPattern {
            // The validator rejects invalid patterns at config load; if one
            // slips through (defense-in-depth), compile to a never-matching
            // pattern — fail closed, never panic on the hot path.
            pattern: crate::osc_pattern::OscPattern::compile(pattern)
                .unwrap_or_else(|_| crate::osc_pattern::OscPattern::never()),
        },
        Trigger::OscArgRange {
            arg_index,
            min,
            max,
            ..
        } => CompiledTrigger::OscArgRange {
            arg_index: *arg_index,
            min: *min,
            max: *max,
        },
    }
}

/// Compile an ActionConfig into an Action.
///
/// Routes through [`crate::config::compile::lower_action`] (ADR-025
/// Phase 2.E) so context-switch sugar (`PcContextSwitch`,
/// `CcContextSwitch`) is expanded into nested `Conditional` chains
/// before the rule-compiler caches the compiled rule set. Non-sugar
/// variants delegate to `From<ActionConfig> for Action` unchanged.
///
/// (ADR-009 Phase 3: extracted from MappingEngine for
/// reuse by rule_compiler.)
pub(crate) fn compile_action(action: &crate::config::ActionConfig) -> Action {
    crate::config::compile::lower_action(action.clone())
}

/// Check if event channel matches trigger channel filter.
/// - `trigger_channel: None` → match any event channel (backward compatible)
/// - `trigger_channel: Some(ch)` → match only events with `channel: Some(ch)`
fn channel_matches(trigger_channel: Option<u8>, event_channel: Option<u8>) -> bool {
    match trigger_channel {
        None => true, // No filter: match any channel
        Some(tch) => event_channel == Some(tch),
    }
}

/// Check if a CompiledTrigger matches a ProcessedEvent
/// (ADR-009 Phase 3: extracted from MappingEngine for reuse by rule_set)
pub(crate) fn trigger_matches_processed(trigger: &CompiledTrigger, event: &ProcessedEvent) -> bool {
    match (trigger, event) {
        // Note trigger matches PadPressed for MIDI notes (note < 128)
        (
            CompiledTrigger::Note {
                note,
                velocity_min,
                channel: tch,
            },
            ProcessedEvent::PadPressed {
                note: ev_note,
                velocity,
                channel: ev_ch,
                ..
            },
        ) => {
            // Only match MIDI range (0-127), not gamepad (128+)
            *note == *ev_note
                && *velocity >= *velocity_min
                && *ev_note < 128
                && channel_matches(*tch, *ev_ch)
        }
        (
            CompiledTrigger::NoteChord {
                notes,
                channel: tch,
            },
            ProcessedEvent::ChordDetected {
                notes: detected_notes,
                channel: ev_ch,
                ..
            },
        ) => {
            // Exact match: detected notes must exactly equal required notes (no subset matching)
            // Both lists are sorted for order-independent comparison (ADR-002)
            let mut required = notes.clone();
            let mut detected = detected_notes.clone();
            required.sort_unstable();
            detected.sort_unstable();

            required == detected && channel_matches(*tch, *ev_ch)
        }
        // Gamepad button press
        (
            CompiledTrigger::GamepadButton {
                button,
                velocity_min,
            },
            ProcessedEvent::PadPressed { note, velocity, .. },
        ) => {
            // Gamepad buttons use IDs 128-255 to avoid MIDI conflicts
            *button == *note && *velocity >= *velocity_min && *note >= 128
        }
        // Gamepad button chord
        (
            CompiledTrigger::GamepadButtonChord { buttons },
            ProcessedEvent::ChordDetected {
                notes: detected_buttons,
                ..
            },
        ) => {
            // Check if all required gamepad buttons are present
            // Sort both lists for comparison
            let mut required = buttons.clone();
            let mut detected = detected_buttons.clone();
            required.sort_unstable();
            detected.sort_unstable();

            // Only match if all buttons are in gamepad range (128-255)
            required == detected && required.iter().all(|b| *b >= 128)
        }
        // Gamepad analog stick
        (
            CompiledTrigger::GamepadAnalogStick { axis, direction },
            ProcessedEvent::EncoderTurned {
                cc,
                direction: ev_direction,
                ..
            },
        ) => {
            // Gamepad analog sticks use axis IDs 128-131; ADR-047 §D3b also
            // exposes d-pad-as-axis on the reserved encoder ids 147/148.
            let stick_axis = (128..=131).contains(cc) || matches!(*cc, 147 | 148);
            if *axis != *cc || !stick_axis {
                return false;
            }

            // Check direction if specified
            match direction {
                Some(dir) if dir == "Clockwise" => {
                    matches!(
                        ev_direction,
                        crate::event_processor::EncoderDirection::Clockwise
                    )
                }
                Some(dir) if dir == "CounterClockwise" => {
                    matches!(
                        ev_direction,
                        crate::event_processor::EncoderDirection::CounterClockwise
                    )
                }
                _ => true, // Any direction
            }
        }
        // Gamepad analog trigger
        (
            CompiledTrigger::GamepadTrigger { trigger, threshold },
            ProcessedEvent::EncoderTurned { cc, value, .. },
        ) => {
            // Gamepad analog triggers use IDs 132-133
            *trigger == *cc && *value >= *threshold && (*cc == 132 || *cc == 133)
        }
        // ADR-002 Gap Fixes
        // DoubleTap trigger matches DoubleTap event (match by note only, ignore new fields)
        (
            CompiledTrigger::DoubleTap { note, channel: tch },
            ProcessedEvent::DoubleTap {
                note: ev_note,
                channel: ev_ch,
                ..
            },
        ) => *note == *ev_note && channel_matches(*tch, *ev_ch),
        // LongPress trigger matches LongPress event when duration exceeds threshold
        (
            CompiledTrigger::LongPress {
                note,
                duration_ms,
                channel: tch,
            },
            ProcessedEvent::LongPress {
                note: ev_note,
                duration_ms: ev_duration,
                channel: ev_ch,
                ..
            },
        ) => {
            *note == *ev_note
                && *ev_duration >= (*duration_ms as u128)
                && channel_matches(*tch, *ev_ch)
        }
        // Aftertouch trigger matches AftertouchChanged when pressure >= threshold
        (
            CompiledTrigger::Aftertouch {
                pressure_min,
                channel: tch,
            },
            ProcessedEvent::AftertouchChanged {
                pressure,
                channel: ev_ch,
            },
        ) => *pressure >= *pressure_min && channel_matches(*tch, *ev_ch),
        // PolyAftertouch trigger matches PolyAftertouchChanged when
        // note matches AND pressure >= threshold.
        (
            CompiledTrigger::PolyAftertouch {
                note,
                pressure_min,
                channel: tch,
            },
            ProcessedEvent::PolyAftertouchChanged {
                note: ev_note,
                pressure,
                channel: ev_ch,
            },
        ) => *note == *ev_note && *pressure >= *pressure_min && channel_matches(*tch, *ev_ch),
        // PitchBend trigger matches PitchBendMoved when value is in range
        (
            CompiledTrigger::PitchBend {
                value_min,
                value_max,
                channel: tch,
            },
            ProcessedEvent::PitchBendMoved {
                value,
                channel: ev_ch,
            },
        ) => {
            let min_ok = value_min.is_none_or(|min| *value >= min);
            let max_ok = value_max.is_none_or(|max| *value <= max);
            min_ok && max_ok && channel_matches(*tch, *ev_ch)
        }
        // VelocityRange trigger matches PadPressed (MIDI range only)
        // Fix: use config soft_max/medium_max instead of ignoring them (GitHub #48)
        (
            CompiledTrigger::VelocityRange {
                note,
                soft_max,
                medium_max,
                channel: tch,
            },
            ProcessedEvent::PadPressed {
                note: ev_note,
                velocity,
                velocity_level: _,
                channel: ev_ch,
                ..
            },
        ) => {
            // Only match MIDI range (0-127), not gamepad (128+)
            if *note != *ev_note || *ev_note >= 128 || !channel_matches(*tch, *ev_ch) {
                return false;
            }
            // Re-classify velocity using config thresholds (not EventProcessor defaults)
            let level = classify_velocity(*velocity, *soft_max, *medium_max);
            trace!(
                note = *ev_note,
                velocity = *velocity,
                soft_max = *soft_max,
                medium_max = *medium_max,
                level = ?level,
                "VelocityRange trigger matched with config thresholds"
            );
            true
        }
        // CC trigger matches CCReceived for pedals/buttons
        (
            CompiledTrigger::CC {
                cc,
                value_min,
                channel: tch,
            },
            ProcessedEvent::CCReceived {
                cc: ev_cc,
                value,
                channel: ev_ch,
            },
        ) => {
            // Match CC number and check value threshold
            *cc == *ev_cc && *value >= *value_min && channel_matches(*tch, *ev_ch)
        }
        // CC trigger also matches EncoderTurned for backwards compatibility
        (
            CompiledTrigger::CC {
                cc,
                value_min,
                channel: tch,
            },
            ProcessedEvent::EncoderTurned {
                cc: ev_cc,
                value,
                channel: ev_ch,
                ..
            },
        ) => {
            // Only match MIDI CC range (0-127), not gamepad
            *cc == *ev_cc && *value >= *value_min && *ev_cc < 128 && channel_matches(*tch, *ev_ch)
        }
        // EncoderTurn trigger matches EncoderTurned for MIDI encoders (cc < 128)
        (
            CompiledTrigger::EncoderTurn {
                cc,
                direction,
                channel: tch,
            },
            ProcessedEvent::EncoderTurned {
                cc: ev_cc,
                direction: ev_direction,
                channel: ev_ch,
                ..
            },
        ) => {
            // Only match MIDI encoder range (0-127), not gamepad
            if *cc != *ev_cc || *ev_cc >= 128 || !channel_matches(*tch, *ev_ch) {
                return false;
            }

            // Check direction if specified
            match direction {
                Some(dir) if dir == "Clockwise" => {
                    matches!(
                        ev_direction,
                        crate::event_processor::EncoderDirection::Clockwise
                    )
                }
                Some(dir) if dir == "CounterClockwise" => {
                    matches!(
                        ev_direction,
                        crate::event_processor::EncoderDirection::CounterClockwise
                    )
                }
                _ => true, // Any direction
            }
        }
        // ProgramChange trigger matches ProcessedEvent::ProgramChange (ADR-025)
        (
            CompiledTrigger::ProgramChange { pc, channel: tch },
            ProcessedEvent::ProgramChange {
                program,
                channel: ev_ch,
            },
        ) => {
            let pc_ok = pc.is_none_or(|p| p == *program);
            pc_ok && channel_matches(*tch, *ev_ch)
        }
        // OSC triggers (ADR-039-A): matched only against
        // ProcessedEvent::OscReceived. The address comes off the wire
        // (attacker-controlled) — exact compare / pre-compiled glob /
        // fallible numeric coercion only, never a panic.
        (
            CompiledTrigger::OscMessage { address },
            ProcessedEvent::OscReceived {
                address: ev_address,
                ..
            },
        ) => address == ev_address,
        (
            CompiledTrigger::OscAddressPattern { pattern },
            ProcessedEvent::OscReceived {
                address: ev_address,
                ..
            },
        ) => pattern.matches(ev_address),
        (
            CompiledTrigger::OscArgRange {
                arg_index,
                min,
                max,
            },
            ProcessedEvent::OscReceived { args, .. },
        ) => match args.get(*arg_index) {
            Some(crate::actions::OscArg::Float(f)) => f.is_finite() && *min <= *f && *f <= *max,
            Some(crate::actions::OscArg::Int(i)) => {
                let v = *i as f32;
                *min <= v && v <= *max
            }
            // String args and missing indices never match.
            _ => false,
        },
        // Fallback for unhandled combinations (ADR-002 safeguard)
        // Note: Most non-matches are expected (e.g., GamepadButton + EncoderTurned)
        // Use trace! to avoid log noise - only visible with DEBUG=1
        (trigger, event) => {
            trace!(
                trigger_type = std::any::type_name_of_val(trigger),
                event_type = std::any::type_name_of_val(event),
                "Non-matching trigger/event combination (expected for most cases)"
            );
            false
        }
    }
}

impl MappingEngine {
    pub fn new() -> Self {
        Self {
            mode_mappings: HashMap::new(),
            global_mappings: Vec::new(),
        }
    }

    /// Returns the number of modes currently loaded.
    /// Used for testing that stale modes are cleared on config reload.
    pub fn mode_count(&self) -> usize {
        self.mode_mappings.len()
    }

    pub fn load_from_config(&mut self, config: &Config) {
        // Clear existing mappings to prevent stale modes when config
        // has fewer modes than before (ADR-002)
        self.mode_mappings.clear();

        // Load mode-specific mappings
        // Warn if more than 256 modes (u8 limit)
        if config.modes.len() > 256 {
            warn!(
                mode_count = config.modes.len(),
                "Config has more than 256 modes; only first 256 will be accessible"
            );
        }
        for (mode_idx, mode) in config.modes.iter().enumerate().take(256) {
            let compiled: Vec<CompiledMapping> =
                mode.mappings.iter().map(Self::compile_mapping).collect();

            self.mode_mappings.insert(mode_idx as u8, compiled);
        }

        // Load global mappings
        self.global_mappings = config
            .global_mappings
            .iter()
            .map(Self::compile_mapping)
            .collect();
    }

    fn compile_mapping(mapping: &Mapping) -> CompiledMapping {
        // Extract device filter from trigger (ADR-009)
        let device = mapping.trigger.device().cloned();

        CompiledMapping {
            trigger: compile_trigger(&mapping.trigger),
            action: compile_action(&mapping.action),
            description: mapping.description.clone(),
            device,
        }
    }

    pub fn get_action(&self, event: &MidiEvent, mode: u8) -> Option<Action> {
        // Check mode-specific mappings first, fall back to global if no match
        if let Some(mode_mappings) = self.mode_mappings.get(&mode)
            && let Some(action) = self.find_matching_action(event, mode_mappings)
        {
            return Some(action);
        }

        // Check global mappings as fallback
        self.find_matching_action(event, &self.global_mappings)
    }

    /// Get action for a processed event (supports advanced triggers like chords)
    pub fn get_action_for_processed(&self, event: &ProcessedEvent, mode: u8) -> Option<Action> {
        // Check mode-specific mappings first
        if let Some(mode_mappings) = self.mode_mappings.get(&mode)
            && let Some(action) = self.find_matching_action_for_processed(event, mode_mappings)
        {
            return Some(action);
        }

        // Check global mappings
        self.find_matching_action_for_processed(event, &self.global_mappings)
    }

    /// Get action for a processed event with device filtering (ADR-009)
    ///
    /// When `device_id` is Some, only mappings with matching device filter (or no filter) are considered.
    /// When `device_id` is None, only mappings with no device filter are considered.
    pub fn get_action_for_processed_with_device(
        &self,
        event: &ProcessedEvent,
        mode: u8,
        device_id: Option<&str>,
    ) -> Option<Action> {
        // Check mode-specific mappings first
        if let Some(mode_mappings) = self.mode_mappings.get(&mode)
            && let Some(action) =
                self.find_matching_action_for_processed_with_device(event, mode_mappings, device_id)
        {
            return Some(action);
        }

        // Check global mappings
        self.find_matching_action_for_processed_with_device(event, &self.global_mappings, device_id)
    }

    fn find_matching_action(
        &self,
        event: &MidiEvent,
        mappings: &[CompiledMapping],
    ) -> Option<Action> {
        for mapping in mappings {
            if self.trigger_matches_raw(&mapping.trigger, event) {
                if let Some(desc) = &mapping.description {
                    debug!(mapping = desc, "Executing mapped action");
                }
                return Some(mapping.action.clone());
            }
        }
        trace!("No mapping found for MIDI event");
        None
    }

    fn find_matching_action_for_processed(
        &self,
        event: &ProcessedEvent,
        mappings: &[CompiledMapping],
    ) -> Option<Action> {
        for mapping in mappings {
            if trigger_matches_processed(&mapping.trigger, event) {
                if let Some(desc) = &mapping.description {
                    debug!(
                        mapping = desc,
                        "Executing mapped action for processed event"
                    );
                }
                return Some(mapping.action.clone());
            }
        }
        trace!("No mapping found for processed event");
        None
    }

    /// Find matching action with device filter (ADR-009)
    fn find_matching_action_for_processed_with_device(
        &self,
        event: &ProcessedEvent,
        mappings: &[CompiledMapping],
        device_id: Option<&str>,
    ) -> Option<Action> {
        for mapping in mappings {
            // Device filter check
            if !device_matches(&mapping.device, device_id) {
                continue;
            }
            if trigger_matches_processed(&mapping.trigger, event) {
                if let Some(desc) = &mapping.description {
                    debug!(mapping = desc, device = ?device_id, "Executing device-filtered action");
                }
                return Some(mapping.action.clone());
            }
        }
        trace!("No mapping found for processed event with device filter");
        None
    }

    fn trigger_matches_raw(&self, trigger: &CompiledTrigger, event: &MidiEvent) -> bool {
        match (trigger, event) {
            (
                CompiledTrigger::Note {
                    note,
                    velocity_min,
                    channel: tch,
                    ..
                },
                MidiEvent::NoteOn {
                    note: ev_note,
                    velocity,
                    channel: ev_ch,
                    ..
                },
            ) => {
                *note == *ev_note
                    && *velocity >= *velocity_min
                    && channel_matches(*tch, Some(*ev_ch))
            }
            (
                CompiledTrigger::CC {
                    cc,
                    value_min,
                    channel: tch,
                    ..
                },
                MidiEvent::ControlChange {
                    cc: ev_cc,
                    value,
                    channel: ev_ch,
                    ..
                },
            ) => *cc == *ev_cc && *value >= *value_min && channel_matches(*tch, Some(*ev_ch)),
            // Channel aftertouch: pressure clears the threshold. Mirrors
            // the processed-event predicate so direct `get_action(&MidiEvent)`
            // lookups honour the same contract as the EventProcessor path.
            (
                CompiledTrigger::Aftertouch {
                    pressure_min,
                    channel: tch,
                },
                MidiEvent::Aftertouch {
                    pressure,
                    channel: ev_ch,
                    ..
                },
            ) => *pressure >= *pressure_min && channel_matches(*tch, Some(*ev_ch)),
            // Polyphonic aftertouch: same note AND pressure over threshold.
            (
                CompiledTrigger::PolyAftertouch {
                    note,
                    pressure_min,
                    channel: tch,
                },
                MidiEvent::PolyPressure {
                    note: ev_note,
                    pressure,
                    channel: ev_ch,
                    ..
                },
            ) => {
                *note == *ev_note
                    && *pressure >= *pressure_min
                    && channel_matches(*tch, Some(*ev_ch))
            }
            // Pitch bend: 14-bit value within the configured [min, max] window
            // (either bound optional).
            (
                CompiledTrigger::PitchBend {
                    value_min,
                    value_max,
                    channel: tch,
                },
                MidiEvent::PitchBend {
                    value,
                    channel: ev_ch,
                    ..
                },
            ) => {
                value_min.is_none_or(|min| *value >= min)
                    && value_max.is_none_or(|max| *value <= max)
                    && channel_matches(*tch, Some(*ev_ch))
            }
            // Program change: program number matches, or the trigger leaves it
            // unconstrained.
            (
                CompiledTrigger::ProgramChange { pc, channel: tch },
                MidiEvent::ProgramChange {
                    program,
                    channel: ev_ch,
                    ..
                },
            ) => pc.is_none_or(|p| p == *program) && channel_matches(*tch, Some(*ev_ch)),
            // Remaining triggers fall through to the processed-event path:
            //  - temporal triggers genuinely need EventProcessor state
            //    (NoteChord, DoubleTap, LongPress, and EncoderTurn direction);
            //  - VelocityRange is simply not implemented here — its band is a
            //    pure function of the NoteOn velocity and configured thresholds
            //    (see `classify_velocity`), so it could be added, but raw
            //    callers currently use the processed path for it;
            //  - gamepad triggers are non-MIDI.
            _ => false,
        }
    }
}

/// Check if a mapping's device filter matches the event's device_id (ADR-009)
/// (extracted as free function for reuse by rule_set)
pub(crate) fn device_matches(mapping_device: &Option<String>, event_device: Option<&str>) -> bool {
    match (mapping_device, event_device) {
        // Mapping has no device filter → matches any device
        (None, _) => true,
        // Mapping has device filter, event has matching device
        (Some(filter), Some(device)) => filter == device,
        // Mapping has device filter, but event has no device → no match
        (Some(_), None) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-047 §D3b: the named-but-unmapped extras (fixed ids 145–148) are
    /// matchable. `GamepadButton` already accepts any id ≥ 128 (C=145); the
    /// `GamepadAnalogStick` matcher is widened to accept the d-pad-as-axis
    /// encoder ids 147/148.
    #[test]
    fn gamepad_d3b_extra_controls_match() {
        use crate::event_processor::{EncoderDirection, ProcessedEvent, VelocityLevel};

        let c_trigger = CompiledTrigger::GamepadButton {
            button: 145, // Button::C
            velocity_min: 1,
        };
        let c_event = ProcessedEvent::PadPressed {
            note: 145,
            velocity: 100,
            velocity_level: VelocityLevel::Hard,
            channel: None,
        };
        assert!(trigger_matches_processed(&c_trigger, &c_event));

        let dpad_trigger = CompiledTrigger::GamepadAnalogStick {
            axis: 147, // Axis::DPadX
            direction: None,
        };
        let dpad_event = ProcessedEvent::EncoderTurned {
            cc: 147,
            value: 100,
            direction: EncoderDirection::Clockwise,
            delta: 1,
            channel: None,
        };
        assert!(trigger_matches_processed(&dpad_trigger, &dpad_event));

        // An out-of-range encoder id is still rejected by GamepadAnalogStick.
        let bad_trigger = CompiledTrigger::GamepadAnalogStick {
            axis: 200,
            direction: None,
        };
        let bad_event = ProcessedEvent::EncoderTurned {
            cc: 200,
            value: 100,
            direction: EncoderDirection::Clockwise,
            delta: 1,
            channel: None,
        };
        assert!(!trigger_matches_processed(&bad_trigger, &bad_event));
    }

    /// `classify_velocity` is the unit that actually
    /// respects user-configured `soft_max` / `medium_max`. These tests
    /// exercise the classifier directly (rather than asserting only that a
    /// VelocityRange trigger matched), so a regression that ignores the custom
    /// thresholds and falls back to the hard-coded 40/80 defaults is caught.
    ///
    /// Boundaries are inclusive on the lower zone: `v <= soft_max` is Soft,
    /// `v <= medium_max` is Medium, else Hard.
    #[test]
    fn classify_velocity_respects_default_thresholds() {
        // Defaults are soft_max=40, medium_max=80.
        assert_eq!(classify_velocity(0, 40, 80), VelocityLevel::Soft);
        assert_eq!(classify_velocity(40, 40, 80), VelocityLevel::Soft); // == soft_max
        assert_eq!(classify_velocity(41, 40, 80), VelocityLevel::Medium); // soft_max + 1
        assert_eq!(classify_velocity(80, 40, 80), VelocityLevel::Medium); // == medium_max
        assert_eq!(classify_velocity(81, 40, 80), VelocityLevel::Hard); // medium_max + 1
        assert_eq!(classify_velocity(127, 40, 80), VelocityLevel::Hard);
    }

    #[test]
    fn classify_velocity_respects_custom_soft_max() {
        // Custom soft_max=30, medium_max=70.
        assert_eq!(classify_velocity(25, 30, 70), VelocityLevel::Soft);
        assert_eq!(classify_velocity(30, 30, 70), VelocityLevel::Soft); // == soft_max
        assert_eq!(classify_velocity(31, 30, 70), VelocityLevel::Medium); // just over

        // The regression that proves custom thresholds are honored: velocity 35
        // is Soft under the DEFAULT soft_max=40, but Medium under soft_max=30.
        assert_eq!(classify_velocity(35, 40, 80), VelocityLevel::Soft);
        assert_eq!(classify_velocity(35, 30, 70), VelocityLevel::Medium);
    }

    #[test]
    fn classify_velocity_respects_custom_medium_max() {
        // Custom soft_max=20, medium_max=60.
        assert_eq!(classify_velocity(20, 20, 60), VelocityLevel::Soft); // == soft_max
        assert_eq!(classify_velocity(55, 20, 60), VelocityLevel::Medium);
        assert_eq!(classify_velocity(60, 20, 60), VelocityLevel::Medium); // == medium_max
        assert_eq!(classify_velocity(61, 20, 60), VelocityLevel::Hard); // just over

        // Velocity 65 is Medium under the DEFAULT medium_max=80, but Hard under
        // medium_max=60 — the custom upper bound is honored.
        assert_eq!(classify_velocity(65, 20, 80), VelocityLevel::Medium);
        assert_eq!(classify_velocity(65, 20, 60), VelocityLevel::Hard);
    }
}
