//! Port of `dispatch.c`, `internal/dispatch.h`: the per-track VOC cursor.
//!
//! DEVIATION: streaming and the stream-switch crossfade are not ported (no game starts a stream), so a dispatch
//! record always reads an in-memory VOC. Reads past the end of the sound end it.

use super::Imuse;
use super::commands::{ImuseHost, WaveStartFlags};
use super::files;
use super::mixer::{OUT_DESC, SoundFrame};
use super::tracks::TRACK_COUNT;

/// Per-track cursor.
#[doc(alias("ImWaveDispatch", "ImDispatch_GetById"))]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct WaveDispatch {
    /// The VOC rate divisor is above 0xC4: the source already runs at the output rate.
    pub source_is_high_rate: bool,
    /// Byte cursor.
    pub cur_offset: i32,
    /// Bytes left in the current block.
    pub audio_remaining: i32,
    /// Loop re-seek target.
    pub voc_loop_starting_point: i32,
    /// First audio block, or 0 when whole-sound looping is off.
    pub whole_loop_offset: i32,
    /// Exact rate decoded from the VOC divisor.
    pub source_rate_hz: u32,
    /// 16.16 source position fraction.
    pub resample_phase: u32,
    /// 16.16 source samples per mix sample.
    pub resample_step: u32,
}

#[doc(alias = "ImDispatchState")]
#[derive(Debug, Clone)]
pub(crate) struct DispatchState {
    pub pool: [WaveDispatch; TRACK_COUNT],
    pub resample_buffer: [u8; 1024],
}

impl Default for DispatchState {
    fn default() -> Self {
        Self {
            pool: [WaveDispatch::default(); TRACK_COUNT],
            resample_buffer: [0; 1024],
        }
    }
}

/// Result of a block seek: 0 = an audio block is ready, -1 = end of sound.
type ChunkStatus = i32;

impl Imuse {
    /// Resets a track's cursor and parses its header.
    #[doc(alias = "ImDispatch_SetupSound")]
    pub(crate) fn dispatch_setup_sound(&mut self, host: &mut dyn ImuseHost, track: u8) -> i32 {
        self.dispatch.pool[usize::from(track)] = WaveDispatch::default();
        self.dispatch_seek_to_next_chunk(host, track)
    }

    /// Mixes a variable-rate track (explicit frequency): zero-order hold at the requested step.
    fn play_variable_rate_frame(&mut self, host: &mut dyn ImuseHost, track: u8) {
        let index = usize::from(track);
        let output_count = OUT_DESC
            .mix_sample_count
            .min(self.dispatch.resample_buffer.len());
        let mut out = [0u8; 1024];
        let mut generated = 0;
        let mut terminal = 0;
        'outer: while generated < output_count {
            if self.dispatch.pool[index].audio_remaining == 0 {
                terminal = self.dispatch_seek_to_next_chunk(host, track);
                if terminal != 0 {
                    break;
                }
            }
            let sound = self.tracks.pool[index].sound;
            let Some(base) = files::get_sound_ptr(host, sound) else {
                terminal = -1;
                break;
            };
            let d = &self.dispatch.pool[index];
            let Some(&sample) = base.get(d.cur_offset as usize) else {
                terminal = -1;
                break;
            };
            out[generated] = sample;
            generated += 1;
            let d = &mut self.dispatch.pool[index];
            let phase = u64::from(d.resample_phase) + u64::from(d.resample_step);
            let mut advance = (phase >> 16) as u32;
            d.resample_phase = phase as u32 & 0xffff;
            while advance != 0 {
                if self.dispatch.pool[index].audio_remaining == 0 {
                    terminal = self.dispatch_seek_to_next_chunk(host, track);
                    if terminal != 0 {
                        break 'outer;
                    }
                }
                let d = &mut self.dispatch.pool[index];
                let count = advance.min(d.audio_remaining as u32);
                d.cur_offset += count as i32;
                d.audio_remaining -= count as i32;
                advance -= count;
            }
        }
        if generated != 0 {
            let t = self.tracks.pool[index];
            let frame = SoundFrame {
                buffer: &out[..generated],
                source_is_high_rate: true,
            };
            self.mixer_audio_process_frame(&frame, 0, t.eff_vol, t.pan);
        }
        if terminal == -1 {
            self.tracks_clear(track);
        }
    }

    /// Mixes one half's worth of a track; the end of the sound clears the track.
    #[doc(alias = "ImDispatch_PlaySoundFrame")]
    pub(crate) fn dispatch_play_sound_frame(&mut self, host: &mut dyn ImuseHost, track: u8) {
        let index = usize::from(track);
        if self.dispatch.pool[index].resample_step != 0 {
            self.play_variable_rate_frame(host, track);
            return;
        }
        // Half-rate sources deliver half as many bytes: the mixer writes each one twice.
        let mix_sample_count = if self.dispatch.pool[index].source_is_high_rate {
            OUT_DESC.mix_sample_count
        } else {
            OUT_DESC.mix_sample_count >> 1
        };
        let mut offset = 0;
        let mut samples_left = mix_sample_count;
        let chunk_status = loop {
            if self.dispatch.pool[index].audio_remaining == 0 {
                let status = self.dispatch_seek_to_next_chunk(host, track);
                if status != 0 {
                    break status;
                }
            }
            if samples_left == 0 {
                return;
            }
            let d = self.dispatch.pool[index];
            let count = samples_left.min(d.audio_remaining as usize);
            let sound = self.tracks.pool[index].sound;
            let Some(base) = files::get_sound_ptr(host, sound) else {
                tracing::info!(target: "imuse", "ERR: dispatch got NULL file addr...");
                return;
            };
            let start = d.cur_offset as usize;
            let Some(src) = base.get(start..start + count) else {
                break -1;
            };
            if count != 0 {
                let t = self.tracks.pool[index];
                let frame = SoundFrame {
                    buffer: src,
                    source_is_high_rate: d.source_is_high_rate,
                };
                self.mixer_audio_process_frame(&frame, offset, t.eff_vol, t.pan);
                offset += count;
                samples_left -= count;
                let d = &mut self.dispatch.pool[index];
                d.cur_offset += count as i32;
                d.audio_remaining -= count as i32;
            }
        };
        if chunk_status == -1 {
            self.tracks_clear(track);
        }
    }

    /// Parses VOC blocks at the cursor until an audio block: markers fire triggers, loop blocks rewind, the file
    /// header is skipped. Returns 0 at an audio block and -1 at the end of the sound.
    #[doc(alias = "ImDispatch_SeekToNextChunk")]
    pub(crate) fn dispatch_seek_to_next_chunk(
        &mut self,
        host: &mut dyn ImuseHost,
        track: u8,
    ) -> ChunkStatus {
        let index = usize::from(track);
        let mut whole_loop_rewinds = false;
        let header = loop {
            let t = self.tracks.pool[index];
            let Some(base) = files::get_sound_ptr(host, t.sound) else {
                tracing::info!(target: "imuse", "ERR: null sound addr in SeekToNextChunk...");
                return -1;
            };
            let d = self.dispatch.pool[index];
            let at = d.cur_offset as usize;
            let Some(&chunk_type) = base.get(at) else {
                return -1;
            };
            // Read only what each block needs: the terminator and loop blocks 1 byte, audio and markers 6, the
            // file header 24.
            let needed = match chunk_type {
                1 | 4 => 6,
                8.. => 24,
                _ => 1,
            };
            let Some(chunk) = base.get(at..at + needed) else {
                return -1;
            };
            let mut data = [0u8; 24];
            data[..needed].copy_from_slice(chunk);
            let flags = WaveStartFlags::from_bits_retain(t.start_flags);
            match chunk_type {
                0 => {
                    if flags.contains(WaveStartFlags::LOOP)
                        && d.whole_loop_offset > 0
                        && !whole_loop_rewinds
                    {
                        self.dispatch.pool[index].cur_offset = d.whole_loop_offset;
                        whole_loop_rewinds = true;
                        continue;
                    }
                    return -1;
                }
                1 => {
                    // Some loaders expose only the first audio block; this reproduces that on request.
                    if flags.contains(WaveStartFlags::LOOP_FIRST_BLOCK)
                        && d.whole_loop_offset > 0
                        && d.cur_offset != d.whole_loop_offset
                    {
                        self.dispatch.pool[index].cur_offset = d.whole_loop_offset;
                        continue;
                    }
                    break data;
                }
                4 => {
                    // DEVIATION: the 16-bit marker is read little-endian; upstream reads it in host order.
                    let marker = i16::from_le_bytes([data[4], data[5]]);
                    self.triggers_process_marker(host, t.sound, i32::from(marker));
                    self.dispatch.pool[index].cur_offset += 6;
                }
                6 => {
                    let d = &mut self.dispatch.pool[index];
                    d.voc_loop_starting_point = d.cur_offset;
                    d.cur_offset += 6;
                }
                7 => {
                    let d = &mut self.dispatch.pool[index];
                    d.cur_offset = d.voc_loop_starting_point;
                }
                8.. if &data[..4] == b"Crea" && data[20..24] == [0x1A, 0, 0x0A, 0x01] => {
                    // "Creative Voice File\x1A" preamble and version fields.
                    self.dispatch.pool[index].cur_offset += 26;
                }
                _ => {
                    tracing::info!(target: "imuse", "ERR: Illegal chunk in sound {}...", t.sound.0);
                    return -1;
                }
            }
        };
        // Audio block: a 24-bit little-endian size including the 2-byte rate/format field.
        let audio_size =
            (i32::from(header[3]) << 16) | (i32::from(header[2]) << 8) | i32::from(header[1]);
        if audio_size <= 2 {
            return -1;
        }
        let flags = WaveStartFlags::from_bits_retain(self.tracks.pool[index].start_flags);
        let d = &mut self.dispatch.pool[index];
        if flags.contains(WaveStartFlags::LOOP) && d.whole_loop_offset == 0 {
            d.whole_loop_offset = d.cur_offset;
        }
        // Rate = 1,000,000 / (256 - divisor); above 0xC4 the source already runs at the output rate.
        let divisor = u32::from(header[4]);
        d.source_rate_hz = 1_000_000 / (256 - divisor);
        d.source_is_high_rate = divisor > 0xC4;
        d.audio_remaining = audio_size - 2;
        d.cur_offset += 6;
        0
    }
}
