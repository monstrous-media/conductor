// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use crate::config::types::MidiLedConfig;
use midir::{MidiOutput, MidiOutputConnection};
use std::error::Error;
use tracing::info;

pub struct MidiFeedback {
    connection: Option<MidiOutputConnection>,
    pub(crate) config: Option<MidiLedConfig>,
    /// Channel-voice messages that would have been sent while no real output
    /// port is connected. Captured so LED feedback (custom mappings, lighting
    /// schemes) can be unit-tested without hardware. Always empty in
    /// production: a connected `MidiFeedback` sends to the port instead.
    captured: Vec<[u8; 3]>,
}

impl MidiFeedback {
    pub fn new() -> Self {
        Self {
            connection: None,
            config: None,
            captured: Vec::new(),
        }
    }

    pub fn with_config(config: MidiLedConfig) -> Self {
        Self {
            connection: None,
            config: Some(config),
            captured: Vec::new(),
        }
    }

    /// Messages captured while not connected to a real port (test
    /// observability for LED feedback). Each entry is the 3-byte
    /// channel-voice message `[status, data1, data2]`.
    pub fn captured(&self) -> &[[u8; 3]] {
        &self.captured
    }

    pub fn connect(&mut self, port_index: usize) -> Result<(), Box<dyn Error>> {
        let midi_out = MidiOutput::new("Conductor Feedback")?;
        let ports = midi_out.ports();

        if port_index >= ports.len() {
            return Err("Invalid output port index".into());
        }

        let port = &ports[port_index];
        let port_name = midi_out.port_name(port)?;
        info!("MIDI feedback connected to: {}", port_name);

        let conn = midi_out.connect(port, "feedback")?;
        self.connection = Some(conn);

        Ok(())
    }

    pub fn send_note_on(
        &mut self,
        note: u8,
        velocity: u8,
        channel: u8,
    ) -> Result<(), Box<dyn Error>> {
        if !(1..=16).contains(&channel) {
            return Err(format!("MIDI channel {} out of range (1-16)", channel).into());
        }
        let msg = [0x90 | (channel - 1), note, velocity];
        if let Some(ref mut conn) = self.connection {
            conn.send(&msg)?;
        } else {
            self.captured.push(msg);
        }
        Ok(())
    }

    pub fn send_note_off(&mut self, note: u8, channel: u8) -> Result<(), Box<dyn Error>> {
        if !(1..=16).contains(&channel) {
            return Err(format!("MIDI channel {} out of range (1-16)", channel).into());
        }
        let msg = [0x80 | (channel - 1), note, 0];
        if let Some(ref mut conn) = self.connection {
            conn.send(&msg)?;
        } else {
            self.captured.push(msg);
        }
        Ok(())
    }

    pub fn send_note_off_vel(
        &mut self,
        note: u8,
        velocity: u8,
        channel: u8,
    ) -> Result<(), Box<dyn Error>> {
        if !(1..=16).contains(&channel) {
            return Err(format!("MIDI channel {} out of range (1-16)", channel).into());
        }
        let msg = [0x80 | (channel - 1), note, velocity];
        if let Some(ref mut conn) = self.connection {
            conn.send(&msg)?;
        } else {
            self.captured.push(msg);
        }
        Ok(())
    }

    pub fn send_cc(&mut self, cc: u8, value: u8, channel: u8) -> Result<(), Box<dyn Error>> {
        if !(1..=16).contains(&channel) {
            return Err(format!("MIDI channel {} out of range (1-16)", channel).into());
        }
        let msg = [0xB0 | (channel - 1), cc, value];
        if let Some(ref mut conn) = self.connection {
            conn.send(&msg)?;
        } else {
            self.captured.push(msg);
        }
        Ok(())
    }

    pub fn send_sysex(&mut self, data: &[u8]) -> Result<(), Box<dyn Error>> {
        if let Some(ref mut conn) = self.connection {
            let mut msg = vec![0xF0]; // SysEx start
            msg.extend_from_slice(data);
            msg.push(0xF7); // SysEx end
            conn.send(&msg)?;
        }
        Ok(())
    }

    // Flash a pad LED (if device supports MIDI note LED feedback)
    /// Flash a pad LED on, then off after `duration_ms`.
    ///
    /// Synchronous: sends note-on, sleeps for `duration_ms`, then sends
    /// note-off — so the LED is actually turned back off. Previously this only
    /// sent note-on (the LED stayed lit indefinitely on devices that hold LED
    /// state from note-on) and discarded any send error. Returns the first
    /// error from either send. Note: this blocks the caller for `duration_ms`.
    pub fn flash_pad(
        &mut self,
        note: u8,
        on_velocity: u8,
        channel: u8,
        duration_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        self.send_note_on(note, on_velocity, channel)?;
        std::thread::sleep(std::time::Duration::from_millis(duration_ms));
        self.send_note_off(note, channel)?;
        Ok(())
    }
}
