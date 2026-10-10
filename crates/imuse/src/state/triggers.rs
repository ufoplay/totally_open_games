//! Port of `triggers.c`, `internal/triggers.h`: marker triggers and 60 Hz deferred commands.

use super::Imuse;
use super::commands::{Command, ImuseCmd, ImuseError, ImuseHost, Opcode, SoundId, status};

#[doc(alias = "IM_NUM_TRIGGERS")]
pub(crate) const TRIGGER_COUNT: usize = 16;
#[doc(alias = "IM_NUM_DEFER_CMDS")]
pub(crate) const DEFER_COUNT: usize = 8;

/// A marker-fired trigger. `sound == 0` marks a free slot.
#[doc(alias = "ImTrigger")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Trigger {
    pub sound: SoundId,
    /// 0 = any marker, 1..=127 = that marker, 128 = large markers.
    pub marker: i32,
    pub command: Command,
    pub args: [isize; 10],
}

/// A deferred command. `counter == 0` marks a free slot.
#[doc(alias = "ImDeferCmd")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeferCmd {
    pub counter: i32,
    pub command: Command,
    pub args: [isize; 10],
}

const IDLE: Command = Command::Op(Opcode::Init);

#[doc(alias = "ImTriggersState")]
#[derive(Debug, Clone)]
pub(crate) struct TriggersState {
    pub triggers: [Trigger; TRIGGER_COUNT],
    pub defers: [DeferCmd; DEFER_COUNT],
    /// Self-healing "any defer live" flag.
    pub defers_on: bool,
}

impl Default for TriggersState {
    fn default() -> Self {
        Self {
            triggers: [Trigger {
                sound: SoundId(0),
                marker: 0,
                command: IDLE,
                args: [0; 10],
            }; TRIGGER_COUNT],
            defers: [DeferCmd {
                counter: 0,
                command: IDLE,
                args: [0; 10],
            }; DEFER_COUNT],
            defers_on: false,
        }
    }
}

/// Which stored commands a check or clear matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandMatch {
    /// The -1 wildcard.
    Any,
    Exact(Command),
    /// A raw value that cannot name a stored command.
    Never,
}

impl From<Option<Command>> for CommandMatch {
    fn from(command: Option<Command>) -> Self {
        command.map_or(Self::Any, Self::Exact)
    }
}

/// Whether a live slot matches the query; -1 in `sound` or `marker` is a wildcard.
fn trigger_matches(slot: &Trigger, sound: SoundId, marker: i32, command: CommandMatch) -> bool {
    if slot.sound.0 == 0 {
        return false;
    }
    if sound.0 != -1 && sound != slot.sound {
        return false;
    }
    if marker != -1 && marker != slot.marker {
        return false;
    }
    match command {
        CommandMatch::Any => true,
        CommandMatch::Exact(command) => command == slot.command,
        CommandMatch::Never => false,
    }
}

impl Imuse {
    #[doc(alias = "ImTriggers_Init")]
    pub(crate) fn triggers_init(&mut self) {
        tracing::info!(target: "imuse", "TRIGGERS module...");
        self.triggers_deinit();
    }

    /// Frees every slot. Only the free sentinels are cleared, as upstream.
    #[doc(alias = "ImTriggers_Deinit")]
    pub(crate) fn triggers_deinit(&mut self) -> i32 {
        for trigger in &mut self.triggers.triggers {
            trigger.sound = SoundId(0);
        }
        for defer in &mut self.triggers.defers {
            defer.counter = 0;
        }
        self.triggers.defers_on = false;
        0
    }

    /// Registers a trigger. Returns 0, -5 for sound 0, or -6 when the table is full.
    #[doc(alias = "ImTriggers_Set")]
    pub(crate) fn triggers_set(
        &mut self,
        sound: SoundId,
        marker: i32,
        command: Command,
        args: &[isize; 10],
    ) -> i32 {
        if sound.0 == 0 {
            return -5;
        }
        match self
            .triggers
            .triggers
            .iter_mut()
            .find(|slot| slot.sound.0 == 0)
        {
            Some(slot) => {
                *slot = Trigger {
                    sound,
                    marker,
                    command,
                    args: *args,
                };
                0
            }
            None => {
                tracing::info!(target: "imuse", "ERR: tr unable to alloc trigger...");
                -6
            }
        }
    }

    /// Counts the triggers matching the query.
    #[doc(alias = "ImTriggers_Check")]
    pub(crate) fn triggers_check(
        &self,
        sound: SoundId,
        marker: i32,
        command: impl Into<CommandMatch>,
    ) -> i32 {
        let command = command.into();
        self.triggers
            .triggers
            .iter()
            .filter(|slot| trigger_matches(slot, sound, marker, command))
            .count() as i32
    }

    /// Frees the triggers matching the query. Returns 0.
    #[doc(alias = "ImTriggers_Clear")]
    pub(crate) fn triggers_clear(
        &mut self,
        sound: SoundId,
        marker: i32,
        command: impl Into<CommandMatch>,
    ) -> i32 {
        let command = command.into();
        for slot in &mut self.triggers.triggers {
            if trigger_matches(slot, sound, marker, command) {
                slot.sound = SoundId(0);
            }
        }
        0
    }

    /// Fires one trigger. The slot is freed before the handler runs, so the handler may set a new trigger.
    fn exec_trigger(&mut self, host: &mut dyn ImuseHost, index: usize, marker: i32) {
        let Trigger { command, args, .. } = self.triggers.triggers[index];
        self.triggers.triggers[index].sound = SoundId(0);
        match command {
            Command::Callback(callback) => host.trigger_callback(self, callback, marker, &args),
            Command::Op(opcode) => {
                self.exec_opcode(host, opcode, &args);
            }
        }
    }

    /// Fires every trigger of `sound` whose marker matches: marker 0 fires on anything, 1..=127 on that byte
    /// marker, 128 on large markers. Large markers are passed as `marker >= 128`.
    #[doc(alias = "ImTriggers_ProcessMarker")]
    pub(crate) fn triggers_process_marker(
        &mut self,
        host: &mut dyn ImuseHost,
        sound: SoundId,
        marker: i32,
    ) {
        for index in 0..TRIGGER_COUNT {
            let slot = &self.triggers.triggers[index];
            if slot.sound != sound {
                continue;
            }
            let large = marker as u32 >= 128;
            let matches = slot.marker == 0
                || (!large && marker == slot.marker)
                || (large && slot.marker == 128);
            if matches {
                self.exec_trigger(host, index, marker);
            }
        }
    }

    /// Schedules a command `count` 60 Hz ticks from now. Returns 0, -5 for a zero count, or -6 when full.
    #[doc(alias = "ImTriggers_DeferCommand")]
    pub(crate) fn triggers_defer_command(
        &mut self,
        count: i32,
        command: Command,
        args: &[isize; 10],
    ) -> i32 {
        if count == 0 {
            return -5;
        }
        match self
            .triggers
            .defers
            .iter_mut()
            .find(|slot| slot.counter == 0)
        {
            Some(slot) => {
                *slot = DeferCmd {
                    counter: count,
                    command,
                    args: *args,
                };
                self.triggers.defers_on = true;
                0
            }
            None => {
                tracing::info!(target: "imuse", "ERR: tr unable to alloc deferred cmd...");
                -6
            }
        }
    }

    fn exec_defer(&mut self, host: &mut dyn ImuseHost, index: usize) {
        let DeferCmd { command, args, .. } = self.triggers.defers[index];
        match command {
            Command::Callback(callback) => host.defer_callback(self, callback, &args),
            Command::Op(opcode) => {
                self.exec_opcode(host, opcode, &args);
            }
        }
    }

    /// Counts down every live deferred command and runs those that reach zero.
    #[doc(alias = "ImTriggers_Update")]
    pub(crate) fn triggers_update(&mut self, host: &mut dyn ImuseHost) {
        if !self.triggers.defers_on {
            return;
        }
        self.triggers.defers_on = false;
        for index in 0..DEFER_COUNT {
            let slot = &mut self.triggers.defers[index];
            if slot.counter == 0 {
                continue;
            }
            self.triggers.defers_on = true;
            slot.counter -= 1;
            if slot.counter == 0 {
                self.exec_defer(host, index);
            }
        }
    }

    /// Counts the triggers and deferred commands that will start `sound`.
    #[doc(alias = "ImTriggers_GetPendingSoundCount")]
    pub(crate) fn triggers_get_pending_sound_count(&self, sound: SoundId) -> i32 {
        let start = Command::Op(Opcode::StartSound);
        let triggers = self
            .triggers
            .triggers
            .iter()
            .filter(|t| t.sound.0 != 0 && t.command == start && t.args[0] == sound.0)
            .count();
        let defers = self
            .triggers
            .defers
            .iter()
            .filter(|d| d.counter != 0 && d.command == start && d.args[0] == sound.0)
            .count();
        (triggers + defers) as i32
    }

    /// Runs `cmd` when `sound` passes `marker` (0: any marker, 128: large markers).
    #[doc(alias = "imuse_set_trigger")]
    pub fn set_trigger(
        &mut self,
        sound: SoundId,
        marker: i32,
        cmd: &ImuseCmd,
    ) -> Result<(), ImuseError> {
        status(self.triggers_set(sound, marker, cmd.command, &cmd.args))
    }

    /// Counts the matching triggers. -1 in `sound` or `marker`, or `None` for the command, match anything.
    #[doc(alias = "imuse_check_trigger")]
    pub fn check_trigger(&self, sound: SoundId, marker: i32, command: Option<Command>) -> i32 {
        self.triggers_check(sound, marker, command)
    }

    /// Clears the matching triggers, with the wildcards of [`Self::check_trigger`].
    #[doc(alias = "imuse_clear_trigger")]
    pub fn clear_trigger(&mut self, sound: SoundId, marker: i32, command: Option<Command>) {
        self.triggers_clear(sound, marker, command);
    }

    /// Runs `cmd` after `ticks` 60 Hz ticks.
    ///
    /// DEVIATION: upstream truncates a callback opcode to `int`; ids are kept whole.
    #[doc(alias = "imuse_defer_command")]
    pub fn defer_command(&mut self, ticks: i32, cmd: &ImuseCmd) -> Result<(), ImuseError> {
        status(self.triggers_defer_command(ticks, cmd.command, &cmd.args))
    }
}
