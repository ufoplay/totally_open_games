//! Port of `wave.c`, `internal/wave.h`: the wave facade and its re-entry lock. TRACKS owns the pause state.

use super::Imuse;
use super::commands::{ImuseHost, SoundId};

#[doc(alias = "ImWaveModuleState")]
#[derive(Debug, Clone, Default)]
pub(crate) struct WaveModuleState {
    /// Re-entry counter; a non-zero count skips the update.
    pub wv_slicing_halted: i32,
}

impl Imuse {
    #[doc(alias = "ImWave_Init")]
    pub(crate) fn wave_init(&mut self) -> i32 {
        if self.tracks_init() != 0 {
            return -1;
        }
        self.wave.wv_slicing_halted = 0;
        0
    }

    #[doc(alias = "ImWave_Deinit")]
    pub(crate) fn wave_deinit(&mut self) -> i32 {
        self.tracks_deinit()
    }

    #[doc(alias = "ImWave_Lock")]
    pub(crate) fn wave_lock(&mut self) {
        self.wave.wv_slicing_halted += 1;
    }

    #[doc(alias = "ImWave_Unlock")]
    pub(crate) fn wave_unlock(&mut self) {
        if self.wave.wv_slicing_halted != 0 {
            self.wave.wv_slicing_halted -= 1;
        }
    }

    #[doc(alias = "ImWave_Update")]
    pub(crate) fn wave_update(&mut self, host: &mut dyn ImuseHost) {
        if self.wave.wv_slicing_halted == 0 {
            self.tracks_update(host);
        }
    }

    #[doc(alias = "ImWave_Pause")]
    pub(crate) fn wave_pause(&mut self) {
        self.wave_lock();
        self.tracks_pause();
        self.wave_unlock();
    }

    #[doc(alias = "ImWave_Resume")]
    pub(crate) fn wave_resume(&mut self) {
        self.wave_lock();
        self.tracks_resume();
        self.wave_unlock();
    }

    #[doc(alias = "ImWave_ApplyGroupVol")]
    pub(crate) fn wave_apply_group_vol(&mut self) {
        self.wave_lock();
        self.tracks_apply_group_vol();
        self.wave_unlock();
    }

    #[doc(alias = "ImWave_StartSound")]
    pub(crate) fn wave_start_sound(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        priority: i32,
        flags: u32,
    ) -> i32 {
        self.wave_lock();
        let result = self.tracks_start_sound(host, sound, priority, flags);
        self.wave_unlock();
        result
    }

    #[doc(alias = "ImWave_StopSoundById")]
    pub(crate) fn wave_stop_sound_by_id(&mut self, sound: SoundId) -> i32 {
        self.wave_lock();
        let result = self.tracks_stop_sound_by_id(sound);
        self.wave_unlock();
        result
    }

    #[doc(alias = "ImWave_StopAllSounds")]
    pub(crate) fn wave_stop_all_sounds(&mut self) -> i32 {
        self.wave_lock();
        let result = self.tracks_stop_all_sounds();
        self.wave_unlock();
        result
    }
}
