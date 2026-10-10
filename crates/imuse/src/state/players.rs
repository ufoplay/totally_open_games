//! Port of `players.c`, `internal/players.h`: the MIDI players and the mapping of their virtual channels to
//! physical voice pairs.
//!
//! Upstream links parts, channels and players with pointers. Here a player is its pool index, a channel is a
//! [`ChannelRef`] and a part is a [`PartRef`]; player `i` is always paired with sequencer slot `i`.

use super::Imuse;
use super::commands::{
    ImuseError, ImuseHost, PARAM_KIND_MASK, PARAM_MIDI_PART_TRIM, PARAM_MIDI_SPEED,
    PARAM_SOUND_DETUNE, PARAM_SOUND_GROUP, PARAM_SOUND_MAILBOX, PARAM_SOUND_PAN,
    PARAM_SOUND_PLAY_COUNT, PARAM_SOUND_PRIORITY, PARAM_SOUND_TRANSPOSE, PARAM_SOUND_VOL, SoundId,
    status,
};
use super::groups::Group;
use super::seq::SeqRef;
use super::slots::PartRef;
use super::utils::{Linked, Links, clamp, list_add_item, list_remove_item, wrap_semitones};

/// Players in the pool. TIE uses the first two; X-Wing timing uses all eight.
#[doc(alias = "IMUSE_MIDI_PLAYERS")]
pub(crate) const PLAYER_COUNT: usize = 8;

/// A channel of a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChannelRef {
    pub player: u8,
    pub channel: u8,
}

/// Per-channel MIDI state of a player.
#[doc(alias = "ImMidiOutChannel")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct MidiOutChannel {
    /// The bound physical part, or `None` while virtual.
    pub data: Option<PartRef>,
    pub instrument: [u8; 246],
    pub instrument_size: u16,
    /// 0..=31 logical MT-32 slot, -1 when unused.
    pub roland_slot: i16,
    pub enabled: bool,
    pub legacy_transpose: i8,
    /// CC17 - 64, in 1/128 semitone.
    pub adlib_detune: i8,
    /// 0 = virtual, 1 = wants a part.
    pub part_status: i32,
    pub part_pgm: i32,
    pub part_trim: i32,
    pub part_priority: i32,
    /// `player.priority + part_priority`.
    pub priority: i32,
    pub part_note_req: i32,
    /// CC 7.
    pub part_volume: i32,
    /// Cached effective volume.
    pub group_volume: i32,
    /// CC 10.
    pub part_pan: i32,
    /// CC 1.
    pub modulation: i32,
    /// CC 64.
    pub sustain: i32,
    pub pitch_bend: i32,
    /// CC 16; 0 turns the bend wheel into a volume control.
    pub pb_range: i32,
    /// Cached composite pitch for the part.
    pub effective_detune: i32,
}

impl Default for MidiOutChannel {
    fn default() -> Self {
        Self {
            data: None,
            instrument: [0; 246],
            instrument_size: 0,
            roland_slot: 0,
            enabled: false,
            legacy_transpose: 0,
            adlib_detune: 0,
            part_status: 0,
            part_pgm: 0,
            part_trim: 0,
            part_priority: 0,
            priority: 0,
            part_note_req: 0,
            part_volume: 0,
            group_volume: 0,
            part_pan: 0,
            modulation: 0,
            sustain: 0,
            pitch_bend: 0,
            pb_range: 0,
            effective_detune: 0,
        }
    }
}

#[doc(alias = "ImMidiPlayer")]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MidiPlayer {
    pub links: Links,
    pub shared_part_player: Option<u8>,
    /// The share partner's id.
    pub shared_sound: SoundId,
    /// 0 = free.
    pub sound: SoundId,
    /// Last fired trigger marker, -1 for none.
    pub marker: i32,
    pub group: i32,
    pub priority: i32,
    pub volume: i32,
    /// `(volume + 1) * effGroupVol >> 7`.
    pub group_volume: i32,
    pub pan: i32,
    pub detune: i32,
    pub transpose: i32,
    /// Game-side scratchpad.
    pub mailbox: i32,
    pub legacy_hooks: [[u8; 16]; 6],
    pub hook: i32,
    pub channels: [MidiOutChannel; 16],
}

impl Linked for MidiPlayer {
    fn links(&self) -> &Links {
        &self.links
    }
    fn links_mut(&mut self) -> &mut Links {
        &mut self.links
    }
}

#[doc(alias = "ImPlayersState")]
#[derive(Debug, Clone, Default)]
pub(crate) struct PlayersState {
    pub players: [MidiPlayer; PLAYER_COUNT],
    pub list: Option<u8>,
}

/// Reverse-priority scan order of `GetParam`, verbatim from IMUSE.ENG `players_getParamCatLookup`.
#[doc(alias = "s_getParamCatLookup")]
const GET_PARAM_CAT_LOOKUP: [i32; 23] = [
    0x1700, 0x1600, 0x1500, 0x1400, 0x1300, 0x1200, 0x1100, 0x0F00, 0x0E00, 0x0D00, 0x0C00, 0x0B00,
    0x0A00, 0x0900, 0x0800, 0x0700, 0x0600, 0x0500, 0x0400, 0x0300, 0x0200, 0x0100, 0x0000,
];

impl Imuse {
    pub(crate) fn player(&self, player: u8) -> &MidiPlayer {
        &self.players.players[usize::from(player)]
    }

    pub(crate) fn player_mut(&mut self, player: u8) -> &mut MidiPlayer {
        &mut self.players.players[usize::from(player)]
    }

    pub(crate) fn channel(&self, channel: ChannelRef) -> &MidiOutChannel {
        &self.player(channel.player).channels[usize::from(channel.channel)]
    }

    pub(crate) fn channel_mut(&mut self, channel: ChannelRef) -> &mut MidiOutChannel {
        &mut self.player_mut(channel.player).channels[usize::from(channel.channel)]
    }

    /// The dialect of the sound a player plays: 0 TIE, 1 GMD, 2 ADL, 3 ROL.
    pub(crate) fn dialect(&self, player: u8) -> i32 {
        self.seq.data[usize::from(player)].dialect
    }

    #[doc(alias = "ImPlayers_Init")]
    pub(crate) fn players_init(&mut self) {
        tracing::info!(target: "imuse", "PLAYERS module...");
        self.players.list = None;
        for i in 0..PLAYER_COUNT {
            let player = &mut self.players.players[i];
            player.links = Links::default();
            player.sound = SoundId(0);
            self.seq.data[i].player = i as u8;
        }
    }

    #[doc(alias = "ImPlayers_Deinit")]
    pub(crate) fn players_deinit(&mut self) -> i32 {
        self.players_stop_all_sounds()
    }

    #[doc(alias = "ImPlayers_ApplyGroupVol")]
    pub(crate) fn players_apply_group_vol(&mut self) {
        let mut cur = self.players.list;
        while let Some(player) = cur {
            let p = self.player(player);
            let group_volume = ((p.volume + 1) * self.get_group_volume_raw(p.group)) >> 7;
            self.player_mut(player).group_volume = group_volume;
            for c in 0..16 {
                self.players_handle_channel_volume_change(ChannelRef { player, channel: c });
            }
            cur = self.player(player).links.next;
        }
    }

    /// Steps every live player's sequencer. The successor is read before the step, which may stop the player.
    #[doc(alias = "ImPlayers_Update")]
    pub(crate) fn players_update(&mut self, host: &mut dyn ImuseHost) {
        let mut cur = self.players.list;
        while let Some(player) = cur {
            let next = self.player(player).links.next;
            self.seq_update(host, player);
            cur = next;
        }
    }

    #[doc(alias = "ImPlayers_StartSound")]
    pub(crate) fn players_start_sound(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        priority: i32,
    ) -> i32 {
        let pri = clamp(priority, 0, 127);
        let Some(player) = self.players_get_free_player(pri) else {
            return -6;
        };
        let group_volume = self.get_group_volume_raw(Group::Music as i32);
        let p = self.player_mut(player);
        p.shared_part_player = None;
        p.shared_sound = SoundId(0);
        p.marker = -1;
        p.group = Group::Music as i32;
        p.priority = pri;
        p.volume = 127;
        p.group_volume = group_volume;
        p.pan = 64;
        p.detune = 0;
        p.transpose = 0;
        p.mailbox = 0;
        p.hook = 0;
        p.legacy_hooks = [[0; 16]; 6];
        // partPgm 128 means "no program yet"; partStatus 0 keeps the channel virtual until the first
        // ProgramChange.
        for ch in &mut p.channels {
            *ch = MidiOutChannel {
                data: None,
                instrument: ch.instrument,
                instrument_size: 0,
                roland_slot: -1,
                enabled: true,
                legacy_transpose: 0,
                adlib_detune: 0,
                part_status: 0,
                part_pgm: 128,
                part_trim: 127,
                part_priority: 0,
                priority: pri,
                part_note_req: 1,
                part_volume: 127,
                group_volume,
                part_pan: 64,
                modulation: 0,
                sustain: 0,
                pitch_bend: 0,
                pb_range: 2,
                effective_detune: 0,
            };
        }
        if self.seq_start_sound(host, player, sound) != 0 {
            return -1;
        }
        self.player_mut(player).sound = sound;
        let Self { players, .. } = self;
        list_add_item(&mut players.players, &mut players.list, player);
        0
    }

    #[doc(alias = "ImPlayers_StopSound")]
    pub(crate) fn players_stop_sound(&mut self, sound: SoundId) -> i32 {
        let mut result = -1;
        while let Some(player) = self.players_get_sound_player(sound) {
            self.players_stop_player(player);
            result = 0;
        }
        result
    }

    #[doc(alias = "ImPlayers_StopAllSounds")]
    pub(crate) fn players_stop_all_sounds(&mut self) -> i32 {
        while let Some(player) = self.players.list {
            self.players_stop_player(player);
        }
        0
    }

    /// The smallest live id above `sound` (unsigned order), or 0.
    #[doc(alias("ImPlayers_FindNextSound", "ImMidi_GetNextSound"))]
    pub(crate) fn players_find_next_sound(&self, sound: SoundId) -> SoundId {
        let mut smallest_greater = SoundId(0);
        let mut cur = self.players.list;
        while let Some(player) = cur {
            let p = self.player(player);
            if sound.unsigned() < p.sound.unsigned()
                && (smallest_greater.0 == 0 || smallest_greater.unsigned() > p.sound.unsigned())
            {
                smallest_greater = p.sound;
            }
            cur = p.links.next;
        }
        smallest_greater
    }

    /// Retires a player: unlinks it, cancels its fades, triggers and sustains, releases its parts, breaks its share
    /// pair and reallocates the freed parts.
    #[doc(alias = "ImPlayers_StopPlayer")]
    pub(crate) fn players_stop_player(&mut self, player: u8) {
        {
            let Self { players, .. } = self;
            list_remove_item(&mut players.players, &mut players.list, player);
        }
        let sound = self.player(player).sound;
        self.fades_disable_for_sound(sound, -1);
        self.triggers_clear(sound, -1, None);
        self.sustain_clear_for_player(player);
        self.seq_set_end_of_track();
        for channel in 0..16 {
            self.players_reset_midi_out_channel(ChannelRef { player, channel });
        }
        self.player_mut(player).sound = SoundId(0);
        if let Some(partner) = self.player(player).shared_part_player {
            let partner = self.player_mut(partner);
            partner.shared_part_player = None;
            partner.shared_sound = SoundId(0);
        }
        self.players_midi_setup_parts();
    }

    #[doc(alias("ImPlayers_SetParam", "ImMidi_SetParam"))]
    pub(crate) fn players_set_param(&mut self, sound: SoundId, param: i32, value: i32) -> i32 {
        let Some(player) = self.players_get_sound_player(sound) else {
            return -4;
        };
        let kind = param & PARAM_KIND_MASK;
        match kind {
            PARAM_SOUND_GROUP => {
                if value as u32 >= 0x10 {
                    return -5;
                }
                let group_volume =
                    ((self.player(player).volume + 1) * self.get_group_volume_raw(value)) >> 7;
                let p = self.player_mut(player);
                p.group = value;
                p.group_volume = group_volume;
                for channel in 0..16 {
                    self.players_handle_channel_volume_change(ChannelRef { player, channel });
                }
                0
            }
            PARAM_SOUND_PRIORITY => {
                if value as u32 > 0x7F {
                    return -5;
                }
                self.player_mut(player).priority = value;
                for channel in 0..16 {
                    self.players_handle_channel_priority_change(ChannelRef { player, channel });
                }
                self.players_midi_setup_parts();
                0
            }
            PARAM_SOUND_VOL => {
                if value as u32 > 0x7F {
                    return -5;
                }
                let group_volume =
                    ((value + 1) * self.get_group_volume_raw(self.player(player).group)) >> 7;
                let p = self.player_mut(player);
                p.volume = value;
                p.group_volume = group_volume;
                for channel in 0..16 {
                    self.players_handle_channel_volume_change(ChannelRef { player, channel });
                }
                0
            }
            PARAM_SOUND_PAN => {
                if value as u32 > 0x7F {
                    return -5;
                }
                self.player_mut(player).pan = value;
                // Composite pan keeps each channel's offset from the player's.
                for channel in 0..16 {
                    let ch = self.player(player).channels[channel];
                    if ch.data.is_some() {
                        let final_pan = clamp(value + ch.part_pan - 64, 0, 127);
                        self.slots_part_set_pan(ch.data, final_pan);
                    }
                }
                0
            }
            PARAM_SOUND_DETUNE => {
                if !(-9216..=9216).contains(&value) {
                    return -5;
                }
                self.player_mut(player).detune = value;
                for channel in 0..16 {
                    self.players_handle_channel_detune_change(ChannelRef { player, channel });
                }
                0
            }
            PARAM_SOUND_TRANSPOSE => {
                if !(-12..=12).contains(&value) {
                    return -5;
                }
                // Zero resets; anything else adds and wraps.
                let p = self.player_mut(player);
                p.transpose = if value != 0 {
                    wrap_semitones(p.transpose + value, -12, 12)
                } else {
                    0
                };
                for channel in 0..16 {
                    self.players_handle_channel_detune_change(ChannelRef { player, channel });
                }
                0
            }
            PARAM_SOUND_MAILBOX => {
                self.player_mut(player).mailbox = value;
                0
            }
            PARAM_MIDI_SPEED => self.seq_set_speed(SeqRef::Player(player), value),
            PARAM_MIDI_PART_TRIM => {
                let channel = (param & 0xFF) as u8;
                if channel >= 16 || value as u32 > 0x7F {
                    return -5;
                }
                self.player_mut(player).channels[usize::from(channel)].part_trim = value;
                self.players_handle_channel_volume_change(ChannelRef { player, channel });
                0
            }
            _ => {
                tracing::info!(target: "imuse", "ERR: PlSetParam() couldn't set param {param}...");
                -5
            }
        }
    }

    #[doc(alias("ImPlayers_GetParam", "ImMidi_GetParam"))]
    pub(crate) fn players_get_param(&self, sound: SoundId, param: i32) -> i32 {
        let mut cur = self.players.list;
        let player = loop {
            let Some(player) = cur else {
                // The live count is always 0 here: a match breaks out first.
                return if param == PARAM_SOUND_PLAY_COUNT {
                    0
                } else {
                    -4
                };
            };
            if self.player(player).sound == sound {
                break player;
            }
            cur = self.player(player).links.next;
        };
        let p = self.player(player);
        let kind = param & PARAM_KIND_MASK;
        if kind > 0x1700 {
            return -5;
        }
        let tbl_rem = GET_PARAM_CAT_LOOKUP
            .iter()
            .position(|&entry| entry == kind)
            .map_or(0, |i| 23 - i);
        match tbl_rem {
            1 | 3 => -1,
            2 => {
                let mut count = 1;
                let mut cur = p.links.next;
                while let Some(other) = cur {
                    if self.player(other).sound == sound {
                        count += 1;
                    }
                    cur = self.player(other).links.next;
                }
                count
            }
            4 => p.marker,
            5 => p.group,
            6 => p.priority,
            7 => p.volume,
            8 => p.pan,
            9 => p.detune,
            10 => p.transpose,
            11 => p.mailbox,
            12..=16 => self.seq_get_time_param(SeqRef::Player(player), param),
            17..=23 => {
                let channel = (param & 0xFF) as usize;
                if channel >= 16 {
                    return -5;
                }
                let ch = &p.channels[channel];
                match tbl_rem {
                    17 => ch.part_trim,
                    18 => i32::from(ch.part_status != 0),
                    19 => ch.part_priority,
                    20 => ch.part_note_req,
                    21 => ch.part_volume,
                    22 => ch.part_pan,
                    _ => ch.part_pgm,
                }
            }
            _ => -5,
        }
    }

    #[doc(alias("ImPlayers_SetHook", "ImMidi_SetHook"))]
    pub(crate) fn players_set_hook(&mut self, sound: SoundId, hook: u32) -> i32 {
        let Some(player) = self.players_get_sound_player(sound) else {
            return -4;
        };
        if self.dialect(player) != 0 {
            return self.xwing_set_packed_hook(player, hook);
        }
        if hook > 128 {
            return -5;
        }
        self.player_mut(player).hook = hook as i32;
        0
    }

    #[doc(alias("ImPlayers_GetHook", "ImMidi_GetHook"))]
    pub(crate) fn players_get_hook(&self, sound: SoundId) -> i32 {
        match self.players_get_sound_player(sound) {
            Some(player) if self.dialect(player) != 0 => {
                i32::from(self.player(player).legacy_hooks[0][0])
            }
            Some(player) => self.player(player).hook,
            None => -4,
        }
    }

    /// Silences all 16 channels without releasing their parts.
    #[doc(alias = "ImPlayers_StopAllNotes")]
    pub(crate) fn players_stop_all_notes(&mut self, player: u8) {
        for channel in 0..16 {
            self.players_midi_command(player, channel, 123, 0);
            self.players_midi_command(player, channel, 1, 0);
            self.players_handle_channel_pitch_bend(player, channel, 0, 64);
        }
    }

    /// Three passes of NoteOff on every channel and note through player 0.
    #[doc(alias("ImPlayers_Panic", "ImMidi_Panic"))]
    pub(crate) fn players_panic(&mut self) -> i32 {
        for _ in 0..3 {
            for note in 0..128 {
                for channel in 0..16 {
                    self.players_midi_note_off(0, channel, note);
                }
            }
        }
        0
    }

    #[doc(alias("ImPlayers_Jump", "ImMidi_Jump"))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn players_jump(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        chunk: i32,
        measure: i32,
        beat: i32,
        tick: i32,
        sustain: i32,
    ) -> i32 {
        match self.players_get_sound_player(sound) {
            Some(player) => self.seq_jump(
                host,
                SeqRef::Player(player),
                chunk,
                measure,
                beat,
                tick,
                sustain,
            ),
            None => -4,
        }
    }

    #[doc(alias("ImPlayers_Scan", "ImMidi_Scan"))]
    pub(crate) fn players_scan(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        chunk: i32,
        measure: i32,
        beat: i32,
        tick: i32,
    ) -> i32 {
        match self.players_get_sound_player(sound) {
            Some(player) => self.seq_scan(host, SeqRef::Player(player), chunk, measure, beat, tick),
            None => -4,
        }
    }

    #[doc(alias("ImPlayers_SendMidiMsg", "ImMidi_SendMidiMsg"))]
    pub(crate) fn players_send_midi_msg(
        &mut self,
        sound: SoundId,
        status: i32,
        data1: i32,
        data2: i32,
    ) -> i32 {
        match self.players_get_sound_player(sound) {
            Some(player) => self.seq_handle_midi_msg(player, status, data1, data2),
            None => -4,
        }
    }

    /// Pairs two live sounds so they share physical parts for a seamless crossfade.
    #[doc(alias("ImPlayers_ShareParts", "ImMidi_ShareParts"))]
    pub(crate) fn players_share_parts(&mut self, sound1: SoundId, sound2: SoundId) -> i32 {
        let (Some(p1), Some(p2)) = (
            self.players_get_sound_player(sound1),
            self.players_get_sound_player(sound2),
        ) else {
            return -1;
        };
        if self.player(p1).shared_part_player.is_some()
            || self.player(p2).shared_part_player.is_some()
        {
            return -1;
        }
        self.player_mut(p1).shared_part_player = Some(p2);
        self.player_mut(p1).shared_sound = sound2;
        self.player_mut(p2).shared_part_player = Some(p1);
        self.player_mut(p2).shared_sound = sound1;
        self.players_midi_setup_parts();
        0
    }

    #[doc(alias = "ImPlayers_ProgramChange")]
    pub(crate) fn players_program_change(&mut self, player: u8, channel: u8, program: i32) {
        let dialect = self.dialect(player);
        // DOS93 ADLIB.IMS 0x13A12: ordinary program changes are ignored.
        if dialect == 2 {
            return;
        }
        let at = ChannelRef { player, channel };
        let ch = self.channel(at);
        if dialect == 3 && ch.part_status != 0 && ch.instrument_size == 0 && ch.part_pgm == program
        {
            return;
        }
        let ch = self.channel_mut(at);
        ch.instrument_size = 0;
        ch.part_pgm = program;
        if dialect == 3 {
            self.xwing_set_roland_program(player, channel, program);
        }
        let ch = *self.channel(at);
        if ch.part_status != 0 {
            if ch.data.is_some() {
                self.slots_part_set_pgm(ch.data, program);
                if dialect == 3 {
                    self.xwing_select_roland_part(at);
                }
            }
        } else {
            // The first program change activates the channel.
            self.channel_mut(at).part_status = 1;
            self.players_midi_setup_parts();
        }
    }

    #[doc(alias = "ImPlayers_MidiNoteOn")]
    pub(crate) fn players_midi_note_on(
        &mut self,
        player: u8,
        channel: u8,
        note: i32,
        velocity: i32,
    ) {
        let ch = *self.channel(ChannelRef { player, channel });
        if self.dialect(player) == 2 && ch.instrument_size == 0 {
            // ADLB has no GM drum fallback or default program.
            return;
        }
        if !ch.enabled {
            return;
        }
        if velocity == 0 {
            self.players_midi_note_off(player, channel, note);
            return;
        }
        if ch.part_status != 0 {
            if ch.data.is_some() {
                self.slots_handle_note_on(ch.data, note, velocity);
            }
        } else {
            self.slots_handle_drum_note_on(
                ch.priority,
                ch.part_note_req,
                ch.group_volume,
                note,
                velocity,
            );
        }
    }

    #[doc(alias = "ImPlayers_MidiNoteOff")]
    pub(crate) fn players_midi_note_off(&mut self, player: u8, channel: u8, note: i32) {
        let ch = *self.channel(ChannelRef { player, channel });
        if self.dialect(player) == 2 && ch.part_status == 0 {
            return;
        }
        if ch.part_status != 0 {
            if ch.data.is_some() {
                self.slots_handle_note_off(ch.data, note);
            }
        } else {
            self.slots_note_off_drum_channel(note);
        }
    }

    #[doc(alias = "ImPlayers_MidiCommand")]
    pub(crate) fn players_midi_command(&mut self, player: u8, channel: u8, cc: i32, value: i32) {
        let at = ChannelRef { player, channel };
        let dialect = self.dialect(player);
        match cc {
            1 => {
                self.channel_mut(at).modulation = value;
                let data = self.channel(at).data;
                if data.is_some() {
                    self.slots_part_set_modulation(data, value);
                }
            }
            7 => {
                self.channel_mut(at).part_volume = value;
                self.players_handle_channel_volume_change(at);
            }
            10 => {
                self.channel_mut(at).part_pan = value;
                let data = self.channel(at).data;
                if data.is_some() {
                    let clamped = clamp(self.player(player).pan + value - 64, 0, 127);
                    self.slots_part_set_pan(data, clamped);
                }
            }
            16 => {
                if value as u32 > 0xC {
                    return;
                }
                self.channel_mut(at).pb_range = value;
                // Re-centre the bend after a range change.
                self.players_handle_channel_pitch_bend(player, channel, 0, 64);
            }
            17 => {
                if dialect == 2 {
                    self.channel_mut(at).adlib_detune = (value - 64) as i8;
                    self.players_handle_channel_detune_change(at);
                    return;
                }
                self.channel_mut(at).part_note_req = value;
                let data = self.channel(at).data;
                if data.is_some() {
                    self.slots_part_set_note_req(data, value);
                }
            }
            18 => {
                self.channel_mut(at).part_priority = if dialect == 2 { value - 64 } else { value };
                self.players_handle_channel_priority_change(at);
                self.players_midi_setup_parts();
            }
            64 => {
                self.channel_mut(at).sustain = value;
                let data = self.channel(at).data;
                if data.is_some() {
                    self.slots_set_channel_sustain(data, value);
                }
            }
            123 => {
                self.channel_mut(at).sustain = 0;
                let data = self.channel(at).data;
                if data.is_some() {
                    self.slots_free_midi_channel(data);
                }
            }
            _ => {}
        }
    }

    #[doc(alias = "ImPlayers_HandleChannelPitchBend")]
    pub(crate) fn players_handle_channel_pitch_bend(
        &mut self,
        player: u8,
        channel: u8,
        data1: i32,
        data2: i32,
    ) {
        let at = ChannelRef { player, channel };
        let dialect = self.dialect(player);
        let signed_bend = (data1 | (data2 << 7)) - 0x2000;
        let pb_range = self.channel(at).pb_range;
        if pb_range != 0 || dialect == 2 {
            // DOS93 drivers use 128 units per semitone; FM4 uses 256.
            let shift = if dialect == 2 || dialect == 3 { 6 } else { 5 };
            let bend = pb_range * signed_bend;
            self.channel_mut(at).pitch_bend = if bend >= 0 {
                bend >> shift
            } else {
                -((-bend + (1 << shift) - 1) >> shift)
            };
            self.players_handle_channel_detune_change(at);
        } else {
            // A zero range turns the bend wheel into a volume control.
            self.channel_mut(at).pitch_bend = signed_bend >> 6;
            self.players_handle_channel_volume_change(at);
        }
    }

    /// A free player, or the lowest-priority live one (ties to the most recent) if `priority` beats it.
    #[doc(alias = "ImPlayers_GetFreePlayer")]
    pub(crate) fn players_get_free_player(&mut self, priority: i32) -> Option<u8> {
        let count = if self.commands.config.xwing_timing {
            PLAYER_COUNT
        } else {
            2
        };
        if let Some(free) = (0..count).find(|&i| self.players.players[i].sound.0 == 0) {
            return Some(free as u8);
        }
        tracing::info!(target: "imuse", "ERR: no spare player slot, will try to preempt...");
        let mut lowest = 127;
        let mut victim = None;
        let mut cur = self.players.list;
        while let Some(player) = cur {
            let p = self.player(player);
            if lowest >= p.priority {
                lowest = p.priority;
                victim = Some(player);
            }
            cur = p.links.next;
        }
        let victim = victim?;
        if lowest > priority {
            return None;
        }
        self.players_stop_player(victim);
        Some(victim)
    }

    #[doc(alias = "ImPlayers_GetSoundPlayer")]
    pub(crate) fn players_get_sound_player(&self, sound: SoundId) -> Option<u8> {
        let mut cur = self.players.list;
        while let Some(player) = cur {
            if self.player(player).sound == sound {
                return Some(player);
            }
            cur = self.player(player).links.next;
        }
        None
    }

    fn channel_is_audible(&self, at: ChannelRef) -> bool {
        let dialect = self.dialect(at.player);
        let ch = self.channel(at);
        if dialect == 2 {
            // DOS93 retains parts and notes through silence.
            return true;
        }
        if dialect == 3 {
            return ch.enabled && ch.roland_slot >= 0;
        }
        self.player(at.player).group_volume != 0 && ch.part_trim != 0 && ch.part_volume != 0
    }

    /// Recomputes the channel-to-part mapping until no audible channel wants a part it can get.
    #[doc(alias = "ImPlayers_MidiSetupParts")]
    pub(crate) fn players_midi_setup_parts(&mut self) {
        // No ported backend has Roland part selection.
        let roland = false;
        loop {
            let mut out_channel: Option<ChannelRef> = None;
            let mut chan: Option<ChannelRef> = None;
            let mut last_update_is_chan = false;

            let mut cur = self.players.list;
            while let Some(player) = cur {
                for channel in 0..16u8 {
                    let iter = ChannelRef { player, channel };
                    let partner =
                        self.player(player)
                            .shared_part_player
                            .map(|partner| ChannelRef {
                                player: partner,
                                channel,
                            });

                    // Share-parts reconciliation: the silent side inherits its partner's physical voice.
                    if let Some(partner) = partner {
                        let iter_ch = *self.channel(iter);
                        let partner_ch = *self.channel(partner);
                        if let (Some(data), None) = (iter_ch.data, partner_ch.data) {
                            if partner_ch.part_status != 0 && self.channel_is_audible(partner) {
                                self.players_assign_midi_channel(partner, data.shared());
                            }
                        } else if let (None, Some(data)) = (iter_ch.data, partner_ch.data)
                            && iter_ch.part_status != 0
                            && self.channel_is_audible(iter)
                        {
                            self.players_assign_midi_channel(iter, data.shared());
                        }
                    }

                    let iter_ch = *self.channel(iter);
                    if iter_ch.data.is_some() {
                        // Don't break a share pair whose partner holds a higher-priority part.
                        let partner_locks = partner.is_some_and(|partner| {
                            let partner_ch = self.channel(partner);
                            partner_ch.data.is_some() && partner_ch.priority > iter_ch.priority
                        });
                        if !partner_locks
                            && chan
                                .is_none_or(|chan| iter_ch.priority <= self.channel(chan).priority)
                        {
                            chan = Some(iter);
                            last_update_is_chan = true;
                        }
                    } else if iter_ch.part_status != 0 && self.channel_is_audible(iter) {
                        let better = out_channel.is_none_or(|out| {
                            let out_priority = self.channel(out).priority;
                            iter_ch.priority > out_priority
                                || (roland && iter_ch.priority == out_priority)
                        });
                        if better {
                            out_channel = Some(iter);
                            last_update_is_chan = false;
                        }
                    }
                }
                cur = self.player(player).links.next;
            }

            let Some(out) = out_channel else {
                return;
            };

            if let Some(pair) = self.slots_get_free_player_parts() {
                self.players_assign_midi_channel(out, PartRef { pair, half: 0 });
                continue;
            }

            // No free pair: try to steal `chan`.
            let chan_pri = chan.map_or(0, |chan| self.channel(chan).priority);
            let out_pri = self.channel(out).priority;
            if roland && (chan.is_none() || out_pri <= chan_pri) {
                return;
            }
            let Some(victim) = chan else {
                return;
            };
            if out_pri <= chan_pri
                && (chan_pri != out_pri || !last_update_is_chan || out.player == victim.player)
            {
                return;
            }

            let stolen = self
                .channel(victim)
                .data
                .expect("the steal candidate holds a part");
            self.players_reset_midi_out_channel(victim);
            // Release the sibling's binding too so both halves are cleaned up.
            if let Some(sibling_channel) = self.part(stolen.shared()).out_channel {
                self.players_reset_midi_out_channel(sibling_channel);
            }
            self.players_assign_midi_channel(out, stolen);
        }
    }

    /// Binds a channel to a part and pushes the channel's state so the binding is audible at once.
    #[doc(alias = "ImPlayers_AssignMidiChannel")]
    pub(crate) fn players_assign_midi_channel(&mut self, at: ChannelRef, part: PartRef) {
        self.channel_mut(at).data = Some(part);
        let p = self.part_mut(part);
        p.player = Some(at.player);
        p.out_channel = Some(at);
        let ch = *self.channel(at);
        self.slots_part_set_pgm(Some(part), ch.part_pgm);
        if self.dialect(at.player) == 3 {
            self.xwing_select_roland_part(at);
        } else {
            self.xwing_apply_instrument(at);
        }
        let ch = *self.channel(at);
        self.slots_part_set_priority(Some(part), ch.priority);
        self.slots_part_set_note_req(Some(part), ch.part_note_req);
        self.slots_part_set_volume(Some(part), ch.group_volume);
        self.slots_part_set_pan(Some(part), ch.part_pan);
        self.slots_part_set_modulation(Some(part), ch.modulation);
        self.slots_part_set_pitch_bend(Some(part), ch.effective_detune);
        self.slots_set_channel_sustain(Some(part), ch.sustain);
    }

    /// Unbinds a channel from its part, silencing the part's notes.
    #[doc(alias = "ImPlayers_ResetMidiOutChannel")]
    pub(crate) fn players_reset_midi_out_channel(&mut self, at: ChannelRef) {
        let Some(part) = self.channel(at).data else {
            return;
        };
        let owner = self.part(part).player;
        let adlib_shared = owner.is_some_and(|owner| self.dialect(owner) == 2)
            && self.part(part.shared()).player.is_some();
        if adlib_shared {
            // The surviving shared owner keeps the original part's voices.
            let bit = 1u32 << self.part(part).out_channel_id;
            let half = usize::from(part.half);
            for note in 0..128 {
                self.slots.active_notes[half][note] &= !bit;
                self.slots.sustained_notes[half][note] &= !bit;
            }
        } else {
            self.slots_free_midi_channel(Some(part));
        }
        let p = self.part_mut(part);
        p.player = None;
        p.out_channel = None;
        self.channel_mut(at).data = None;
    }

    #[doc(alias = "ImPlayers_HandleChannelPriorityChange")]
    pub(crate) fn players_handle_channel_priority_change(&mut self, at: ChannelRef) {
        let mut eff = self.player(at.player).priority + self.channel(at).part_priority;
        if self.dialect(at.player) == 2 {
            eff = clamp(eff, 0, 255);
        }
        self.channel_mut(at).priority = eff;
        let data = self.channel(at).data;
        if data.is_some() {
            self.slots_part_set_priority(data, eff);
        }
    }

    #[doc(alias = "ImPlayers_HandleChannelVolumeChange")]
    pub(crate) fn players_handle_channel_volume_change(&mut self, at: ChannelRef) {
        let dialect = self.dialect(at.player);
        let player_group_volume = self.player(at.player).group_volume;
        let ch = *self.channel(at);
        let trim_plus1 = ch.part_trim + 1;
        let modulated_vol = if ch.pb_range != 0 || dialect == 2 {
            ch.part_volume
        } else {
            // A zero bend range reuses the bend as a ratio-preserving volume modulator.
            let headroom = if ch.pitch_bend >= 0 {
                127 - ch.part_volume
            } else {
                ch.part_volume
            };
            ch.part_volume + ((ch.pitch_bend * (headroom + 1)) >> 7)
        };
        let group_volume = ((player_group_volume
            .wrapping_mul(trim_plus1)
            .wrapping_mul(modulated_vol + 1)) as u32
            >> 14) as i32;
        self.channel_mut(at).group_volume = group_volume;
        if dialect == 2 || dialect == 3 {
            if ch.data.is_some() {
                self.slots_part_set_volume(ch.data, group_volume);
            } else if ch.part_status != 0 && (dialect == 2 || ch.roland_slot >= 0) {
                self.players_midi_setup_parts();
            }
            return;
        }
        if player_group_volume != 0 && ch.part_trim != 0 && ch.part_volume != 0 {
            if ch.data.is_some() {
                self.slots_part_set_volume(ch.data, group_volume);
                return;
            }
        } else {
            if ch.data.is_none() {
                return;
            }
            self.players_reset_midi_out_channel(at);
        }
        self.players_midi_setup_parts();
    }

    #[doc(alias = "ImPlayers_HandleChannelDetuneChange")]
    pub(crate) fn players_handle_channel_detune_change(&mut self, at: ChannelRef) {
        let p = self.player(at.player);
        let ch = self.channel(at);
        let legacy = i32::from(ch.legacy_transpose);
        let eff = match self.dialect(at.player) {
            2 => {
                wrap_semitones(p.transpose + legacy, -12, 12) * 128
                    + ch.pitch_bend
                    + clamp(p.detune + i32::from(ch.adlib_detune), -128, 127)
            }
            3 => (p.transpose + legacy) * 128 + ch.pitch_bend + clamp(p.detune, -128, 127),
            _ => (p.transpose + legacy) * 256 + ch.pitch_bend + p.detune,
        };
        self.channel_mut(at).effective_detune = eff;
        let data = self.channel(at).data;
        if data.is_some() {
            self.slots_part_set_pitch_bend(data, eff);
        }
    }

    /// Jumps a MIDI sound to `chunk:measure:beat:tick`. With `sustain`, notes ringing at the jump point carry over.
    #[doc(alias = "imuse_midi_jump")]
    #[allow(clippy::too_many_arguments)]
    pub fn midi_jump(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        chunk: i32,
        measure: i32,
        beat: i32,
        tick: i32,
        sustain: bool,
    ) -> Result<(), ImuseError> {
        status(self.players_jump(host, sound, chunk, measure, beat, tick, i32::from(sustain)))
    }

    /// Moves a MIDI sound to `chunk:measure:beat:tick`, replaying controller state silently.
    #[doc(alias = "imuse_midi_scan")]
    pub fn midi_scan(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        chunk: i32,
        measure: i32,
        beat: i32,
        tick: i32,
    ) -> Result<(), ImuseError> {
        status(self.players_scan(host, sound, chunk, measure, beat, tick))
    }

    /// Shares physical parts between two live sounds for a seamless crossfade.
    #[doc(alias = "imuse_share_parts")]
    pub fn share_parts(&mut self, sound1: SoundId, sound2: SoundId) -> Result<(), ImuseError> {
        status(self.players_share_parts(sound1, sound2))
    }
}
