//! iMUSE interactive music: MIDI sequencer, digital wave path and synth backends.
//!
//! Port of [imuse](https://github.com/elyosh/imuse). Bevy-free; the host drives it through its clock.
//!
//! - [`Imuse`] is the engine. The game thread drives it with [`Imuse::advance`] and the commands; it never waits
//!   for audio output.
//! - [`ImuseRenderer`] plays it on the audio thread with [`ImuseRenderer::mix_s16`].
//! - [`ImuseHost`] supplies sound data and receives the game callbacks of triggers and deferred commands.
//!
//! DEVIATION: diagnostics go to `tracing` (target `imuse`) instead of a host log callback (`debug.c`).

pub mod render;
pub mod state;
mod synth_queue;

pub use render::ImuseRenderer;
pub use render::midi_fluidsynth::{
    FluidSynthConfig, FluidSynthError, SoundFontSource, fluidsynth_backend_available,
    fluidsynth_soundfont_is_valid,
};
pub use state::commands::{
    CallbackId, Command, ImuseCmd, ImuseConfig, ImuseError, ImuseHost, Opcode, Param, SoundId,
    WaveOutputFilter, WaveStartFlags,
};
pub use state::files::SoundType;
pub use state::groups::Group;
pub use state::xwing::validate_sound as xwing_validate_sound;
pub use state::{Imuse, MidiBackendKind};
