//! Port of `tracks.c`, `internal/tracks.h`: the wave tracks and the per-tick mix pump.
//!
//! Track `i` is paired with dispatch record `i`. Streaming is not ported: every track plays an in-memory VOC.

use super::Imuse;
use super::commands::{
    ImuseHost, PARAM_SOUND_DETUNE, PARAM_SOUND_FREQUENCY, PARAM_SOUND_GROUP, PARAM_SOUND_MAILBOX,
    PARAM_SOUND_MARKER, PARAM_SOUND_PAN, PARAM_SOUND_PEND_COUNT, PARAM_SOUND_PLAY_COUNT,
    PARAM_SOUND_PRIORITY, PARAM_SOUND_TRANSPOSE, PARAM_SOUND_TYPE, PARAM_SOUND_VOL,
    PARAM_WAVE_STREAM_FLAG, SoundId,
};
use super::groups::Group;
use super::timer::{SERVICE_PERIOD_US, XWING_TICK_US};
use super::utils::{Linked, Links, clamp, list_add_item, list_remove_item, wrap_semitones};

/// Tracks in the pool; `wave_mix_count` of them are used.
pub(crate) const TRACK_COUNT: usize = 16;

#[doc(alias = "ImWaveTrack")]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct WaveTrack {
    pub links: Links,
    /// 0 = free.
    pub sound: SoundId,
    pub marker: i32,
    pub group: i32,
    pub priority: i32,
    pub vol: i32,
    /// `(groupVol * (vol + 1)) >> 7`.
    pub eff_vol: i32,
    pub pan: i32,
    /// 8.8 fixed-point semitones.
    pub detune: i32,
    pub transpose: i32,
    /// `(transpose << 8) + detune`.
    pub detune_trans: i32,
    pub mailbox: i32,
    pub jump_hook: i32,
    /// `WaveStartFlags` bits.
    pub start_flags: u32,
    /// Explicit playback rate; 0 plays at the asset's rate.
    pub frequency_hz: i32,
}

impl Linked for WaveTrack {
    fn links(&self) -> &Links {
        &self.links
    }
    fn links_mut(&mut self) -> &mut Links {
        &mut self.links
    }
}

#[doc(alias("ImTracksState", "ImTracks_GetWaveMixCount"))]
#[derive(Debug, Clone, Default)]
pub(crate) struct TracksState {
    pub pool: [WaveTrack; TRACK_COUNT],
    pub list: Option<u8>,
    /// 1..=16, default 4.
    pub wave_mix_count: i32,
    /// 0 = running, 1..=3 = paused.
    pub pause_timer: i32,
}

impl Imuse {
    #[doc(alias = "ImTracks_Init")]
    pub(crate) fn tracks_init(&mut self) -> i32 {
        tracing::info!(target: "imuse", "TRACKS module...");
        let mut mix = self.commands.config.wave_mix_count;
        if mix <= 0 || mix > 16 {
            tracing::info!(target: "imuse", "TR: waveMixCount out of range, defaulting to 4...");
            mix = 4;
            self.commands.config.wave_mix_count = mix;
        }
        self.tracks.wave_mix_count = mix;
        self.tracks.pause_timer = 0;
        self.tracks.list = None;
        for track in &mut self.tracks.pool[..mix as usize] {
            track.links = Links::default();
            track.sound = SoundId(0);
        }
        self.wave_renderer_init();
        self.mixer_init();
        0
    }

    #[doc(alias = "ImTracks_Deinit")]
    pub(crate) fn tracks_deinit(&mut self) -> i32 {
        self.tracks_stop_all_sounds();
        self.wave_renderer_deinit();
        0
    }

    #[doc(alias = "ImTracks_Pause")]
    pub(crate) fn tracks_pause(&mut self) {
        self.tracks.pause_timer = 1;
    }

    #[doc(alias = "ImTracks_Resume")]
    pub(crate) fn tracks_resume(&mut self) {
        self.tracks.pause_timer = 0;
    }

    /// The live tracks, head first.
    fn live_tracks(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cur = self.tracks.list;
        while let Some(track) = cur {
            out.push(track);
            cur = self.tracks.pool[usize::from(track)].links.next;
        }
        out
    }

    #[doc(alias = "ImTracks_ApplyGroupVol")]
    pub(crate) fn tracks_apply_group_vol(&mut self) {
        for track in self.live_tracks() {
            let t = self.tracks.pool[usize::from(track)];
            let eff_vol = (self.get_group_volume_raw(t.group) * (t.vol + 1)) >> 7;
            self.tracks.pool[usize::from(track)].eff_vol = eff_vol;
        }
    }

    /// One service: mixes the halves that came due. While paused, one tick is skipped and silent halves follow.
    #[doc(alias = "ImTracks_Update")]
    pub(crate) fn tracks_update(&mut self, host: &mut dyn ImuseHost) {
        if self.tracks.pause_timer != 0 {
            self.tracks.pause_timer += 1;
            if self.tracks.pause_timer < 3 {
                return;
            }
            self.tracks.pause_timer = 3;
        }
        self.wave_lock();
        self.midi_lock();
        let service = if self.commands.config.xwing_timing {
            XWING_TICK_US
        } else {
            SERVICE_PERIOD_US
        };
        for _ in 0..self.wave_renderer_halves_due(service) {
            self.mixer_prepare_digital_output();
            if self.tracks.pause_timer == 0 {
                // The successor is read first: playing a frame may clear the track.
                let mut cur = self.tracks.list;
                while let Some(track) = cur {
                    let next = self.tracks.pool[usize::from(track)].links.next;
                    self.dispatch_play_sound_frame(host, track);
                    cur = next;
                }
            }
            self.mixer_audio_write_to_driver();
        }
        self.midi_unlock();
        self.wave_unlock();
    }

    /// Starts an in-memory VOC on a free (or preempted) track.
    #[doc(alias = "ImTracks_StartSound")]
    pub(crate) fn tracks_start_sound(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        priority: i32,
        start_flags: u32,
    ) -> i32 {
        let pri = clamp(priority, 0, 127);
        let Some(track) = self.tracks_get_free_track(pri) else {
            return -6;
        };
        let eff_vol = self.get_group_volume_raw(Group::Master as i32);
        let t = &mut self.tracks.pool[usize::from(track)];
        t.sound = sound;
        t.marker = 0;
        t.group = 0;
        t.priority = pri;
        t.vol = 127;
        t.eff_vol = eff_vol;
        t.pan = 64;
        t.detune = 0;
        t.transpose = 0;
        t.detune_trans = 0;
        t.mailbox = 0;
        t.jump_hook = 0;
        t.start_flags = start_flags;
        t.frequency_hz = 0;
        if self.dispatch_setup_sound(host, track) != 0 {
            self.tracks.pool[usize::from(track)].sound = SoundId(0);
            return -1;
        }
        self.wave_lock();
        let Self { tracks, .. } = self;
        list_add_item(&mut tracks.pool, &mut tracks.list, track);
        self.wave_unlock();
        0
    }

    /// Stops every track playing `sound`.
    #[doc(alias = "ImTracks_StopSoundById")]
    pub(crate) fn tracks_stop_sound_by_id(&mut self, sound: SoundId) -> i32 {
        let mut result = -1;
        let mut cur = self.tracks.list;
        while let Some(track) = cur {
            let next = self.tracks.pool[usize::from(track)].links.next;
            if self.tracks.pool[usize::from(track)].sound == sound {
                self.tracks_clear(track);
                result = 0;
            }
            cur = next;
        }
        result
    }

    #[doc(alias = "ImTracks_StopAllSounds")]
    pub(crate) fn tracks_stop_all_sounds(&mut self) -> i32 {
        self.wave_lock();
        let mut cur = self.tracks.list;
        while let Some(track) = cur {
            let next = self.tracks.pool[usize::from(track)].links.next;
            self.tracks_clear(track);
            cur = next;
        }
        self.wave_unlock();
        0
    }

    /// The smallest live id above `sound` (unsigned order), or 0.
    #[doc(alias("ImTracks_GetNextSound", "ImWave_GetNextSound"))]
    pub(crate) fn tracks_get_next_sound(&self, sound: SoundId) -> SoundId {
        let mut next = SoundId(0);
        let mut cur = self.tracks.list;
        while let Some(track) = cur {
            let t = &self.tracks.pool[usize::from(track)];
            if t.sound.unsigned() > sound.unsigned()
                && (next.0 == 0 || t.sound.unsigned() < next.unsigned())
            {
                next = t.sound;
            }
            cur = t.links.next;
        }
        next
    }

    /// Unlinks a track and cancels the fades and triggers of its sound.
    #[doc(alias("ImTracks_Clear", "ImDispatch_Release"))]
    pub(crate) fn tracks_clear(&mut self, track: u8) {
        {
            let Self { tracks, .. } = self;
            list_remove_item(&mut tracks.pool, &mut tracks.list, track);
        }
        let sound = self.tracks.pool[usize::from(track)].sound;
        self.fades_disable_for_sound(sound, -1);
        self.triggers_clear(sound, -1, None);
        self.tracks.pool[usize::from(track)].sound = SoundId(0);
    }

    fn track_of(&self, sound: SoundId) -> Option<u8> {
        let mut cur = self.tracks.list;
        while let Some(track) = cur {
            if self.tracks.pool[usize::from(track)].sound == sound {
                return Some(track);
            }
            cur = self.tracks.pool[usize::from(track)].links.next;
        }
        None
    }

    #[doc(alias("ImTracks_SetParam", "ImWave_SetParam"))]
    pub(crate) fn tracks_set_param(&mut self, sound: SoundId, param: i32, value: i32) -> i32 {
        let Some(track) = self.track_of(sound) else {
            return -4;
        };
        let index = usize::from(track);
        match param {
            PARAM_SOUND_GROUP => {
                if value as u32 >= 0x10 {
                    return -5;
                }
                let eff_vol =
                    (self.get_group_volume_raw(value) * (self.tracks.pool[index].vol + 1)) >> 7;
                let t = &mut self.tracks.pool[index];
                t.group = value;
                t.eff_vol = eff_vol;
                0
            }
            PARAM_SOUND_PRIORITY => {
                if value as u32 > 0x7F {
                    return -5;
                }
                self.tracks.pool[index].priority = value;
                0
            }
            PARAM_SOUND_VOL => {
                if value as u32 > 0x7F {
                    return -5;
                }
                let eff_vol =
                    (self.get_group_volume_raw(self.tracks.pool[index].group) * (value + 1)) >> 7;
                let t = &mut self.tracks.pool[index];
                t.vol = value;
                t.eff_vol = eff_vol;
                0
            }
            PARAM_SOUND_PAN => {
                if value as u32 > 0x7F {
                    return -5;
                }
                self.tracks.pool[index].pan = value;
                0
            }
            PARAM_SOUND_FREQUENCY => {
                if value < 0 {
                    return -5;
                }
                if value == 0 {
                    self.tracks.pool[index].frequency_hz = 0;
                    let d = &mut self.dispatch.pool[index];
                    d.resample_step = 0;
                    d.resample_phase = 0;
                    return 0;
                }
                let mix_rate = self.wave_renderer.wave_sample_rate as u64;
                let step = ((value as u64) << 16) / mix_rate;
                if step == 0 || step > u64::from(u32::MAX) {
                    return -5;
                }
                self.tracks.pool[index].frequency_hz = value;
                self.dispatch.pool[index].resample_step = step as u32;
                0
            }
            PARAM_SOUND_DETUNE => {
                if !(-9216..=9216).contains(&value) {
                    return -5;
                }
                let t = &mut self.tracks.pool[index];
                t.detune = value;
                t.detune_trans = (t.transpose << 8) + value;
                0
            }
            PARAM_SOUND_TRANSPOSE => {
                if !(-12..=12).contains(&value) {
                    return -5;
                }
                let t = &mut self.tracks.pool[index];
                t.transpose = if value != 0 {
                    wrap_semitones(t.transpose + value, -12, 12)
                } else {
                    0
                };
                t.detune_trans = (t.transpose << 8) + t.detune;
                0
            }
            PARAM_SOUND_MAILBOX => {
                self.tracks.pool[index].mailbox = value;
                0
            }
            _ => {
                tracing::info!(target: "imuse", "ERR: TrSetParam() couldn't set param {param}...");
                -5
            }
        }
    }

    #[doc(alias("ImTracks_GetParam", "ImWave_GetParam"))]
    pub(crate) fn tracks_get_param(&self, sound: SoundId, param: i32) -> i32 {
        let mut play_count = 0;
        let mut cur = self.tracks.list;
        while let Some(track) = cur {
            let t = &self.tracks.pool[usize::from(track)];
            cur = t.links.next;
            if t.sound != sound {
                continue;
            }
            return match param {
                PARAM_SOUND_TYPE | PARAM_SOUND_PEND_COUNT => -1,
                PARAM_SOUND_PLAY_COUNT => {
                    play_count += 1;
                    continue;
                }
                PARAM_SOUND_MARKER => t.marker,
                PARAM_SOUND_GROUP => t.group,
                PARAM_SOUND_PRIORITY => t.priority,
                PARAM_SOUND_VOL => t.vol,
                PARAM_SOUND_PAN => t.pan,
                PARAM_SOUND_FREQUENCY => {
                    if t.frequency_hz != 0 {
                        t.frequency_hz
                    } else {
                        self.dispatch.pool[usize::from(track)].source_rate_hz as i32
                    }
                }
                PARAM_SOUND_DETUNE => t.detune,
                PARAM_SOUND_TRANSPOSE => t.transpose,
                PARAM_SOUND_MAILBOX => t.mailbox,
                PARAM_WAVE_STREAM_FLAG => 0,
                _ => -5,
            };
        }
        if param == PARAM_SOUND_PLAY_COUNT {
            play_count
        } else {
            -4
        }
    }

    /// A free track, or the lowest-priority live one (ties to the most recent) if `priority` matches or beats it.
    #[doc(alias = "ImTracks_GetFreeTrack")]
    pub(crate) fn tracks_get_free_track(&mut self, priority: i32) -> Option<u8> {
        let count = self.tracks.wave_mix_count as usize;
        if let Some(free) = (0..count).find(|&i| self.tracks.pool[i].sound.0 == 0) {
            return Some(free as u8);
        }
        tracing::trace!(target: "imuse", "ERR: no spare tracks, will try to preempt...");
        let mut lowest = 127;
        let mut victim = None;
        let mut cur = self.tracks.list;
        while let Some(track) = cur {
            let t = &self.tracks.pool[usize::from(track)];
            if t.priority <= lowest {
                lowest = t.priority;
                victim = Some(track);
            }
            cur = t.links.next;
        }
        let victim = victim?;
        if priority < lowest {
            return None;
        }
        self.tracks_clear(victim);
        Some(victim)
    }
}
