//! Port of `src/asset/abp_font.c`, `include/aeron/asset/abp_font.h`.
//!
//! ABP fonts: all 256 glyphs as row-RLE coverage, with the original coverage transfer function and spacing. ABP
//! has foreground coverage only.

use super::decode_internal::{DECODE_MAX_ENTRY_SIZE, decode_u32};
use super::decode_types::{DecodedFont, DecodedGlyph};

const ABP_HEADER_SIZE: usize = 0x60b;
const FONT_COLUMNS: usize = 16;
const FONT_GUTTER: usize = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AbpError {
    /// Code 100.
    #[error("invalid ABP input")]
    InvalidInput,
    /// Code 100.
    #[error("truncated ABP glyph data")]
    TruncatedGlyphData,
    /// Code 100.
    #[error("malformed ABP glyph data")]
    MalformedGlyphData,
}

/// Maps a 5-bit sRGB-encoded coverage value to linear 8-bit coverage.
fn font_alpha(value: u8) -> u8 {
    let encoded = f32::from((value & 0x1f) + 1) / 32.0;
    let linear = if encoded <= 0.04045 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    };
    (linear * 255.0 + 0.5) as u8
}

/// Decodes one glyph's rows into `alpha` (`width * height`). Returns `false` for malformed data.
fn decode_glyph(data: &[u8], width: usize, height: usize, alpha: &mut [u8]) -> bool {
    alpha[..width * height].fill(0);
    let mut row = 0usize;
    for y in 0..height {
        if data.len() - row < 4 {
            return false;
        }
        let row_size = decode_u32(&data[row..]) as usize;
        if row_size <= 4 || row_size > data.len() - row {
            return false;
        }
        let row_end = row + row_size;
        let mut p = row + 4;
        let mut x = 0usize;
        let mut fill = |x: usize, length: usize, value: u8| {
            for i in 0..length {
                if x + i < width {
                    alpha[y * width + x + i] = value;
                }
            }
        };
        while p < row_end {
            let token = data[p];
            p += 1;
            if token == 0x80 {
                break;
            }
            if token & 0x80 != 0 {
                let length = usize::from(token & 0x7f);
                if length > row_end - p {
                    return false;
                }
                for (i, &value) in data[p..p + length].iter().enumerate() {
                    fill(x + i, 1, font_alpha(value));
                }
                p += length;
                x += length;
            } else if token & 0x40 != 0 {
                x += usize::from(token & 0x3f);
            } else {
                if token == 0 || p >= row_end {
                    return false;
                }
                let value = font_alpha(data[p]);
                p += 1;
                fill(x, usize::from(token), value);
                x += usize::from(token);
            }
        }
        row = row_end;
    }
    true
}

/// Decodes an ABP font into a 16×16 atlas of cells with a one-pixel gutter.
#[doc(alias = "AeronAbpFont_Decode")]
pub fn decode(bytes: &[u8]) -> Result<DecodedFont, AbpError> {
    if bytes.len() < ABP_HEADER_SIZE || bytes.len() > DECODE_MAX_ENTRY_SIZE {
        return Err(AbpError::InvalidInput);
    }
    let blob_size = decode_u32(bytes) as usize;
    if blob_size > bytes.len() - ABP_HEADER_SIZE {
        return Err(AbpError::TruncatedGlyphData);
    }
    let heights = &bytes[0x404..0x504];
    let widths = &bytes[0x504..0x604];
    let cell_width = usize::from(widths.iter().copied().max().unwrap_or(0).max(1));
    let cell_height = usize::from(heights.iter().copied().max().unwrap_or(0).max(1));
    let stride_width = cell_width + 2 * FONT_GUTTER;
    let stride_height = cell_height + 2 * FONT_GUTTER;
    let atlas_width = FONT_COLUMNS * stride_width;
    let atlas_height = 16 * stride_height;
    let mut font = DecodedFont {
        foreground: vec![0; atlas_width * atlas_height],
        shadow: None,
        glyphs: vec![DecodedGlyph::default(); 256],
        width: atlas_width as u16,
        height: atlas_height as u16,
        first_char: 0,
        cell_width: cell_width as u16,
        cell_height: cell_height as u16,
        baseline: cell_height as u16,
    };
    let mut alpha = vec![0u8; cell_width * cell_height];
    let blob = &bytes[ABP_HEADER_SIZE..ABP_HEADER_SIZE + blob_size];
    let spacing = bytes[0x609];
    for (glyph, metric) in font.glyphs.iter_mut().enumerate() {
        let width = usize::from(widths[glyph]);
        let height = usize::from(heights[glyph]);
        let offset = decode_u32(&bytes[4 + 4 * glyph..]) as usize;
        *metric = DecodedGlyph {
            x: ((glyph % 16) * stride_width + FONT_GUTTER) as u16,
            y: ((glyph / 16) * stride_height + FONT_GUTTER) as u16,
            width: width as u16,
            height: height as u16,
            advance: (width + usize::from(spacing)) as u16,
            source_offset: offset as u32,
        };
        if width == 0 || height == 0 {
            continue;
        }
        if offset >= blob_size || !decode_glyph(&blob[offset..], width, height, &mut alpha) {
            return Err(AbpError::MalformedGlyphData);
        }
        for y in 0..height {
            let start = (usize::from(metric.y) + y) * atlas_width + usize::from(metric.x);
            font.foreground[start..start + width]
                .copy_from_slice(&alpha[y * width..(y + 1) * width]);
        }
    }
    Ok(font)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_function() {
        assert_eq!(font_alpha(0), 1);
        assert_eq!(font_alpha(31), 255);
        assert_eq!(font_alpha(0x20 | 15), font_alpha(15));
    }

    #[test]
    fn decodes_glyphs() {
        // Glyph 'A' (65): 3×2. Row 0: literal [31, 0], skip 1. Row 1: run of 3 × 31.
        let glyph = [8u8, 0, 0, 0, 0x82, 31, 0, 0x41, 6, 0, 0, 0, 3, 31];
        let mut bytes = vec![0u8; ABP_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&(glyph.len() as u32).to_le_bytes());
        bytes[0x404 + 65] = 2;
        bytes[0x504 + 65] = 3;
        bytes[0x609] = 1;
        bytes.extend_from_slice(&glyph);
        let font = decode(&bytes).unwrap();
        assert_eq!(
            (font.cell_width, font.cell_height, font.width, font.height),
            (3, 2, 80, 64)
        );
        let metric = font.glyphs[65];
        assert_eq!((metric.x, metric.y, metric.advance), (5 + 1, 4 * 4 + 1, 4));
        let at = |x: usize, y: usize| {
            font.foreground[(usize::from(metric.y) + y) * 80 + usize::from(metric.x) + x]
        };
        assert_eq!([at(0, 0), at(1, 0), at(2, 0)], [255, 1, 0]);
        assert_eq!([at(0, 1), at(1, 1), at(2, 1)], [255, 255, 255]);
        assert!(font.shadow.is_none());

        bytes[ABP_HEADER_SIZE] = 99;
        assert_eq!(decode(&bytes), Err(AbpError::MalformedGlyphData));
        assert_eq!(decode(&bytes[..100]), Err(AbpError::InvalidInput));
    }
}
