//! Port of `groups.c`, `include/imuse/groups.h`: master-scaled group volumes.
//!
//! Group zero is the master. A subgroup's effective volume is `master * (raw + 1) >> 7`, so a raw 127 passes the
//! master through unchanged.

use super::Imuse;
use super::commands::ImuseError;

#[doc(alias = "IMUSE_NUM_GROUPS")]
pub(crate) const GROUP_COUNT: usize = 16;
#[doc(alias = "IMUSE_GROUP_MAX_VOL")]
pub(crate) const GROUP_MAX_VOL: i32 = 127;

/// Volume groups, numbered like the DOS engine's. Groups 5..=15 exist but are unused.
#[doc(alias = "ImuseGroup")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Group {
    #[doc(alias = "IMUSE_GROUP_MASTER")]
    Master = 0,
    #[doc(alias = "IMUSE_GROUP_SFX")]
    Sfx = 1,
    #[doc(alias = "IMUSE_GROUP_VOICE")]
    Voice = 2,
    #[doc(alias = "IMUSE_GROUP_MUSIC")]
    Music = 3,
    #[doc(alias = "IMUSE_GROUP_DIPPED")]
    Dipped = 4,
}

#[doc(alias = "ImGroupsState")]
#[derive(Debug, Clone)]
pub(crate) struct GroupsState {
    /// Caller-set volumes.
    pub vols_raw: [i32; GROUP_COUNT],
    /// Master-scaled volumes read by every voice.
    pub vols_eff: [i32; GROUP_COUNT],
}

impl Default for GroupsState {
    fn default() -> Self {
        Self {
            vols_raw: [GROUP_MAX_VOL; GROUP_COUNT],
            vols_eff: [GROUP_MAX_VOL; GROUP_COUNT],
        }
    }
}

impl Imuse {
    #[doc(alias("ImGroups_Init", "ImGroups_Deinit"))]
    pub(crate) fn groups_init(&mut self) {
        self.groups = GroupsState::default();
    }

    /// Sets a group's raw volume and returns the previous one; `vol == -1` only queries. Invalid arguments return
    /// -5.
    pub(crate) fn set_group_volume_raw(&mut self, group: i32, vol: i32) -> i32 {
        let Ok(index) = usize::try_from(group) else {
            return -5;
        };
        if index >= GROUP_COUNT {
            return -5;
        }
        if vol == -1 {
            return self.groups.vols_raw[index];
        }
        if !(0..=GROUP_MAX_VOL).contains(&vol) {
            return -5;
        }
        let old = self.groups.vols_raw[index];
        if index == Group::Master as usize {
            self.groups.vols_raw[0] = vol;
            self.groups.vols_eff[0] = vol;
            for g in 1..GROUP_COUNT {
                self.groups.vols_eff[g] = (vol * (self.groups.vols_raw[g] + 1)) >> 7;
            }
        } else {
            self.groups.vols_raw[index] = vol;
            self.groups.vols_eff[index] = (self.groups.vols_raw[0] * (vol + 1)) >> 7;
        }
        self.midi_apply_group_vol();
        self.wave_apply_group_vol();
        old
    }

    /// Sets a group's raw volume (0..=127) and returns the previous one.
    #[doc(alias = "imuse_set_group_volume")]
    pub fn set_group_volume(&mut self, group: Group, vol: i32) -> Result<i32, ImuseError> {
        ImuseError::from_code(self.set_group_volume_raw(group as i32, vol))
    }

    /// The raw volume last set for a group.
    pub fn group_volume_raw(&mut self, group: Group) -> i32 {
        self.set_group_volume_raw(group as i32, -1)
    }

    /// A group's effective (master-scaled) volume, or -5 for an invalid group.
    #[doc(alias = "imuse_get_group_volume")]
    pub(crate) fn get_group_volume_raw(&self, group: i32) -> i32 {
        match usize::try_from(group) {
            Ok(index) if index < GROUP_COUNT => self.groups.vols_eff[index],
            _ => -5,
        }
    }

    /// A group's effective (master-scaled) volume.
    pub fn group_volume(&self, group: Group) -> i32 {
        self.get_group_volume_raw(group as i32)
    }
}
