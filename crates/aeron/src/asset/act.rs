//! Port of `src/asset/act.c`, `include/aeron/asset/act.h`.
//!
//! Landru ACT images: a frame table of packed RLE frames, each with an RGB-plus-unused palette (the 24-bit source
//! variant). Transparent runs leave coverage zero independently of their colour index.

use super::decode_internal::{DECODE_MAX_ENTRY_SIZE, decode_i16, decode_u16, decode_u32};
use super::decode_types::{IndexedFrame, IndexedFrames};

const RUN_MASK: [u8; 16] = [
    0x00, 0x01, 0x03, 0x07, 0x0f, 0x1f, 0x3f, 0x7f, 0xff, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06,
];
const RUN_SHIFT: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 0, 1, 2, 3, 4, 5, 6];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActError {
    /// Code 60.
    #[error("ACT frame {0} header is truncated")]
    TruncatedHeader(u16),
    /// Code 61.
    #[error("ACT frame {0} dimensions or palette are invalid")]
    InvalidDimensions(u16),
    /// Code 62.
    #[error("ACT frame {0} offset overflows")]
    OffsetOverflow(u16),
    /// Code 63.
    #[error("ACT frame {0} data is truncated")]
    TruncatedData(u16),
    /// Code 66.
    #[error("ACT frame {0} RLE is truncated")]
    TruncatedRle(u16),
    /// Code 67.
    #[error("ACT frame {0} run exceeds its row")]
    RunExceedsRow(u16),
    /// Code 68.
    #[error("ACT frame {0} palette index is invalid")]
    InvalidColor(u16),
    /// Code 69.
    #[error("invalid ACT payload size")]
    InvalidSize,
    /// Code 70.
    #[error("invalid ACT frame table")]
    InvalidFrameTable,
    /// Code 72.
    #[error("ACT frame {0} requires a 24-bit palette")]
    UnsupportedBitDepth(u16),
}

#[doc(alias = "AeronAct_RangeValid")]
fn range_valid(size: usize, offset: u32, bytes: usize) -> bool {
    let offset = offset as usize;
    offset <= size && bytes <= size - offset
}

#[doc(alias = "AeronAct_DecodeFrame")]
fn decode_frame(blob: &[u8], sub_offset: u32, frame_index: u16) -> Result<IndexedFrame, ActError> {
    let size = blob.len();
    if !range_valid(size, sub_offset, 44) {
        return Err(ActError::TruncatedHeader(frame_index));
    }
    let header = &blob[sub_offset as usize..];
    let rgb_relative = decode_u32(&header[4..]);
    let rle_relative = decode_u32(&header[8..]);
    let width = decode_u32(&header[16..]);
    let height = decode_u32(&header[20..]);
    let split = decode_u32(&header[32..]);
    let bit_depth = decode_u32(&header[36..]);
    let palette_count = decode_u32(&header[40..]);
    if bit_depth != 24 {
        return Err(ActError::UnsupportedBitDepth(frame_index));
    }
    if width == 0
        || height == 0
        || width > 4096
        || height > 4096
        || palette_count == 0
        || palette_count > 256
    {
        return Err(ActError::InvalidDimensions(frame_index));
    }
    let (Some(rgb_offset), Some(rle_offset)) = (
        sub_offset.checked_add(rgb_relative),
        sub_offset.checked_add(rle_relative),
    ) else {
        return Err(ActError::OffsetOverflow(frame_index));
    };
    if !range_valid(size, rgb_offset, palette_count as usize * 4)
        || !range_valid(size, rle_offset, 16)
    {
        return Err(ActError::TruncatedData(frame_index));
    }

    let mut out = IndexedFrame::new(width as u16, height as u16);
    out.palette_count = palette_count as u16;
    let palette = &blob[rgb_offset as usize..][..palette_count as usize * 4];
    for (color, rgb) in out.palette.iter_mut().zip(palette.as_chunks::<4>().0) {
        *color = [rgb[0], rgb[1], rgb[2], 255];
    }
    let rle = rle_offset as usize;
    out.anchor_x = decode_i16(&blob[rle..]);
    out.anchor_y = decode_i16(&blob[rle + 4..]).wrapping_neg();
    out.frame_index = frame_index;
    let mask = RUN_MASK[(split & 15) as usize];
    let shift = RUN_SHIFT[(split & 15) as usize];
    let (width, height) = (width as usize, height as usize);
    let mut cursor = rle + 16;
    // 0xFB remains active across row terminators, until another base update.
    let mut base = 0u16;
    for y in 0..height {
        if cursor >= size {
            return Err(ActError::TruncatedRle(frame_index));
        }
        if blob[cursor] == 0xff {
            return Ok(out);
        }
        let mut x = 0usize;
        let mut terminated = false;
        while cursor < size {
            let op = blob[cursor];
            cursor += 1;
            let (count, color) = match op {
                0xff => return Ok(out),
                0xfe => {
                    terminated = true;
                    break;
                }
                0xfb => {
                    if size - cursor < 2 {
                        return Err(ActError::TruncatedRle(frame_index));
                    }
                    base = decode_u16(&blob[cursor..]);
                    cursor += 2;
                    continue;
                }
                0xfc => {
                    if cursor == size {
                        return Err(ActError::TruncatedRle(frame_index));
                    }
                    let count = usize::from(blob[cursor]) + 1;
                    cursor += 1;
                    (count, None)
                }
                0xfd => {
                    if size - cursor < 2 {
                        return Err(ActError::TruncatedRle(frame_index));
                    }
                    let count = usize::from(blob[cursor]) + 1;
                    let color = blob[cursor + 1];
                    cursor += 2;
                    (count, Some(color))
                }
                _ => {
                    let count = usize::from(op & mask) + 1;
                    (count, Some(base.wrapping_add(u16::from(op) >> shift) as u8))
                }
            };
            if count > width - x {
                return Err(ActError::RunExceedsRow(frame_index));
            }
            if let Some(color) = color {
                if u32::from(color) >= palette_count {
                    return Err(ActError::InvalidColor(frame_index));
                }
                let start = y * width + x;
                out.indices[start..start + count].fill(color);
                out.coverage[start..start + count].fill(255);
            }
            x += count;
        }
        if !terminated {
            return Err(ActError::TruncatedRle(frame_index));
        }
    }
    Ok(out)
}

/// Decodes every frame of an ACT image.
#[doc(alias = "AeronAct_Decode")]
pub fn decode(blob: &[u8]) -> Result<IndexedFrames, ActError> {
    if blob.len() < 52 || blob.len() > DECODE_MAX_ENTRY_SIZE {
        return Err(ActError::InvalidSize);
    }
    let table = decode_u32(&blob[16..]);
    let count = decode_u32(&blob[24..]);
    if count == 0 || count > 256 || !range_valid(blob.len(), table, count as usize * 4) {
        return Err(ActError::InvalidFrameTable);
    }
    (0..count as usize)
        .map(|index| {
            let sub_offset = decode_u32(&blob[table as usize + index * 4..]);
            decode_frame(blob, sub_offset, index as u16)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ACT blob with one frame: the frame table at 52, the frame header at 56, then the palette and the RLE.
    fn act(width: u32, height: u32, split: u32, palette_count: u32, rle: &[u8]) -> Vec<u8> {
        let mut blob = vec![0u8; 56 + 44];
        blob[16..20].copy_from_slice(&52u32.to_le_bytes());
        blob[24..28].copy_from_slice(&1u32.to_le_bytes());
        blob[52..56].copy_from_slice(&56u32.to_le_bytes());
        let header = &mut blob[56..];
        header[4..8].copy_from_slice(&44u32.to_le_bytes());
        header[8..12].copy_from_slice(&(44 + palette_count * 4).to_le_bytes());
        header[16..20].copy_from_slice(&width.to_le_bytes());
        header[20..24].copy_from_slice(&height.to_le_bytes());
        header[32..36].copy_from_slice(&split.to_le_bytes());
        header[36..40].copy_from_slice(&24u32.to_le_bytes());
        header[40..44].copy_from_slice(&palette_count.to_le_bytes());
        for color in 0..palette_count {
            blob.extend_from_slice(&[color as u8, 0x10, 0x20, 0]);
        }
        let mut anchors = [0u8; 16];
        anchors[0..2].copy_from_slice(&3i16.to_le_bytes());
        anchors[4..6].copy_from_slice(&5i16.to_le_bytes());
        blob.extend_from_slice(&anchors);
        blob.extend_from_slice(rle);
        blob
    }

    #[test]
    fn decodes_runs() {
        // split 2: count = (op & 3) + 1, colour = base + (op >> 2).
        let rle = [0x05, 0xfc, 0, 0xfe, 0xfb, 2, 0, 0x04, 0xfd, 1, 1, 0xfe];
        let frames = decode(&act(4, 2, 2, 4, &rle)).unwrap();
        let frame = &frames[0];
        assert_eq!(
            (frame.width, frame.height, frame.anchor_x, frame.anchor_y),
            (4, 2, 3, -5)
        );
        assert_eq!(frame.palette[1], [1, 0x10, 0x20, 255]);
        assert_eq!(frame.palette_count, 4);
        assert_eq!(frame.indices, [1, 1, 0, 0, 3, 1, 1, 0]);
        assert_eq!(frame.coverage, [255, 255, 0, 0, 255, 255, 255, 0]);
    }

    #[test]
    fn early_end_and_errors() {
        let frames = decode(&act(2, 3, 2, 2, &[0x01, 0xfe, 0xff])).unwrap();
        assert_eq!(frames[0].coverage, [255, 255, 0, 0, 0, 0]);
        assert_eq!(
            decode(&act(2, 1, 2, 2, &[0x03, 0xfe])),
            Err(ActError::RunExceedsRow(0))
        );
        assert_eq!(
            decode(&act(2, 1, 2, 2, &[0x08, 0xfe])),
            Err(ActError::InvalidColor(0))
        );
        assert_eq!(
            decode(&act(2, 1, 2, 2, &[0x00])),
            Err(ActError::TruncatedRle(0))
        );
        assert_eq!(
            decode(&act(0, 1, 2, 2, &[])),
            Err(ActError::InvalidDimensions(0))
        );
        assert_eq!(decode(&[0; 51]), Err(ActError::InvalidSize));
        let mut blob = act(2, 1, 2, 2, &[0xfe]);
        blob[56 + 36] = 8;
        assert_eq!(decode(&blob), Err(ActError::UnsupportedBitDepth(0)));
    }
}
