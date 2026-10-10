//! Port of `seq.c`, `internal/seq.h`: the MIDI sequencer.
//!
//! Sequencer slots pair one-to-one with players. Playback, jump and scan share one walker; upstream selects their
//! behaviour with 9-slot handler tables, here a [`WalkMode`]. A jump or scan works on scratch copies of a slot and
//! commits the copy on success.
//!
//! Packed time: bits 0..=15 are the tick within the beat, 16..=19 the beat within the measure, 20..=31 the
//! measure. `beats_per_measure` is stored shifted left by 16 so it compares directly with the beat bits.

use std::sync::Arc;

use super::Imuse;
use super::commands::{
    ImuseHost, PARAM_MIDI_BEAT, PARAM_MIDI_CHUNK, PARAM_MIDI_MEASURE, PARAM_MIDI_SPEED,
    PARAM_MIDI_TICK, SoundId,
};
use super::files::{self, goto_chunk};
use super::players::PLAYER_COUNT;
use super::timer::{LOGICAL_TICK_US, XWING_TICK_US};
use super::utils::{check_hook_value, swap32};

/// MIDI channel message sizes by `(status & 0x70) >> 4`.
#[doc(alias = "s_midiMsgSize")]
const MIDI_MSG_SIZE: [usize; 7] = [3, 3, 3, 3, 2, 2, 3];

/// A sequencer slot.
#[doc(alias = "ImSeqData")]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SeqData {
    /// The paired player.
    pub player: u8,
    pub sound: SoundId,
    /// Current `MTrk` chunk.
    pub seq_index: i32,
    /// Exclusive resource-relative bound of the track.
    pub chunk_end: i32,
    /// 0 TIE, 1 GMD, 2 ADL, 3 ROL.
    pub dialect: i32,
    pub running_status: i32,
    pub loop_count: i32,
    pub loop_chunk: i32,
    pub loop_beat: i32,
    pub loop_tick: i32,
    pub loop_end_beat: i32,
    pub loop_end_tick: i32,
    pub scanning: i32,
    /// Payload offset of the track inside the sound.
    pub chunk_offset: i32,
    /// Packed position to advance up to.
    pub next_tick: i32,
    /// Packed position playing now.
    pub cur_tick: i32,
    /// Cursor relative to `chunk_offset`.
    pub chunk_ptr: i32,
    pub ticks_per_beat: i32,
    /// Stored shifted left by 16.
    pub beats_per_measure: i32,
    /// 16.16 accumulator.
    pub tick_accum: i32,
    /// µs per quarter note.
    pub tempo: i32,
    /// 16.16 ticks per service at speed 1.
    pub step: i32,
    /// 0x80 = 1.0.
    pub speed: i32,
    /// `step * speed >> 7`.
    pub step_fixed: i32,
}

#[doc(alias = "ImSeqState")]
#[derive(Debug, Clone, Default)]
pub(crate) struct SeqState {
    pub data: [SeqData; PLAYER_COUNT],
    /// Raised to make the running walker bail out.
    pub is_end_of_track: bool,
    pub sound_prev_state: SeqData,
    pub sound_next_state: SeqData,
    pub scan_prev_state: SeqData,
    pub scan_next_state: SeqData,
}

/// A sequencer slot or one of the jump/scan scratch copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeqRef {
    Player(u8),
    SoundPrev,
    SoundNext,
    ScanPrev,
    ScanNext,
}

/// The handler table a walk uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalkMode {
    /// `s_midiCmdFunc`: every event; end of track retires the player.
    Live,
    /// `s_jumpMidiCmdFunc`: meta events only.
    Jump,
    /// `s_scanMidiCmdFuncs`: voice events (aftertouch aside) and meta events.
    Scan,
}

/// Reads a variable-length quantity of at most four bytes before `end`.
#[doc(alias = "seq_readBoundedVlq")]
pub(crate) fn read_bounded_vlq(data: &[u8], cursor: &mut usize, end: usize) -> Option<u32> {
    let mut value = 0u32;
    for _ in 0..4 {
        if *cursor >= end {
            return None;
        }
        let byte = u32::from(data[*cursor]);
        *cursor += 1;
        value = (value << 7) | (byte & 127);
        if byte & 128 == 0 {
            return Some(value);
        }
    }
    None
}

/// Wraps the tick bits into the beat and the beat bits into the measure.
#[doc(alias = "ImSeq_FixupSoundTick")]
pub(crate) fn fixup_sound_tick(data: &SeqData, value: u32) -> i32 {
    let mut result = value;
    loop {
        let tpb = data.ticks_per_beat as u32;
        if result & 0xFFFF < tpb {
            break;
        }
        result = result.wrapping_add(0x10000).wrapping_sub(tpb);
        loop {
            let bpm = data.beats_per_measure as u32;
            if result & 0xF0000 < bpm {
                break;
            }
            result = result.wrapping_add(0x100000).wrapping_sub(bpm);
        }
    }
    result as i32
}

/// The tick distance `prev - tick` between two packed positions in one measure. The measure coefficient is wrong
/// by 65536 for TIE (`beats_per_measure` is pre-shifted) and is only valid within a measure, as upstream.
#[doc(alias = "ImSeq_MidiGetTickDelta")]
pub(crate) fn midi_get_tick_delta(data: &SeqData, prev_tick: i32, tick: i32) -> i32 {
    let tpb = data.ticks_per_beat as u32;
    let tpm = if data.dialect != 0 {
        (data.beats_per_measure as u32 >> 16).wrapping_mul(tpb)
    } else {
        (data.beats_per_measure as u32).wrapping_mul(tpb)
    };
    let total = |packed: u32| {
        ((packed & 0xF0000) >> 16)
            .wrapping_mul(tpb)
            .wrapping_add(tpm.wrapping_mul(packed >> 20))
            .wrapping_add(packed & 0xFFFF)
    };
    total(prev_tick as u32).wrapping_sub(total(tick as u32)) as i32
}

impl Imuse {
    #[doc(alias = "ImSeq_GetSeqDataForPlayerId")]
    pub(crate) fn seq_data(&self, seq: SeqRef) -> &SeqData {
        match seq {
            SeqRef::Player(player) => &self.seq.data[usize::from(player)],
            SeqRef::SoundPrev => &self.seq.sound_prev_state,
            SeqRef::SoundNext => &self.seq.sound_next_state,
            SeqRef::ScanPrev => &self.seq.scan_prev_state,
            SeqRef::ScanNext => &self.seq.scan_next_state,
        }
    }

    pub(crate) fn seq_data_mut(&mut self, seq: SeqRef) -> &mut SeqData {
        match seq {
            SeqRef::Player(player) => &mut self.seq.data[usize::from(player)],
            SeqRef::SoundPrev => &mut self.seq.sound_prev_state,
            SeqRef::SoundNext => &mut self.seq.sound_next_state,
            SeqRef::ScanPrev => &mut self.seq.scan_prev_state,
            SeqRef::ScanNext => &mut self.seq.scan_next_state,
        }
    }

    /// Binds a player's slot to a sound and positions it at the first `MTrk`: 120 BPM, speed 1.0, 480 ticks per
    /// beat, 4/4.
    #[doc(alias = "ImSeq_StartSound")]
    pub(crate) fn seq_start_sound(
        &mut self,
        host: &mut dyn ImuseHost,
        player: u8,
        sound: SoundId,
    ) -> i32 {
        let Some(snd) = files::get_sound_ptr(host, sound) else {
            return -4;
        };
        let dialect = if snd.starts_with(b"GMD ") {
            1
        } else if snd.starts_with(b"ADL ") {
            2
        } else if snd.starts_with(b"ROL ") {
            3
        } else {
            0
        };
        // DEVIATION: the size is the length of the host's data; upstream asks `getSoundSizeFunc`.
        if dialect != 0 && !crate::state::xwing::validate_sound(&snd) {
            return -4;
        }
        let seq = SeqRef::Player(player);
        let data = self.seq_data_mut(seq);
        data.dialect = dialect;
        data.loop_count = 0;
        data.scanning = 0;
        data.sound = sound;
        data.tick_accum = 0;
        self.seq_set_tempo(seq, 500_000);
        self.seq_set_speed(seq, 128);
        self.seq_set_ticks_per_beat(seq, 480, 4);
        self.seq_go_to_chunk(seq, &snd, 0)
    }

    #[doc(alias = "ImSeq_SetEndOfTrack")]
    pub(crate) fn seq_set_end_of_track(&mut self) {
        self.seq.is_end_of_track = true;
    }

    #[doc(alias = "ImSeq_SetSpeed")]
    pub(crate) fn seq_set_speed(&mut self, seq: SeqRef, speed: i32) -> i32 {
        if speed as u32 > 0xFF {
            return -5;
        }
        self.midi_lock();
        let data = self.seq_data_mut(seq);
        data.speed = speed;
        data.step_fixed = data.step.wrapping_mul(speed) >> 7;
        self.midi_unlock();
        0
    }

    /// Sets µs per quarter note. The step is `(480 * tick_us / tempo) << 16`, computed with both operands shifted
    /// into 16 bits.
    #[doc(alias = "ImSeq_SetTempo")]
    pub(crate) fn seq_set_tempo(&mut self, seq: SeqRef, tempo: i32) -> i32 {
        let tick_us = if self.commands.config.xwing_timing {
            XWING_TICK_US
        } else {
            LOGICAL_TICK_US
        };
        let mut dividend = 480u32 * tick_us as u32;
        let mut divisor = tempo as u32;
        if tempo <= 0 {
            return -5;
        }
        let data = self.seq_data_mut(seq);
        data.tempo = tempo;
        while dividend & 0xFFFF_0000 != 0 || divisor & 0xFFFF_0000 != 0 {
            dividend >>= 1;
            divisor >>= 1;
        }
        data.step = ((dividend << 16) / divisor) as i32;
        let speed = data.speed;
        self.seq_set_speed(seq, speed)
    }

    #[doc(alias = "ImSeq_SetTicksPerBeat")]
    pub(crate) fn seq_set_ticks_per_beat(
        &mut self,
        seq: SeqRef,
        ticks_per_beat: i32,
        beats_per_measure: i32,
    ) {
        self.midi_lock();
        let data = self.seq_data_mut(seq);
        data.ticks_per_beat = ticks_per_beat;
        data.beats_per_measure = beats_per_measure << 16;
        self.midi_unlock();
    }

    #[doc(alias = "ImSeq_GetTimeParam")]
    pub(crate) fn seq_get_time_param(&self, seq: SeqRef, param: i32) -> i32 {
        let data = self.seq_data(seq);
        let next = data.next_tick as u32;
        match param {
            PARAM_MIDI_CHUNK => data.seq_index + 1,
            PARAM_MIDI_MEASURE => (next >> 20) as i32 + 1,
            PARAM_MIDI_BEAT => ((next >> 16) & 0xF) as i32 + 1,
            PARAM_MIDI_TICK => (next & 0xFFFF) as i32,
            PARAM_MIDI_SPEED => data.speed,
            _ => -5,
        }
    }

    /// Positions a slot at chunk `chunk_id` (zero-based) after its first delta time.
    #[doc(alias = "ImSeq_GoToChunk")]
    pub(crate) fn seq_go_to_chunk(&mut self, seq: SeqRef, snd: &[u8], chunk_id: i32) -> i32 {
        let Some(header) = goto_chunk(snd, b"MTrk", chunk_id) else {
            return -1;
        };
        let size = swap32(&snd[header + 4..]) as usize;
        let offset = header + 8;
        if offset > i32::MAX as usize || size > i32::MAX as usize - offset {
            return -1;
        }
        let mut cursor = offset;
        let Some(dt) = read_bounded_vlq(snd, &mut cursor, offset + size) else {
            return -1;
        };
        let data = self.seq_data_mut(seq);
        data.seq_index = chunk_id;
        data.chunk_offset = offset as i32;
        data.chunk_end = (offset + size) as i32;
        data.running_status = 0;
        data.cur_tick = fixup_sound_tick(data, dt);
        data.next_tick = 0;
        data.chunk_ptr = (cursor - offset) as i32;
        0
    }

    /// Runs the events of a slot up to its `next_tick` through the handlers of `mode`.
    #[doc(alias(
        "ImSeq_AdvanceMidi",
        "ImSeq_HandleMetaJumpScan",
        "ImParser_HandleMetaEvent"
    ))]
    pub(crate) fn seq_advance_midi(
        &mut self,
        host: &mut dyn ImuseHost,
        seq: SeqRef,
        mut snd: Arc<[u8]>,
        mode: WalkMode,
    ) {
        self.seq.is_end_of_track = false;
        let data = *self.seq_data(seq);
        let player = data.player;
        let mut tick = data.cur_tick as u32;
        let target = data.next_tick as u32;
        let mut cursor = (data.chunk_offset + data.chunk_ptr) as usize;
        let mut end = (data.chunk_end as usize).min(snd.len());
        let mut event_budget = 65536u32;
        let malformed = loop {
            if tick >= target || event_budget == 0 {
                break false;
            }
            event_budget -= 1;
            if cursor >= end {
                break true;
            }
            let mut status = u32::from(snd[cursor]);
            if status & 128 != 0 {
                cursor += 1;
            } else {
                status = self.seq_data(seq).running_status as u32;
            }
            if (0x80..0xf0).contains(&status) {
                let index = ((status & 0x70) >> 4) as usize;
                let count = MIDI_MSG_SIZE[index] - 1;
                if end - cursor < count
                    || snd[cursor] > 127
                    || (count == 2 && snd[cursor + 1] > 127)
                {
                    break true;
                }
                self.seq_data_mut(seq).running_status = status as i32;
                let data1 = i32::from(snd[cursor]);
                let data2 = if count == 2 {
                    i32::from(snd[cursor + 1])
                } else {
                    0
                };
                self.seq_dispatch_channel(mode, player, index, (status & 15) as u8, data1, data2);
                cursor += count;
            } else if status == 0xf0 || status == 0xf7 || status == 0xff {
                let mut kind = 0;
                self.seq_data_mut(seq).running_status = 0;
                if status == 0xff {
                    if cursor == end {
                        break true;
                    }
                    kind = snd[cursor];
                    cursor += 1;
                }
                let Some(length) = read_bounded_vlq(&snd, &mut cursor, end) else {
                    break true;
                };
                let length = length as usize;
                if length > end - cursor {
                    break true;
                }
                let payload = &snd[cursor..cursor + length];
                let dialect = self.seq_data(seq).dialect;
                if status == 0xf0 && mode == WalkMode::Live {
                    self.parser_handle_sysex(host, seq, payload);
                } else if status == 0xf0 && dialect != 0 && self.seq_data(seq).scanning != 0 {
                    self.xwing_sysex(host, seq, payload);
                } else if status == 0xff {
                    if kind == 0x2f {
                        if mode == WalkMode::Live {
                            self.players_stop_player(player);
                        } else {
                            self.seq.is_end_of_track = true;
                        }
                    } else if kind == 0x51 && length == 3 {
                        let tempo = (i32::from(payload[0]) << 16)
                            | (i32::from(payload[1]) << 8)
                            | i32::from(payload[2]);
                        self.seq_set_tempo(seq, tempo);
                    } else if kind == 0x58
                        && length >= 2
                        && dialect == 0
                        && payload[0] != 0
                        && payload[0] <= 16
                        && payload[1] <= 8
                    {
                        self.seq_set_ticks_per_beat(seq, 1920 >> payload[1], i32::from(payload[0]));
                    }
                }
                if self.seq.is_end_of_track {
                    return;
                }
                cursor += length;
                // A callback may have moved the sound; re-fetch it and keep the offset.
                let sound = self.seq_data(seq).sound;
                let Some(relocated) = files::get_sound_ptr(host, sound) else {
                    return;
                };
                snd = relocated;
                end = (self.seq_data(seq).chunk_end as usize).min(snd.len());
            } else {
                break true;
            }
            if self.seq.is_end_of_track {
                return;
            }
            let Some(dt) = read_bounded_vlq(&snd, &mut cursor, end) else {
                break true;
            };
            tick = fixup_sound_tick(self.seq_data(seq), tick.wrapping_add(dt)) as u32;
        };
        if malformed {
            tracing::error!(target: "imuse", "Malformed MIDI track");
            self.players_stop_player(player);
            return;
        }
        let data = self.seq_data_mut(seq);
        data.cur_tick = tick as i32;
        data.chunk_ptr = (cursor as i32).wrapping_sub(data.chunk_offset);
    }

    /// Runs one channel voice event through the handlers of `mode`.
    fn seq_dispatch_channel(
        &mut self,
        mode: WalkMode,
        player: u8,
        index: usize,
        channel: u8,
        data1: i32,
        data2: i32,
    ) {
        match (mode, index) {
            (WalkMode::Jump, _) => {}
            (_, 0) => self.players_midi_note_off(player, channel, data1),
            (_, 1) => self.players_midi_note_on(player, channel, data1, data2),
            (WalkMode::Live, 2 | 5) => self.seq_midi_pressure(),
            (_, 3) => self.players_midi_command(player, channel, data1, data2),
            (_, 4) => self.players_program_change(player, channel, data1),
            (_, 6) => self.players_handle_channel_pitch_bend(player, channel, data1, data2),
            _ => {}
        }
    }

    /// Steps a player's slot by one service and plays the events that came due.
    #[doc(alias = "ImSeq_Update")]
    pub(crate) fn seq_update(&mut self, host: &mut dyn ImuseHost, player: u8) {
        let seq = SeqRef::Player(player);
        let data = self.seq_data_mut(seq);
        let new_accum = (data.step_fixed as u32).wrapping_add(data.tick_accum as u32);
        data.tick_accum = new_accum as i32;
        data.next_tick = data.next_tick.wrapping_add((new_accum >> 16) as i32);
        data.tick_accum &= 0xFFFF;
        if (data.next_tick as u32 & 0xFFFF) >= data.ticks_per_beat as u32 {
            data.next_tick = fixup_sound_tick(data, data.next_tick as u32);
        }
        let data = *data;
        if data.dialect != 0 && data.loop_count != 0 {
            let next = data.next_tick as u32;
            let beat = (next >> 20) * 4 + ((next >> 16) & 15) + 1;
            if beat >= data.loop_end_beat as u32 && (next & 0xFFFF) as i32 >= data.loop_end_tick {
                self.seq_data_mut(seq).loop_count -= 1;
                let destination = if data.loop_beat != 0 {
                    data.loop_beat
                } else {
                    1
                };
                self.seq_jump(
                    host,
                    seq,
                    data.loop_chunk + 1,
                    (destination - 1) / 4 + 1,
                    (destination - 1) % 4 + 1,
                    data.loop_tick,
                    1,
                );
            }
        }
        let data = *self.seq_data(seq);
        if (data.cur_tick as u32) < data.next_tick as u32 {
            match files::get_sound_ptr(host, data.sound) {
                Some(snd) => self.seq_advance_midi(host, seq, snd, WalkMode::Live),
                None => {
                    tracing::info!(target: "imuse", "ERR: sq int handler got null addr...");
                    self.players_stop_player(player);
                }
            }
        }
    }

    /// Validates a jump or scan target and prepares the scratch pair. Returns 0, -4 (no data), -5 (bad position) or
    /// -1 (no such chunk).
    #[doc(alias = "seq_commonSetupAndValidate")]
    #[allow(clippy::too_many_arguments)]
    fn seq_common_setup_and_validate(
        &mut self,
        host: &mut dyn ImuseHost,
        seq: SeqRef,
        chunk: i32,
        measure: i32,
        beat: i32,
        tick: u32,
        prev: SeqRef,
        next: SeqRef,
    ) -> Result<(Arc<[u8]>, u32), i32> {
        let data = *self.seq_data(seq);
        let snd = files::get_sound_ptr(host, data.sound).ok_or(-4)?;
        if (measure.wrapping_sub(1) as u32) >= 0x3E8 {
            return Err(-5);
        }
        let beat_index = beat.wrapping_sub(1) as u32;
        if beat_index >= 0xC || tick >= 0x1E0 {
            return Err(-5);
        }
        *self.seq_data_mut(prev) = data;
        *self.seq_data_mut(next) = data;
        let packed = tick
            .wrapping_add(beat_index << 16)
            .wrapping_add((measure.wrapping_sub(1) as u32).wrapping_mul(0x100000));
        let target = fixup_sound_tick(&data, packed) as u32;
        let chunk_index = chunk.wrapping_sub(1);
        let next_data = *self.seq_data(next);
        if (chunk_index != next_data.seq_index || target < next_data.next_tick as u32)
            && self.seq_go_to_chunk(next, &snd, chunk_index) != 0
        {
            return Err(-1);
        }
        Ok((snd, target))
    }

    /// Moves a slot to a new position, silencing its notes or (with `sustain`) bridging them.
    #[doc(alias = "ImSeq_Jump")]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn seq_jump(
        &mut self,
        host: &mut dyn ImuseHost,
        seq: SeqRef,
        chunk: i32,
        measure: i32,
        beat: i32,
        tick: i32,
        sustain: i32,
    ) -> i32 {
        self.midi_lock();
        let setup = self.seq_common_setup_and_validate(
            host,
            seq,
            chunk,
            measure,
            beat,
            tick as u32,
            SeqRef::SoundPrev,
            SeqRef::SoundNext,
        );
        let (snd, target) = match setup {
            Ok(setup) => setup,
            Err(code) => {
                if code == -1 {
                    tracing::info!(target: "imuse", "ERR: Sq jump to invalid pos...");
                }
                self.midi_unlock();
                return code;
            }
        };
        self.seq.sound_next_state.next_tick = target as i32;
        self.seq_advance_midi(host, SeqRef::SoundNext, Arc::clone(&snd), WalkMode::Jump);
        if self.seq.is_end_of_track {
            tracing::info!(target: "imuse", "ERR: Sq jump to invalid pos...");
            self.midi_unlock();
            return -1;
        }
        let player = self.seq_data(seq).player;
        if sustain != 0 {
            // Start the bridge from a clean controller state.
            for channel in 0..16 {
                self.players_midi_command(player, channel, 64, 0);
                self.players_midi_command(player, channel, 1, 0);
                self.players_handle_channel_pitch_bend(player, channel, 0, 64);
            }
            self.sustain_jump(player, &snd, SeqRef::SoundPrev, SeqRef::SoundNext);
        } else {
            self.players_stop_all_notes(player);
        }
        *self.seq_data_mut(seq) = self.seq.sound_next_state;
        // Break any outer walker.
        self.seq.is_end_of_track = true;
        self.midi_unlock();
        0
    }

    /// Moves a slot to a new position, replaying voice events silently and restarting the notes that result.
    #[doc(alias = "ImSeq_Scan")]
    pub(crate) fn seq_scan(
        &mut self,
        host: &mut dyn ImuseHost,
        seq: SeqRef,
        chunk: i32,
        measure: i32,
        beat: i32,
        tick: i32,
    ) -> i32 {
        self.midi_lock();
        let setup = self.seq_common_setup_and_validate(
            host,
            seq,
            chunk,
            measure,
            beat,
            tick as u32,
            SeqRef::ScanPrev,
            SeqRef::ScanNext,
        );
        let (snd, target) = match setup {
            Ok(setup) => setup,
            Err(code) => {
                if code == -1 {
                    tracing::info!(target: "imuse", "ERR: Sq scan to invalid pos...");
                }
                self.midi_unlock();
                return code;
            }
        };
        let player = self.seq_data(seq).player;
        self.players_stop_all_notes(player);
        self.slots_set_drv_ignore_notes();
        self.seq.scan_next_state.next_tick = target as i32;
        self.seq.scan_next_state.scanning = 1;
        self.seq_advance_midi(host, SeqRef::ScanNext, Arc::clone(&snd), WalkMode::Scan);
        self.seq.scan_next_state.scanning = 0;
        if self.seq.is_end_of_track {
            tracing::info!(target: "imuse", "ERR: Sq scan to invalid pos...");
            self.players_stop_all_notes(player);
            self.slots_clear_drv_ignore_notes();
            self.midi_unlock();
            return -1;
        }
        self.slots_clear_drv_ignore_notes();
        self.slots_restart_player_notes(player);
        *self.seq_data_mut(seq) = self.seq.scan_next_state;
        self.seq.is_end_of_track = true;
        self.midi_unlock();
        0
    }

    /// Injects a channel voice message (0x80..=0xEF) into a player through the live handlers.
    #[doc(alias = "ImSeq_HandleMidiMsg")]
    pub(crate) fn seq_handle_midi_msg(
        &mut self,
        player: u8,
        status: i32,
        data1: i32,
        data2: i32,
    ) -> i32 {
        if status as u32 >= 0xF0 || status & 0x80 == 0 {
            return -5;
        }
        let index = ((status & 0x70) >> 4) as usize;
        self.seq_dispatch_channel(
            WalkMode::Live,
            player,
            index,
            (status & 0x0F) as u8,
            data1,
            data2,
        );
        0
    }

    /// Aftertouch is dropped.
    #[doc(alias = "ImSeq_MidiPressure")]
    fn seq_midi_pressure(&mut self) {
        tracing::info!(target: "imuse", "ERROR: hey, quit the aftertouch...");
    }

    /// LucasArts SysEx `F0 7D`: sub-op 0 is a marker, 1 a hooked jump, 3 a large marker.
    #[doc(alias = "ImParser_HandleSysEx")]
    fn parser_handle_sysex(&mut self, host: &mut dyn ImuseHost, seq: SeqRef, payload: &[u8]) {
        if self.seq_data(seq).dialect != 0 {
            self.xwing_sysex(host, seq, payload);
            return;
        }
        if payload.len() < 3 || payload[0] != 0x7d {
            return;
        }
        let player = self.seq_data(seq).player;
        let p = &payload[2..];
        match payload[1] {
            0 => {
                self.player_mut(player).marker = i32::from(p[0]);
                let sound = self.player(player).sound;
                self.triggers_process_marker(host, sound, i32::from(p[0]));
            }
            1 => {
                if payload.len() >= 10
                    && check_hook_value(&mut self.player_mut(player).hook, i32::from(p[0])) == 0
                {
                    self.seq_jump(
                        host,
                        seq,
                        i32::from(p[1]),
                        (i32::from(p[2]) << 7) | i32::from(p[3]),
                        i32::from(p[4]),
                        (i32::from(p[5]) << 7) | i32::from(p[6]),
                        i32::from(p[7]),
                    );
                }
            }
            3 => {
                self.player_mut(player).marker = 128;
                let sound = self.player(player).sound;
                self.triggers_process_marker(host, sound, 128);
            }
            _ => {}
        }
    }
}
