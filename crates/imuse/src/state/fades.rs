//! Port of `fades.c`, `internal/fades.h`: 16 Bresenham parameter ramps stepped at 60 Hz.

use super::Imuse;
use super::commands::{
    PARAM_KIND_MASK, PARAM_MIDI_PART_TRIM, PARAM_MIDI_SPEED, PARAM_SOUND_DETUNE, PARAM_SOUND_PAN,
    PARAM_SOUND_PRIORITY, PARAM_SOUND_VOL, SoundId,
};

#[doc(alias = "IM_NUM_FADERS")]
pub(crate) const FADER_COUNT: usize = 16;

/// Pushes to the sound are throttled to every sixth tick; the last tick always pushes.
#[doc(alias = "IM_FADE_PUSH_EVERY_N_TICKS")]
const PUSH_EVERY_N_TICKS: i32 = 6;

#[doc(alias = "ImSoundFader")]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SoundFader {
    pub active: bool,
    pub sound: SoundId,
    pub param: i32,
    /// Integer part of the live value.
    pub current_val: i32,
    /// Ticks remaining.
    pub counter: i32,
    pub length: i32,
    /// `(target - start) / length`.
    pub slope: i32,
    /// `|target - start| % length`.
    pub slope_mod: i32,
    pub mod_ovflo_counter: i32,
    pub step_dir: i32,
}

#[doc(alias = "ImFadesState")]
#[derive(Debug, Clone, Default)]
pub(crate) struct FadesState {
    pub pool: [SoundFader; FADER_COUNT],
    /// Self-healing "any fader live" flag.
    pub on: bool,
}

/// The parameters that can ramp.
fn fade_param_is_valid(param: i32) -> bool {
    matches!(
        param,
        PARAM_SOUND_PRIORITY
            | PARAM_SOUND_VOL
            | PARAM_SOUND_PAN
            | PARAM_SOUND_DETUNE
            | PARAM_MIDI_SPEED
    ) || param & PARAM_KIND_MASK == PARAM_MIDI_PART_TRIM
}

impl Imuse {
    #[doc(alias = "ImFades_Init")]
    pub(crate) fn fades_init(&mut self) {
        tracing::info!(target: "imuse", "FADES module...");
        self.fades_deinit();
    }

    #[doc(alias = "ImFades_Deinit")]
    pub(crate) fn fades_deinit(&mut self) -> i32 {
        for fader in &mut self.fades.pool {
            fader.active = false;
        }
        self.fades.on = false;
        0
    }

    /// Ramps `param` of `sound` to `value` over `time` 60 Hz ticks. `time == 0` applies it at once, and fading
    /// the volume to 0 at once stops the sound.
    #[doc(alias = "ImFades_Param")]
    pub(crate) fn fades_param(&mut self, sound: SoundId, param: i32, value: i32, time: i32) -> i32 {
        if sound.0 == 0 || time < 0 || !fade_param_is_valid(param) {
            return -5;
        }
        self.fades_disable_for_sound(sound, param);
        if time == 0 {
            if param == PARAM_SOUND_VOL && value == 0 {
                return self.stop_sound_raw(sound);
            }
            return self.set_param_raw(sound, param, value);
        }
        let Some(index) = self.fades.pool.iter().position(|fader| !fader.active) else {
            tracing::info!(target: "imuse", "ERROR: fd unable to alloc fade...");
            return -6;
        };
        let current_val = self.get_param_raw(sound, param);
        let mut delta = value - current_val;
        let slot = &mut self.fades.pool[index];
        slot.sound = sound;
        slot.param = param;
        slot.current_val = current_val;
        slot.length = time;
        slot.counter = time;
        slot.slope = delta / time;
        slot.step_dir = if delta >= 0 { 1 } else { -1 };
        delta = delta.abs();
        slot.slope_mod = delta % time;
        slot.mod_ovflo_counter = 0;
        slot.active = true;
        self.fades.on = true;
        0
    }

    /// Stops the ramps of `sound` on `param` (-1: every parameter). The value stays where the ramp left it.
    #[doc(alias = "ImFades_DisableForSound")]
    pub(crate) fn fades_disable_for_sound(&mut self, sound: SoundId, param: i32) {
        for fader in &mut self.fades.pool {
            if fader.active && fader.sound == sound && (param == fader.param || param == -1) {
                fader.active = false;
            }
        }
    }

    fn fade_push_current(&mut self, index: usize) {
        let SoundFader {
            sound,
            param,
            current_val,
            ..
        } = self.fades.pool[index];
        if param == PARAM_SOUND_VOL && current_val == 0 {
            self.stop_sound_raw(sound);
        } else {
            self.set_param_raw(sound, param, current_val);
        }
    }

    #[doc(alias = "ImFades_Update")]
    pub(crate) fn fades_update(&mut self) {
        if !self.fades.on {
            return;
        }
        self.fades.on = false;
        for index in 0..FADER_COUNT {
            let slot = &mut self.fades.pool[index];
            if !slot.active {
                continue;
            }
            self.fades.on = true;
            slot.counter -= 1;
            if slot.counter == 0 {
                slot.active = false;
            }
            let mut new_val = slot.current_val + slot.slope;
            let mod_accum = (slot.slope_mod + slot.mod_ovflo_counter) as u32;
            if mod_accum >= slot.length as u32 {
                slot.mod_ovflo_counter = (mod_accum - slot.length as u32) as i32;
                new_val += slot.step_dir;
            } else {
                slot.mod_ovflo_counter = mod_accum as i32;
            }
            if new_val == slot.current_val {
                continue;
            }
            slot.current_val = new_val;
            if slot.counter % PUSH_EVERY_N_TICKS == 0 {
                self.fade_push_current(index);
            }
        }
    }

    /// Ramps a parameter to `target` over `time` 60 Hz ticks.
    #[doc(alias = "imuse_fade_param")]
    pub fn fade_param(
        &mut self,
        sound: SoundId,
        param: super::commands::Param,
        target: i32,
        time: i32,
    ) -> Result<(), super::commands::ImuseError> {
        super::commands::status(self.fades_param(sound, param.raw(), target, time))
    }
}
