//! Port of `midi.c`, `internal/midi.h`: the MIDI facade, its re-entry lock and the per-service update.

use super::Imuse;
use super::commands::{ImuseHost, SoundId};
use super::midi_backend::MidiOut;

/// Debug overlay cadence (every 76th service). The overlay is not ported; the counter keeps the cadence.
#[doc(alias = "IM_MIDI_DEBUG_REFRESH_TICKS")]
const DEBUG_REFRESH_TICKS: i32 = 76;

#[doc(alias = "ImMidiState")]
#[derive(Debug)]
pub(crate) struct MidiState {
    /// Re-entry counter, not a mutex: a non-zero count skips the update.
    pub lock: i32,
    pub paused: bool,
    pub debug_refresh_counter: i32,
    pub backend: MidiOut,
}

impl Imuse {
    /// SLOTS, then SUSTAIN, then PLAYERS.
    #[doc(alias = "ImMidi_Init")]
    pub(crate) fn midi_init(&mut self) {
        tracing::info!(target: "imuse", "MIDI engine...");
        self.slots_init();
        self.sustain_init();
        self.players_init();
        self.midi.paused = false;
        self.midi.lock = 0;
    }

    /// Fences the update, then tears down.
    #[doc(alias = "ImMidi_Deinit")]
    pub(crate) fn midi_deinit(&mut self) -> i32 {
        self.midi.paused = true;
        self.midi.lock = 1;
        let mut err = 0;
        err |= self.players_deinit();
        err |= self.sustain_deinit();
        err
    }

    #[doc(alias = "ImMidi_Lock")]
    pub(crate) fn midi_lock(&mut self) {
        self.midi.lock += 1;
    }

    #[doc(alias = "ImMidi_Unlock")]
    pub(crate) fn midi_unlock(&mut self) {
        if self.midi.lock != 0 {
            self.midi.lock -= 1;
        }
    }

    /// One service: advance the players and the sustain bridge, then the AdLib driver. The engine locks itself
    /// so a callback cannot re-enter the update.
    #[doc(alias("ImMidi_Update", "ImPlayers_Debug", "ImSlots_Debug"))]
    pub(crate) fn midi_update(&mut self, host: &mut dyn ImuseHost) {
        if self.midi.lock != 0 {
            return;
        }
        self.midi_lock();
        self.midi.debug_refresh_counter += 1;
        if self.midi.debug_refresh_counter > DEBUG_REFRESH_TICKS - 1 {
            self.midi.debug_refresh_counter = 0;
        }
        if !self.midi.paused {
            self.players_update(host);
            self.sustain_update();
            self.midi.backend.adlib_service();
        }
        self.midi_unlock();
    }

    #[doc(alias = "ImMidi_Pause")]
    pub(crate) fn midi_pause(&mut self) {
        self.midi.paused = true;
    }

    #[doc(alias = "ImMidi_Resume")]
    pub(crate) fn midi_resume(&mut self) {
        self.midi.paused = false;
    }

    #[doc(alias = "ImMidi_StartSound")]
    pub(crate) fn midi_start_sound(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        priority: i32,
    ) -> i32 {
        self.players_start_sound(host, sound, priority)
    }

    #[doc(alias = "ImMidi_StopSound")]
    pub(crate) fn midi_stop_sound(&mut self, sound: SoundId) -> i32 {
        self.players_stop_sound(sound)
    }

    #[doc(alias = "ImMidi_StopAllSounds")]
    pub(crate) fn midi_stop_all_sounds(&mut self) -> i32 {
        self.players_stop_all_sounds()
    }

    #[doc(alias = "ImMidi_ApplyGroupVol")]
    pub(crate) fn midi_apply_group_vol(&mut self) {
        self.players_apply_group_vol();
    }
}
