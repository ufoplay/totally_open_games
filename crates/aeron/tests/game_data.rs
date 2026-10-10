//! Tests on user-supplied game data. `TOG_DATA_XW`, `TOG_DATA_TIE`, `TOG_DATA_XVT` and `TOG_DATA_XWA` hold a list of
//! install folders or GOG disc images (`game.gog`, or its `game.ins` descriptor), separated like `PATH`. Each test is
//! skipped when its variable is unset.
//!
//! Every file of a format Aeron decodes is read through the VFS and must decode: LFD archives with their `PANL` and
//! `XACT` entries, and standalone ACT, PNL, BMP, ABP and FNT files.

use std::collections::BTreeMap;

use aeron::asset::bitmap_font::{self, RowBytes};
use aeron::asset::lfd::{FourCc, Lfd};
use aeron::asset::{abp_font, act, bmp, pnl};
use aeron::vfs::{GlobFlags, RootOptions, Vfs, VfsConfig, VfsRoot};

/// The data paths named by `variable`, or an empty list when the variable is unset.
fn data_paths(variable: &str) -> Vec<String> {
    let Some(value) = std::env::var_os(variable) else {
        eprintln!("skipped: {variable} is not set");
        return Vec::new();
    };
    std::env::split_paths(&value)
        .filter(|path| !path.as_os_str().is_empty())
        .map(|path| {
            path.into_os_string()
                .into_string()
                .expect("data path is UTF-8")
        })
        .collect()
}

/// Mounts `path` as the asset root.
fn mount(path: &str) -> Vfs {
    let mut vfs = Vfs::new(&VfsConfig::default());
    let metadata = std::fs::metadata(path).unwrap_or_else(|error| panic!("{path}: {error}"));
    if metadata.is_dir() {
        vfs.set_root(VfsRoot::Asset, path).unwrap();
    } else {
        vfs.set_disc_root(VfsRoot::Asset, path).unwrap();
    }
    vfs.set_root_options(VfsRoot::Asset, RootOptions::CASE_INSENSITIVE_LOOKUP);
    vfs
}

/// Every file path below `directory`.
fn find_files(vfs: &mut Vfs, directory: &str, found: &mut Vec<String>) {
    let mut children = Vec::new();
    vfs.glob(
        VfsRoot::Asset,
        directory,
        "*",
        GlobFlags::empty(),
        |entry| {
            children.push((entry.name.to_owned(), entry.is_directory));
            true
        },
    )
    .unwrap();
    for (name, is_directory) in children {
        let path = if directory.is_empty() {
            name
        } else {
            format!("{directory}/{name}")
        };
        if is_directory {
            find_files(vfs, &path, found);
        } else {
            found.push(path);
        }
    }
}

/// Decodes a cockpit font. Like OpenXW's `hud_decode.c`, trailing bytes short of a whole glyph record are dropped
/// first (DOS X-Wing fonts carry such a trailer). Exactly one row width must fit.
fn decode_font(path: &str, bytes: &[u8]) {
    let fitting: Vec<RowBytes> = [RowBytes::One, RowBytes::Four]
        .into_iter()
        .filter(|&row_bytes| {
            let stride = 2 + 2 * usize::from(bytes[1]) * row_bytes as usize;
            let size = bytes.len() / stride * stride;
            bitmap_font::decode(&bytes[..size], row_bytes, 32).is_ok()
        })
        .collect();
    assert_eq!(
        fitting.len(),
        1,
        "{path}: decodes with row widths {fitting:?}"
    );
}

/// Reads and decodes every asset below `data` through the VFS. Returns how many items of each kind were decoded.
fn check_data_at(data: &str) -> BTreeMap<&'static str, usize> {
    let mut vfs = mount(data);
    let mut paths = Vec::new();
    find_files(&mut vfs, "", &mut paths);
    let mut counts = BTreeMap::new();
    for path in &paths {
        let extension = path
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_uppercase())
            .unwrap_or_default();
        if !["LFD", "ACT", "PNL", "BMP", "ABP", "FNT"].contains(&extension.as_str()) {
            continue;
        }
        let size = vfs.stat(VfsRoot::Asset, path).unwrap().size;
        let bytes = vfs.read_all(VfsRoot::Asset, path, None).unwrap();
        assert_eq!(bytes.len() as u64, size, "{path}");
        let fail = |error: &dyn std::fmt::Display| -> ! { panic!("{path}: {error}") };
        let kind = match extension.as_str() {
            "LFD" => {
                let lfd = Lfd::parse(&bytes).unwrap_or_else(|error| fail(&error));
                assert!(!lfd.entries.is_empty(), "{path}");
                for entry in &lfd.entries {
                    if entry.kind == FourCc::PANL {
                        pnl::decode(entry.data).unwrap_or_else(|error| fail(&error));
                        *counts.entry("PANL entry").or_default() += 1;
                    } else if entry.kind == FourCc::new(*b"XACT") {
                        act::decode(entry.data).unwrap_or_else(|error| fail(&error));
                        *counts.entry("XACT entry").or_default() += 1;
                    }
                }
                "LFD"
            }
            // 768-byte `.ACT` files (`NEWPAL.ACT`) are colour tables, not Landru ACT images.
            "ACT" if bytes.len() == 768 => continue,
            "ACT" => {
                act::decode(&bytes).unwrap_or_else(|error| fail(&error));
                "ACT"
            }
            "PNL" => {
                pnl::decode(&bytes).unwrap_or_else(|error| fail(&error));
                "PNL"
            }
            "BMP" => {
                bmp::decode(&bytes).unwrap_or_else(|error| fail(&error));
                "BMP"
            }
            "ABP" => {
                abp_font::decode(&bytes).unwrap_or_else(|error| fail(&error));
                "ABP"
            }
            _ => {
                decode_font(path, &bytes);
                "FNT"
            }
        };
        *counts.entry(kind).or_default() += 1;
    }
    eprintln!("{data}: {counts:?}");
    counts
}

/// Checks every data path named by `variable`. Together they must hold at least one item of every kind in
/// `expected`; each edition holds only some of them.
fn check_data(variable: &str, expected: &[&str]) {
    let paths = data_paths(variable);
    let mut found = BTreeMap::new();
    for data in &paths {
        for (kind, count) in check_data_at(data) {
            *found.entry(kind).or_insert(0) += count;
        }
    }
    if !paths.is_empty() {
        for kind in expected {
            assert!(found.contains_key(kind), "{variable}: no {kind} found");
        }
    }
}

#[test]
fn game_data_xw() {
    check_data(
        "TOG_DATA_XW",
        &["LFD", "PANL entry", "XACT entry", "ACT", "PNL", "FNT"],
    );
}

#[test]
fn game_data_tie() {
    check_data(
        "TOG_DATA_TIE",
        &["LFD", "PANL entry", "XACT entry", "ACT", "PNL", "FNT"],
    );
}

#[test]
fn game_data_xvt() {
    check_data(
        "TOG_DATA_XVT",
        &["LFD", "PANL entry", "ACT", "PNL", "BMP", "ABP", "FNT"],
    );
}

#[test]
fn game_data_xwa() {
    check_data("TOG_DATA_XWA", &["BMP", "ABP", "FNT"]);
}
