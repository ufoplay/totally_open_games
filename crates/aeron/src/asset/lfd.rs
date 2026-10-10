//! Port of `src/asset/lfd.c`, `include/aeron/asset/lfd.h`.
//!
//! LFD archives: sequential 16-byte resource headers (FOURCC type, 8-byte name, size) each followed by its data.

use super::decode_internal::{DECODE_MAX_ENTRY_SIZE, decode_u32};

/// An LFD resource type tag such as `PANL` or `PLTT`, stored big-endian as in `AERON_LFD_FOURCC`.
///
/// Aeron does not interpret the types, so this is an open set rather than an enum.
#[doc(alias = "AERON_LFD_FOURCC")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FourCc(pub u32);

impl FourCc {
    pub const PANL: Self = Self::new(*b"PANL");
    pub const MASK: Self = Self::new(*b"MASK");
    pub const PLTT: Self = Self::new(*b"PLTT");

    pub const fn new(bytes: [u8; 4]) -> Self {
        Self(u32::from_be_bytes(bytes))
    }

    pub const fn bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

/// One resource. `data` borrows the archive blob.
#[doc(alias = "AeronLfdEntry")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LfdEntry<'a> {
    pub kind: FourCc,
    /// The raw 8-byte name field; see [`LfdEntry::name`].
    pub raw_name: [u8; 8],
    pub data: &'a [u8],
}

impl LfdEntry<'_> {
    /// The name up to its first NUL, as the C string `name[9]` reads.
    pub fn name(&self) -> &[u8] {
        let end = self
            .raw_name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(8);
        &self.raw_name[..end]
    }
}

/// A parsed LFD archive.
#[doc(alias("AeronLfd", "AeronLfd_Free"))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lfd<'a> {
    pub entries: Vec<LfdEntry<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LfdError {
    /// Code 20.
    #[error("invalid LFD size")]
    InvalidSize,
    /// Code 21.
    #[error("truncated LFD entry header")]
    TruncatedHeader,
    /// Code 22.
    #[error("LFD entry {0} is truncated")]
    TruncatedEntry(u32),
}

impl<'a> Lfd<'a> {
    /// Parses the entries, which must exactly fill `bytes`.
    ///
    /// DEVIATION: upstream fails with code 23 ("too many entries") past 64 entries, a cap inherited from OpenTIE's
    /// cockpit-panel parser. There is no such cap here.
    #[doc(alias = "AeronLfd_Parse")]
    pub fn parse(bytes: &'a [u8]) -> Result<Self, LfdError> {
        let size = bytes.len();
        if !(16..=DECODE_MAX_ENTRY_SIZE).contains(&size) {
            return Err(LfdError::InvalidSize);
        }
        let mut entries = Vec::new();
        let mut offset = 0;
        while offset < size {
            if size - offset < 16 {
                return Err(LfdError::TruncatedHeader);
            }
            let header = &bytes[offset..offset + 16];
            let entry_size = decode_u32(&header[12..]) as usize;
            if entry_size > size - offset - 16 {
                return Err(LfdError::TruncatedEntry(entries.len() as u32));
            }
            entries.push(LfdEntry {
                kind: FourCc::new([header[0], header[1], header[2], header[3]]),
                raw_name: header[4..12].try_into().unwrap_or_default(),
                data: &bytes[offset + 16..offset + 16 + entry_size],
            });
            offset += 16 + entry_size;
        }
        Ok(Self { entries })
    }

    /// The single entry of type `kind`. A missing or duplicated type gives `None`.
    #[doc(alias = "AeronLfd_Find")]
    pub fn find(&self, kind: FourCc) -> Option<&LfdEntry<'a>> {
        let mut matches = self.entries.iter().filter(|entry| entry.kind == kind);
        let found = matches.next()?;
        matches.next().is_none().then_some(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &[u8; 4], name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut bytes = kind.to_vec();
        let mut raw_name = [0u8; 8];
        raw_name[..name.len()].copy_from_slice(name);
        bytes.extend_from_slice(&raw_name);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }

    #[test]
    fn parse_and_find() {
        let mut bytes = entry(b"RMAP", b"cockpit", &[0; 32]);
        bytes.extend(entry(b"PANL", b"cockpit", b"panel"));
        bytes.extend(entry(b"PLTT", b"cockpit", b"pal"));
        bytes.extend(entry(b"PLTT", b"other", b"pal2"));
        let lfd = Lfd::parse(&bytes).unwrap();
        assert_eq!(lfd.entries.len(), 4);
        let panl = lfd.find(FourCc::PANL).unwrap();
        assert_eq!(panl.data, b"panel");
        assert_eq!(panl.name(), b"cockpit");
        assert_eq!(panl.kind.bytes(), *b"PANL");
        assert!(lfd.find(FourCc::PLTT).is_none());
        assert!(lfd.find(FourCc::MASK).is_none());
    }

    #[test]
    fn rejects_malformed_archives() {
        assert_eq!(Lfd::parse(&[0; 15]), Err(LfdError::InvalidSize));
        let mut bytes = entry(b"PANL", b"x", b"abc");
        bytes.extend_from_slice(&[0; 4]);
        assert_eq!(Lfd::parse(&bytes), Err(LfdError::TruncatedHeader));
        let mut bytes = entry(b"PANL", b"x", b"abc");
        bytes.truncate(18);
        assert_eq!(Lfd::parse(&bytes), Err(LfdError::TruncatedEntry(0)));
    }

    #[test]
    fn parses_more_than_64_entries() {
        let bytes: Vec<u8> = (0..65).flat_map(|_| entry(b"TEXT", b"t", b"")).collect();
        assert_eq!(Lfd::parse(&bytes).unwrap().entries.len(), 65);
    }
}
