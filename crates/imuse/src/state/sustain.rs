//! Port of `sustain.c`, `internal/sustain.h`: the jump bridge of US5315057A.
//!
//! Before a sequencer jump lands, the notes still sounding are paired with their Note-Offs in the old sequence and
//! released when those come due; a matching Note-On in the new sequence shortens the release to a 10-tick
//! crossfade.

use super::Imuse;
use super::seq::{SeqRef, midi_get_tick_delta, read_bounded_vlq};
use super::utils::{Linked, Links, list_add_item, list_remove_item};

#[doc(alias = "SUSTAIN_POOL_SIZE")]
const POOL_SIZE: usize = 24;
/// Ticks between a new Note-On and the release of the held note it replaces.
#[doc(alias = "SUSTAIN_CROSSFADE_TICKS")]
const CROSSFADE_TICKS: i32 = 10;
/// MIDI channel message sizes by `(status & 0x70) >> 4`.
#[doc(alias = "c_midiMsgSize")]
const MIDI_MSG_SIZE: [usize; 7] = [3, 3, 3, 3, 2, 2, 3];

#[doc(alias = "ImSustainedSound")]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SustainedSound {
    pub links: Links,
    pub midi_player: u8,
    pub note: i32,
    pub channel_id: i32,
    pub cur_tick: i32,
    /// 16.16 accumulator of the player's step.
    pub cur_tick_fixed: i32,
}

impl Linked for SustainedSound {
    fn links(&self) -> &Links {
        &self.links
    }
    fn links_mut(&mut self) -> &mut Links {
        &mut self.links
    }
}

#[doc(alias = "ImSustainState")]
#[derive(Debug, Clone)]
pub(crate) struct SustainState {
    pub pool: [SustainedSound; POOL_SIZE],
    pub free_list: Option<u8>,
    pub active_list: Option<u8>,
    pub cur_midi_note_mask: [u32; 128],
    pub cur_note_count: i32,
    pub midi_sustain_channel_mask: [u32; 16],
    pub track_ticks_remaining: i32,
    pub midi_tick_delta: i32,
    pub midi_track_end: bool,
}

impl Default for SustainState {
    fn default() -> Self {
        Self {
            pool: [SustainedSound::default(); POOL_SIZE],
            free_list: None,
            active_list: None,
            cur_midi_note_mask: [0; 128],
            cur_note_count: 0,
            midi_sustain_channel_mask: [0; 16],
            track_ticks_remaining: 0,
            midi_tick_delta: 0,
            midi_track_end: false,
        }
    }
}

/// The handlers of a bridge scan: the old sequence looks for Note-Offs, the new one for Note-Ons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JumpPass {
    Open,
    Main,
}

impl Imuse {
    #[doc(alias = "sustain_resetPool")]
    fn sustain_reset_pool(&mut self) {
        let s = &mut self.sustain;
        s.free_list = None;
        s.active_list = None;
        for i in 0..POOL_SIZE {
            s.pool[i].links = Links::default();
            list_add_item(&mut s.pool, &mut s.free_list, i as u8);
        }
    }

    #[doc(alias = "ImSustain_Init")]
    pub(crate) fn sustain_init(&mut self) {
        tracing::info!(target: "imuse", "SUSTAIN module...");
        self.sustain_reset_pool();
    }

    #[doc(alias = "ImSustain_Deinit")]
    pub(crate) fn sustain_deinit(&mut self) -> i32 {
        self.sustain_reset_pool();
        0
    }

    fn sustain_release(&mut self, node: u8) {
        let s = &mut self.sustain;
        list_remove_item(&mut s.pool, &mut s.active_list, node);
        list_add_item(&mut s.pool, &mut s.free_list, node);
    }

    /// Frees the nodes of `player` without sending Note-Offs; the caller already silenced its parts.
    #[doc(alias = "ImSustain_ClearForPlayer")]
    pub(crate) fn sustain_clear_for_player(&mut self, player: u8) {
        let mut cur = self.sustain.active_list;
        while let Some(node) = cur {
            let next = self.sustain.pool[usize::from(node)].links.next;
            if self.sustain.pool[usize::from(node)].midi_player == player {
                self.sustain_release(node);
            }
            cur = next;
        }
    }

    /// Advances every held note by its player's step and sends the delayed Note-Off when it expires.
    #[doc(alias = "ImSustain_Update")]
    pub(crate) fn sustain_update(&mut self) {
        let mut cur = self.sustain.active_list;
        while let Some(node) = cur {
            let index = usize::from(node);
            let next = self.sustain.pool[index].links.next;
            let player = self.sustain.pool[index].midi_player;
            let step = self.seq.data[usize::from(player)].step_fixed;
            let s = &mut self.sustain.pool[index];
            let new_fixed = (step as u32).wrapping_add(s.cur_tick_fixed as u32);
            s.cur_tick_fixed = new_fixed as i32;
            s.cur_tick -= (new_fixed >> 16) as i32;
            s.cur_tick_fixed &= 0xFFFF;
            if s.cur_tick < 0 {
                let (channel, note) = (s.channel_id as u8, s.note);
                self.players_midi_note_off(player, channel, note);
                self.sustain_release(node);
            }
            cur = next;
        }
    }

    /// Walks a sequence from its position, feeding the bridge handlers.
    #[doc(alias(
        "sustain_processJump",
        "sustain_readBounded",
        "sustain_jumpOpen_NoteOn",
        "sustain_metaTrackEnd"
    ))]
    fn sustain_process_jump(&mut self, seq: SeqRef, snd: &[u8], pass: JumpPass, player: u8) {
        self.sustain.midi_track_end = false;
        let data = *self.seq_data(seq);
        let end = (data.chunk_end as usize).min(snd.len());
        let mut p = (data.chunk_offset + data.chunk_ptr) as usize;
        let mut running = data.running_status as u32;
        self.sustain.midi_tick_delta = midi_get_tick_delta(&data, data.cur_tick, data.next_tick);
        while !self.sustain.midi_track_end && p < end {
            let mut status = u32::from(snd[p]);
            if status & 128 != 0 {
                p += 1;
            } else {
                status = running;
            }
            if (0x80..0xf0).contains(&status) {
                let index = ((status & 0x70) >> 4) as usize;
                let count = MIDI_MSG_SIZE[index] - 1;
                if end - p < count || snd[p] > 127 || (count == 2 && snd[p + 1] > 127) {
                    return;
                }
                running = status;
                let channel = (status & 15) as u8;
                let data1 = i32::from(snd[p]);
                let data2 = if count == 2 { i32::from(snd[p + 1]) } else { 0 };
                match (pass, index) {
                    (JumpPass::Open, 0) => self.sustain_jump_open_note_off(player, channel, data1),
                    (JumpPass::Open, 1) => {
                        // A NoteOn of velocity 0 is a NoteOff.
                        if data2 == 0 {
                            self.sustain_jump_open_note_off(player, channel, data1);
                        }
                    }
                    (JumpPass::Main, 1) => self.sustain_jump_note_on(player, channel, data1),
                    _ => {}
                }
                p += count;
            } else if status == 0xf0 || status == 0xf7 || status == 0xff {
                let mut meta = None;
                running = 0;
                if status == 0xff {
                    if p == end {
                        return;
                    }
                    meta = Some(snd[p]);
                    p += 1;
                }
                let Some(length) = read_bounded_vlq(snd, &mut p, end) else {
                    return;
                };
                if length as usize > end - p {
                    return;
                }
                // Meta 0x2F (end of track) halts the scan.
                if meta == Some(0x2F) {
                    self.sustain.midi_track_end = true;
                }
                p += length as usize;
            } else {
                return;
            }
            if self.sustain.midi_track_end {
                return;
            }
            let Some(dt) = read_bounded_vlq(snd, &mut p, end) else {
                return;
            };
            self.sustain.midi_tick_delta = self.sustain.midi_tick_delta.wrapping_add(dt as i32);
        }
    }

    /// A Note-Off in the old sequence closes one of the sounding notes: hold it until then.
    #[doc(alias = "sustain_jumpOpen_NoteOff")]
    fn sustain_jump_open_note_off(&mut self, player: u8, channel: u8, note: i32) {
        let s = &mut self.sustain;
        let note_bits = s.cur_midi_note_mask[note as usize];
        let chan_bit = s.midi_sustain_channel_mask[usize::from(channel)];
        if chan_bit & note_bits == 0 {
            return;
        }
        s.cur_midi_note_mask[note as usize] = note_bits & !chan_bit;
        s.cur_note_count -= 1;
        if s.cur_note_count == 0 {
            s.midi_track_end = true;
        }
        let Some(node) = s.free_list else {
            tracing::info!(target: "imuse", "ERROR:su unable to alloc Sustain...");
            return;
        };
        list_remove_item(&mut s.pool, &mut s.free_list, node);
        list_add_item(&mut s.pool, &mut s.active_list, node);
        let n = &mut s.pool[usize::from(node)];
        n.midi_player = player;
        n.note = note;
        n.channel_id = i32::from(channel);
        n.cur_tick = s.midi_tick_delta;
        n.cur_tick_fixed = 0;
    }

    /// A Note-On in the new sequence for a held note: release the old one just after it.
    #[doc(alias = "sustain_jump_NoteOn")]
    fn sustain_jump_note_on(&mut self, player: u8, channel: u8, note: i32) {
        let s = &mut self.sustain;
        if s.midi_tick_delta > s.track_ticks_remaining {
            s.midi_track_end = true;
            return;
        }
        let chan_bit = s.midi_sustain_channel_mask[usize::from(channel)];
        if s.cur_midi_note_mask[note as usize] & chan_bit == 0 {
            return;
        }
        s.cur_midi_note_mask[note as usize] &= !chan_bit;
        let target = s.midi_tick_delta - CROSSFADE_TICKS;
        let mut cur = s.active_list;
        while let Some(node) = cur {
            let n = &mut s.pool[usize::from(node)];
            if n.midi_player == player
                && n.note == note
                && n.channel_id == i32::from(channel)
                && n.cur_tick >= target
            {
                n.cur_tick = target;
                return;
            }
            cur = n.links.next;
        }
    }

    /// Bridges the notes of `player` across a jump from `prev` to `next`.
    #[doc(alias = "ImSustain_Jump")]
    pub(crate) fn sustain_jump(&mut self, player: u8, snd: &[u8], prev: SeqRef, next: SeqRef) {
        // Channels without a part cannot be sounding.
        for ch in 0..16 {
            let part = self.player(player).channels[ch].data;
            self.sustain.midi_sustain_channel_mask[ch] =
                part.map_or(0, |part| 1u32 << self.part(part).out_channel_id);
        }
        let mut mask = [0; 128];
        self.sustain.cur_note_count = self.slots_get_player_notes(player, &mut mask);
        self.sustain.cur_midi_note_mask = mask;

        // Notes already held need no new node.
        let s = &mut self.sustain;
        let mut cur = s.active_list;
        while let Some(node) = cur {
            if s.cur_note_count == 0 {
                break;
            }
            let n = s.pool[usize::from(node)];
            if n.midi_player == player {
                let chan_bit = s.midi_sustain_channel_mask[n.channel_id as usize];
                let note_bits = s.cur_midi_note_mask[n.note as usize];
                if chan_bit & note_bits != 0 {
                    s.cur_midi_note_mask[n.note as usize] = note_bits & !chan_bit;
                    s.cur_note_count -= 1;
                }
            }
            cur = n.links.next;
        }

        if self.sustain.cur_note_count != 0 {
            self.sustain_process_jump(prev, snd, JumpPass::Open, player);
            if self.sustain.cur_note_count != 0 {
                tracing::info!(target: "imuse", "ERR: su couldn't find all note-offs...");
                for note in 0..128 {
                    if self.sustain.cur_midi_note_mask[note] == 0 {
                        continue;
                    }
                    for ch in 0..16 {
                        if self.sustain.cur_midi_note_mask[note]
                            & self.sustain.midi_sustain_channel_mask[ch]
                            != 0
                        {
                            tracing::info!(target: "imuse", "missing note {note} on chan {}...", ch + 1);
                            self.players_midi_note_off(player, ch as u8, note as i32);
                        }
                    }
                }
            }
        }

        let mut remaining = 0;
        let mut cur = self.sustain.active_list;
        while let Some(node) = cur {
            let n = &self.sustain.pool[usize::from(node)];
            remaining = remaining.max(n.cur_tick);
            cur = n.links.next;
        }
        self.sustain.track_ticks_remaining = remaining;

        let mut mask = [0; 128];
        self.sustain.cur_note_count = self.slots_get_player_notes(player, &mut mask);
        self.sustain.cur_midi_note_mask = mask;
        self.sustain_process_jump(next, snd, JumpPass::Main, player);
    }
}
