//! Tests on user-supplied game data. `TOG_DATA_XW` and `TOG_DATA_TIE` hold a list of install folders or GOG disc
//! images (`game.gog`, or its `game.ins` descriptor), separated like `PATH`. Each test is skipped when its variable is
//! unset.

use aeron::asset::lfd::Lfd;
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

/// Every `*.LFD` path below `directory`.
fn find_lfds(vfs: &mut Vfs, directory: &str, found: &mut Vec<String>) {
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
            name.clone()
        } else {
            format!("{directory}/{name}")
        };
        if is_directory {
            find_lfds(vfs, &path, found);
        } else if name.to_ascii_uppercase().ends_with(".LFD") {
            found.push(path);
        }
    }
}

/// Reads and parses every LFD archive of every data path named by `variable` through the VFS.
fn read_lfds(variable: &str) {
    for path in data_paths(variable) {
        read_lfds_at(&path);
    }
}

/// Reads and parses every LFD archive below `data` through the VFS.
fn read_lfds_at(data: &str) {
    let mut vfs = mount(data);
    let mut paths = Vec::new();
    find_lfds(&mut vfs, "", &mut paths);
    assert!(!paths.is_empty(), "{data}: no LFD archives found");
    for path in &paths {
        let size = vfs.stat(VfsRoot::Asset, path).unwrap().size;
        let data = vfs.read_all(VfsRoot::Asset, path, None).unwrap();
        assert_eq!(data.len() as u64, size, "{path}");
        let lfd = Lfd::parse(&data).unwrap_or_else(|error| panic!("{path}: {error}"));
        assert!(!lfd.entries.is_empty(), "{path}");
    }
    eprintln!("{data}: {} LFD archives parsed", paths.len());
}

#[test]
fn game_data_xw_lfds() {
    read_lfds("TOG_DATA_XW");
}

#[test]
fn game_data_tie_lfds() {
    read_lfds("TOG_DATA_TIE");
}
