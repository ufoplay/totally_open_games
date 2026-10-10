//! Engine layer for the games: VFS + ISO9660, the YAML config tree, asset decoders and the Bevy runtime.
//!
//! Port of [Aeron](https://github.com/elyosh/Aeron). Most of upstream is replaced by Bevy; see `docs/PLAN.md`.

pub mod asset;
pub mod config_file;
pub mod numeric;
mod sdl;
pub mod vfs;
mod vfs_iso9660;

pub use vfs_iso9660::IsoError;
