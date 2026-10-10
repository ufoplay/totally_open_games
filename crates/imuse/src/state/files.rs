//! Port of `files.c`, `internal/files.h`: host I/O wrappers and the IFF-like chunk walker.
//!
//! DEVIATION: every read of sound data is bounds-checked. Upstream trusts the container headers.

use std::sync::Arc;

use super::Imuse;
use super::commands::{ImuseHost, SoundId};
use super::utils::swap32;

/// What a live sound is playing as.
#[doc(alias = "IMUSE_SOUND_TYPE_*")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum SoundType {
    #[doc(alias = "IMUSE_SOUND_TYPE_INVALID")]
    Invalid = -1,
    #[doc(alias = "IMUSE_SOUND_TYPE_MIDI")]
    Midi = 1,
    #[doc(alias = "IMUSE_SOUND_TYPE_WAVE")]
    Wave = 2,
}

/// Rejects 0 (no sound) and -1 (wildcard).
#[doc(alias = "ImFiles_IsSoundIdValid")]
pub(crate) fn is_sound_id_valid(sound: SoundId) -> bool {
    sound.0 != 0 && sound.0 != -1
}

/// The bytes of a loaded sound, from the host.
#[doc(alias = "ImFiles_GetSoundPtr")]
pub(crate) fn get_sound_ptr(host: &mut dyn ImuseHost, sound: SoundId) -> Option<Arc<[u8]>> {
    let data = if is_sound_id_valid(sound) {
        host.sound(sound)
    } else {
        None
    };
    if data.is_none() {
        tracing::info!(target: "imuse", "ERR: soundAddrFunc failure in files.c...");
    }
    data
}

impl Imuse {
    /// Whether `sound` is live as MIDI, then as a wave.
    #[doc(alias = "ImFiles_GetSoundType")]
    pub(crate) fn get_sound_type(&self, sound: SoundId) -> SoundType {
        let mut iter = SoundId(0);
        loop {
            iter = self.players_find_next_sound(iter);
            if iter.0 == 0 {
                break;
            }
            if iter == sound {
                return SoundType::Midi;
            }
        }
        loop {
            iter = self.tracks_get_next_sound(iter);
            if iter.0 == 0 {
                break;
            }
            if iter == sound {
                return SoundType::Wave;
            }
        }
        SoundType::Invalid
    }
}

/// Finds the zero-based `chunk_id`th chunk of type `chunk_type` in an iMUSE container and returns its offset (the
/// payload starts 8 bytes later). Mismatches do not consume the index.
#[doc(alias = "ImFiles_GotoChunk")]
pub(crate) fn goto_chunk(buf: &[u8], chunk_type: &[u8; 4], mut chunk_id: i32) -> Option<usize> {
    if chunk_id < 0 || buf.len() < 8 {
        return None;
    }
    let total = swap32(&buf[4..]);
    let mut cursor: u32 = 0;
    while total - cursor >= 8 {
        let chunk = 8 + cursor as usize;
        let header = buf.get(chunk..chunk + 8)?;
        let size = swap32(&header[4..]);
        if size > total - cursor - 8 {
            return None;
        }
        if &header[..4] == chunk_type {
            if chunk_id == 0 {
                return Some(chunk);
            }
            chunk_id -= 1;
        }
        cursor += 8 + size;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = kind.to_vec();
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn goto_chunk_counts_matching_chunks_only() {
        let mut body = chunk(b"MDhd", &[0; 4]);
        body.extend(chunk(b"MTrk", &[1, 2]));
        body.extend(chunk(b"MTrk", &[3]));
        let mut file = b"MIDI".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_be_bytes());
        file.extend(body);
        assert_eq!(goto_chunk(&file, b"MTrk", 0), Some(20));
        assert_eq!(goto_chunk(&file, b"MTrk", 1), Some(30));
        assert_eq!(goto_chunk(&file, b"MTrk", 2), None);
        assert_eq!(goto_chunk(&file, b"MTrk", -1), None);
        assert_eq!(goto_chunk(&file[..25], b"MTrk", 1), None);
    }
}
