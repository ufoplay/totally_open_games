//! Port of `src/asset/pnl.c`, `include/aeron/asset/pnl.h`.
//!
//! PNL cockpit bitmaps: a byte-code stream of short runs (`0x00..=0xFA`), a palette base update (`0xFB`),
//! alternating-colour runs (`0xFC`), long runs (`0xFD`), row ends (`0xFE`) and the image end (`0xFF`).

use super::decode_internal::DECODE_MAX_ENTRY_SIZE;
use super::decode_types::IndexedFrame;

/// `AeronPnlBitmap`. Palette and colour-key policy are supplied by the game; there is no embedded palette.
#[doc(alias = "AeronPnlBitmap")]
pub type PnlBitmap = IndexedFrame;

/// Bitmap streams of a PNL list. Each one borrows the input.
#[doc(alias("AeronPnlList", "AeronPnl_Free"))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PnlList<'a> {
    pub bitmaps: Vec<&'a [u8]>,
}

/// Dimensions of a bitmap stream and the number of bytes it occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PnlMeasure {
    pub width: u16,
    pub height: u16,
    pub consumed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PnlError {
    /// Code 30.
    #[error("invalid PNL bitmap list")]
    InvalidList,
    /// Code 32.
    #[error("PNL contains no complete bitmaps")]
    NoBitmaps,
    /// Code 40.
    #[error("empty PNL bitmap stream")]
    Empty,
    /// Code 41.
    #[error("truncated PNL bitmap at byte {0}")]
    Truncated(usize),
    /// Code 42.
    #[error("PNL bitmap dimensions exceed 4096")]
    TooLarge,
    /// Code 43.
    #[error("PNL bitmap has no pixels")]
    NoPixels,
    /// Code 45.
    #[error("truncated PNL bitmap 0xFB")]
    TruncatedBase,
    /// Code 46.
    #[error("truncated PNL bitmap 0xFC")]
    TruncatedAlternating,
    /// Code 47.
    #[error("truncated PNL bitmap 0xFD")]
    TruncatedRun,
    /// Code 48.
    #[error("PNL bitmap has too many rows")]
    TooManyRows,
    /// Code 49.
    #[error("PNL bitmap stream has no terminator")]
    NoTerminator,
}

/// How one row ended.
enum RowEnd {
    /// `0xFE`: another row follows.
    More,
    /// `0xFF`: the bitmap ends.
    Last,
    Truncated,
}

/// Walks one row and returns its end and its width in pixels.
#[doc(alias = "AeronPnl_WalkRow")]
fn walk_row(bytes: &[u8], cursor: &mut usize) -> (RowEnd, i32) {
    let mut p = *cursor;
    let mut width = 0i32;
    while p < bytes.len() {
        let op = bytes[p];
        p += 1;
        match op {
            0x00..=0xfa => {
                width += i32::from(op & 3) + 1;
                if width > 4096 {
                    return (RowEnd::Truncated, width);
                }
            }
            0xfb => {
                if p == bytes.len() {
                    return (RowEnd::Truncated, width);
                }
                p += 1;
            }
            0xfc | 0xfd => {
                if bytes.len() - p < 2 {
                    return (RowEnd::Truncated, width);
                }
                width += i32::from(if op == 0xfc { bytes[p + 1] } else { bytes[p] }) + 1;
                if width > 4096 {
                    return (RowEnd::Truncated, width);
                }
                p += 2;
            }
            0xfe | 0xff => {
                *cursor = p;
                return (
                    if op == 0xfe {
                        RowEnd::More
                    } else {
                        RowEnd::Last
                    },
                    width,
                );
            }
        }
    }
    (RowEnd::Truncated, width)
}

fn pnl_measure(bytes: &[u8], allow_empty: bool) -> Result<PnlMeasure, PnlError> {
    if bytes.is_empty() || bytes.len() > DECODE_MAX_ENTRY_SIZE {
        return Err(PnlError::Empty);
    }
    let mut cursor = 0;
    let (mut max_width, mut rows) = (0, 0);
    loop {
        let row_start = cursor;
        let (end, width) = walk_row(bytes, &mut cursor);
        let more = match end {
            RowEnd::Truncated => return Err(PnlError::Truncated(cursor)),
            RowEnd::More => true,
            RowEnd::Last => false,
        };
        max_width = max_width.max(width);
        if more || cursor - row_start != 1 || width != 0 {
            rows += 1;
        }
        if rows > 4096 || max_width > 4096 {
            return Err(PnlError::TooLarge);
        }
        if !more {
            break;
        }
    }
    if !allow_empty && (max_width <= 0 || rows <= 0) {
        return Err(PnlError::NoPixels);
    }
    Ok(PnlMeasure {
        width: max_width as u16,
        height: rows as u16,
        consumed: cursor,
    })
}

#[doc(alias = "AeronPnl_WritePixel")]
fn write_pixel(bitmap: &mut PnlBitmap, x: i32, y: i32, color: u8) {
    if x < 0 || y < 0 || x >= i32::from(bitmap.width) || y >= i32::from(bitmap.height) {
        return;
    }
    let pixel = y as usize * usize::from(bitmap.width) + x as usize;
    bitmap.indices[pixel] = color;
    bitmap.coverage[pixel] = 255;
}

fn pnl_rasterize(
    bytes: &[u8],
    ignore_base: bool,
    key: Option<u8>,
    bitmap: &mut PnlBitmap,
) -> Result<(), PnlError> {
    let mut p = 0;
    let (mut x, mut y) = (0i32, 0i32);
    let mut base = 0u8;
    while p < bytes.len() {
        let op = bytes[p];
        p += 1;
        match op {
            0x00..=0xfa => {
                let color = (op >> 2).wrapping_add(base);
                for _ in 0..(op & 3) + 1 {
                    if Some(color) != key {
                        write_pixel(bitmap, x, y, color);
                    }
                    x += 1;
                }
            }
            0xfb => {
                if p == bytes.len() {
                    return Err(PnlError::TruncatedBase);
                }
                base = if ignore_base { 0 } else { bytes[p] };
                p += 1;
            }
            0xfc => {
                if bytes.len() - p < 2 {
                    return Err(PnlError::TruncatedAlternating);
                }
                let color = bytes[p];
                let mut remaining = i32::from(bytes[p + 1]) + 1;
                p += 2;
                while remaining > 0 {
                    write_pixel(bitmap, x, y, color);
                    x += 1;
                    remaining -= 1;
                    if remaining > 0 {
                        write_pixel(bitmap, x, y, color.wrapping_add(1));
                        x += 1;
                        remaining -= 1;
                    }
                }
            }
            0xfd => {
                if bytes.len() - p < 2 {
                    return Err(PnlError::TruncatedRun);
                }
                let count = i32::from(bytes[p]) + 1;
                let color = bytes[p + 1];
                p += 2;
                for _ in 0..count {
                    if Some(color) != key {
                        write_pixel(bitmap, x, y, color);
                    }
                    x += 1;
                }
            }
            0xff => return Ok(()),
            0xfe => {
                y += 1;
                if y > i32::from(bitmap.height) {
                    return Err(PnlError::TooManyRows);
                }
                x = 0;
            }
        }
    }
    Err(PnlError::NoTerminator)
}

/// The dimensions of one bitmap stream and how many bytes it occupies.
#[doc(alias = "AeronPnl_Measure")]
pub fn measure(bytes: &[u8]) -> Result<PnlMeasure, PnlError> {
    pnl_measure(bytes, false)
}

/// Decodes one bitmap with every pixel opaque and the palette base applied.
#[doc(alias = "AeronPnl_Decode")]
pub fn decode(bytes: &[u8]) -> Result<PnlBitmap, PnlError> {
    decode_indexed(bytes, false, None)
}

/// Decodes one bitmap with explicit coverage for the caller's `transparent_index`. Alternating-colour runs remain
/// opaque. `ignore_palette_base` exposes the unshifted indices for recolouring.
#[doc(alias = "AeronPnl_DecodeIndexed")]
pub fn decode_indexed(
    bytes: &[u8],
    ignore_palette_base: bool,
    transparent_index: Option<u8>,
) -> Result<PnlBitmap, PnlError> {
    let size = measure(bytes)?;
    let mut bitmap = PnlBitmap::new(size.width, size.height);
    pnl_rasterize(bytes, ignore_palette_base, transparent_index, &mut bitmap)?;
    Ok(bitmap)
}

impl<'a> PnlList<'a> {
    /// Splits `bytes` into at most `declared_count` bitmap streams. A clean end of input can shorten the list.
    #[doc(alias = "AeronPnl_Parse")]
    pub fn parse(bytes: &'a [u8], declared_count: u32) -> Result<Self, PnlError> {
        if declared_count == 0 || declared_count > 4096 || bytes.len() > DECODE_MAX_ENTRY_SIZE {
            return Err(PnlError::InvalidList);
        }
        let mut bitmaps = Vec::new();
        let mut offset = 0;
        while bitmaps.len() < declared_count as usize && offset < bytes.len() {
            // Walk opcodes: 0xFF inside a run operand is not a terminator.
            let consumed = pnl_measure(&bytes[offset..], true)?.consumed;
            bitmaps.push(&bytes[offset..offset + consumed]);
            offset += consumed;
        }
        if bitmaps.is_empty() {
            return Err(PnlError::NoBitmaps);
        }
        Ok(Self { bitmaps })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_runs_and_rows() {
        // Row 0: 2 pixels of colour 1, base 10, 1 pixel of colour 10+2. Row 1: alternating 5/6 × 3, a key run.
        let bytes = [0x05, 0xfb, 10, 0x08, 0xfe, 0xfc, 5, 2, 0xfd, 1, 12, 0xff];
        let size = measure(&bytes).unwrap();
        assert_eq!(
            size,
            PnlMeasure {
                width: 5,
                height: 2,
                consumed: bytes.len()
            }
        );

        let bitmap = decode(&bytes).unwrap();
        assert_eq!(bitmap.indices, [1, 1, 12, 0, 0, 5, 6, 5, 12, 12]);
        assert_eq!(
            bitmap.coverage,
            [255, 255, 255, 0, 0, 255, 255, 255, 255, 255]
        );

        let keyed = decode_indexed(&bytes, true, Some(2)).unwrap();
        assert_eq!(keyed.indices, [1, 1, 0, 0, 0, 5, 6, 5, 12, 12]);
        assert_eq!(keyed.coverage, [255, 255, 0, 0, 0, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn list_walks_operands() {
        // The first bitmap carries 0xFF as a run operand.
        let bytes = [0xfd, 0xff, 3, 0xff, 0x00, 0xff, 0x04, 0xfe];
        let list = PnlList::parse(&bytes, 2).unwrap();
        assert_eq!(list.bitmaps, [&bytes[..4], &bytes[4..6]]);
        // The third stream ends inside its second row.
        assert_eq!(PnlList::parse(&bytes, 3), Err(PnlError::Truncated(2)));
        assert_eq!(PnlList::parse(&bytes, 0), Err(PnlError::InvalidList));
        assert_eq!(PnlList::parse(&[], 1), Err(PnlError::NoBitmaps));
    }

    #[test]
    fn rejects_malformed_streams() {
        assert_eq!(measure(&[]), Err(PnlError::Empty));
        assert_eq!(measure(&[0x00, 0xfe]), Err(PnlError::Truncated(2)));
        assert_eq!(measure(&[0xff]), Err(PnlError::NoPixels));
        assert_eq!(decode(&[0x00, 0xfb]), Err(PnlError::Truncated(0)));
        assert_eq!(pnl_measure(&[0xff], true).unwrap().height, 0);
    }
}
