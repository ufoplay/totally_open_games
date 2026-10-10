//! Port of the audio-thread half of `midi_fm4_opl3.c`: the FM4 driver on a Nuked OPL3.
//!
//! DEVIATION: closing does not clear the chip's registers (`im_fm4_driver_deinit`); the renderer is dropped.

use super::fm4_driver::{Fm4Driver, OplWriter};
use super::opl3::Chip;
use crate::synth_queue::{QueueConsumer, SynthEvent};

/// Frames generated between event drains.
#[doc(alias = "FM4_RENDER_CHUNK_FRAMES")]
const RENDER_CHUNK_FRAMES: usize = 512;
/// A two-bank NoteOn writes at most 40 registers; other events write fewer.
#[doc(alias = "FM4_EVENT_MAX_WRITES")]
const EVENT_MAX_WRITES: usize = 64;

impl OplWriter for Chip {
    #[doc(alias = "opl_write")]
    fn write(&mut self, reg: u16, value: u8) {
        self.write_reg_buffered(reg, value);
    }
}

/// DOS94 FM4.MDR banks at file offsets 0x10e0 / 0x18b4: the X-Wing variant of three tones.
fn use_xwing_bank(driver: &mut Fm4Driver) {
    const TONE_70: [u8; 12] = [164, 150, 157, 217, 64, 162, 135, 158, 232, 192, 12, 0];
    const TONE_73: [u8; 12] = [178, 101, 109, 233, 16, 162, 73, 157, 232, 248, 8, 0];
    for bank in 0..2 {
        for (tone, bytes) in [(70, TONE_70), (71, TONE_70), (73, TONE_73)] {
            let record = &mut driver.tones[bank][tone];
            record.packed.copy_from_slice(&bytes[..11]);
            record.duration = bytes[11];
        }
    }
}

/// The FM4 synthesizer (`ImFm4Opl3Backend` without the engine-side queue end).
#[doc(alias("ImFm4Opl3Backend", "vt_release"))]
pub(crate) struct Fm4Opl3 {
    queue: QueueConsumer,
    driver: Fm4Driver,
    chip: Chip,
    sample_rate: u32,
    xwing_bank: bool,
}

impl Fm4Opl3 {
    #[doc(alias(
        "imuse_fm4_opl3_backend_create",
        "imuse_fm4_opl3_xwing_backend_create",
        "vt_open"
    ))]
    pub fn new(queue: QueueConsumer, sample_rate: u32, xwing_bank: bool) -> Self {
        let mut chip = Chip::new(sample_rate);
        let driver = Fm4Driver::new(&mut chip);
        let mut backend = Self {
            queue,
            driver,
            chip,
            sample_rate,
            xwing_bank,
        };
        if xwing_bank {
            use_xwing_bank(&mut backend.driver);
        }
        backend
    }

    fn reset_synth(&mut self) {
        self.chip.reset(self.sample_rate);
        self.driver = Fm4Driver::new(&mut self.chip);
        if self.xwing_bank {
            use_xwing_bank(&mut self.driver);
        }
    }

    fn apply_event(&mut self, event: SynthEvent) {
        let out = &mut self.chip;
        match event {
            SynthEvent::Program { channel, program } => {
                self.driver
                    .program_change(i32::from(channel), i32::from(program));
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
            } => self.driver.control_change(
                out,
                i32::from(channel),
                u32::from(controller),
                i32::from(value),
            ),
            SynthEvent::PitchBend { channel, value } => {
                self.driver
                    .pitch_bend(out, i32::from(channel), i32::from(value));
            }
            SynthEvent::OplWrite { .. } => {}
        }
    }

    /// Applies queued events while the chip's write buffer has room; an overflow resets the synthesizer.
    fn drain_events(&mut self) {
        if self.queue.overflowed() {
            self.queue.reset();
            self.reset_synth();
            return;
        }
        while self.chip.write_space() >= EVENT_MAX_WRITES {
            let Some(event) = self.queue.pop() else {
                break;
            };
            self.apply_event(event);
        }
    }

    /// Overwrites `buf` with interleaved stereo frames.
    #[doc(alias = "vt_render_float")]
    pub fn render_float(&mut self, buf: &mut [f32]) {
        let mut scratch = [0i16; RENDER_CHUNK_FRAMES * 2];
        for chunk in buf.chunks_mut(RENDER_CHUNK_FRAMES * 2) {
            self.drain_events();
            let frames = chunk.len() / 2;
            self.chip
                .generate_stream(&mut scratch[..frames * 2], frames);
            for (out, &sample) in chunk.iter_mut().zip(&scratch) {
                *out = f32::from(sample) / 32768.0;
            }
        }
    }
}
