//! Port of `commands.c`, `include/imuse/commands.h`, `internal/commands.h`: the public types, the init cascade,
//! the host-driven tick and the per-sound control surface.

use std::sync::Arc;

use super::Imuse;
use super::files::{self, SoundType};
use super::groups::Group;
use super::timer::{LOGICAL_TICK_US, SERVICE_PERIOD_US, XWING_TICK_US};
use super::triggers::{CommandMatch, DEFER_COUNT, TRIGGER_COUNT};

/// A sound identity chosen by the host. 0 means "no sound" and -1 is the wildcard of trigger and fade queries.
///
/// Upstream types it `intptr_t` so a host can pass pointers. Enumeration orders ids as unsigned values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SoundId(pub isize);

impl SoundId {
    pub const NONE: Self = Self(0);
    pub const WILDCARD: Self = Self(-1);

    pub(crate) fn unsigned(self) -> usize {
        self.0 as usize
    }
}

/// A game callback carried by a trigger or a deferred command, dispatched through [`ImuseHost`].
///
/// DEVIATION: upstream stores a function pointer in the opcode field (any value ≥ 30); the game names its
/// callbacks with ids instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallbackId(pub u32);

/// Command opcodes, numbered like the DOS engine's `imuseFuncPtrs[]` table.
#[doc(alias = "ImuseOpcode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Opcode {
    #[doc(alias = "IMUSE_CMD_INIT")]
    Init = 0,
    #[doc(alias = "IMUSE_CMD_TERMINATE")]
    Terminate = 1,
    #[doc(alias = "IMUSE_CMD_PRINTF")]
    Printf = 2,
    #[doc(alias = "IMUSE_CMD_PAUSE")]
    Pause = 3,
    #[doc(alias = "IMUSE_CMD_RESUME")]
    Resume = 4,
    #[doc(alias = "IMUSE_CMD_SAVE")]
    Save = 5,
    #[doc(alias = "IMUSE_CMD_RESTORE")]
    Restore = 6,
    #[doc(alias = "IMUSE_CMD_SET_GROUP_VOL")]
    SetGroupVol = 7,
    #[doc(alias = "IMUSE_CMD_START_SOUND")]
    StartSound = 8,
    #[doc(alias = "IMUSE_CMD_STOP_SOUND")]
    StopSound = 9,
    #[doc(alias = "IMUSE_CMD_STOP_ALL_SOUNDS")]
    StopAllSounds = 10,
    #[doc(alias = "IMUSE_CMD_GET_NEXT_SOUND")]
    GetNextSound = 11,
    #[doc(alias = "IMUSE_CMD_SET_PARAM")]
    SetParam = 12,
    #[doc(alias = "IMUSE_CMD_GET_PARAM")]
    GetParam = 13,
    #[doc(alias = "IMUSE_CMD_FADE_PARAM")]
    FadeParam = 14,
    #[doc(alias = "IMUSE_CMD_SET_HOOK")]
    SetHook = 15,
    #[doc(alias = "IMUSE_CMD_GET_HOOK")]
    GetHook = 16,
    #[doc(alias = "IMUSE_CMD_SET_TRIGGER")]
    SetTrigger = 17,
    #[doc(alias = "IMUSE_CMD_CHECK_TRIGGER")]
    CheckTrigger = 18,
    #[doc(alias = "IMUSE_CMD_CLEAR_TRIGGER")]
    ClearTrigger = 19,
    #[doc(alias = "IMUSE_CMD_DEFER_CMD")]
    DeferCmd = 20,
    #[doc(alias = "IMUSE_CMD_JUMP_MIDI")]
    JumpMidi = 21,
    #[doc(alias = "IMUSE_CMD_SCAN_MIDI")]
    ScanMidi = 22,
    #[doc(alias = "IMUSE_CMD_SEND_MIDI_MSG")]
    SendMidiMsg = 23,
    #[doc(alias = "IMUSE_CMD_SHARE_PARTS")]
    ShareParts = 24,
    #[doc(alias = "IMUSE_CMD_START_STREAM")]
    StartStream = 25,
    #[doc(alias = "IMUSE_CMD_SWITCH_STREAM")]
    SwitchStream = 26,
    #[doc(alias = "IMUSE_CMD_PROCESS_STREAM")]
    ProcessStream = 27,
    #[doc(alias = "IMUSE_CMD_QUERY_STREAM")]
    QueryStream = 28,
    #[doc(alias = "IMUSE_CMD_PANIC_INTERNAL")]
    PanicInternal = 29,
}

/// What a trigger or deferred command runs: an engine opcode or a game callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Op(Opcode),
    Callback(CallbackId),
}

/// A trigger or deferred command with its ten arguments, passed verbatim to the handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImuseCmd {
    pub command: Command,
    pub args: [isize; 10],
}

impl ImuseCmd {
    pub fn new(command: Command, args: &[isize]) -> Self {
        let mut all = [0; 10];
        all[..args.len()].copy_from_slice(args);
        Self { command, args: all }
    }
}

/// Sound and MIDI parameters. Per-part parameters carry the channel in the low byte of the raw value.
#[doc(alias = "ImuseParam")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Param {
    #[doc(alias = "IMUSE_PARAM_SOUND_TYPE")]
    SoundType,
    #[doc(alias = "IMUSE_PARAM_SOUND_PLAY_COUNT")]
    SoundPlayCount,
    #[doc(alias = "IMUSE_PARAM_SOUND_PEND_COUNT")]
    SoundPendCount,
    #[doc(alias = "IMUSE_PARAM_SOUND_MARKER")]
    SoundMarker,
    #[doc(alias = "IMUSE_PARAM_SOUND_GROUP")]
    SoundGroup,
    #[doc(alias = "IMUSE_PARAM_SOUND_PRIORITY")]
    SoundPriority,
    #[doc(alias = "IMUSE_PARAM_SOUND_VOL")]
    SoundVol,
    #[doc(alias = "IMUSE_PARAM_SOUND_PAN")]
    SoundPan,
    #[doc(alias = "IMUSE_PARAM_SOUND_FREQUENCY")]
    SoundFrequency,
    #[doc(alias = "IMUSE_PARAM_SOUND_DETUNE")]
    SoundDetune,
    #[doc(alias = "IMUSE_PARAM_SOUND_TRANSPOSE")]
    SoundTranspose,
    #[doc(alias = "IMUSE_PARAM_SOUND_MAILBOX")]
    SoundMailbox,
    #[doc(alias = "IMUSE_PARAM_MIDI_CHUNK")]
    MidiChunk,
    #[doc(alias = "IMUSE_PARAM_MIDI_MEASURE")]
    MidiMeasure,
    #[doc(alias = "IMUSE_PARAM_MIDI_BEAT")]
    MidiBeat,
    #[doc(alias = "IMUSE_PARAM_MIDI_TICK")]
    MidiTick,
    #[doc(alias = "IMUSE_PARAM_MIDI_SPEED")]
    MidiSpeed,
    #[doc(alias = "IMUSE_PARAM_MIDI_PART_TRIM")]
    MidiPartTrim(u8),
    #[doc(alias = "IMUSE_PARAM_MIDI_PART_STATUS")]
    MidiPartStatus(u8),
    #[doc(alias = "IMUSE_PARAM_MIDI_PART_PRIORITY")]
    MidiPartPriority(u8),
    #[doc(alias = "IMUSE_PARAM_MIDI_PART_NOTE_REQ")]
    MidiPartNoteReq(u8),
    #[doc(alias = "IMUSE_PARAM_MIDI_PART_VOL")]
    MidiPartVol(u8),
    #[doc(alias = "IMUSE_PARAM_MIDI_PART_PAN")]
    MidiPartPan(u8),
    #[doc(alias = "IMUSE_PARAM_MIDI_PART_PGM")]
    MidiPartPgm(u8),
    #[doc(alias = "IMUSE_PARAM_WAVE_STREAM_FLAG")]
    WaveStreamFlag,
}

impl Param {
    /// The upstream integer encoding.
    pub fn raw(self) -> i32 {
        match self {
            Param::SoundType => PARAM_SOUND_TYPE,
            Param::SoundPlayCount => PARAM_SOUND_PLAY_COUNT,
            Param::SoundPendCount => PARAM_SOUND_PEND_COUNT,
            Param::SoundMarker => PARAM_SOUND_MARKER,
            Param::SoundGroup => PARAM_SOUND_GROUP,
            Param::SoundPriority => PARAM_SOUND_PRIORITY,
            Param::SoundVol => PARAM_SOUND_VOL,
            Param::SoundPan => PARAM_SOUND_PAN,
            Param::SoundFrequency => PARAM_SOUND_FREQUENCY,
            Param::SoundDetune => PARAM_SOUND_DETUNE,
            Param::SoundTranspose => PARAM_SOUND_TRANSPOSE,
            Param::SoundMailbox => PARAM_SOUND_MAILBOX,
            Param::MidiChunk => PARAM_MIDI_CHUNK,
            Param::MidiMeasure => PARAM_MIDI_MEASURE,
            Param::MidiBeat => PARAM_MIDI_BEAT,
            Param::MidiTick => PARAM_MIDI_TICK,
            Param::MidiSpeed => PARAM_MIDI_SPEED,
            Param::MidiPartTrim(channel) => PARAM_MIDI_PART_TRIM + i32::from(channel),
            Param::MidiPartStatus(channel) => PARAM_MIDI_PART_STATUS + i32::from(channel),
            Param::MidiPartPriority(channel) => PARAM_MIDI_PART_PRIORITY + i32::from(channel),
            Param::MidiPartNoteReq(channel) => PARAM_MIDI_PART_NOTE_REQ + i32::from(channel),
            Param::MidiPartVol(channel) => PARAM_MIDI_PART_VOL + i32::from(channel),
            Param::MidiPartPan(channel) => PARAM_MIDI_PART_PAN + i32::from(channel),
            Param::MidiPartPgm(channel) => PARAM_MIDI_PART_PGM + i32::from(channel),
            Param::WaveStreamFlag => PARAM_WAVE_STREAM_FLAG,
        }
    }
}

pub(crate) const PARAM_SOUND_TYPE: i32 = 0x000;
pub(crate) const PARAM_SOUND_PLAY_COUNT: i32 = 0x100;
pub(crate) const PARAM_SOUND_PEND_COUNT: i32 = 0x200;
pub(crate) const PARAM_SOUND_MARKER: i32 = 0x300;
pub(crate) const PARAM_SOUND_GROUP: i32 = 0x400;
pub(crate) const PARAM_SOUND_PRIORITY: i32 = 0x500;
pub(crate) const PARAM_SOUND_VOL: i32 = 0x600;
pub(crate) const PARAM_SOUND_PAN: i32 = 0x700;
pub(crate) const PARAM_SOUND_FREQUENCY: i32 = 0x777;
pub(crate) const PARAM_SOUND_DETUNE: i32 = 0x800;
pub(crate) const PARAM_SOUND_TRANSPOSE: i32 = 0x900;
pub(crate) const PARAM_SOUND_MAILBOX: i32 = 0xA00;
pub(crate) const PARAM_MIDI_CHUNK: i32 = 0xB00;
pub(crate) const PARAM_MIDI_MEASURE: i32 = 0xC00;
pub(crate) const PARAM_MIDI_BEAT: i32 = 0xD00;
pub(crate) const PARAM_MIDI_TICK: i32 = 0xE00;
pub(crate) const PARAM_MIDI_SPEED: i32 = 0xF00;
pub(crate) const PARAM_MIDI_PART_TRIM: i32 = 0x1100;
pub(crate) const PARAM_MIDI_PART_STATUS: i32 = 0x1200;
pub(crate) const PARAM_MIDI_PART_PRIORITY: i32 = 0x1300;
pub(crate) const PARAM_MIDI_PART_NOTE_REQ: i32 = 0x1400;
pub(crate) const PARAM_MIDI_PART_VOL: i32 = 0x1500;
pub(crate) const PARAM_MIDI_PART_PAN: i32 = 0x1600;
pub(crate) const PARAM_MIDI_PART_PGM: i32 = 0x1700;
pub(crate) const PARAM_WAVE_STREAM_FLAG: i32 = 0x1800;
/// Isolates the parameter kind from the channel index of the per-part range.
#[doc(alias = "IMUSE_PARAM_KIND_MASK")]
pub(crate) const PARAM_KIND_MASK: i32 = 0xFF00;

bitflags::bitflags! {
    /// Wave-specific start behaviour.
    #[doc(alias = "ImuseWaveStartFlags")]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct WaveStartFlags: u32 {
        #[doc(alias = "IMUSE_WAVE_START_LOOP")]
        const LOOP = 1 << 0;
        /// Port extension for loaders that expose only the first VOC audio block.
        #[doc(alias = "IMUSE_WAVE_START_LOOP_FIRST_BLOCK")]
        const LOOP_FIRST_BLOCK = 1 << 1;
    }
}

/// Reconstruction filter applied to digital wave output.
#[doc(alias = "ImuseWaveOutputFilter")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaveOutputFilter {
    #[doc(alias = "IMUSE_WAVE_OUTPUT_FILTER_NONE")]
    #[default]
    None,
    #[doc(alias = "IMUSE_WAVE_OUTPUT_FILTER_SB16")]
    Sb16,
}

/// Engine settings, read once at creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImuseConfig {
    /// Rate of the mixed output; also the MIDI synthesis rate. 0 means 44100. Should be an integer multiple of the
    /// wave rate.
    pub output_sample_rate: i32,
    /// `false` for the ~11 kHz wave engine, `true` for ~22 kHz.
    pub wave_speed_high: bool,
    /// 1..=16; anything else becomes 4.
    pub wave_mix_count: i32,
    /// DOS93 PIT timing: services of 3433 µs instead of 8000 µs, and eight MIDI players instead of two.
    pub xwing_timing: bool,
    pub wave_output_filter: WaveOutputFilter,
}

impl Default for ImuseConfig {
    fn default() -> Self {
        Self {
            output_sample_rate: 44100,
            wave_speed_high: true,
            wave_mix_count: 4,
            xwing_timing: false,
            wave_output_filter: WaveOutputFilter::None,
        }
    }
}

/// Upstream's negative return codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImuseError {
    #[error("general failure")]
    General,
    #[error("hook unsupported")]
    HookUnsupported,
    #[error("not found")]
    NotFound,
    #[error("no data")]
    NoData,
    #[error("bad argument")]
    BadArgument,
    #[error("table full")]
    TableFull,
    #[error("error code {0}")]
    Other(i32),
}

impl ImuseError {
    pub(crate) fn from_code(code: i32) -> Result<i32, Self> {
        match code {
            0.. => Ok(code),
            -1 => Err(Self::General),
            -2 => Err(Self::HookUnsupported),
            -3 => Err(Self::NotFound),
            -4 => Err(Self::NoData),
            -5 => Err(Self::BadArgument),
            -6 => Err(Self::TableFull),
            _ => Err(Self::Other(code)),
        }
    }
}

/// Converts an upstream status code.
pub(crate) fn status(code: i32) -> Result<(), ImuseError> {
    ImuseError::from_code(code).map(|_| ())
}

/// What the engine needs from the game: sound bytes, name-keyed loading and the game callbacks.
///
/// The host is passed to every call that reads sound data or can fire a callback. A callback receives the engine
/// back, so it can issue commands from inside the tick, as upstream callbacks do.
pub trait ImuseHost {
    /// The bytes of a loaded sound, re-fetched every tick. `None` when it is not loaded.
    #[doc(alias("getSoundPtrFunc", "getSoundSizeFunc"))]
    fn sound(&mut self, sound: SoundId) -> Option<Arc<[u8]>>;

    /// Loads a sound by name for the file list (`ImuseLoadSoundFunc`).
    #[doc(alias = "ImuseLoadSoundFunc")]
    fn load_sound(&mut self, name: &str) -> Option<SoundId> {
        let _ = name;
        None
    }

    /// Releases a sound loaded by [`Self::load_sound`] (`ImuseUnloadSoundFunc`).
    #[doc(alias = "ImuseUnloadSoundFunc")]
    fn unload_sound(&mut self, sound: SoundId) {
        let _ = sound;
    }

    /// A trigger fired with a callback command. `marker` is the marker that fired it.
    ///
    /// DEVIATION: a "large marker" (SysEx sub-op 3) passes 128; upstream passes the truncated payload address.
    #[doc(alias = "ImTriggerCallback")]
    fn trigger_callback(
        &mut self,
        imuse: &mut Imuse,
        callback: CallbackId,
        marker: i32,
        args: &[isize; 10],
    ) {
        let _ = (imuse, callback, marker, args);
    }

    /// A deferred callback command came due.
    #[doc(alias = "ImDeferCallback")]
    fn defer_callback(&mut self, imuse: &mut Imuse, callback: CallbackId, args: &[isize; 10]) {
        let _ = (imuse, callback, args);
    }
}

/// Real-time tiers of the host-driven clock.
const TICK_60HZ_US: i32 = 16667;
const TICK_10HZ_US: i32 = 100_000;
/// One advance never runs more than this many service ticks.
const MAX_CATCHUP_TICKS: i32 = 8;

/// Module state of `commands.c`.
#[doc(alias = "ImCommandsState")]
#[derive(Debug, Clone)]
pub(crate) struct CommandsState {
    pub config: ImuseConfig,
    pub initialized: bool,
    pub paused: i32,
    /// 0..=128 multiplier of the music group while a voice plays; 47 matches the original.
    pub music_ducking_factor: i32,
    /// Real host time toward the next service.
    pub timer_core_accum: i32,
    /// Logical time.
    pub timer_60hz_accum: i32,
    /// Logical time.
    pub timer_10hz_accum: i32,
}

impl CommandsState {
    pub fn new(config: ImuseConfig) -> Self {
        Self {
            config,
            initialized: false,
            paused: 0,
            music_ducking_factor: 47,
            timer_core_accum: 0,
            timer_60hz_accum: 0,
            timer_10hz_accum: 0,
        }
    }
}

const MAGIC_MIDI: &[u8; 4] = b"MIDI";
const MAGIC_CREA: &[u8; 4] = b"Crea";

impl Imuse {
    /// Brings up the subsystems in the canonical order. The MIDI backend was opened by the caller.
    #[doc(alias(
        "ImCommands_Init",
        "ImTimer_Init",
        "ImTimer_Deinit",
        "ImFiles_Init",
        "ImFiles_Deinit"
    ))]
    pub(crate) fn commands_init(&mut self) -> i32 {
        if self.commands.initialized {
            tracing::info!(target: "imuse", "ERROR:system already initialized...");
            return -1;
        }
        tracing::info!(target: "imuse", "Initializing...COMMANDS module...");
        // Timer and files have nothing to initialize: the cadence is fixed and the host is passed per call.
        self.groups_init();
        self.fades_init();
        self.triggers_init();
        self.midi_init();
        if self.wave_init() != 0 {
            self.midi_deinit();
            self.triggers_deinit();
            self.fades_deinit();
            return -1;
        }
        self.commands.paused = 0;
        self.commands.initialized = true;
        tracing::info!(target: "imuse", "Initialization complete...");
        48
    }

    /// Tears down in reverse order and closes the MIDI backend last.
    #[doc(alias = "ImCommands_Terminate")]
    pub(crate) fn commands_terminate(&mut self) -> i32 {
        tracing::info!(target: "imuse", "Terminating...");
        let mut err = 0;
        err |= self.wave_deinit();
        err |= self.midi_deinit();
        err |= self.triggers_deinit();
        err |= self.fades_deinit();
        self.midi.backend.close();
        self.commands.initialized = false;
        self.commands.paused = 0;
        self.commands.timer_core_accum = 0;
        self.commands.timer_60hz_accum = 0;
        self.commands.timer_10hz_accum = 0;
        if err != 0 {
            tracing::info!(target: "imuse", "ERROR:termination error...");
        } else {
            tracing::info!(target: "imuse", "Termination complete...");
        }
        err
    }

    /// Drives the engine by `usec_elapsed` microseconds of host time.
    ///
    /// Service ticks (sequencer and wave pump) fire every 8000 µs of host time (3433 µs with X-Wing timing), each
    /// counting as 8060 µs (3433 µs) of logical time, which feeds the 60 Hz fade/trigger tier and the 10 Hz
    /// ducking tier. A stall is capped at eight service ticks; time beyond the cap is dropped.
    #[doc(alias = "imuse_advance")]
    pub fn advance(&mut self, host: &mut dyn ImuseHost, usec_elapsed: i32) {
        if !self.commands.initialized || usec_elapsed <= 0 {
            return;
        }
        let xwing = self.commands.config.xwing_timing;
        let service_period = if xwing {
            XWING_TICK_US
        } else {
            SERVICE_PERIOD_US
        };
        let logical_tick = if xwing {
            XWING_TICK_US
        } else {
            LOGICAL_TICK_US
        };
        let usec_elapsed = usec_elapsed.min(service_period * MAX_CATCHUP_TICKS);

        let mut logical_elapsed = 0;
        self.commands.timer_core_accum += usec_elapsed;
        while self.commands.timer_core_accum >= service_period {
            self.commands.timer_core_accum -= service_period;
            self.midi_update(host);
            self.wave_update(host);
            logical_elapsed += logical_tick;
        }

        if self.commands.paused != 0 || logical_elapsed == 0 {
            return;
        }

        self.commands.timer_60hz_accum += logical_elapsed;
        while self.commands.timer_60hz_accum >= TICK_60HZ_US {
            self.commands.timer_60hz_accum -= TICK_60HZ_US;
            self.fades_update();
            self.triggers_update(host);
        }

        // Voice-ducks-music ramp: fast duck (-18 per tick), slow release (+3 per tick).
        self.commands.timer_10hz_accum += logical_elapsed;
        while self.commands.timer_10hz_accum >= TICK_10HZ_US {
            self.commands.timer_10hz_accum -= TICK_10HZ_US;

            let mut target_vol = self.set_group_volume_raw(Group::Music as i32, -1) as u32;
            let mut id = self.next_sound(SoundId(0));
            while id.0 != 0 {
                if self.get_param_raw(id, PARAM_SOUND_GROUP) == Group::Voice as i32 {
                    target_vol = (self.commands.music_ducking_factor as u32 * target_vol) >> 7;
                    break;
                }
                id = self.next_sound(id);
            }

            let cur_vol = self.set_group_volume_raw(Group::Dipped as i32, -1) as u32;
            let new_vol = if cur_vol < target_vol {
                (cur_vol + 3).min(target_vol)
            } else if cur_vol > target_vol {
                let new_vol = cur_vol.wrapping_sub(18);
                if new_vol <= target_vol || new_vol > cur_vol {
                    target_vol
                } else {
                    new_vol
                }
            } else {
                continue;
            };
            self.set_group_volume_raw(Group::Dipped as i32, new_vol as i32);
        }
    }

    /// Sets the /128 factor that dipped music keeps while a voice plays: 0 mutes, 128 disables the reduction and
    /// 47 matches the original.
    #[doc(alias = "imuse_set_music_ducking_factor")]
    pub fn set_music_ducking_factor(&mut self, factor: i32) -> Result<(), ImuseError> {
        if !(0..=128).contains(&factor) {
            return Err(ImuseError::BadArgument);
        }
        self.commands.music_ducking_factor = factor;
        Ok(())
    }

    /// Pauses; paired calls nest. Returns the new depth.
    #[doc(alias = "imuse_pause")]
    pub fn pause(&mut self) -> i32 {
        if self.commands.paused == 0 {
            self.midi_pause();
            self.wave_pause();
        }
        self.commands.paused += 1;
        self.commands.paused
    }

    /// Undoes one [`Self::pause`]. Returns the remaining depth.
    #[doc(alias = "imuse_resume")]
    pub fn resume(&mut self) -> i32 {
        if self.commands.paused == 1 {
            self.midi_resume();
            self.wave_resume();
        }
        if self.commands.paused != 0 {
            self.commands.paused -= 1;
        }
        self.commands.paused
    }

    #[doc(alias = "start_wave")]
    fn start_wave_raw(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        priority: i32,
        flags: u32,
    ) -> i32 {
        let data = files::get_sound_ptr(host, sound);
        let valid = WaveStartFlags::all().bits();
        let loop_first_without_loop = flags & WaveStartFlags::LOOP_FIRST_BLOCK.bits() != 0
            && flags & WaveStartFlags::LOOP.bits() == 0;
        match data {
            Some(data)
                if data.starts_with(MAGIC_CREA)
                    && flags & !valid == 0
                    && !loop_first_without_loop =>
            {
                self.wave_start_sound(host, sound, priority, flags)
            }
            _ => -1,
        }
    }

    /// Starts an in-memory Creative VOC with wave-specific behaviour.
    #[doc(alias = "imuse_start_wave")]
    pub fn start_wave(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        priority: i32,
        flags: WaveStartFlags,
    ) -> Result<(), ImuseError> {
        status(self.start_wave_raw(host, sound, priority, flags.bits()))
    }

    /// Starts a sound, dispatching on its magic: `MIDI`, `GMD `, `ADL `, `ROL ` or `Crea`. `priority` is 0..=127.
    #[doc(alias = "imuse_start_sound")]
    pub fn start_sound(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        priority: i32,
    ) -> Result<(), ImuseError> {
        status(self.start_sound_raw(host, sound, priority))
    }

    pub(crate) fn start_sound_raw(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        priority: i32,
    ) -> i32 {
        let Some(data) = files::get_sound_ptr(host, sound) else {
            tracing::trace!(target: "imuse", "imuse_start_sound id={:#x} pri={priority} -> no-data", sound.0);
            tracing::error!(target: "imuse", "ERR: null sound addr in StartSound()...");
            return -1;
        };
        if [MAGIC_MIDI, b"GMD ", b"ADL ", b"ROL "]
            .iter()
            .any(|magic| data.starts_with(*magic))
        {
            tracing::trace!(target: "imuse", "imuse_start_sound id={:#x} pri={priority} kind=MIDI", sound.0);
            return self.midi_start_sound(host, sound, priority);
        }
        if data.starts_with(MAGIC_CREA) {
            tracing::trace!(target: "imuse", "imuse_start_sound id={:#x} pri={priority} kind=WAVE", sound.0);
            return self.start_wave_raw(host, sound, priority, 0);
        }
        let head: Vec<u8> = data.iter().take(4).copied().collect();
        tracing::warn!(target: "imuse", "imuse_start_sound id={:#x} pri={priority} -> bad-magic {head:02x?}", sound.0);
        -1
    }

    /// Before the host releases a sound's bytes: stops it and cancels the triggers, fades and queued commands
    /// that target it.
    #[doc(alias("imuse_forget_sound", "command_targets_sound"))]
    pub fn forget_sound(&mut self, sound: SoundId) {
        if sound.0 == 0 || sound.0 == -1 {
            return;
        }
        let _ = self.stop_sound(sound);
        self.fades_disable_for_sound(sound, -1);
        let targets = |command: Command, args: &[isize; 10]| {
            let Command::Op(opcode) = command else {
                return false;
            };
            opcode as u8 >= Opcode::StartSound as u8
                && (args[0] == sound.0 || (opcode == Opcode::ShareParts && args[1] == sound.0))
        };
        for index in 0..TRIGGER_COUNT {
            let trigger = &mut self.triggers.triggers[index];
            if trigger.sound == sound || targets(trigger.command, &trigger.args) {
                trigger.sound = SoundId(0);
            }
        }
        for index in 0..DEFER_COUNT {
            let defer = &mut self.triggers.defers[index];
            if targets(defer.command, &defer.args) {
                defer.counter = 0;
            }
        }
    }

    #[doc(alias = "imuse_stop_sound")]
    pub fn stop_sound(&mut self, sound: SoundId) -> Result<(), ImuseError> {
        status(self.stop_sound_raw(sound))
    }

    pub(crate) fn stop_sound_raw(&mut self, sound: SoundId) -> i32 {
        let kind = self.get_sound_type(sound);
        tracing::trace!(target: "imuse", "imuse_stop_sound id={:#x} kind={kind:?}", sound.0);
        match kind {
            SoundType::Midi => self.midi_stop_sound(sound),
            SoundType::Wave => self.wave_stop_sound_by_id(sound),
            SoundType::Invalid => -1,
        }
    }

    /// Hard stop: clears fades and triggers first so nothing restarts a sound.
    #[doc(alias = "imuse_stop_all_sounds")]
    pub fn stop_all_sounds(&mut self) -> Result<(), ImuseError> {
        status(self.stop_all_sounds_raw())
    }

    pub(crate) fn stop_all_sounds_raw(&mut self) -> i32 {
        let mut err = 0;
        err |= self.fades_deinit();
        err |= self.triggers_deinit();
        err |= self.midi_stop_all_sounds();
        err |= self.wave_stop_all_sounds();
        err
    }

    /// The live sound after `sound` in ascending (unsigned) id order; 0 ends the walk. Pass 0 to start.
    #[doc(alias = "imuse_next_sound")]
    pub fn next_sound(&self, sound: SoundId) -> SoundId {
        let midi = self.players_find_next_sound(sound);
        let wave = self.tracks_get_next_sound(sound);
        if midi.0 != 0 && (wave.0 == 0 || midi.unsigned() < wave.unsigned()) {
            midi
        } else {
            wave
        }
    }

    #[doc(alias = "imuse_set_param")]
    pub fn set_param(
        &mut self,
        sound: SoundId,
        param: Param,
        value: i32,
    ) -> Result<(), ImuseError> {
        status(self.set_param_raw(sound, param.raw(), value))
    }

    pub(crate) fn set_param_raw(&mut self, sound: SoundId, param: i32, value: i32) -> i32 {
        match self.get_sound_type(sound) {
            SoundType::Midi => self.players_set_param(sound, param, value),
            SoundType::Wave => self.tracks_set_param(sound, param, value),
            SoundType::Invalid => -1,
        }
    }

    /// The value of a parameter. Errors are negative codes, as upstream: some values (a marker of -1) overlap.
    #[doc(alias = "imuse_get_param")]
    pub fn get_param(&self, sound: SoundId, param: Param) -> i32 {
        self.get_param_raw(sound, param.raw())
    }

    pub(crate) fn get_param_raw(&self, sound: SoundId, param: i32) -> i32 {
        let kind = self.get_sound_type(sound);
        if param == PARAM_SOUND_TYPE {
            return kind as i32;
        }
        if param == PARAM_SOUND_PEND_COUNT {
            return self.triggers_get_pending_sound_count(sound);
        }
        match kind {
            SoundType::Midi => self.players_get_param(sound, param),
            SoundType::Wave => self.tracks_get_param(sound, param),
            SoundType::Invalid if param == PARAM_SOUND_PLAY_COUNT => 0,
            SoundType::Invalid => -1,
        }
    }

    #[doc(alias("imuse_set_hook", "ImWave_SetHook"))]
    pub fn set_hook(&mut self, sound: SoundId, hook: u32) {
        if self.get_sound_type(sound) == SoundType::Midi {
            self.players_set_hook(sound, hook);
        }
        // Wave tracks have no hooks.
    }

    #[doc(alias("imuse_get_hook", "ImWave_GetHook"))]
    pub fn get_hook(&self, sound: SoundId) -> i32 {
        match self.get_sound_type(sound) {
            SoundType::Midi => self.players_get_hook(sound),
            SoundType::Wave => -2,
            SoundType::Invalid => -1,
        }
    }

    /// Replays an opcode stored by a trigger or deferred command. `SetTrigger` and `DeferCmd` are refused because
    /// they would mutate the table being replayed.
    ///
    /// DEVIATION: the streaming opcodes return -1; streaming is not ported (no game starts a stream).
    #[doc(alias = "ImCommands_ExecOpcode")]
    pub(crate) fn exec_opcode(
        &mut self,
        host: &mut dyn ImuseHost,
        opcode: Opcode,
        args: &[isize; 10],
    ) -> i32 {
        let sound = SoundId(args[0]);
        let int = |index: usize| args[index] as i32;
        match opcode {
            Opcode::Init => -1,
            Opcode::Terminate => self.commands_terminate(),
            Opcode::Printf => 0,
            Opcode::Pause => self.pause(),
            Opcode::Resume => self.resume(),
            Opcode::Save | Opcode::Restore => -1,
            Opcode::SetGroupVol => self.set_group_volume_raw(int(0), int(1)),
            Opcode::StartSound => self.start_sound_raw(host, sound, int(1)),
            Opcode::StopSound => self.stop_sound_raw(sound),
            Opcode::StopAllSounds => self.stop_all_sounds_raw(),
            Opcode::GetNextSound => self.next_sound(sound).0 as i32,
            Opcode::SetParam => self.set_param_raw(sound, int(1), int(2)),
            Opcode::GetParam => self.get_param_raw(sound, int(1)),
            Opcode::FadeParam => self.fades_param(sound, int(1), int(2), int(3)),
            Opcode::SetHook => {
                self.set_hook(sound, args[1] as u32);
                0
            }
            Opcode::GetHook => self.get_hook(sound),
            Opcode::SetTrigger | Opcode::DeferCmd => -1,
            Opcode::CheckTrigger => self.triggers_check(sound, int(1), command_from_raw(args[2])),
            Opcode::ClearTrigger => self.triggers_clear(sound, int(1), command_from_raw(args[2])),
            Opcode::JumpMidi => {
                self.players_jump(host, sound, int(1), int(2), int(3), int(4), int(5))
            }
            Opcode::ScanMidi => self.players_scan(host, sound, int(1), int(2), int(3), int(4)),
            Opcode::SendMidiMsg => self.players_send_midi_msg(sound, int(1), int(2), int(3)),
            Opcode::ShareParts => self.players_share_parts(sound, SoundId(args[1])),
            Opcode::StartStream
            | Opcode::SwitchStream
            | Opcode::ProcessStream
            | Opcode::QueryStream => -1,
            Opcode::PanicInternal => {
                tracing::info!(target: "imuse", "Panic Button!...");
                self.players_panic()
            }
        }
    }
}

/// Decodes an opcode argument of a replayed check/clear: -1 is the wildcard; values ≥ 30 cannot name a callback
/// here and match nothing.
fn command_from_raw(raw: isize) -> CommandMatch {
    if raw == -1 {
        return CommandMatch::Any;
    }
    opcode_from_raw(raw).map_or(CommandMatch::Never, |opcode| {
        CommandMatch::Exact(Command::Op(opcode))
    })
}

pub(crate) fn opcode_from_raw(raw: isize) -> Option<Opcode> {
    use Opcode::*;
    const ALL: [Opcode; 30] = [
        Init,
        Terminate,
        Printf,
        Pause,
        Resume,
        Save,
        Restore,
        SetGroupVol,
        StartSound,
        StopSound,
        StopAllSounds,
        GetNextSound,
        SetParam,
        GetParam,
        FadeParam,
        SetHook,
        GetHook,
        SetTrigger,
        CheckTrigger,
        ClearTrigger,
        DeferCmd,
        JumpMidi,
        ScanMidi,
        SendMidiMsg,
        ShareParts,
        StartStream,
        SwitchStream,
        ProcessStream,
        QueryStream,
        PanicInternal,
    ];
    usize::try_from(raw)
        .ok()
        .and_then(|index| ALL.get(index).copied())
}
