//! Port of `filelist.c`, `include/imuse/filelist.h`: reference-counted sounds loaded by name.
//!
//! Ten entries, each with a name of at most 23 bytes. Entries whose count drops to zero are released through the
//! host once their sound neither plays nor is pending.
//!
//! DEVIATION: the load and unload callbacks are [`ImuseHost`] methods. Only the load/unload half is ported; the
//! open/close half (streamed sounds) is unused by the games.

use super::Imuse;
use super::commands::{ImuseHost, PARAM_SOUND_PEND_COUNT, PARAM_SOUND_PLAY_COUNT, SoundId};
use super::utils::{Linked, Links, list_add_item, list_remove_item};

#[doc(alias = "IMUSE_MAX_SOUND_ENTRIES")]
const MAX_SOUND_ENTRIES: usize = 10;
#[doc(alias = "IMUSE_MAX_SOUND_NAME")]
const MAX_SOUND_NAME: usize = 23;

#[doc(alias = "ImSoundEntry")]
#[derive(Debug, Clone, Default)]
pub(crate) struct SoundEntry {
    pub links: Links,
    /// 0 = free.
    pub handle: SoundId,
    pub name: String,
    pub ref_count: i32,
}

impl Linked for SoundEntry {
    fn links(&self) -> &Links {
        &self.links
    }
    fn links_mut(&mut self) -> &mut Links {
        &mut self.links
    }
}

#[doc(alias = "ImFilelistState")]
#[derive(Debug, Clone, Default)]
pub(crate) struct FilelistState {
    pub entries: [SoundEntry; MAX_SOUND_ENTRIES],
    pub loaded_list: Option<u8>,
    pub init_flag: bool,
}

impl Imuse {
    /// Clears the list. Upstream also binds the four host callbacks here; the host passed to each call supplies
    /// them instead.
    #[doc(alias = "imuse_filelist_init")]
    pub fn filelist_init(&mut self) {
        for entry in &mut self.filelist.entries {
            entry.links = Links::default();
            entry.handle = SoundId(0);
        }
        self.filelist.loaded_list = None;
        self.filelist.init_flag = true;
    }

    /// Loads a sound by name, or adds a reference to it. Returns 0 on failure.
    #[doc(alias("imuse_filelist_load", "AddToList", "FindFreeSlot"))]
    pub fn filelist_load(&mut self, host: &mut dyn ImuseHost, name: &str) -> SoundId {
        if !self.filelist.init_flag || name.is_empty() {
            return SoundId(0);
        }
        let found = self.filelist_find(name);
        if found.0 != 0 {
            let mut cur = self.filelist.loaded_list;
            while let Some(index) = cur {
                let entry = &mut self.filelist.entries[usize::from(index)];
                if entry.handle == found {
                    entry.ref_count += 1;
                    return found;
                }
                cur = entry.links.next;
            }
        }
        let Some(slot) = self
            .filelist
            .entries
            .iter()
            .position(|entry| entry.handle.0 == 0)
        else {
            return SoundId(0);
        };
        let Some(handle) = host.load_sound(name).filter(|handle| handle.0 != 0) else {
            return SoundId(0);
        };
        if name.len() > MAX_SOUND_NAME {
            return SoundId(0);
        }
        let entry = &mut self.filelist.entries[slot];
        entry.name = name.to_owned();
        entry.handle = handle;
        entry.ref_count = 1;
        let FilelistState {
            entries,
            loaded_list,
            ..
        } = &mut self.filelist;
        list_add_item(entries, loaded_list, slot as u8);
        handle
    }

    /// Drops a reference, then flushes.
    #[doc(alias = "imuse_filelist_unload")]
    pub fn filelist_unload(&mut self, host: &mut dyn ImuseHost, handle: SoundId) {
        if !self.filelist.init_flag {
            return;
        }
        let mut cur = self.filelist.loaded_list;
        while let Some(index) = cur {
            let entry = &mut self.filelist.entries[usize::from(index)];
            if entry.handle == handle {
                if entry.ref_count > 0 {
                    entry.ref_count -= 1;
                }
                break;
            }
            cur = entry.links.next;
        }
        self.filelist_flush(host);
    }

    /// Drops every reference, then flushes.
    #[doc(alias = "imuse_filelist_unload_all")]
    pub fn filelist_unload_all(&mut self, host: &mut dyn ImuseHost) {
        if !self.filelist.init_flag {
            return;
        }
        let mut cur = self.filelist.loaded_list;
        while let Some(index) = cur {
            let entry = &mut self.filelist.entries[usize::from(index)];
            entry.ref_count = 0;
            cur = entry.links.next;
        }
        self.filelist_flush(host);
    }

    /// The handle loaded under `name`, or 0.
    #[doc(alias = "imuse_filelist_find")]
    pub fn filelist_find(&self, name: &str) -> SoundId {
        let mut cur = self.filelist.loaded_list;
        while let Some(index) = cur {
            let entry = &self.filelist.entries[usize::from(index)];
            if entry.name == name {
                return entry.handle;
            }
            cur = entry.links.next;
        }
        SoundId(0)
    }

    /// Releases the unreferenced entries whose sound neither plays nor is pending.
    #[doc(alias("imuse_filelist_flush", "RemoveFromList"))]
    pub fn filelist_flush(&mut self, host: &mut dyn ImuseHost) {
        let mut cur = self.filelist.loaded_list;
        while let Some(index) = cur {
            let entry = &self.filelist.entries[usize::from(index)];
            let next = entry.links.next;
            let handle = entry.handle;
            if entry.ref_count == 0
                && self.get_param_raw(handle, PARAM_SOUND_PLAY_COUNT) == 0
                && self.get_param_raw(handle, PARAM_SOUND_PEND_COUNT) == 0
            {
                host.unload_sound(handle);
                self.filelist.entries[usize::from(index)].handle = SoundId(0);
                let FilelistState {
                    entries,
                    loaded_list,
                    ..
                } = &mut self.filelist;
                list_remove_item(entries, loaded_list, index);
            }
            cur = next;
        }
    }
}
