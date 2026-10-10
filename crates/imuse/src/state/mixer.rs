//! Port of `mixer.c`, `internal/mixer.h`: the 16-bit mix accumulator and the companding back to 8-bit.
//!
//! The mixer runs with the engine tick: each output half is accumulated from the tracks, companded to offset-binary
//! unsigned 8-bit samples and handed to the wave renderer.

use super::Imuse;
use super::mixer_tables::{PAN_TABLE_BASE, VOLUME_CURVES_256};
use super::wave_renderer::{HALF_BYTES, HALF_FRAMES};

/// The largest supported `wave_mix_count`.
#[doc(alias = "IM_MIXER_MAX_MIX_COUNT")]
const MAX_MIX_COUNT: usize = 16;
/// Length of the companding table, centred on its midpoint.
const NORM_LEN: usize = MAX_MIX_COUNT * 128 * 2;

/// The output descriptor of the renderer's halves (`ImDigitalOutBuf`).
#[doc(alias = "ImDigitalOutBuf")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct DigitalOutBuf {
    /// Source samples consumed per half.
    pub mix_sample_count: usize,
    /// Distance between consecutive output frames.
    pub sample_stride: usize,
    /// 1 = normal panning, 2 = left and right swapped, 0 = mono.
    pub stereo_mode: i32,
}

/// The renderer's halves: 512 stereo frames with normal panning.
pub(crate) const OUT_DESC: DigitalOutBuf = DigitalOutBuf {
    mix_sample_count: HALF_FRAMES,
    sample_stride: 2,
    stereo_mode: 1,
};

#[doc(alias = "ImMixerState")]
#[derive(Debug, Clone)]
pub(crate) struct MixerState {
    /// Offset-binary 8-bit output by mix value, indexed from the midpoint (`normalization`).
    pub norm_buf: Vec<u8>,
    /// The 16-bit accumulator of the half being mixed.
    pub out_buf: [i16; HALF_BYTES],
    /// Between `prepareDigitalOutput` and `audioWriteToDriver`.
    pub primed: bool,
}

impl Default for MixerState {
    fn default() -> Self {
        Self {
            norm_buf: vec![0; NORM_LEN],
            out_buf: [0; HALF_BYTES],
            primed: false,
        }
    }
}

/// One source slice handed to the mix kernels (`ImSoundFrameState`).
#[doc(alias = "ImSoundFrameState")]
pub(crate) struct SoundFrame<'a> {
    pub buffer: &'a [u8],
    /// When false the source is half rate and every sample is written twice.
    pub source_is_high_rate: bool,
}

impl Imuse {
    /// Builds the companding curve that soft-limits `M` summed voices:
    /// `q = (i * 127 * M) / (127 * M + i * (M - 1))`, `table[i] = 128 + q`, `table[-i-1] = 127 - q`.
    #[doc(alias("ImMixer_Init", "ImMixer_Deinit"))]
    pub(crate) fn mixer_init(&mut self) {
        let mix = self.commands.config.wave_mix_count;
        let span = (mix as usize * 128).min(MAX_MIX_COUNT * 128);
        let mid = NORM_LEN / 2;
        self.mixer.norm_buf = vec![0; NORM_LEN];
        for i in 0..span {
            let i32_i = i as i32;
            let numer = i32_i * 127 * mix;
            let denom = 127 * mix + i32_i * (mix - 1);
            let q = ((numer << 8) / denom + 128) >> 8;
            self.mixer.norm_buf[mid + i] = (128 + q) as u8;
            self.mixer.norm_buf[mid - i - 1] = (128 - q - 1) as u8;
        }
    }

    /// Clears the accumulator for a new half.
    #[doc(alias("ImMixer_prepareDigitalOutput", "ImWaveRenderer_AcquireProduceFrame"))]
    pub(crate) fn mixer_prepare_digital_output(&mut self) {
        self.mixer.out_buf.fill(0);
        self.mixer.primed = true;
    }

    /// Compands the accumulator into a half and publishes it.
    #[doc(alias("ImMixer_audioWriteToDriver", "mixer_compandToPcm8"))]
    pub(crate) fn mixer_audio_write_to_driver(&mut self) -> i32 {
        if !self.mixer.primed {
            return -1;
        }
        let mid = (NORM_LEN / 2) as isize;
        let mut half = [0u8; HALF_BYTES];
        for (out, &sample) in half.iter_mut().zip(&self.mixer.out_buf) {
            // DEVIATION: mix values beyond the table read silence-level 0 instead of out of bounds.
            *out = self
                .mixer
                .norm_buf
                .get((mid + sample as isize) as usize)
                .copied()
                .unwrap_or(0);
        }
        self.wave_renderer_commit_produce_frame(&half);
        self.mixer.primed = false;
        0
    }

    /// Adds a source slice at `offset` (in source samples) with the given volume (0..=127) and pan (0..=127).
    #[doc(alias(
        "ImMixer_audioProcessFrame",
        "mixer_mixMono1x",
        "mixer_mixMono2x",
        "mixer_mixStereo1x",
        "mixer_mixStereo2x"
    ))]
    pub(crate) fn mixer_audio_process_frame(
        &mut self,
        frame: &SoundFrame<'_>,
        offset: usize,
        volume: i32,
        pan: i32,
    ) {
        if !self.mixer.primed || frame.buffer.is_empty() {
            return;
        }
        // 17 volume buckets: 0 for silence, 1..=16 otherwise.
        let mut vol_bucket = volume >> 3;
        if volume != 0 {
            vol_bucket += 1;
        }
        let vol_bucket = vol_bucket.min(16) as usize;
        let stride = OUT_DESC.sample_stride;
        let dst = &mut self.mixer.out_buf[stride * offset..];
        let src = frame.buffer;
        if OUT_DESC.stereo_mode != 0 {
            let mut pan_bucket = (pan >> 3) - 8;
            if pan > 64 {
                pan_bucket += 1;
            }
            let l_factor = PAN_TABLE_BASE[vol_bucket][(8 - pan_bucket) as usize] as usize;
            let r_factor = PAN_TABLE_BASE[vol_bucket][(8 + pan_bucket) as usize] as usize;
            let (l_table, r_table) = if OUT_DESC.stereo_mode == 1 {
                (&VOLUME_CURVES_256[l_factor], &VOLUME_CURVES_256[r_factor])
            } else {
                (&VOLUME_CURVES_256[r_factor], &VOLUME_CURVES_256[l_factor])
            };
            if frame.source_is_high_rate {
                for (out, &s) in dst.as_chunks_mut::<2>().0.iter_mut().zip(src) {
                    out[0] = out[0].wrapping_add(i16::from(l_table[usize::from(s)]));
                    out[1] = out[1].wrapping_add(i16::from(r_table[usize::from(s)]));
                }
            } else {
                for (out, &s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src) {
                    let mix_l = i16::from(l_table[usize::from(s)]);
                    let mix_r = i16::from(r_table[usize::from(s)]);
                    out[0] = out[0].wrapping_add(mix_l);
                    out[2] = out[2].wrapping_add(mix_l);
                    out[1] = out[1].wrapping_add(mix_r);
                    out[3] = out[3].wrapping_add(mix_r);
                }
            }
        } else {
            let table = &VOLUME_CURVES_256[vol_bucket];
            if frame.source_is_high_rate {
                for (out, &s) in dst.iter_mut().zip(src) {
                    *out = out.wrapping_add(i16::from(table[usize::from(s)]));
                }
            } else {
                for (out, &s) in dst.as_chunks_mut::<2>().0.iter_mut().zip(src) {
                    let mix = i16::from(table[usize::from(s)]);
                    out[0] = out[0].wrapping_add(mix);
                    out[1] = out[1].wrapping_add(mix);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn companding_is_offset_binary_and_saturates() {
        let mut im = crate::state::tests::engine();
        im.mixer_init();
        let mid = super::NORM_LEN / 2;
        assert_eq!(im.mixer.norm_buf[mid], 128);
        assert_eq!(im.mixer.norm_buf[mid - 1], 127);
        // Four voices: the curve flattens towards the 8-bit limits.
        assert!(im.mixer.norm_buf[mid + 511] >= 254);
        assert!(im.mixer.norm_buf[mid - 512] <= 1);
    }
}
