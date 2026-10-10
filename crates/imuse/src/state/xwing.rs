//! Port of `xwing.c`, `internal/xwing.h`, `include/imuse/xwing.h`: the DOS93 IMS command dialect. Addresses refer
//! to DOS93_ADLIB/ROLAND.EXE.
//!
//! DEVIATION: the MT-32 backend is dropped, so no backend has Roland hooks. The Roland program, part selection,
//! timbre and SysEx writes upstream sends to such hooks do nothing; channel instrument edits are still recorded.

use super::Imuse;
use super::commands::{ImuseError, ImuseHost, PARAM_MIDI_PART_TRIM, SoundId};
use super::players::ChannelRef;
use super::seq::SeqRef;
use super::utils::wrap_semitones;

fn be16(p: &[u8]) -> u32 {
    (u32::from(p[0]) << 8) | u32::from(p[1])
}

fn be32(p: &[u8]) -> u32 {
    (be16(p) << 16) | be16(&p[2..])
}

/// Module state: the 32 global instrument slots.
#[doc(alias = "ImXwingState")]
#[derive(Debug, Clone)]
pub(crate) struct XwingState {
    pub instruments: [[u8; 246]; 32],
    pub sizes: [u16; 32],
}

impl Default for XwingState {
    fn default() -> Self {
        Self {
            instruments: [[0; 246]; 32],
            sizes: [0; 32],
        }
    }
}

/// Validates a `GMD `, `ROL ` or `ADL ` container: its chunks must tile the declared length, with a header and at
/// least one non-empty track. Trailing LFD padding is allowed.
#[doc(alias = "imuse_xwing_validate_sound")]
pub fn validate_sound(data: &[u8]) -> bool {
    let size = data.len();
    if size < 8
        || ![b"GMD ", b"ROL ", b"ADL "]
            .iter()
            .any(|magic| data.starts_with(*magic))
    {
        return false;
    }
    let length = be32(&data[4..]) as usize;
    if length > size - 8 || length > i32::MAX as usize - 8 {
        return false;
    }
    let end = length + 8;
    let mut pos = 8;
    let mut tracks = 0;
    let mut header = false;
    while end - pos >= 8 {
        let count = be32(&data[pos + 4..]) as usize;
        if count > end - pos - 8 {
            return false;
        }
        if &data[pos..pos + 4] == b"MDhd" {
            header = true;
        }
        if &data[pos..pos + 4] == b"MTrk" {
            if count == 0 {
                return false;
            }
            tracks += 1;
        }
        pos += 8 + count;
    }
    pos == end && tracks != 0 && header
}

/// Splits nibble pairs into bytes. Returns the length, or 0 for an odd or invalid payload.
fn unpack(out: &mut [u8; 246], p: &[u8]) -> usize {
    if !p.len().is_multiple_of(2) || p.len() / 2 > out.len() {
        return 0;
    }
    for i in 0..p.len() / 2 {
        if p[2 * i] > 15 || p[2 * i + 1] > 15 {
            return 0;
        }
        out[i] = (p[2 * i] << 4) | p[2 * i + 1];
    }
    p.len() / 2
}

impl Imuse {
    #[doc(alias = "set_hook")]
    fn xwing_set_hook_raw(
        &mut self,
        player: Option<u8>,
        kind: i32,
        value: i32,
        channel: i32,
    ) -> i32 {
        let Some(player) = player else {
            return -5;
        };
        if !(0..=5).contains(&kind) || !(0..=255).contains(&value) || !(0..=16).contains(&channel) {
            return -5;
        }
        let hooks = &mut self.player_mut(player).legacy_hooks[kind as usize];
        if kind < 2 {
            hooks[0] = value as u8;
        } else if channel == 16 {
            hooks.fill(value as u8);
        } else {
            hooks[channel as usize] = value as u8;
        }
        0
    }

    /// Arms a legacy hook: kinds 0..=5 are jump, transpose, part enable, volume, program and part transpose.
    /// Channel 16 sets every channel of a part hook. Values of 128 and above persist after use.
    #[doc(alias = "imuse_xwing_set_hook")]
    pub fn xwing_set_hook(
        &mut self,
        sound: SoundId,
        kind: i32,
        value: i32,
        channel: i32,
    ) -> Result<(), ImuseError> {
        let player = self.players_get_sound_player(sound);
        super::commands::status(self.xwing_set_hook_raw(player, kind, value, channel))
    }

    /// A hook set through the generic hook call: below 256 a jump hook, else a part-enable hook whose high byte is
    /// the channel plus one.
    #[doc(alias = "ImXwing_SetPackedHook")]
    pub(crate) fn xwing_set_packed_hook(&mut self, player: u8, hook: u32) -> i32 {
        if hook < 256 {
            self.xwing_set_hook_raw(Some(player), 0, hook as i32, 0)
        } else {
            self.xwing_set_hook_raw(Some(player), 2, (hook & 255) as i32, (hook >> 8) as i32 - 1)
        }
    }

    /// Sets a channel's trim.
    #[doc(alias = "imuse_xwing_set_part_volume")]
    pub fn xwing_set_part_volume(
        &mut self,
        sound: SoundId,
        channel: i32,
        value: i32,
    ) -> Result<(), ImuseError> {
        if !(0..=15).contains(&channel) || !(0..=127).contains(&value) {
            return Err(ImuseError::BadArgument);
        }
        if self.players_get_sound_player(sound).is_none() {
            return Err(ImuseError::NoData);
        }
        self.players_set_param(sound, PARAM_MIDI_PART_TRIM + channel, value);
        Ok(())
    }

    /// 0x1204e..0x12243: wildcard 0 is unconditional; only 1..=127 consume the hook.
    fn hook_matches(&mut self, player: u8, kind: usize, channel: usize, value: u8) -> bool {
        let hook =
            &mut self.player_mut(player).legacy_hooks[kind][if kind < 2 { 0 } else { channel }];
        if value != 0 && value != *hook {
            return false;
        }
        if value != 0 && value < 128 {
            *hook = 0;
        }
        true
    }

    /// Sends a channel's instrument to the backend, if it takes instruments.
    #[doc(alias = "ImXwing_ApplyInstrument")]
    pub(crate) fn xwing_apply_instrument(&mut self, at: ChannelRef) {
        let ch = self.channel(at);
        if let Some(part) = ch.data
            && ch.instrument_size != 0
            && self.midi.backend.has_instrument()
        {
            let out = self.part(part).out_channel_id;
            let instrument = ch.instrument;
            let size = usize::from(ch.instrument_size);
            self.midi.backend.instrument(out, &instrument[..size]);
        }
    }

    /// Sends a program to the channel's Roland slot. No ported backend has Roland hooks.
    #[doc(alias = "ImXwing_SetRolandProgram")]
    pub(crate) fn xwing_set_roland_program(&mut self, _player: u8, _channel: u8, _program: i32) {}

    /// Selects the channel's Roland slot on its part. No ported backend has Roland hooks.
    #[doc(alias = "ImXwing_SelectRolandPart")]
    pub(crate) fn xwing_select_roland_part(&mut self, _at: ChannelRef) {}

    #[doc(alias = "set_instrument")]
    fn xwing_set_instrument(&mut self, player: u8, channel: usize, data: &[u8]) {
        if channel > 15 || (data.len() != 30 && data.len() != 246) {
            return;
        }
        let at = ChannelRef {
            player,
            channel: channel as u8,
        };
        let ch = self.channel_mut(at);
        ch.instrument[..data.len()].copy_from_slice(data);
        ch.instrument_size = data.len() as u16;
        ch.part_status = 1;
        let prior_part = ch.data;
        self.players_midi_setup_parts();
        if self.dialect(player) == 3 {
            if prior_part.is_some() && self.channel(at).data == prior_part {
                self.xwing_select_roland_part(at);
            }
        } else {
            self.xwing_apply_instrument(at);
        }
    }

    /// Stores a global instrument slot. Roland 0x1421a would also write it to MT-32 address 0x22000 + slot * 0x100.
    #[doc(alias = "global_instrument")]
    fn xwing_global_instrument(&mut self, slot: usize, data: &[u8]) {
        if slot >= 32 || (data.len() != 30 && data.len() != 246) {
            return;
        }
        self.xwing.instruments[slot][..data.len()].copy_from_slice(data);
        self.xwing.sizes[slot] = data.len() as u16;
    }

    /// Roland DT1 SysEx of a `ROL ` sound: whole timbres, or one- and two-byte edits of a channel's timbre.
    #[doc(alias = "roland_sysex")]
    fn xwing_roland_sysex(&mut self, player: u8, data: &[u8]) {
        if data.len() < 10 {
            return;
        }
        let address =
            ((u32::from(data[4]) << 14) | (u32::from(data[5]) << 7) | u32::from(data[6])) & 0xffff;
        let channel = usize::from(data[1]);
        if data.len() == 255 {
            if address == 0 && channel < 16 {
                self.xwing_set_instrument(player, channel, &data[7..7 + 246]);
            } else if address == 246 {
                self.xwing_global_instrument(channel, &data[7..7 + 246]);
            }
        } else if (data.len() == 10 || data.len() == 11) && channel < 16 && address < 246 {
            let ch = &mut self.player_mut(player).channels[channel];
            if ch.instrument_size != 246 {
                return;
            }
            ch.instrument[address as usize] = data[7];
            if data.len() == 11 && address < 245 {
                ch.instrument[address as usize + 1] = data[8];
            }
        }
    }

    /// Hook-gated controls 0x30..=0x35: jump, transpose, part enable, volume, program and part transpose.
    #[doc(alias = "control_hook")]
    fn xwing_control_hook(
        &mut self,
        host: &mut dyn ImuseHost,
        seq: SeqRef,
        op: u32,
        channel: usize,
        d: &[u8],
    ) {
        let data = *self.seq_data(seq);
        let player = data.player;
        let n = d.len();
        if op == 0x30 && n >= 7 && data.scanning == 0 && self.hook_matches(player, 0, 0, d[0]) {
            let beat = match be16(&d[3..]) {
                0 => 1,
                beat => beat as i32,
            };
            self.seq_jump(
                host,
                seq,
                be16(&d[1..]) as i32 + 1,
                (beat - 1) / 4 + 1,
                (beat - 1) % 4 + 1,
                be16(&d[5..]) as i32,
                1,
            );
        } else if op == 0x31 && n >= 3 && self.hook_matches(player, 1, 0, d[0]) {
            let p = self.player_mut(player);
            p.transpose = wrap_semitones(
                if d[1] != 0 { p.transpose } else { 0 } + i32::from(d[2] as i8),
                -12,
                12,
            );
            for channel in 0..16 {
                self.players_handle_channel_detune_change(ChannelRef { player, channel });
            }
        } else if (0x32..=0x35).contains(&op)
            && n >= 2
            && self.hook_matches(player, (op - 0x30) as usize, channel, d[0])
        {
            let at = ChannelRef {
                player,
                channel: channel as u8,
            };
            if op == 0x32 {
                let enabled = d[1] != 0;
                self.channel_mut(at).enabled = enabled;
                if !enabled {
                    self.players_midi_command(player, channel as u8, 123, 0);
                }
            } else if op == 0x33 {
                self.players_midi_command(player, channel as u8, 7, i32::from(d[1]));
            } else if op == 0x34 && data.dialect != 2 {
                self.players_program_change(player, channel as u8, i32::from(d[1]));
            } else if op == 0x35 && n >= 3 {
                let ch = self.channel_mut(at);
                let base = if d[1] != 0 {
                    i32::from(ch.legacy_transpose)
                } else {
                    0
                };
                ch.legacy_transpose = wrap_semitones(base + i32::from(d[2] as i8), -12, 12) as i8;
                self.players_handle_channel_detune_change(at);
            }
        }
    }

    /// The DOS93 SysEx dialect: markers, instruments, hooks, global instrument slots and loops.
    #[doc(alias = "ImXwing_SysEx")]
    pub(crate) fn xwing_sysex(&mut self, host: &mut dyn ImuseHost, seq: SeqRef, p: &[u8]) {
        let size = p.len();
        if size < 3 || p[size - 1] != 0xf7 {
            return;
        }
        let data = *self.seq_data(seq);
        let player = data.player;
        if p[0] == 0x41 && data.dialect == 3 {
            self.xwing_roland_sysex(player, p);
            return;
        }
        if p[0] != 0x7d || size < 4 {
            return;
        }
        let op = u32::from(p[1]);
        let channel = usize::from(p[2] & 15);
        let mut decoded = [0u8; 246];
        if op == 0x40 && data.scanning == 0 {
            for &marker in &p[3..size - 1] {
                let sound = self.player(player).sound;
                self.player_mut(player).marker = i32::from(marker);
                self.triggers_process_marker(host, sound, i32::from(marker));
                if self.seq.is_end_of_track || self.player(player).sound != sound {
                    return;
                }
            }
        } else if op == 0x10
            && size >= 5
            && u32::from(p[3])
                == match data.dialect {
                    2 => 1,
                    3 => 2,
                    _ => 3,
                }
        {
            let n = unpack(&mut decoded, &p[4..size - 1]);
            self.xwing_set_instrument(player, channel, &decoded[..n]);
        } else if op == 0x11
            && size >= 6
            && u32::from(p[3]) == if data.dialect == 2 { 1 } else { 2 }
        {
            let n = unpack(&mut decoded, &p[5..size - 1]);
            self.xwing_global_instrument(usize::from(p[4]), &decoded[..n]);
        } else if (0x30..=0x35).contains(&op) {
            let n = unpack(&mut decoded, &p[3..size - 1]);
            self.xwing_control_hook(host, seq, op, channel, &decoded[..n]);
        } else if op == 0x60 && size == 8 {
            let n = unpack(&mut decoded, &p[3..7]);
            let slot = if n == 2 { be16(&decoded) as usize } else { 32 };
            if slot < 32 {
                let instrument = self.xwing.instruments[slot];
                let size = usize::from(self.xwing.sizes[slot]);
                self.xwing_set_instrument(player, channel, &instrument[..size]);
            }
        } else if op == 0x50 && data.scanning == 0 {
            let n = unpack(&mut decoded, &p[3..size - 1]);
            if n >= 10 && be16(&decoded[2..]) + 1 < be16(&decoded[6..]) {
                let s = self.seq_data_mut(seq);
                s.loop_count = be16(&decoded) as i32;
                s.loop_chunk = s.seq_index;
                s.loop_beat = be16(&decoded[2..]) as i32;
                s.loop_tick = be16(&decoded[4..]) as i32;
                s.loop_end_beat = be16(&decoded[6..]) as i32;
                s.loop_end_tick = be16(&decoded[8..]) as i32;
            }
        } else if op == 0x51 {
            self.seq_data_mut(seq).loop_count = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = kind.to_vec();
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn validate_sound_requires_a_header_and_a_track() {
        let mut body = chunk(b"MDhd", &[0; 2]);
        body.extend(chunk(b"MTrk", &[0, 0xff, 0x2f, 0]));
        let mut file = b"ADL ".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_be_bytes());
        file.extend(&body);
        assert!(validate_sound(&file));
        file.extend_from_slice(&[0; 3]);
        assert!(validate_sound(&file), "trailing padding is allowed");
        let mut no_header = b"GMD ".to_vec();
        let track = chunk(b"MTrk", &[0]);
        no_header.extend_from_slice(&(track.len() as u32).to_be_bytes());
        no_header.extend(track);
        assert!(!validate_sound(&no_header));
        assert!(!validate_sound(b"MIDI\0\0\0\0"));
    }
}
