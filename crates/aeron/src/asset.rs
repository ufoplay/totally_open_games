//! Port of `src/asset/*.c`, `include/aeron/asset/*.h`: decoders for the original game asset formats.
//!
//! Decoders take the whole input blob. Views (LFD entries, PNL bitmap lists) borrow it; decoded images and font
//! atlases own their pixels.

mod decode_internal;

pub mod abp_font;
pub mod act;
pub mod bitmap_font;
pub mod bmp;
pub mod decode_types;
pub mod image_coverage;
pub mod lfd;
pub mod pnl;
