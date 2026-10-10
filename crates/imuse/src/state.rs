//! Simulation state: the engine that runs on the game thread. It is changed only by the commands and by
//! [`Imuse::advance`]; its audio output crosses to [`crate::render::ImuseRenderer`] through SPSC rings.
//!
//! Port of `state.c` and `internal/state.h`: one sub-state per C module.

pub(crate) mod commands;
pub(crate) mod dispatch;
pub(crate) mod fades;
pub(crate) mod filelist;
pub(crate) mod files;
pub(crate) mod groups;
pub(crate) mod hilevel;
pub(crate) mod midi;
pub(crate) mod midi_backend;
pub(crate) mod midi_xwing_adlib;
pub(crate) mod mixer;
pub(crate) mod mixer_tables;
pub(crate) mod players;
pub(crate) mod seq;
pub(crate) mod slots;
pub(crate) mod sustain;
pub(crate) mod timer;
pub(crate) mod tracks;
pub(crate) mod triggers;
pub(crate) mod utils;
pub(crate) mod wave;
pub(crate) mod wave_renderer;
pub(crate) mod xwing;
pub(crate) mod xwing_adlib_data;

use std::sync::Arc;

use crate::render::ImuseRenderer;
use crate::render::midi_backend::MidiRenderer;
use crate::render::midi_fluidsynth::{FluidSynth, FluidSynthConfig};
use crate::render::midi_fm4_opl3::Fm4Opl3;
use crate::render::midi_xwing_adlib::AdlibRenderer;
use crate::render::wave_renderer::WaveConsumer;
use crate::synth_queue;
use commands::{CommandsState, ImuseConfig, ImuseError};
use midi::MidiState;
use midi_backend::MidiOut;
use midi_xwing_adlib::AdlibDriver;
use wave_renderer::{RING_HALVES, WaveProducer, WaveShared, wave_rates};

/// The MIDI synthesizer of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiBackendKind {
    /// MIDI is silent; the engine keeps its bookkeeping.
    None,
    /// The TIE FM4 driver on an OPL3.
    #[doc(alias("imuse_fm4_opl3_backend_create", "imuse_fm4_opl3_backend_available"))]
    Fm4Opl3,
    /// FM4 with the X-Wing DOS94 tone bank, for X-Wing's General MIDI arrangement.
    #[doc(alias = "imuse_fm4_opl3_xwing_backend_create")]
    Fm4Opl3Xwing,
    /// The DOS93 ADLIB.IMS driver, for X-Wing's AdLib arrangement.
    #[doc(alias = "imuse_midi_xwing_adlib_create")]
    XwingAdlib,
    /// A General MIDI SoundFont synthesizer (FluidSynth upstream).
    FluidSynth(FluidSynthConfig),
}

/// An iMUSE session: the engine state, driven by the game thread.
#[doc(alias("imuse_t", "imuse"))]
#[derive(Debug)]
pub struct Imuse {
    pub(crate) xwing: xwing::XwingState,
    pub(crate) commands: CommandsState,
    pub(crate) groups: groups::GroupsState,
    pub(crate) fades: fades::FadesState,
    pub(crate) triggers: triggers::TriggersState,
    pub(crate) midi: MidiState,
    pub(crate) dispatch: dispatch::DispatchState,
    pub(crate) players: players::PlayersState,
    pub(crate) slots: slots::SlotsState,
    pub(crate) sustain: sustain::SustainState,
    pub(crate) seq: seq::SeqState,
    pub(crate) tracks: tracks::TracksState,
    pub(crate) mixer: mixer::MixerState,
    pub(crate) filelist: filelist::FilelistState,
    pub(crate) wave: wave::WaveModuleState,
    pub(crate) wave_renderer: WaveProducer,
}

impl Imuse {
    /// Creates a session and the renderer that plays it. The renderer goes to the audio thread.
    #[doc(alias("imuse_create", "ImCommands_Init"))]
    pub fn new(
        config: ImuseConfig,
        backend: MidiBackendKind,
    ) -> Result<(Self, ImuseRenderer), ImuseError> {
        let rate = if config.output_sample_rate > 0 {
            config.output_sample_rate
        } else {
            44100
        };
        let (midi_out, midi_renderer) = match &backend {
            MidiBackendKind::None => (MidiOut::Silent, MidiRenderer::Silent),
            MidiBackendKind::Fm4Opl3 | MidiBackendKind::Fm4Opl3Xwing => {
                let (producer, consumer) = synth_queue::queue();
                let xwing_bank = backend == MidiBackendKind::Fm4Opl3Xwing;
                (
                    MidiOut::Events {
                        queue: producer,
                        opened: true,
                    },
                    MidiRenderer::Fm4(Box::new(Fm4Opl3::new(consumer, rate as u32, xwing_bank))),
                )
            }
            MidiBackendKind::XwingAdlib => {
                let (producer, consumer) = synth_queue::queue();
                let mut driver = Box::new(AdlibDriver::new(producer));
                driver.open(rate);
                (
                    MidiOut::Adlib(driver),
                    MidiRenderer::Adlib(Box::new(AdlibRenderer::new(consumer, rate as u32))),
                )
            }
            MidiBackendKind::FluidSynth(config) => {
                let (producer, consumer) = synth_queue::queue();
                let synth = FluidSynth::new(consumer, config, rate).map_err(|error| {
                    tracing::error!(target: "imuse", "midi_fluidsynth: open failed: {error}");
                    ImuseError::General
                })?;
                (
                    MidiOut::Events {
                        queue: producer,
                        opened: true,
                    },
                    MidiRenderer::FluidSynth(Box::new(synth)),
                )
            }
        };
        let (wave_producer, wave_consumer) = rtrb::RingBuffer::new(RING_HALVES);
        let wave_shared = Arc::new(WaveShared::default());
        let mut imuse = Self::with_backend(
            config,
            midi_out,
            WaveProducer::new(wave_producer, Arc::clone(&wave_shared)),
        );
        if imuse.commands_init() < 0 {
            return Err(ImuseError::General);
        }
        let (_, _, ratio) = wave_rates(config.wave_speed_high, config.output_sample_rate);
        let renderer = ImuseRenderer {
            midi: midi_renderer,
            wave: WaveConsumer::new(wave_consumer, wave_shared, ratio),
        };
        Ok((imuse, renderer))
    }

    fn with_backend(config: ImuseConfig, backend: MidiOut, wave_renderer: WaveProducer) -> Self {
        Self {
            xwing: Default::default(),
            commands: CommandsState::new(config),
            groups: Default::default(),
            fades: Default::default(),
            triggers: Default::default(),
            midi: MidiState {
                lock: 0,
                paused: false,
                debug_refresh_counter: 0,
                backend,
            },
            dispatch: Default::default(),
            players: Default::default(),
            slots: Default::default(),
            sustain: Default::default(),
            seq: Default::default(),
            tracks: Default::default(),
            mixer: Default::default(),
            filelist: Default::default(),
            wave: Default::default(),
            wave_renderer,
        }
    }
}

impl Drop for Imuse {
    #[doc(alias("imuse_destroy", "ImCommands_Terminate"))]
    fn drop(&mut self) {
        if self.commands.initialized {
            self.commands_terminate();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An uninitialized engine with a silent backend, for unit tests of single modules.
    pub(crate) fn engine() -> Imuse {
        let (producer, _consumer) = rtrb::RingBuffer::new(RING_HALVES);
        Imuse::with_backend(
            ImuseConfig::default(),
            MidiOut::Silent,
            WaveProducer::new(producer, Arc::default()),
        )
    }
}
