//! Port of the producer half of `wave_renderer.c`, `internal/wave_renderer.h`: the engine side of the wave ring.
//!
//! DEVIATION: upstream produces a half on each service tick only while the 3-half ring has room, so wave playback
//! is clocked by the audio device. Here the engine produces halves on its own clock (one per 512 wave frames of
//! service time) and a full ring drops the half; the renderer absorbs clock drift (see `render::wave_renderer`).
//! The simulation therefore never depends on audio-device consumption (PLAN §1.5).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::Imuse;
use super::commands::WaveOutputFilter;

/// Bytes per half: 512 stereo frames of offset-binary unsigned 8-bit samples.
#[doc(alias = "IM_WAVERENDERER_HALF_BYTES")]
pub(crate) const HALF_BYTES: usize = 1024;
/// Frames per half.
pub(crate) const HALF_FRAMES: usize = HALF_BYTES / 2;
/// Halves the ring holds.
pub(crate) const RING_HALVES: usize = 8;

/// One half of wave output.
pub(crate) type Half = [u8; HALF_BYTES];

/// State shared with the consumer.
#[derive(Debug, Default)]
pub(crate) struct WaveShared {
    /// The output filter the engine asks for; the consumer applies it at its next mix.
    pub requested_filter: AtomicBool,
    /// Halves dropped because the ring was full.
    pub dropped_halves: AtomicU32,
}

/// The engine end of the wave ring.
#[doc(alias = "ImWaveRendererState")]
#[derive(Debug)]
pub(crate) struct WaveProducer {
    pub producer: rtrb::Producer<Half>,
    pub shared: Arc<WaveShared>,
    /// 22050 or 11025.
    pub wave_sample_rate: i32,
    pub output_sample_rate: i32,
    pub upsample_ratio: i32,
    /// Service time not yet turned into halves, in µs × wave rate.
    pub sample_clock: u64,
    pub initialized: bool,
}

impl WaveProducer {
    pub fn new(producer: rtrb::Producer<Half>, shared: Arc<WaveShared>) -> Self {
        Self {
            producer,
            shared,
            wave_sample_rate: 22050,
            output_sample_rate: 44100,
            upsample_ratio: 2,
            sample_clock: 0,
            initialized: false,
        }
    }
}

/// The wave and output rates for a configuration, and the integer upsampling ratio between them.
pub(crate) fn wave_rates(high_speed: bool, output_sample_rate: i32) -> (i32, i32, i32) {
    let wave = if high_speed { 22050 } else { 11025 };
    let output = if output_sample_rate > 0 {
        output_sample_rate
    } else {
        44100
    };
    let ratio = (output / wave).max(1);
    (wave, output, ratio)
}

impl Imuse {
    #[doc(alias = "ImWaveRenderer_Init")]
    pub(crate) fn wave_renderer_init(&mut self) {
        let config = self.commands.config;
        let (wave, output, ratio) = wave_rates(config.wave_speed_high, config.output_sample_rate);
        if output != ratio * wave {
            tracing::info!(
                target: "imuse",
                "WR: outputSampleRate {output} not an integer multiple of wave rate {wave} -- rounding upsample ratio to {ratio} (expect mild pitch drift)"
            );
        }
        let filter = ratio > 1 && config.wave_output_filter == WaveOutputFilter::Sb16;
        let r = &mut self.wave_renderer;
        r.wave_sample_rate = wave;
        r.output_sample_rate = output;
        r.upsample_ratio = ratio;
        r.sample_clock = 0;
        r.shared.requested_filter.store(filter, Ordering::Relaxed);
        r.initialized = true;
        tracing::info!(target: "imuse", "wave-renderer module{}...", if filter { " (SB16 filter)" } else { "" });
    }

    #[doc(alias = "ImWaveRenderer_Deinit")]
    pub(crate) fn wave_renderer_deinit(&mut self) {
        self.wave_renderer.initialized = false;
    }

    /// Advances the production clock by one service of `service_us` and returns how many halves came due.
    pub(crate) fn wave_renderer_halves_due(&mut self, service_us: i32) -> usize {
        let r = &mut self.wave_renderer;
        if !r.initialized {
            return 0;
        }
        r.sample_clock += service_us as u64 * r.wave_sample_rate as u64;
        let half = HALF_FRAMES as u64 * 1_000_000;
        let due = r.sample_clock / half;
        r.sample_clock %= half;
        due as usize
    }

    /// Publishes a finished half; a full ring drops it.
    #[doc(alias = "ImWaveRenderer_CommitProduceFrame")]
    pub(crate) fn wave_renderer_commit_produce_frame(&mut self, half: &Half) {
        let r = &mut self.wave_renderer;
        if !r.initialized {
            return;
        }
        if r.producer.push(*half).is_err() {
            r.shared.dropped_halves.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Selects the reconstruction filter of the wave output from the next mix on. It only applies when the output
    /// rate is a multiple of the wave rate.
    #[doc(alias = "imuse_set_wave_output_filter")]
    pub fn set_wave_output_filter(&mut self, filter: WaveOutputFilter) {
        let enabled = filter == WaveOutputFilter::Sb16 && self.wave_renderer.upsample_ratio > 1;
        self.wave_renderer
            .shared
            .requested_filter
            .store(enabled, Ordering::Relaxed);
    }
}
