//! Port of `src/asset/bmp.c`, `include/aeron/asset/bmp.h`.
//!
//! 4- and 8-bit Windows BMP, plus RLE8, decoded top-down. Coverage includes index zero; the caller selects any
//! colour key. RLE gaps keep index zero.

use super::decode_internal::{DECODE_MAX_ENTRY_SIZE, decode_i32, decode_u16, decode_u32};
use super::decode_types::IndexedFrame;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BmpError {
    /// Code 110.
    #[error("invalid BMP input")]
    InvalidInput,
    /// Code 110.
    #[error("unsupported BMP header")]
    UnsupportedHeader,
    /// Code 110.
    #[error("unsupported BMP layout")]
    UnsupportedLayout,
    /// Code 110.
    #[error("malformed BMP palette")]
    MalformedPalette,
    /// Code 110.
    #[error("malformed BMP RLE8 pixels")]
    MalformedRle,
    /// Code 110.
    #[error("truncated BMP pixels")]
    TruncatedPixels,
    /// Code 111.
    #[error("BMP palette index is invalid")]
    InvalidIndex,
}

/// Decodes RLE8 pixels. Returns `false` for malformed data.
#[doc(alias = "bmp_decode_rle8")]
fn decode_rle8(data: &[u8], indices: &mut [u8], width: i64, height: i64, top_down: bool) -> bool {
    let pixels = width * height;
    let row_step = if top_down { width } else { -width };
    let mut row_start = if top_down { 0 } else { pixels - width };
    let mut destination = row_start;
    let mut p = 0;
    // Original XvT assets use deltas and runs beyond the nominal row width. Keep the row anchor independent of the
    // linear write cursor: only EOL resets the column, and every write must stay within the image allocation.
    while p < data.len() {
        if data.len() - p < 2 {
            return false;
        }
        let count = i64::from(data[p]);
        let value = data[p + 1];
        p += 2;
        if count != 0 {
            if destination < 0 || destination > pixels || count > pixels - destination {
                return false;
            }
            indices[destination as usize..(destination + count) as usize].fill(value);
            destination += count;
        } else if value == 0 {
            row_start += row_step;
            destination = row_start;
            if row_start < 0 || row_start >= pixels {
                return true;
            }
        } else if value == 1 {
            return true;
        } else if value == 2 {
            if data.len() - p < 2 {
                return false;
            }
            let dx = i64::from(data[p]);
            let dy = i64::from(data[p + 1]);
            p += 2;
            row_start += row_step * dy;
            destination += dx + row_step * dy;
            if destination < 0 || destination > pixels {
                return false;
            }
        } else {
            let literal = usize::from(value);
            let encoded = literal + (literal & 1);
            if encoded > data.len() - p
                || destination < 0
                || destination > pixels
                || i64::from(value) > pixels - destination
            {
                return false;
            }
            let start = destination as usize;
            indices[start..start + literal].copy_from_slice(&data[p..p + literal]);
            p += encoded;
            destination += i64::from(value);
        }
    }
    false
}

/// Decodes a BMP into a fully opaque frame with its palette.
#[doc(alias = "AeronBmp_Decode")]
pub fn decode(bytes: &[u8]) -> Result<IndexedFrame, BmpError> {
    let size = bytes.len();
    if !(54..=DECODE_MAX_ENTRY_SIZE).contains(&size) || bytes[0] != b'B' || bytes[1] != b'M' {
        return Err(BmpError::InvalidInput);
    }
    let pixel_offset = decode_u32(&bytes[10..]) as usize;
    let dib_size = decode_u32(&bytes[14..]) as usize;
    if dib_size < 40 || dib_size > size - 14 {
        return Err(BmpError::UnsupportedHeader);
    }
    let width = decode_i32(&bytes[18..]);
    let signed_height = decode_i32(&bytes[22..]);
    let planes = decode_u16(&bytes[26..]);
    let bpp = decode_u16(&bytes[28..]);
    let compression = decode_u32(&bytes[30..]);
    if width <= 0
        || width > 4096
        || signed_height == 0
        || !(-4096..=4096).contains(&signed_height)
        || planes != 1
        || (compression != 0 && !(compression == 1 && bpp == 8))
        || (bpp != 4 && bpp != 8)
    {
        return Err(BmpError::UnsupportedLayout);
    }
    let height = signed_height.unsigned_abs() as usize;
    let width = width as usize;
    let mut color_count = decode_u32(&bytes[46..]) as usize;
    if color_count == 0 {
        color_count = 1 << bpp;
    }
    if color_count > 256
        || color_count * 4 > size - 14 - dib_size
        || pixel_offset > size
        || pixel_offset < 14 + dib_size + color_count * 4
    {
        return Err(BmpError::MalformedPalette);
    }
    let mut frame = IndexedFrame::new(width as u16, height as u16);
    let disk_palette = &bytes[14 + dib_size..][..color_count * 4];
    for (color, bgr) in frame
        .palette
        .iter_mut()
        .zip(disk_palette.as_chunks::<4>().0)
    {
        *color = [bgr[2], bgr[1], bgr[0], 255];
    }
    frame.palette_count = color_count as u16;

    let pixels = &bytes[pixel_offset..];
    if compression == 1 {
        if !decode_rle8(
            pixels,
            &mut frame.indices,
            width as i64,
            height as i64,
            signed_height < 0,
        ) {
            return Err(BmpError::MalformedRle);
        }
    } else {
        let row_stride = (width * usize::from(bpp)).div_ceil(32) * 4;
        if row_stride * height > pixels.len() {
            return Err(BmpError::TruncatedPixels);
        }
        for y in 0..height {
            let source_y = if signed_height > 0 { height - 1 - y } else { y };
            let row = &pixels[source_y * row_stride..];
            for x in 0..width {
                frame.indices[y * width + x] = if bpp == 8 {
                    row[x]
                } else {
                    (row[x / 2] >> if x & 1 != 0 { 0 } else { 4 }) & 0x0f
                };
            }
        }
    }
    if frame
        .indices
        .iter()
        .any(|&index| usize::from(index) >= color_count)
    {
        return Err(BmpError::InvalidIndex);
    }
    frame.coverage.fill(255);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bmp(
        width: i32,
        height: i32,
        bpp: u16,
        compression: u32,
        colors: u32,
        pixels: &[u8],
    ) -> Vec<u8> {
        let mut bytes = vec![0u8; 54];
        bytes[0..2].copy_from_slice(b"BM");
        let pixel_offset = 54 + colors * 4;
        bytes[10..14].copy_from_slice(&pixel_offset.to_le_bytes());
        bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
        bytes[18..22].copy_from_slice(&width.to_le_bytes());
        bytes[22..26].copy_from_slice(&height.to_le_bytes());
        bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
        bytes[28..30].copy_from_slice(&bpp.to_le_bytes());
        bytes[30..34].copy_from_slice(&compression.to_le_bytes());
        bytes[46..50].copy_from_slice(&colors.to_le_bytes());
        for color in 0..colors {
            bytes.extend_from_slice(&[color as u8, 0x40, 0x80, 0]);
        }
        bytes.extend_from_slice(pixels);
        bytes
    }

    #[test]
    fn bottom_up_8bit() {
        // Rows are padded to 4 bytes and stored bottom-up.
        let frame = decode(&bmp(3, 2, 8, 0, 4, &[3, 2, 1, 0, 0, 1, 2, 0])).unwrap();
        assert_eq!(frame.indices, [0, 1, 2, 3, 2, 1]);
        assert_eq!(frame.coverage, [255; 6]);
        assert_eq!(frame.palette[2], [0x80, 0x40, 2, 255]);
        assert_eq!(frame.palette_count, 4);
    }

    #[test]
    fn top_down_4bit() {
        let frame = decode(&bmp(3, -1, 4, 0, 16, &[0x12, 0x30, 0, 0])).unwrap();
        assert_eq!(frame.indices, [1, 2, 3]);
    }

    #[test]
    fn rle8() {
        // Bottom row: run of 2 × 5, literal [1, 2, 3] (padded), EOL; top row: delta +1, run of 1 × 4, EOB.
        let rle = [2, 5, 0, 3, 1, 2, 3, 0, 0, 0, 0, 2, 1, 0, 1, 4, 0, 1];
        let frame = decode(&bmp(5, 2, 8, 1, 8, &rle)).unwrap();
        assert_eq!(frame.indices, [0, 4, 0, 0, 0, 5, 5, 1, 2, 3]);
        assert_eq!(
            decode(&bmp(5, 2, 8, 1, 8, &[6, 1])),
            Err(BmpError::MalformedRle)
        );
    }

    #[test]
    fn rejects_malformed_files() {
        assert_eq!(decode(&[0; 54]), Err(BmpError::InvalidInput));
        assert_eq!(
            decode(&bmp(3, 2, 24, 0, 0, &[])),
            Err(BmpError::UnsupportedLayout)
        );
        assert_eq!(
            decode(&bmp(3, 2, 8, 0, 4, &[0; 4])),
            Err(BmpError::TruncatedPixels)
        );
        assert_eq!(
            decode(&bmp(1, 1, 8, 0, 2, &[7, 0, 0, 0])),
            Err(BmpError::InvalidIndex)
        );
        let mut bytes = bmp(1, 1, 8, 0, 2, &[0; 4]);
        bytes[10] = 0;
        assert_eq!(decode(&bytes), Err(BmpError::MalformedPalette));
    }
}
