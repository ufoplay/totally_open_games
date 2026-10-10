//! Port of `src/asset/bitmap_font.c`, `include/aeron/asset/bitmap_font.h`.
//!
//! DOS cockpit fonts: per-glyph width/height bytes followed by interleaved foreground/shadow mask rows of one or
//! four little-endian bytes.

use super::decode_internal::DECODE_MAX_ENTRY_SIZE;
use super::decode_types::{DecodedFont, DecodedGlyph};

/// Width of one mask row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RowBytes {
    One = 1,
    Four = 4,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BitmapFontError {
    /// Code 80.
    #[error("invalid cockpit font input")]
    InvalidInput,
    /// Code 81.
    #[error("invalid cockpit font height")]
    InvalidHeight,
    /// Code 82.
    #[error("cockpit font size does not match its variant")]
    SizeMismatch,
    /// Code 83.
    #[error("invalid cockpit font glyph count")]
    InvalidGlyphCount,
    /// Code 84.
    #[error("cockpit font glyph {0} is invalid")]
    InvalidGlyph(u32),
    /// Code 85.
    #[error("cockpit font is empty")]
    Empty,
    /// Code 86.
    #[error("cockpit font atlas is too large")]
    AtlasTooLarge,
}

/// Decodes a cockpit font into foreground and shadow atlases of 16 columns. `first_char` is the code of the first
/// glyph.
#[doc(alias = "AeronBitmapFont_Decode")]
pub fn decode(
    bytes: &[u8],
    row_bytes: RowBytes,
    first_char: u16,
) -> Result<DecodedFont, BitmapFontError> {
    let row_bytes = row_bytes as usize;
    let size = bytes.len();
    if !(2..=DECODE_MAX_ENTRY_SIZE).contains(&size) {
        return Err(BitmapFontError::InvalidInput);
    }
    let glyph_height = usize::from(bytes[1]);
    if glyph_height == 0 || glyph_height > 64 {
        return Err(BitmapFontError::InvalidHeight);
    }
    let glyph_stride = 2 + 2 * glyph_height * row_bytes;
    if !size.is_multiple_of(glyph_stride) {
        return Err(BitmapFontError::SizeMismatch);
    }
    let count = size / glyph_stride;
    if count == 0 || count > 256 || count > 65536 - usize::from(first_char) {
        return Err(BitmapFontError::InvalidGlyphCount);
    }
    let mut max_width = 0;
    for (index, slot) in bytes.chunks_exact(glyph_stride).enumerate() {
        if usize::from(slot[0]) > row_bytes * 8 || usize::from(slot[1]) != glyph_height {
            return Err(BitmapFontError::InvalidGlyph(index as u32));
        }
        max_width = max_width.max(usize::from(slot[0]));
    }
    if max_width == 0 {
        return Err(BitmapFontError::Empty);
    }

    // Keep filtering footprints inside transparent texels. The sampled cell retains its previous dimensions, while
    // an external guard surrounds every side for linear and mip-filtered sampling.
    const ATLAS_COLUMNS: usize = 16;
    const GUARD: usize = 2;

    let cell_width = max_width + 1;
    let stride_width = cell_width + 2 * GUARD;
    let stride_height = glyph_height + 2 * GUARD;
    let atlas_rows = count.div_ceil(ATLAS_COLUMNS);
    let atlas_width = ATLAS_COLUMNS * stride_width;
    let atlas_height = atlas_rows * stride_height;
    if atlas_width > usize::from(u16::MAX) || atlas_height > usize::from(u16::MAX) {
        return Err(BitmapFontError::AtlasTooLarge);
    }
    let mut foreground = vec![0u8; atlas_width * atlas_height];
    let mut shadow = vec![0u8; atlas_width * atlas_height];
    let mut glyphs = Vec::with_capacity(count);
    for (index, slot) in bytes.chunks_exact(glyph_stride).enumerate() {
        let origin_x = (index % ATLAS_COLUMNS) * stride_width + GUARD;
        let origin_y = (index / ATLAS_COLUMNS) * stride_height + GUARD;
        glyphs.push(DecodedGlyph {
            x: origin_x as u16,
            y: origin_y as u16,
            width: cell_width as u16,
            height: glyph_height as u16,
            advance: u16::from(slot[0]),
            source_offset: (index * glyph_stride) as u32,
        });
        for y in 0..glyph_height {
            let row = &slot[2 + y * row_bytes * 2..];
            for x in 0..usize::from(slot[0]) {
                let byte_index = row_bytes - 1 - (x >> 3);
                let bit = 7 - (x & 7);
                let pixel = (origin_y + y) * atlas_width + origin_x + x;
                foreground[pixel] = if (row[byte_index] >> bit) & 1 != 0 {
                    255
                } else {
                    0
                };
                shadow[pixel] = if (row[row_bytes + byte_index] >> bit) & 1 != 0 {
                    255
                } else {
                    0
                };
            }
        }
    }
    Ok(DecodedFont {
        foreground,
        shadow: Some(shadow),
        glyphs,
        width: atlas_width as u16,
        height: atlas_height as u16,
        first_char,
        cell_width: cell_width as u16,
        cell_height: glyph_height as u16,
        baseline: glyph_height as u16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_byte_rows() {
        // Two glyphs of height 2. Glyph 0: width 3, rows 0b101 / 0b010 with shadow 0b111 on row 1.
        let bytes = [
            3,
            2,
            0b1010_0000,
            0,
            0b0100_0000,
            0b1110_0000,
            1,
            2,
            0x80,
            0x80,
            0,
            0,
        ];
        let font = decode(&bytes, RowBytes::One, 32).unwrap();
        assert_eq!(font.glyphs.len(), 2);
        assert_eq!(
            (font.cell_width, font.cell_height, font.width, font.height),
            (4, 2, 128, 6)
        );
        let glyph = font.glyphs[0];
        assert_eq!(
            (glyph.x, glyph.y, glyph.advance, glyph.source_offset),
            (2, 2, 3, 0)
        );
        let shadow = font.shadow.as_ref().unwrap();
        let at = |plane: &[u8], x: usize, y: usize| plane[(2 + y) * 128 + 2 + x];
        assert_eq!([0, 1, 2].map(|x| at(&font.foreground, x, 0)), [255, 0, 255]);
        assert_eq!([0, 1, 2].map(|x| at(&font.foreground, x, 1)), [0, 255, 0]);
        assert_eq!([0, 1, 2].map(|x| at(shadow, x, 1)), [255, 255, 255]);
        assert_eq!(font.glyphs[1].x, 2 + 8);
    }

    #[test]
    fn four_byte_rows_are_little_endian() {
        // Width 9: pixel 0 is the top bit of the last byte, pixel 8 the top bit of the byte before it.
        let bytes = [9, 1, 0, 0, 0x80, 0x80, 0, 0, 0, 0];
        let font = decode(&bytes, RowBytes::Four, 0).unwrap();
        let row = &font.foreground[2 * font.width as usize + 2..][..9];
        assert_eq!(row, [255, 0, 0, 0, 0, 0, 0, 0, 255]);
    }

    #[test]
    fn rejects_malformed_fonts() {
        assert_eq!(
            decode(&[0], RowBytes::One, 0),
            Err(BitmapFontError::InvalidInput)
        );
        assert_eq!(
            decode(&[1, 0], RowBytes::One, 0),
            Err(BitmapFontError::InvalidHeight)
        );
        assert_eq!(
            decode(&[1, 1, 0], RowBytes::One, 0),
            Err(BitmapFontError::SizeMismatch)
        );
        assert_eq!(
            decode(&[9, 1, 0, 0], RowBytes::One, 0),
            Err(BitmapFontError::InvalidGlyph(0))
        );
        assert_eq!(
            decode(&[0, 1, 0, 0], RowBytes::One, 0),
            Err(BitmapFontError::Empty)
        );
        assert!(decode(&[1, 1, 0, 0], RowBytes::One, 65535).is_ok());
        assert_eq!(
            decode(&[1, 1, 0, 0, 1, 1, 0, 0], RowBytes::One, 65535),
            Err(BitmapFontError::InvalidGlyphCount)
        );
    }
}
