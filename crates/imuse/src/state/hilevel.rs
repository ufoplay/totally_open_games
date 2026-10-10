//! Port of `hilevel.c`, `include/imuse/hilevel.h`: per-group volume helpers.
//!
//! The `start_*` helpers and the wave-fill callback are unused by the games and not ported.

use super::Imuse;
use super::groups::Group;

impl Imuse {
    #[doc(alias = "imuse_set_master_vol")]
    pub fn set_master_vol(&mut self, vol: i32) {
        self.set_group_volume_raw(Group::Master as i32, vol);
    }

    #[doc(alias = "imuse_get_master_vol")]
    pub fn get_master_vol(&self) -> i32 {
        self.get_group_volume_raw(Group::Master as i32)
    }

    #[doc(alias = "imuse_set_music_vol")]
    pub fn set_music_vol(&mut self, vol: i32) {
        self.set_group_volume_raw(Group::Music as i32, vol);
    }

    #[doc(alias = "imuse_set_sfx_vol")]
    pub fn set_sfx_vol(&mut self, vol: i32) {
        self.set_group_volume_raw(Group::Sfx as i32, vol);
    }

    #[doc(alias = "imuse_set_voice_vol")]
    pub fn set_voice_vol(&mut self, vol: i32) {
        self.set_group_volume_raw(Group::Voice as i32, vol);
    }
}
