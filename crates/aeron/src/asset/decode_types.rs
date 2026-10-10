//! Port of `src/asset/decode_types.c`, `include/aeron/asset/decode_types.h`.
//!
//! The `*_Free` functions become `Drop`.

/// An 8-bit indexed image with a coverage plane and an optional embedded palette.
#[doc(alias("AeronIndexedFrame", "AeronIndexedFrame_Free"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFrame {
    /// `width * height` palette indices, row-major, top-down.
    pub indices: Vec<u8>,
    /// `width * height` coverage values: 0 transparent, 255 opaque.
    pub coverage: Vec<u8>,
    pub width: u16,
    pub height: u16,
    pub anchor_x: i16,
    pub anchor_y: i16,
    pub frame_index: u16,
    /// Explicit RGBA bytes. Only the first `palette_count` entries are meaningful.
    pub palette: [[u8; 4]; 256],
    pub palette_count: u16,
}

impl IndexedFrame {
    /// A transparent `width × height` frame with no palette.
    pub fn new(width: u16, height: u16) -> Self {
        let pixels = usize::from(width) * usize::from(height);
        Self {
            indices: vec![0; pixels],
            coverage: vec![0; pixels],
            width,
            height,
            anchor_x: 0,
            anchor_y: 0,
            frame_index: 0,
            palette: [[0; 4]; 256],
            palette_count: 0,
        }
    }
}

/// `AeronIndexedFrames`, the frames of a multi-frame image.
#[doc(alias("AeronIndexedFrames", "AeronIndexedFrames_Free"))]
pub type IndexedFrames = Vec<IndexedFrame>;

/// One glyph of a [`DecodedFont`]. The foreground and shadow planes share its atlas rectangle.
#[doc(alias = "AeronDecodedGlyph")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodedGlyph {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub advance: u16,
    /// ABP: the offset in the glyph-data blob; bitmap fonts: the offset in the file.
    pub source_offset: u32,
}

/// A font decoded into coverage atlases.
///
/// `glyphs.len()` replaces upstream's `glyph_count`.
#[doc(alias("AeronDecodedFont", "AeronDecodedFont_Free"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFont {
    /// `width * height` foreground coverage.
    pub foreground: Vec<u8>,
    /// `width * height` shadow coverage, when the format has a shadow plane.
    pub shadow: Option<Vec<u8>>,
    pub glyphs: Vec<DecodedGlyph>,
    pub width: u16,
    pub height: u16,
    pub first_char: u16,
    pub cell_width: u16,
    pub cell_height: u16,
    pub baseline: u16,
}
