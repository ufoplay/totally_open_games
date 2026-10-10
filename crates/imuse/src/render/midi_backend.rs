//! The audio-thread end of the selected MIDI backend (the `render_float` slot of `internal/midi_backend.h`).

use super::midi_fluidsynth::FluidSynth;
use super::midi_fm4_opl3::Fm4Opl3;
use super::midi_xwing_adlib::AdlibRenderer;

pub(crate) enum MidiRenderer {
    Silent,
    Fm4(Box<Fm4Opl3>),
    Adlib(Box<AdlibRenderer>),
    FluidSynth(Box<FluidSynth>),
}

impl MidiRenderer {
    /// Overwrites `buf` with interleaved stereo frames.
    #[doc(alias = "ImMidi_RenderFloat")]
    pub fn render_float(&mut self, buf: &mut [f32]) {
        match self {
            MidiRenderer::Silent => buf.fill(0.0),
            MidiRenderer::Fm4(backend) => backend.render_float(buf),
            MidiRenderer::Adlib(backend) => backend.render_float(buf),
            MidiRenderer::FluidSynth(backend) => backend.render_float(buf),
        }
    }
}
