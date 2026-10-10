//! Port of the consumer half of `wave_renderer.c`: drains the wave ring, upsamples it by zero-order hold and adds
//! it to the mix bus, with the optional SB16 reconstruction filter.
//!
//! DEVIATION: the engine produces halves on its own clock (see `state::wave_renderer`), so this end absorbs the
//! drift between that clock and the audio device: it waits for two halves before playing, drops one source frame
//! when the ring runs ahead and repeats one when it runs dry. An underrun plays silence.

use std::f64::consts::PI;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::state::wave_renderer::{HALF_BYTES, Half, WaveShared};

#[doc(alias = "IM_WAVERENDERER_FIR_TAPS")]
const FIR_TAPS: usize = 127;
#[doc(alias = "IM_WAVERENDERER_FIR_PAIRS")]
const FIR_PAIRS: usize = (FIR_TAPS - 1) / 2;
/// Halves buffered before playback starts or resumes after an underrun.
const PRIME_HALVES: usize = 2;
/// Queued halves above which a frame is dropped.
const HIGH_WATER_HALVES: usize = 4;

pub(crate) struct WaveConsumer {
    consumer: rtrb::Consumer<Half>,
    shared: Arc<WaveShared>,
    current: Half,
    /// Bytes already drained from `current`; 0 when a new half is needed.
    consume_offset: usize,
    /// Repeat the next source frame once (the ring ran dry).
    repeat_frame: bool,
    primed: bool,
    upsample_ratio: i32,
    held_left: f32,
    held_right: f32,
    repeat_remaining: i32,
    filter_enabled: bool,
    fir_history: [[f32; 2]; FIR_TAPS],
    fir_pair_coefficients: [f32; FIR_PAIRS],
    fir_pair_taps: [u8; FIR_PAIRS],
    fir_pair_count: usize,
    fir_center_coefficient: f32,
    fir_history_pos: usize,
}

impl WaveConsumer {
    #[doc(alias = "ImWaveRenderer_Init")]
    pub fn new(
        consumer: rtrb::Consumer<Half>,
        shared: Arc<WaveShared>,
        upsample_ratio: i32,
    ) -> Self {
        let mut consumer = Self {
            consumer,
            shared,
            current: [0x80; HALF_BYTES],
            consume_offset: 0,
            repeat_frame: false,
            primed: false,
            upsample_ratio,
            held_left: 0.0,
            held_right: 0.0,
            repeat_remaining: 0,
            filter_enabled: false,
            fir_history: [[0.0; 2]; FIR_TAPS],
            fir_pair_coefficients: [0.0; FIR_PAIRS],
            fir_pair_taps: [0; FIR_PAIRS],
            fir_pair_count: 0,
            fir_center_coefficient: 0.0,
            fir_history_pos: 0,
        };
        if upsample_ratio > 1 {
            consumer.init_sb16_filter();
        }
        consumer.filter_enabled = consumer.shared.requested_filter.load(Ordering::Relaxed);
        consumer
    }

    /// A Blackman-windowed ideal low-pass at the wave Nyquist rate, normalized to unity DC gain.
    #[doc(alias = "wave_renderer_init_sb16_filter")]
    fn init_sb16_filter(&mut self) {
        let center = FIR_PAIRS;
        let cutoff = 0.5 / f64::from(self.upsample_ratio);
        let mut coefficients = [0f64; FIR_PAIRS];
        let mut sum = 2.0 * cutoff;
        for (tap, coefficient) in coefficients.iter_mut().enumerate() {
            let offset = tap as f64 - center as f64;
            let phase = 2.0 * PI * cutoff * offset;
            let ideal = phase.sin() / (PI * offset);
            let window_phase = 2.0 * PI * tap as f64 / (FIR_TAPS - 1) as f64;
            let window = 0.42 - 0.5 * window_phase.cos() + 0.08 * (2.0 * window_phase).cos();
            *coefficient = ideal * window;
            sum += 2.0 * *coefficient;
        }
        self.fir_pair_count = 0;
        for (tap, coefficient) in coefficients.iter().enumerate() {
            let normalized = coefficient / sum;
            if normalized.abs() <= 1.0e-10 {
                continue;
            }
            self.fir_pair_taps[self.fir_pair_count] = tap as u8;
            self.fir_pair_coefficients[self.fir_pair_count] = normalized as f32;
            self.fir_pair_count += 1;
        }
        self.fir_center_coefficient = ((2.0 * cutoff) / sum) as f32;
        self.fir_history_pos = 0;
        self.fir_history = [[0.0; 2]; FIR_TAPS];
    }

    /// The next source frame, or `None` on an underrun.
    #[doc(alias = "wave_renderer_read_source_frame")]
    fn read_source_frame(&mut self) -> Option<(f32, f32)> {
        if self.consume_offset == 0 {
            if !self.primed {
                if self.consumer.slots() < PRIME_HALVES {
                    return None;
                }
                self.primed = true;
            }
            let Ok(half) = self.consumer.pop() else {
                self.primed = false;
                return None;
            };
            self.current = half;
            let queued = self.consumer.slots();
            if queued > HIGH_WATER_HALVES {
                // Running ahead: skip the first frame of this half.
                self.consume_offset = 2;
            } else if queued == 0 {
                self.repeat_frame = true;
            }
        }
        let src = &self.current[self.consume_offset..self.consume_offset + 2];
        let left = (f32::from(src[0]) - 128.0) * (1.0 / 128.0);
        let right = (f32::from(src[1]) - 128.0) * (1.0 / 128.0);
        if self.repeat_frame {
            self.repeat_frame = false;
        } else {
            self.consume_offset += 2;
            if self.consume_offset >= HALF_BYTES {
                self.consume_offset = 0;
            }
        }
        Some((left, right))
    }

    #[doc(alias = "wave_renderer_filter_frame")]
    fn filter_frame(&mut self, left: f32, right: f32) -> (f32, f32) {
        let position = self.fir_history_pos;
        let center = FIR_PAIRS;
        self.fir_history[position] = [left, right];
        let center_index = (position + FIR_TAPS - center) % FIR_TAPS;
        let mut sum_left = self.fir_center_coefficient * self.fir_history[center_index][0];
        let mut sum_right = self.fir_center_coefficient * self.fir_history[center_index][1];
        for pair in 0..self.fir_pair_count {
            let tap = usize::from(self.fir_pair_taps[pair]);
            let recent = (position + FIR_TAPS - tap) % FIR_TAPS;
            let old = (position + FIR_TAPS - (FIR_TAPS - 1 - tap)) % FIR_TAPS;
            let coefficient = self.fir_pair_coefficients[pair];
            sum_left += coefficient * (self.fir_history[recent][0] + self.fir_history[old][0]);
            sum_right += coefficient * (self.fir_history[recent][1] + self.fir_history[old][1]);
        }
        self.fir_history_pos = (position + 1) % FIR_TAPS;
        (sum_left, sum_right)
    }

    /// Adds `dst.len() / 2` stereo frames of wave output to `dst`.
    #[doc(alias = "ImWaveRenderer_PullSamples")]
    pub fn pull_samples(&mut self, dst: &mut [f32]) {
        let requested = self.shared.requested_filter.load(Ordering::Relaxed);
        if requested != self.filter_enabled {
            self.filter_enabled = requested;
            self.fir_history_pos = 0;
            self.fir_history = [[0.0; 2]; FIR_TAPS];
        }
        for frame in dst.as_chunks_mut::<2>().0 {
            if self.repeat_remaining <= 0 {
                (self.held_left, self.held_right) = self.read_source_frame().unwrap_or((0.0, 0.0));
                self.repeat_remaining = self.upsample_ratio;
            }
            let (mut left, mut right) = (self.held_left, self.held_right);
            self.repeat_remaining -= 1;
            if self.filter_enabled {
                (left, right) = self.filter_frame(left, right);
            }
            frame[0] += left;
            frame[1] += right;
        }
    }
}
