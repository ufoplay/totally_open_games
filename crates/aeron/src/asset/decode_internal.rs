//! Port of `src/asset/decode_internal.h`.

/// Largest input blob any decoder accepts.
#[doc(alias = "AERON_DECODE_MAX_ENTRY_SIZE")]
pub(crate) const DECODE_MAX_ENTRY_SIZE: usize = 64 * 1024 * 1024;

pub(crate) fn decode_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

pub(crate) fn decode_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

pub(crate) fn decode_i16(bytes: &[u8]) -> i16 {
    decode_u16(bytes) as i16
}

pub(crate) fn decode_i32(bytes: &[u8]) -> i32 {
    decode_u32(bytes) as i32
}
