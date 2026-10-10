//! Port of `midi_backend.c`, `internal/midi_backend.h` and the game-thread half of each backend's vtable
//! (`midi_fm4_opl3.c`, `midi_fluidsynth.c`; `midi_xwing_adlib.c` has its own module).
//!
//! Upstream fans engine calls through a vtable whose slots a backend may leave NULL. Here the backend is an enum;
//! each operation does what that backend's slot does, or nothing where upstream's slot is NULL.

use super::midi_xwing_adlib::AdlibDriver;
use crate::synth_queue::{QueueProducer, SynthEvent};

/// The engine-side end of the selected MIDI backend.
#[doc(alias = "ImMidi_BackendRelease")]
#[derive(Debug)]
pub(crate) enum MidiOut {
    /// No synthesizer: MIDI is silent but the engine keeps its bookkeeping.
    Silent,
    /// A synthesizer fed semantic events: the TIE FM4 driver on an OPL3 (`midi_fm4_opl3.c`) or the SoundFont
    /// backend (`midi_fluidsynth.c`). Both accept 16 channels and clamp their values the same way.
    Events { queue: QueueProducer, opened: bool },
    /// The DOS93 X-Wing AdLib driver, which runs here and queues register writes (`midi_xwing_adlib.c`).
    Adlib(Box<AdlibDriver>),
}

/// Channels the FM4 driver accepts.
#[doc(alias = "IM_FM4_CHANNEL_COUNT")]
const FM4_CHANNEL_COUNT: i32 = 16;

#[doc(alias("clamp_int", "clamp_midi"))]
fn clamp_u8(value: i32, max: i32) -> u8 {
    value.clamp(0, max) as u8
}

impl MidiOut {
    /// Queues an event. On overflow the renderer is asked to reset its synth.
    #[doc(alias("enqueue_event", "request_reset"))]
    fn fm4_enqueue(queue: &mut QueueProducer, opened: bool, event: SynthEvent) {
        if !opened || queue.overflowed() {
            return;
        }
        if !queue.push(event) {
            tracing::warn!(target: "imuse", "MIDI event queue overflow; resetting synth");
        }
    }

    #[doc(alias("ImMidi_ProgramChange", "vt_program_change"))]
    pub fn program_change(&mut self, channel: i32, program: i32) {
        if let MidiOut::Events { queue, opened } = self
            && (0..FM4_CHANNEL_COUNT).contains(&channel)
        {
            let event = SynthEvent::Program {
                channel: channel as u8,
                program: clamp_u8(program, 127),
            };
            Self::fm4_enqueue(queue, *opened, event);
        }
    }

    #[doc(alias("ImMidi_NoteOn", "vt_note_on"))]
    pub fn note_on(&mut self, channel: i32, note: i32, velocity: i32) {
        match self {
            MidiOut::Silent => {}
            MidiOut::Events { queue, opened } => {
                if (0..FM4_CHANNEL_COUNT).contains(&channel) {
                    let event = SynthEvent::NoteOn {
                        channel: channel as u8,
                        note: clamp_u8(note, 127),
                        velocity: clamp_u8(velocity, 127),
                    };
                    Self::fm4_enqueue(queue, *opened, event);
                }
            }
            MidiOut::Adlib(driver) => driver.note_on_event(channel, note, velocity),
        }
    }

    #[doc(alias("ImMidi_NoteOff", "vt_note_off"))]
    pub fn note_off(&mut self, channel: i32, note: i32) {
        match self {
            MidiOut::Silent => {}
            MidiOut::Events { queue, opened } => {
                if (0..FM4_CHANNEL_COUNT).contains(&channel) {
                    let event = SynthEvent::NoteOff {
                        channel: channel as u8,
                        note: clamp_u8(note, 127),
                    };
                    Self::fm4_enqueue(queue, *opened, event);
                }
            }
            MidiOut::Adlib(driver) => driver.note_off_event(channel, note),
        }
    }

    #[doc(alias("ImMidi_ControlChange", "vt_control_change"))]
    pub fn control_change(&mut self, channel: i32, controller: u32, value: i32) {
        match self {
            MidiOut::Silent => {}
            MidiOut::Events { queue, opened } => {
                if (0..FM4_CHANNEL_COUNT).contains(&channel) && controller <= 127 {
                    let event = SynthEvent::Control {
                        channel: channel as u8,
                        controller: controller as u8,
                        value: clamp_u8(value, 127),
                    };
                    Self::fm4_enqueue(queue, *opened, event);
                }
            }
            MidiOut::Adlib(driver) => driver.control_event(channel, controller, value),
        }
    }

    /// `wire14bit` is the 14-bit wire value, 0x2000 centred.
    #[doc(alias("ImMidi_PitchBend", "vt_pitch_bend"))]
    pub fn pitch_bend(&mut self, channel: i32, wire14bit: i32) {
        match self {
            MidiOut::Silent => {}
            MidiOut::Events { queue, opened } => {
                if (0..FM4_CHANNEL_COUNT).contains(&channel) {
                    let event = SynthEvent::PitchBend {
                        channel: channel as u8,
                        value: wire14bit.clamp(0, 0x3fff) as u16,
                    };
                    Self::fm4_enqueue(queue, *opened, event);
                }
            }
            MidiOut::Adlib(driver) => driver.wire_bend(channel, wire14bit),
        }
    }

    /// The vtable's `instrument` slot: only the AdLib driver takes instrument payloads.
    pub fn instrument(&mut self, channel: i32, data: &[u8]) {
        if let MidiOut::Adlib(driver) = self {
            driver.instrument(channel, data);
        }
    }

    /// Whether the vtable has an `instrument` slot.
    pub fn has_instrument(&self) -> bool {
        matches!(self, MidiOut::Adlib(_))
    }

    /// Whether the backend is the native DOS93 AdLib driver (upstream tests for `adlib_service`).
    #[doc(alias = "native_adlib")]
    pub fn is_native_adlib(&self) -> bool {
        matches!(self, MidiOut::Adlib(_))
    }

    /// Melodic channels: the standard 15, or the AdLib driver's 32 logical parts without a drum gap.
    #[doc(alias = "melodicChannels")]
    pub fn melodic_channels(&self) -> usize {
        match self {
            MidiOut::Adlib(_) => 32,
            _ => 15,
        }
    }

    /// Per-service hook of the AdLib driver.
    pub fn adlib_service(&mut self) {
        if let MidiOut::Adlib(driver) = self {
            driver.service();
        }
    }

    /// Signed pitch, 128 units per semitone.
    pub fn adlib_bend(&mut self, channel: i32, detune: i32) {
        if let MidiOut::Adlib(driver) = self {
            driver.bend(channel, detune);
        }
    }

    /// The notes the AdLib driver's voices are playing on `channels`.
    pub fn adlib_notes(&self, channels: u32, notes: &mut [u32; 128]) {
        if let MidiOut::Adlib(driver) = self {
            driver.notes(channels, notes);
        }
    }

    /// The vtable's `close` slot.
    #[doc(alias = "vt_close")]
    pub fn close(&mut self) {
        match self {
            MidiOut::Silent => {}
            MidiOut::Events { opened, .. } => *opened = false,
            MidiOut::Adlib(driver) => driver.close(),
        }
    }
}
