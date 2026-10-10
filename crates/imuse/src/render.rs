//! Renderers: the audio-thread side. They read what the engine queued and only write their own state.
//!
//! Port of `mix.c`: the MIDI backend overwrites the float bus, then the wave renderer adds to it.

pub(crate) mod fm4_data;
pub(crate) mod fm4_driver;
pub(crate) mod gmidi_driver;
pub(crate) mod midi_backend;
pub mod midi_fluidsynth;
pub(crate) mod midi_fm4_opl3;
pub(crate) mod midi_xwing_adlib;
pub(crate) mod opl3;
pub(crate) mod wave_renderer;

use midi_backend::MidiRenderer;
use wave_renderer::WaveConsumer;

/// Frames mixed per pass of [`ImuseRenderer::mix_s16`].
#[doc(alias = "IMUSE_MIX_S16_CHUNK_FRAMES")]
const MIX_S16_CHUNK_FRAMES: usize = 512;

/// Plays a session on the audio thread. Created with its [`crate::Imuse`].
pub struct ImuseRenderer {
    pub(crate) midi: MidiRenderer,
    pub(crate) wave: WaveConsumer,
}

impl ImuseRenderer {
    /// Overwrites `buf` with interleaved stereo frames in [-1, 1] at the configured output rate.
    #[doc(alias = "imuse_mix_f32")]
    pub fn mix_f32(&mut self, buf: &mut [f32]) {
        self.midi.render_float(buf);
        self.wave.pull_samples(buf);
    }

    /// Overwrites `buf` with interleaved stereo 16-bit frames, saturating.
    #[doc(alias = "imuse_mix_s16")]
    pub fn mix_s16(&mut self, buf: &mut [i16]) {
        let mut scratch = [0f32; MIX_S16_CHUNK_FRAMES * 2];
        for chunk in buf.chunks_mut(MIX_S16_CHUNK_FRAMES * 2) {
            let scratch = &mut scratch[..chunk.len()];
            self.mix_f32(scratch);
            for (out, &x) in chunk.iter_mut().zip(scratch.iter()) {
                *out = (x.clamp(-1.0, 1.0) * 32767.0) as i16;
            }
        }
    }
}
