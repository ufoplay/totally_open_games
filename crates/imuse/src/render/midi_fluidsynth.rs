//! Port of `midi_fluidsynth.c`, `include/imuse/midi_fluidsynth.h`: the General MIDI SoundFont backend.
//!
//! DEVIATION: `rustysynth` replaces FluidSynth (PLAN §4.1). It ignores SysEx, so the GS init message has no effect,
//! and its output level is not calibrated against FluidSynth's.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use super::gmidi_driver::{GmidiDriver, MidiSink};
use crate::synth_queue::{QueueConsumer, SynthEvent};

/// Where the SoundFont comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundFontSource {
    Path(PathBuf),
    Data(Arc<[u8]>),
}

/// The SoundFont backend's settings.
#[doc(alias = "ImuseFluidSynthConfig")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FluidSynthConfig {
    pub soundfont: SoundFontSource,
    /// 0 means 256.
    pub polyphony: i32,
}

/// Whether this build has the SoundFont backend; it always has.
#[doc(alias = "imuse_fluidsynth_backend_available")]
pub fn fluidsynth_backend_available() -> bool {
    true
}

/// Whether `path` holds a SoundFont the backend can load.
#[doc(alias = "imuse_fluidsynth_soundfont_is_valid")]
pub fn fluidsynth_soundfont_is_valid(path: &std::path::Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| SoundFont::new(&mut Cursor::new(bytes)).is_ok())
}

/// The synthesizer as the GM driver's message sink, with FluidSynth's CC 7 compensation.
struct Sink {
    synth: Synthesizer,
    /// CC 7 is fed `sqrt(v / 127) * 127`, so the synthesizer's square law gives a linear response.
    cc7_linearize: [u8; 128],
}

impl MidiSink for Sink {
    #[doc(alias = "fluidsynth_write_message")]
    fn write_message(&mut self, bytes: &[u8]) -> i32 {
        let Some(&status) = bytes.first() else {
            return -1;
        };
        if status == 0xf0 {
            // Accepted and ignored: rustysynth has no SysEx.
            return if bytes.len() >= 2 && bytes[bytes.len() - 1] == 0xf7 {
                0
            } else {
                -1
            };
        }
        let channel = i32::from(status & 0x0f);
        let expected = match status & 0xf0 {
            0x80 | 0x90 | 0xb0 | 0xe0 => 3,
            0xc0 => 2,
            _ => return -1,
        };
        if bytes.len() != expected {
            return -1;
        }
        let data1 = i32::from(bytes[1]);
        let mut data2 = bytes.get(2).copied().map_or(0, i32::from);
        if status & 0xf0 == 0xb0 && bytes[1] == 7 {
            data2 = i32::from(self.cc7_linearize[usize::from(bytes[2])]);
        }
        self.synth
            .process_midi_message(channel, i32::from(status & 0xf0), data1, data2);
        0
    }
}

#[doc(alias = "init_cc7_linearize")]
fn cc7_linearize() -> [u8; 128] {
    std::array::from_fn(|i| ((i as f64 * 127.0).sqrt() + 0.5).clamp(0.0, 127.0) as u8)
}

/// The SoundFont synthesizer (`FluidSynthBackend` without the engine-side queue end).
#[doc(alias("FluidSynthBackend", "vt_release", "fs_close"))]
pub(crate) struct FluidSynth {
    queue: QueueConsumer,
    driver: GmidiDriver,
    sink: Sink,
    left: Vec<f32>,
    right: Vec<f32>,
}

/// Why the SoundFont backend could not open.
#[derive(Debug, thiserror::Error)]
pub enum FluidSynthError {
    #[error("cannot read the SoundFont: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid SoundFont: {0}")]
    SoundFont(#[from] rustysynth::SoundFontError),
    #[error("cannot create the synthesizer: {0}")]
    Synthesizer(#[from] rustysynth::SynthesizerError),
}

impl FluidSynth {
    /// Loads the SoundFont and starts the synthesizer (`vt_open`, `fs_open`).
    #[doc(alias("imuse_fluidsynth_backend_create", "vt_open", "fs_open"))]
    pub fn new(
        queue: QueueConsumer,
        config: &FluidSynthConfig,
        sample_rate: i32,
    ) -> Result<Self, FluidSynthError> {
        let bytes: Arc<[u8]> = match &config.soundfont {
            SoundFontSource::Path(path) => Arc::from(std::fs::read(path)?),
            SoundFontSource::Data(data) => Arc::clone(data),
        };
        let sound_font = Arc::new(SoundFont::new(&mut Cursor::new(&bytes[..]))?);
        let mut settings =
            SynthesizerSettings::new(if sample_rate > 0 { sample_rate } else { 44100 });
        settings.maximum_polyphony = if config.polyphony > 0 {
            config.polyphony as usize
        } else {
            256
        };
        settings.enable_reverb_and_chorus = true;
        let mut synth = Synthesizer::new(&sound_font, &settings)?;
        synth.set_master_volume(0.6);
        let mut sink = Sink {
            synth,
            cc7_linearize: cc7_linearize(),
        };
        let driver = GmidiDriver::new(&mut sink);
        Ok(Self {
            queue,
            driver,
            sink,
            left: Vec::new(),
            right: Vec::new(),
        })
    }

    fn apply_event(&mut self, event: SynthEvent) {
        let out = &mut self.sink;
        match event {
            SynthEvent::Program { channel, program } => {
                self.driver
                    .program_change(out, i32::from(channel), i32::from(program));
            }
            SynthEvent::NoteOn {
                channel,
                note,
                velocity,
            } => {
                self.driver.note_on(
                    out,
                    i32::from(channel),
                    i32::from(note),
                    i32::from(velocity),
                );
            }
            SynthEvent::NoteOff { channel, note } => {
                self.driver
                    .note_off(out, i32::from(channel), i32::from(note));
            }
            SynthEvent::Control {
                channel,
                controller,
                value,
            } => {
                self.driver.control_change(
                    out,
                    i32::from(channel),
                    u32::from(controller),
                    i32::from(value),
                );
            }
            SynthEvent::PitchBend { channel, value } => {
                self.driver
                    .pitch_bend(out, i32::from(channel), i32::from(value));
            }
            SynthEvent::OplWrite { .. } => {}
        }
    }

    /// Applies every queued event; an overflow resets the GM driver.
    fn drain_events(&mut self) {
        if self.queue.overflowed() {
            self.queue.reset();
            self.driver.deinit(&mut self.sink);
            self.driver = GmidiDriver::new(&mut self.sink);
            tracing::warn!(target: "imuse", "midi_fluidsynth: event queue overflow; resetting synth");
            return;
        }
        while let Some(event) = self.queue.pop() {
            self.apply_event(event);
        }
    }

    /// Overwrites `buf` with interleaved stereo frames.
    #[doc(alias = "vt_render_float")]
    pub fn render_float(&mut self, buf: &mut [f32]) {
        self.drain_events();
        let frames = buf.len() / 2;
        self.left.resize(frames, 0.0);
        self.right.resize(frames, 0.0);
        self.sink.synth.render(&mut self.left, &mut self.right);
        for (frame, (&l, &r)) in buf
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(self.left.iter().zip(&self.right))
        {
            *frame = [l, r];
        }
    }
}
