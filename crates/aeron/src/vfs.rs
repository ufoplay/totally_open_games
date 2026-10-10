//! Port of `src/vfs.c`, `include/aeron/vfs.h`.
//!
//! The virtual filesystem: asset, user, temp and resource roots, optional case-insensitive lookup per root, and
//! read-only ISO9660 disc images mounted as roots.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};

use bitflags::bitflags;

use crate::sdl;
use crate::vfs_iso9660::{Iso9660, IsoError, IsoFile};

/// Root path configuration for a [`Vfs`]. `None` and empty roots fall back to the defaults described on
/// [`Vfs::new`].
#[doc(alias = "AeronVfsConfig")]
#[derive(Debug, Clone, Default)]
pub struct VfsConfig {
    pub org_name: Option<String>,
    pub app_name: Option<String>,
    pub asset_root: Option<String>,
    pub resource_root: Option<String>,
    pub user_root: Option<String>,
    pub temp_root: Option<String>,
}

/// Logical root selected for VFS operations. Being an enum, it is always valid (`Aeron_VfsRootValid`).
#[doc(alias("AeronVfsRoot", "Aeron_VfsRootValid"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VfsRoot {
    #[doc(alias = "AERON_VFS_ROOT_ASSET")]
    Asset,
    #[doc(alias = "AERON_VFS_ROOT_USER")]
    User,
    #[doc(alias = "AERON_VFS_ROOT_TEMP")]
    Temp,
    #[doc(alias = "AERON_VFS_ROOT_RESOURCE")]
    Resource,
}

#[doc(alias = "AERON_VFS_ROOT_COUNT")]
const ROOT_COUNT: usize = 4;

impl VfsRoot {
    fn index(self) -> usize {
        self as usize
    }
}

bitflags! {
    /// Optional behaviour configured independently for each root.
    #[doc(alias = "AeronVfsRootOptions")]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct RootOptions: u32 {
        /// Resolves the on-disk case of existing path components when an exact-case access fails. Reads, stat
        /// and glob require the full path to exist; open-for-write, remove and rename resolve the existing
        /// portion and let a new final component keep its requested case.
        #[doc(alias = "AERON_VFS_ROOT_OPTION_CASE_INSENSITIVE_LOOKUP")]
        const CASE_INSENSITIVE_LOOKUP = 1 << 0;
    }
}

/// File open modes, matching the binary C stdio modes.
#[doc(alias = "AeronVfsOpenMode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// `rb`
    #[doc(alias = "AERON_VFS_READ")]
    Read,
    /// `wb`
    #[doc(alias = "AERON_VFS_WRITE")]
    Write,
    /// `r+b`
    #[doc(alias = "AERON_VFS_READ_WRITE")]
    ReadWrite,
    /// `w+b`
    #[doc(alias = "AERON_VFS_WRITE_READ")]
    WriteRead,
    /// `ab`
    #[doc(alias = "AERON_VFS_APPEND")]
    Append,
}

impl OpenMode {
    fn creates_file(self) -> bool {
        matches!(self, Self::Write | Self::WriteRead | Self::Append)
    }

    /// `Aeron_OpenModeString`, as `OpenOptions`.
    #[doc(alias = "Aeron_OpenModeString")]
    fn options(self) -> fs::OpenOptions {
        let mut options = fs::OpenOptions::new();
        match self {
            Self::Read => options.read(true),
            Self::Write => options.write(true).create(true).truncate(true),
            Self::ReadWrite => options.read(true).write(true),
            Self::WriteRead => options.read(true).write(true).create(true).truncate(true),
            Self::Append => options.append(true).create(true),
        };
        options
    }
}

/// Basic stat result for a resolved VFS path.
#[doc(alias = "AeronFileInfo")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileInfo {
    pub size: u64,
    pub exists: bool,
    pub is_directory: bool,
}

bitflags! {
    /// Filters and matching behaviour for [`Vfs::glob`].
    #[doc(alias = "AeronVfsGlobFlags")]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct GlobFlags: u32 {
        #[doc(alias = "AERON_VFS_GLOB_FILES")]
        const FILES = 1 << 0;
        #[doc(alias = "AERON_VFS_GLOB_DIRECTORIES")]
        const DIRECTORIES = 1 << 1;
        #[doc(alias = "AERON_VFS_GLOB_CASE_INSENSITIVE")]
        const CASE_INSENSITIVE = 1 << 2;
    }
}

/// Directory entry passed to the [`Vfs::glob`] callback.
#[doc(alias = "AeronVfsEntry")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsEntry<'a> {
    pub name: &'a str,
    pub size: u64,
    pub is_directory: bool,
}

/// Why a VFS operation failed. Upstream returns 0 for all of these.
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    #[error("the root does not support this operation")]
    UnsupportedRoot,
    #[error("the path is empty")]
    EmptyPath,
    #[error("the path does not resolve to an existing entry")]
    NotFound,
    #[error("the path is not a regular file")]
    NotAFile,
    #[error("the file is empty or larger than the size limit")]
    InvalidSize,
    #[error("the file could not be read or written completely")]
    ShortTransfer,
    #[error("the glob callback stopped the iteration")]
    Stopped,
    #[error(transparent)]
    Disc(#[from] IsoError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Stream status after a read, as `SDL_IOStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IoStatus {
    Ready,
    Eof,
    Error,
}

#[doc(alias = "AeronVfsCaseEntry")]
#[derive(Debug)]
struct CaseEntry {
    name: String,
    ambiguous: bool,
}

/// Cached listing of one host directory, sorted case-insensitively.
#[doc(alias = "AeronVfsCaseDirectory")]
#[derive(Debug)]
struct CaseDirectory {
    entries: Vec<CaseEntry>,
}

#[doc(alias = "Aeron_CompareCaseEntries")]
fn compare_case_entries(left: &CaseEntry, right: &CaseEntry) -> Ordering {
    sdl::strcasecmp(&left.name, &right.name).then_with(|| left.name.cmp(&right.name))
}

impl CaseDirectory {
    #[doc(alias = "Aeron_MarkAmbiguousCaseEntries")]
    fn mark_ambiguous_entries(&mut self, host_path: &str) {
        let mut first = 0;
        while first < self.entries.len() {
            let mut end = first + 1;
            while end < self.entries.len()
                && sdl::strcasecmp(&self.entries[first].name, &self.entries[end].name)
                    == Ordering::Equal
            {
                end += 1;
            }
            if end - first > 1 {
                for entry in &mut self.entries[first..end] {
                    entry.ambiguous = true;
                }
                tracing::warn!(
                    target: "aeron.vfs",
                    "ambiguous case-insensitive names in '{host_path}': '{}' and '{}'",
                    self.entries[first].name,
                    self.entries[first + 1].name
                );
            }
            first = end;
        }
    }

    /// The on-disk spelling of `requested`. `Err(true)` reports an ambiguous match.
    #[doc(alias = "Aeron_FindCaseEntry")]
    fn find_entry(&self, requested: &str) -> Result<&str, bool> {
        let low = self
            .entries
            .partition_point(|entry| sdl::strcasecmp(&entry.name, requested) == Ordering::Less);
        match self.entries.get(low) {
            Some(entry) if sdl::strcasecmp(&entry.name, requested) == Ordering::Equal => {
                if entry.ambiguous {
                    Err(true)
                } else {
                    Ok(&entry.name)
                }
            }
            _ => Err(false),
        }
    }
}

/// Lists a host directory for case-insensitive lookup.
///
/// DEVIATION: entries whose names are not UTF-8 are left out, so they cannot be found case-insensitively.
#[doc(alias = "Aeron_CollectCaseEntry")]
fn collect_case_directory(host_path: &str) -> Option<CaseDirectory> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(host_path).ok()? {
        if let Ok(name) = entry.ok()?.file_name().into_string() {
            entries.push(CaseEntry {
                name,
                ambiguous: false,
            });
        }
    }
    let mut directory = CaseDirectory { entries };
    if directory.entries.len() > 1 {
        directory.entries.sort_by(compare_case_entries);
        directory.mark_ambiguous_entries(host_path);
    }
    Some(directory)
}

#[doc(alias = "Aeron_AppendPath")]
fn append_path(base: &str, component: &str) -> String {
    let separator = if !base.is_empty() && !base.ends_with(['/', '\\']) {
        "/"
    } else {
        ""
    };
    format!("{base}{separator}{component}")
}

#[doc(alias = "Aeron_SetDefaultPath")]
fn default_path(src: Option<&str>) -> String {
    match src {
        Some(src) if !src.is_empty() => src.to_owned(),
        _ => sdl::current_directory().unwrap_or_else(|| ".".to_owned()),
    }
}

#[doc(alias = "Aeron_NormalizePath")]
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[doc(alias = "Aeron_SkipRootSeparators")]
fn skip_root_separators(path: &str) -> &str {
    path.trim_start_matches(['/', '\\'])
}

/// `SDL_GetPathInfo`: metadata of a host path, following symbolic links.
fn path_info(path: &str) -> Option<fs::Metadata> {
    fs::metadata(path).ok()
}

/// How [`Vfs::resolve_case_path`] treats components that do not exist yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolve {
    #[doc(alias = "AERON_RESOLVE_EXISTING")]
    Existing,
    /// A new final file name is allowed.
    #[doc(alias = "AERON_RESOLVE_MISSING_LAST")]
    MissingLast,
    /// The requested spelling is kept from the first absent directory through the end.
    #[doc(alias = "AERON_RESOLVE_MISSING_TAIL")]
    MissingTail,
}

/// A virtual filesystem with asset, resource, user and temp roots.
///
/// DEVIATION: roots and paths have no length limit; upstream caps them at `AERON_MAX_PATH` (1024) bytes.
#[doc(alias("AeronVfs", "AeronVfs_Destroy", "AeronVfs_DeinitInternal"))]
#[derive(Debug)]
pub struct Vfs {
    asset_root: String,
    resource_root: String,
    user_root: String,
    temp_root: String,
    root_options: [RootOptions; ROOT_COUNT],
    case_directories: [HashMap<String, CaseDirectory>; ROOT_COUNT],
    iso_roots: [Option<Iso9660>; ROOT_COUNT],
}

impl Vfs {
    /// Creates a VFS with the configured roots or their defaults:
    /// - asset and resource roots default to the current directory;
    /// - the user root defaults to the per-user data directory of `org_name`/`app_name` (both default to
    ///   `aeron`), and to the current directory if that is unavailable;
    /// - the temp root defaults to the user root.
    #[doc(alias("AeronVfs_Create", "AeronVfs_Init", "Aeron_InitVfs"))]
    pub fn new(config: &VfsConfig) -> Self {
        let asset_root = normalize_path(&default_path(config.asset_root.as_deref()));
        let resource_root = normalize_path(&default_path(config.resource_root.as_deref()));
        let user_root = match &config.user_root {
            Some(user_root) => default_path(Some(user_root)),
            None => sdl::pref_path(
                config.org_name.as_deref().unwrap_or("aeron"),
                config.app_name.as_deref().unwrap_or("aeron"),
            )
            .unwrap_or_else(|| default_path(None)),
        };
        let user_root = normalize_path(&user_root);
        let temp_root = match &config.temp_root {
            Some(temp_root) => normalize_path(&default_path(Some(temp_root))),
            None => user_root.clone(),
        };
        Self {
            asset_root,
            resource_root,
            user_root,
            temp_root,
            root_options: [RootOptions::empty(); ROOT_COUNT],
            case_directories: Default::default(),
            iso_roots: Default::default(),
        }
    }

    #[doc(alias("Aeron_InvalidateCaseCache", "Aeron_FreeCaseDirectory"))]
    fn invalidate_case_cache(&mut self, root: VfsRoot) {
        self.case_directories[root.index()].clear();
    }

    /// The asset or resource root slot; the user and temp roots are fixed for the VFS lifetime.
    fn replaceable_root(&mut self, root: VfsRoot) -> Result<&mut String, VfsError> {
        match root {
            VfsRoot::Asset => Ok(&mut self.asset_root),
            VfsRoot::Resource => Ok(&mut self.resource_root),
            VfsRoot::User | VfsRoot::Temp => Err(VfsError::UnsupportedRoot),
        }
    }

    /// Replaces the asset or resource root with a host directory.
    #[doc(alias = "AeronVfs_SetRoot")]
    pub fn set_root(&mut self, root: VfsRoot, path: &str) -> Result<(), VfsError> {
        if path.is_empty() {
            return Err(VfsError::EmptyPath);
        }
        *self.replaceable_root(root)? = normalize_path(path);
        self.invalidate_case_cache(root);
        self.iso_roots[root.index()] = None;
        Ok(())
    }

    /// Replaces the asset or resource root with a read-only ISO9660 disc image. Supports direct MODE1/2048 and
    /// MODE1/2352 images, plus single-track CUE-compatible descriptors using either layout.
    #[doc(alias = "AeronVfs_SetDiscRoot")]
    pub fn set_disc_root(&mut self, root: VfsRoot, path: &str) -> Result<(), VfsError> {
        if path.is_empty() {
            return Err(VfsError::EmptyPath);
        }
        self.replaceable_root(root)?;
        let iso = Iso9660::open(path)?;
        self.invalidate_case_cache(root);
        self.iso_roots[root.index()] = Some(iso);
        *self.replaceable_root(root)? = normalize_path(path);
        Ok(())
    }

    /// Sets the lookup options of one root. Configure options before normal use.
    #[doc(alias = "AeronVfs_SetRootOptions")]
    pub fn set_root_options(&mut self, root: VfsRoot, options: RootOptions) {
        if self.root_options[root.index()] != options {
            self.invalidate_case_cache(root);
            self.root_options[root.index()] = options;
        }
    }

    #[doc(alias = "Aeron_RootUsesCaseInsensitiveLookup")]
    fn uses_case_insensitive_lookup(&self, root: VfsRoot) -> bool {
        self.root_options[root.index()].contains(RootOptions::CASE_INSENSITIVE_LOOKUP)
    }

    #[doc(alias = "Aeron_VfsRootPath")]
    fn root_path(&self, root: VfsRoot) -> &str {
        match root {
            VfsRoot::Asset => &self.asset_root,
            VfsRoot::User => &self.user_root,
            VfsRoot::Temp => &self.temp_root,
            VfsRoot::Resource => &self.resource_root,
        }
    }

    fn iso_root(&self, root: VfsRoot) -> Option<&Iso9660> {
        self.iso_roots[root.index()].as_ref()
    }

    #[doc(alias = "Aeron_BuildPath")]
    fn build_path(&self, root: VfsRoot, path: &str) -> Option<String> {
        let base = self.root_path(root);
        if base.is_empty() {
            return None;
        }
        let relative_path = skip_root_separators(path);
        let joined = if base.ends_with(['/', '\\']) {
            format!("{base}{relative_path}")
        } else {
            format!("{base}/{relative_path}")
        };
        Some(normalize_path(&joined))
    }

    #[doc(alias = "Aeron_GetCaseDirectory")]
    fn case_directory(&mut self, root: VfsRoot, host_path: &str) -> Option<&CaseDirectory> {
        let cache = &mut self.case_directories[root.index()];
        if !cache.contains_key(host_path) {
            cache.insert(host_path.to_owned(), collect_case_directory(host_path)?);
        }
        cache.get(host_path)
    }

    /// Resolves the on-disk case of each existing component of `path`.
    #[doc(alias = "Aeron_ResolveCasePath")]
    fn resolve_case_path(&mut self, root: VfsRoot, path: &str, policy: Resolve) -> Option<String> {
        if !self.uses_case_insensitive_lookup(root) {
            return None;
        }
        let base = self.root_path(root);
        if base.is_empty() {
            return None;
        }
        let mut current = normalize_path(base);
        let relative = normalize_path(skip_root_separators(path));
        let mut cursor = relative.as_str();
        while !cursor.is_empty() {
            let (component, rest) = cursor.split_once('/').unwrap_or((cursor, ""));
            cursor = rest.trim_start_matches('/');
            let is_last = cursor.is_empty();
            if component.is_empty() {
                continue;
            }

            let mut candidate = append_path(&current, component);
            let info = match path_info(&candidate) {
                Some(info) => info,
                None => {
                    if component == "." || component == ".." {
                        return None;
                    }
                    let found = match self.case_directory(root, &current) {
                        Some(directory) => directory.find_entry(component).map(str::to_owned),
                        None => Err(false),
                    };
                    match found {
                        Ok(actual) => {
                            candidate = append_path(&current, &actual);
                            path_info(&candidate)?
                        }
                        // New components keep their requested spelling. Ambiguous matches remain failures so
                        // writes cannot add another case variant.
                        Err(false)
                            if (is_last && policy == Resolve::MissingLast)
                                || policy == Resolve::MissingTail =>
                        {
                            current = if is_last {
                                candidate
                            } else {
                                append_path(&candidate, cursor)
                            };
                            break;
                        }
                        Err(_) => return None,
                    }
                }
            };
            if !is_last && !info.is_dir() {
                return None;
            }
            current = candidate;
        }
        Some(current)
    }

    /// Opens a file relative to the selected root. Disc roots and the resource root are read-only.
    #[doc(alias = "AeronVfs_Open")]
    pub fn open(&mut self, root: VfsRoot, path: &str, mode: OpenMode) -> Result<File, VfsError> {
        let case_insensitive = self.uses_case_insensitive_lookup(root);
        if let Some(iso) = self.iso_root(root) {
            if mode != OpenMode::Read {
                return Err(VfsError::UnsupportedRoot);
            }
            let stream = iso
                .open_file(path, case_insensitive)
                .ok_or(VfsError::NotFound)?;
            return Ok(File::new(Stream::Disc(stream)));
        }
        if root == VfsRoot::Resource && mode != OpenMode::Read {
            return Err(VfsError::UnsupportedRoot);
        }
        let mut host_path = self.build_path(root, path).ok_or(VfsError::NotFound)?;

        // Creating an exact-case path can succeed even when a differently-cased file exists. Resolve before a
        // write can create or truncate anything.
        if mode.creates_file() && case_insensitive {
            host_path = self
                .resolve_case_path(root, path, Resolve::MissingLast)
                .ok_or(VfsError::NotFound)?;
        }

        let mut stream = open_host(&host_path, mode);
        if stream.is_err() && case_insensitive && path_info(&host_path).is_none() {
            let policy = if mode.creates_file() {
                Resolve::MissingLast
            } else {
                Resolve::Existing
            };
            if let Some(resolved) = self.resolve_case_path(root, path, policy) {
                stream = open_host(&resolved, mode);
            }
        }
        let stream = stream?;
        if mode != OpenMode::Read {
            // The open may have created a file; drop cached listings so later case-insensitive lookups see it.
            self.invalidate_case_cache(root);
        }
        Ok(File::new(stream))
    }

    /// Reads a complete non-empty file. `max_size` limits the accepted size.
    #[doc(alias = "AeronVfs_ReadAll")]
    pub fn read_all(
        &mut self,
        root: VfsRoot,
        path: &str,
        max_size: Option<u64>,
    ) -> Result<Vec<u8>, VfsError> {
        let mut file = self.open(root, path, OpenMode::Read)?;
        let file_size = file.size().ok_or(VfsError::InvalidSize)?;
        if file_size == 0 || max_size.is_some_and(|max_size| file_size > max_size) {
            return Err(VfsError::InvalidSize);
        }
        let file_size = usize::try_from(file_size).map_err(|_| VfsError::InvalidSize)?;
        let mut data = vec![0; file_size];
        let read_size = file.read(&mut data);
        let close_ok = file.close();
        if read_size != file_size || !close_ok {
            return Err(VfsError::ShortTransfer);
        }
        Ok(data)
    }

    /// Atomically replaces a writable-root file through a flushed sibling temporary file (`<path>.tmp`).
    #[doc(alias = "AeronVfs_WriteAllAtomic")]
    pub fn write_all_atomic(
        &mut self,
        root: VfsRoot,
        path: &str,
        data: &[u8],
    ) -> Result<(), VfsError> {
        if path.is_empty() {
            return Err(VfsError::EmptyPath);
        }
        if root == VfsRoot::Resource {
            return Err(VfsError::UnsupportedRoot);
        }
        let temporary = format!("{path}.tmp");

        // WRITE truncates any stale temporary left by an interrupted process.
        let mut result = self
            .open(root, &temporary, OpenMode::Write)
            .and_then(|mut file| {
                let write_ok = data.is_empty() || file.write(data) == data.len();
                let flush_ok = write_ok && file.flush();
                let close_ok = file.close();
                if write_ok && flush_ok && close_ok {
                    Ok(())
                } else {
                    Err(VfsError::ShortTransfer)
                }
            });
        if result.is_ok() {
            result = self.rename(root, &temporary, path);
        }
        if result.is_err() {
            let _ = self.remove(root, &temporary);
        }
        result
    }

    /// Retrieves path metadata relative to the selected root.
    #[doc(alias = "AeronVfs_Stat")]
    pub fn stat(&mut self, root: VfsRoot, path: &str) -> Result<FileInfo, VfsError> {
        if let Some(iso) = self.iso_root(root) {
            return iso
                .stat(path, self.uses_case_insensitive_lookup(root))
                .ok_or(VfsError::NotFound);
        }
        let host_path = self.build_path(root, path).ok_or(VfsError::NotFound)?;
        let info = match path_info(&host_path) {
            Some(info) => info,
            None => self
                .resolve_case_path(root, path, Resolve::Existing)
                .and_then(|resolved| path_info(&resolved))
                .ok_or(VfsError::NotFound)?,
        };
        Ok(FileInfo {
            size: info.len(),
            exists: true,
            is_directory: info.is_dir(),
        })
    }

    /// Returns `true` when a path exists relative to the selected root.
    #[doc(alias = "AeronVfs_Exists")]
    pub fn exists(&mut self, root: VfsRoot, path: &str) -> bool {
        self.stat(root, path).is_ok_and(|info| info.exists)
    }

    /// Creates a directory and any missing parents relative to a writable root. Succeeds when the path already
    /// names a directory.
    #[doc(alias = "AeronVfs_CreateDirectory")]
    pub fn create_directory(&mut self, root: VfsRoot, path: &str) -> Result<(), VfsError> {
        if root == VfsRoot::Resource || self.iso_root(root).is_some() {
            return Err(VfsError::UnsupportedRoot);
        }
        if path.is_empty() {
            return Err(VfsError::EmptyPath);
        }
        let mut host_path = self.build_path(root, path).ok_or(VfsError::NotFound)?;
        if self.uses_case_insensitive_lookup(root) {
            host_path = self
                .resolve_case_path(root, path, Resolve::MissingTail)
                .ok_or(VfsError::NotFound)?;
        }
        fs::create_dir_all(&host_path)?;
        self.invalidate_case_cache(root);
        Ok(())
    }

    /// Removes a file or empty directory relative to the selected root. A missing path counts as removed.
    #[doc(alias = "AeronVfs_Remove")]
    pub fn remove(&mut self, root: VfsRoot, path: &str) -> Result<(), VfsError> {
        if root == VfsRoot::Resource || self.iso_root(root).is_some() {
            return Err(VfsError::UnsupportedRoot);
        }
        let mut host_path = self.build_path(root, path).ok_or(VfsError::NotFound)?;

        // Removing a missing path reports success, so resolve case up front or a differently-cased file would
        // silently survive.
        if path_info(&host_path).is_none()
            && let Some(resolved) = self.resolve_case_path(root, path, Resolve::Existing)
        {
            host_path = resolved;
        }
        sdl::remove_path(&host_path)?;
        self.invalidate_case_cache(root);
        Ok(())
    }

    /// Renames a file or directory within the selected root, replacing an existing target file.
    #[doc(alias = "AeronVfs_Rename")]
    pub fn rename(
        &mut self,
        root: VfsRoot,
        old_path: &str,
        new_path: &str,
    ) -> Result<(), VfsError> {
        if root == VfsRoot::Resource || self.iso_root(root).is_some() {
            return Err(VfsError::UnsupportedRoot);
        }
        let mut old_host_path = self.build_path(root, old_path).ok_or(VfsError::NotFound)?;
        let mut new_host_path = self.build_path(root, new_path).ok_or(VfsError::NotFound)?;

        if self.uses_case_insensitive_lookup(root) {
            old_host_path = self
                .resolve_case_path(root, old_path, Resolve::Existing)
                .ok_or(VfsError::NotFound)?;
            new_host_path = self
                .resolve_case_path(root, new_path, Resolve::MissingLast)
                .ok_or(VfsError::NotFound)?;
        }

        if let Err(error) = fs::rename(&old_host_path, &new_host_path) {
            let mut resolved = false;
            if path_info(&old_host_path).is_none()
                && let Some(path) = self.resolve_case_path(root, old_path, Resolve::Existing)
            {
                old_host_path = path;
                resolved = true;
            }
            if path_info(&new_host_path).is_none()
                && let Some(path) = self.resolve_case_path(root, new_path, Resolve::MissingLast)
            {
                new_host_path = path;
                resolved = true;
            }
            if !resolved {
                return Err(error.into());
            }
            fs::rename(&old_host_path, &new_host_path)?;
        }
        self.invalidate_case_cache(root);
        Ok(())
    }

    /// Calls `callback` for each entry of `directory` whose name matches `pattern` (`*` and `?` wildcards).
    /// Without a file or directory filter flag both kinds are included. An empty directory means the root, an
    /// empty pattern means `*`. The iteration fails with [`VfsError::Stopped`] when the callback returns `false`.
    ///
    /// DEVIATION: patterns match one path component; upstream (through `SDL_GlobDirectory`) would recurse into
    /// subdirectories for a pattern containing `/`. No game uses such a pattern.
    #[doc(alias = "AeronVfs_Glob")]
    pub fn glob(
        &mut self,
        root: VfsRoot,
        directory: &str,
        pattern: &str,
        flags: GlobFlags,
        mut callback: impl FnMut(&VfsEntry<'_>) -> bool,
    ) -> Result<(), VfsError> {
        if let Some(iso) = self.iso_root(root) {
            let case_insensitive = self.uses_case_insensitive_lookup(root);
            return if iso.glob(directory, pattern, flags, case_insensitive, &mut callback) {
                Ok(())
            } else {
                Err(VfsError::NotFound)
            };
        }

        let directory = if directory.is_empty() { "." } else { directory };
        let pattern = if pattern.is_empty() { "*" } else { pattern };
        let mut host_path = self.build_path(root, directory).ok_or(VfsError::NotFound)?;
        let case_insensitive = flags.contains(GlobFlags::CASE_INSENSITIVE);

        let mut matches = glob_directory(&host_path, pattern, case_insensitive);
        if matches.is_err()
            && self.uses_case_insensitive_lookup(root)
            && path_info(&host_path).is_none()
            && let Some(resolved) = self.resolve_case_path(root, directory, Resolve::Existing)
        {
            host_path = resolved;
            matches = glob_directory(&host_path, pattern, case_insensitive);
        }
        let matches = matches?;

        let mut want_files = flags.contains(GlobFlags::FILES);
        let mut want_directories = flags.contains(GlobFlags::DIRECTORIES);
        if !want_files && !want_directories {
            want_files = true;
            want_directories = true;
        }

        for name in &matches {
            let info = path_info(&format!("{host_path}/{name}")).ok_or(VfsError::NotFound)?;
            let file_type = info.file_type();
            if (file_type.is_file() && !want_files)
                || (file_type.is_dir() && !want_directories)
                || (!file_type.is_file() && !file_type.is_dir())
            {
                continue;
            }
            let entry = VfsEntry {
                name,
                size: info.len(),
                is_directory: file_type.is_dir(),
            };
            if !callback(&entry) {
                return Err(VfsError::Stopped);
            }
        }
        Ok(())
    }
}

/// `SDL_GlobDirectory` for a one-component pattern: the matching entry names of a host directory, in directory
/// order.
///
/// DEVIATION: entries whose names are not UTF-8 are skipped.
fn glob_directory(
    host_path: &str,
    pattern: &str,
    case_insensitive: bool,
) -> io::Result<Vec<String>> {
    let mut matches = Vec::new();
    for entry in fs::read_dir(host_path)? {
        if let Ok(name) = entry?.file_name().into_string()
            && sdl::glob_match(pattern, &name, case_insensitive)
        {
            matches.push(name);
        }
    }
    Ok(matches)
}

/// `SDL_IOFromFile`: opens a regular host file. Read-only files are buffered.
fn open_host(host_path: &str, mode: OpenMode) -> Result<Stream, VfsError> {
    let file = mode.options().open(host_path)?;
    if !file.metadata()?.is_file() {
        return Err(VfsError::NotAFile);
    }
    Ok(match mode {
        OpenMode::Read => Stream::HostRead(BufReader::new(file)),
        _ => Stream::Host(file),
    })
}

#[derive(Debug)]
enum Stream {
    HostRead(BufReader<fs::File>),
    Host(fs::File),
    Disc(IsoFile),
}

/// Reads until `destination` is full, the stream ends or an error occurs, like `SDL_ReadIO`.
fn read_host(reader: &mut impl Read, destination: &mut [u8]) -> (usize, IoStatus) {
    let mut count = 0;
    while count < destination.len() {
        match reader.read(&mut destination[count..]) {
            Ok(0) => return (count, IoStatus::Eof),
            Ok(read) => count += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return (count, IoStatus::Error),
        }
    }
    (count, IoStatus::Ready)
}

fn write_host(file: &mut fs::File, source: &[u8]) -> usize {
    let mut count = 0;
    while count < source.len() {
        match file.write(&source[count..]) {
            Ok(0) => break,
            Ok(written) => count += written,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    count
}

impl Stream {
    fn read(&mut self, destination: &mut [u8]) -> (usize, IoStatus) {
        match self {
            Self::HostRead(reader) => read_host(reader, destination),
            Self::Host(file) => read_host(file, destination),
            Self::Disc(file) => file.read(destination),
        }
    }

    fn write(&mut self, source: &[u8]) -> usize {
        match self {
            Self::Host(file) => write_host(file, source),
            Self::HostRead(_) | Self::Disc(_) => 0,
        }
    }

    fn seek(&mut self, position: SeekFrom) -> Option<u64> {
        match self {
            Self::HostRead(reader) => reader.seek(position).ok(),
            Self::Host(file) => file.seek(position).ok(),
            Self::Disc(file) => file.seek(position),
        }
    }

    fn tell(&mut self) -> Option<u64> {
        match self {
            Self::HostRead(reader) => reader.stream_position().ok(),
            Self::Host(file) => file.stream_position().ok(),
            Self::Disc(file) => Some(file.tell()),
        }
    }

    fn size(&self) -> Option<u64> {
        match self {
            Self::HostRead(reader) => reader
                .get_ref()
                .metadata()
                .ok()
                .map(|metadata| metadata.len()),
            Self::Host(file) => file.metadata().ok().map(|metadata| metadata.len()),
            Self::Disc(file) => Some(file.size()),
        }
    }

    /// `SDL_FlushIO`, which also syncs regular files to disk.
    fn flush(&mut self) -> bool {
        match self {
            Self::Host(file) => file.flush().is_ok() && file.sync_all().is_ok(),
            Self::HostRead(_) | Self::Disc(_) => true,
        }
    }
}

/// An open VFS file. EOF and error indicators are sticky: they stay set after later successful operations until
/// [`File::clear_error`] (or, for EOF only, a seek).
#[doc(alias = "AeronFile")]
#[derive(Debug)]
pub struct File {
    stream: Stream,
    eof: bool,
    error: bool,
}

impl File {
    fn new(stream: Stream) -> Self {
        Self {
            stream,
            eof: false,
            error: false,
        }
    }

    /// Reads up to `destination.len()` bytes and returns the count. A short count means end of file or an error;
    /// see [`File::eof`] and [`File::has_error`].
    #[doc(alias = "AeronVfs_Read")]
    pub fn read(&mut self, destination: &mut [u8]) -> usize {
        if destination.is_empty() {
            return 0;
        }
        let (count, status) = self.stream.read(destination);
        match status {
            IoStatus::Eof => self.eof = true,
            IoStatus::Error => self.error = true,
            IoStatus::Ready if count < destination.len() => self.error = true,
            IoStatus::Ready => {}
        }
        count
    }

    /// Writes `source` and returns the number of bytes written. A short count sets the error indicator.
    #[doc(alias = "AeronVfs_Write")]
    pub fn write(&mut self, source: &[u8]) -> usize {
        if source.is_empty() {
            return 0;
        }
        let count = self.stream.write(source);
        if count < source.len() {
            self.error = true;
        }
        count
    }

    /// Moves the file offset. Success clears the EOF indicator; failure sets the error indicator.
    #[doc(alias = "AeronVfs_Seek")]
    pub fn seek(&mut self, position: SeekFrom) -> bool {
        if self.stream.seek(position).is_none() {
            self.error = true;
            return false;
        }
        self.eof = false;
        true
    }

    /// The current file offset.
    #[doc(alias = "AeronVfs_Tell")]
    pub fn tell(&mut self) -> Option<u64> {
        let position = self.stream.tell();
        if position.is_none() {
            self.error = true;
        }
        position
    }

    /// The file size, preserving the current offset when fallback seeking is needed.
    #[doc(alias = "AeronVfs_GetSize")]
    pub fn size(&mut self) -> Option<u64> {
        if let Some(size) = self.stream.size() {
            return Some(size);
        }
        let current = self.tell()?;
        if !self.seek(SeekFrom::End(0)) {
            return None;
        }
        let size = self.tell();
        if !self.seek(SeekFrom::Start(current)) {
            return None;
        }
        size
    }

    /// Reads one line of at most `max_len` bytes, keeping the consumed newline and normalising CRLF to LF.
    /// Returns `None` when nothing could be read.
    ///
    /// `max_len` is the C buffer size minus its NUL terminator.
    #[doc(alias = "AeronVfs_ReadLine")]
    pub fn read_line(&mut self, max_len: usize) -> Option<Vec<u8>> {
        let mut line = Vec::new();
        while line.len() < max_len {
            let mut byte = [0u8; 1];
            if self.read(&mut byte) != 1 {
                break;
            }
            if byte[0] == b'\n' && line.last() == Some(&b'\r') {
                *line.last_mut()? = b'\n';
                break;
            }
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        (!line.is_empty()).then_some(line)
    }

    /// Flushes pending writes and syncs them to disk.
    #[doc(alias = "AeronVfs_Flush")]
    pub fn flush(&mut self) -> bool {
        if !self.stream.flush() {
            self.error = true;
            return false;
        }
        true
    }

    #[doc(alias = "AeronVfs_Eof")]
    pub fn eof(&self) -> bool {
        self.eof
    }

    #[doc(alias = "AeronVfs_HasError")]
    pub fn has_error(&self) -> bool {
        self.error
    }

    /// Resets both indicators before an explicit retry.
    #[doc(alias = "AeronVfs_ClearError")]
    pub fn clear_error(&mut self) {
        self.eof = false;
        self.error = false;
    }

    /// Closes the file. Dropping a `File` closes it too.
    #[doc(alias = "AeronVfs_Close")]
    pub fn close(self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs_iso9660::test_image::{Node, build, to_raw};

    struct Fixture {
        _dir: tempfile::TempDir,
        root: String,
        vfs: Vfs,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_owned();
        let vfs = Vfs::new(&VfsConfig {
            asset_root: Some(format!("{root}/asset")),
            resource_root: Some(format!("{root}/resource")),
            user_root: Some(format!("{root}/user")),
            ..VfsConfig::default()
        });
        for name in ["asset", "resource", "user"] {
            fs::create_dir(format!("{root}/{name}")).unwrap();
        }
        Fixture {
            _dir: dir,
            root,
            vfs,
        }
    }

    fn names(
        vfs: &mut Vfs,
        root: VfsRoot,
        directory: &str,
        pattern: &str,
        flags: GlobFlags,
    ) -> Vec<String> {
        let mut names = Vec::new();
        vfs.glob(root, directory, pattern, flags, |entry| {
            names.push(entry.name.to_owned());
            true
        })
        .unwrap();
        names.sort();
        names
    }

    #[test]
    fn default_roots() {
        let vfs = Vfs::new(&VfsConfig {
            user_root: Some("C:\\games\\save".to_owned()),
            ..VfsConfig::default()
        });
        assert_eq!(vfs.user_root, "C:/games/save");
        assert_eq!(vfs.temp_root, vfs.user_root);
        assert!(vfs.asset_root.ends_with('/'));
    }

    #[test]
    fn write_read_seek_and_flags() {
        let Fixture { _dir, mut vfs, .. } = fixture();
        let mut file = vfs
            .open(VfsRoot::User, "/save.dat", OpenMode::WriteRead)
            .unwrap();
        assert_eq!(file.write(b"0123456789"), 10);
        assert!(file.seek(SeekFrom::Start(4)));
        assert_eq!(file.tell(), Some(4));
        let mut buffer = [0u8; 8];
        assert_eq!(file.read(&mut buffer), 6);
        assert_eq!(&buffer[..6], b"456789");
        assert!(file.eof() && !file.has_error());
        assert_eq!(file.size(), Some(10));
        assert!(file.seek(SeekFrom::Current(-1)));
        assert!(!file.eof());
        assert!(!file.seek(SeekFrom::Current(-100)));
        assert!(file.has_error());
        file.clear_error();
        assert!(!file.has_error());
        assert!(file.close());

        let mut file = vfs
            .open(VfsRoot::User, "save.dat", OpenMode::Append)
            .unwrap();
        assert_eq!(file.write(b"!"), 1);
        drop(file);
        assert_eq!(
            vfs.read_all(VfsRoot::User, "save.dat", None).unwrap(),
            b"0123456789!"
        );
        assert!(matches!(
            vfs.read_all(VfsRoot::User, "save.dat", Some(4)),
            Err(VfsError::InvalidSize)
        ));
    }

    #[test]
    fn read_line_normalises_crlf() {
        let Fixture { _dir, mut vfs, .. } = fixture();
        vfs.write_all_atomic(VfsRoot::User, "lines.txt", b"one\r\ntwo\nlonger line\nlast")
            .unwrap();
        let mut file = vfs
            .open(VfsRoot::User, "lines.txt", OpenMode::Read)
            .unwrap();
        assert_eq!(file.read_line(80).unwrap(), b"one\n");
        assert_eq!(file.read_line(80).unwrap(), b"two\n");
        assert_eq!(file.read_line(6).unwrap(), b"longer");
        assert_eq!(file.read_line(80).unwrap(), b" line\n");
        assert_eq!(file.read_line(80).unwrap(), b"last");
        assert_eq!(file.read_line(80), None);
        assert!(file.eof());
    }

    #[test]
    fn write_all_atomic_replaces_and_cleans_up() {
        let Fixture {
            _dir,
            mut vfs,
            root,
        } = fixture();
        vfs.write_all_atomic(VfsRoot::User, "config.yaml", b"a: 1")
            .unwrap();
        vfs.write_all_atomic(VfsRoot::User, "config.yaml", b"a: 2")
            .unwrap();
        assert_eq!(
            fs::read(format!("{root}/user/config.yaml")).unwrap(),
            b"a: 2"
        );
        assert!(!vfs.exists(VfsRoot::User, "config.yaml.tmp"));
        assert!(matches!(
            vfs.write_all_atomic(VfsRoot::Resource, "x", b"x"),
            Err(VfsError::UnsupportedRoot)
        ));
    }

    #[test]
    fn resource_root_is_read_only() {
        let Fixture {
            _dir,
            mut vfs,
            root,
        } = fixture();
        fs::write(format!("{root}/resource/shader.bin"), b"x").unwrap();
        assert!(
            vfs.open(VfsRoot::Resource, "shader.bin", OpenMode::Read)
                .is_ok()
        );
        for mode in [OpenMode::Write, OpenMode::ReadWrite, OpenMode::Append] {
            assert!(matches!(
                vfs.open(VfsRoot::Resource, "shader.bin", mode),
                Err(VfsError::UnsupportedRoot)
            ));
        }
        assert!(vfs.remove(VfsRoot::Resource, "shader.bin").is_err());
        assert!(vfs.create_directory(VfsRoot::Resource, "dir").is_err());
        assert!(vfs.set_root(VfsRoot::User, "/tmp").is_err());
    }

    #[test]
    fn directories_are_not_files() {
        let Fixture { _dir, mut vfs, .. } = fixture();
        vfs.create_directory(VfsRoot::User, "a/b/c").unwrap();
        vfs.create_directory(VfsRoot::User, "a/b").unwrap();
        assert!(vfs.stat(VfsRoot::User, "a/b").unwrap().is_directory);
        assert!(matches!(
            vfs.open(VfsRoot::User, "a/b", OpenMode::Read),
            Err(VfsError::NotAFile | VfsError::Io(_))
        ));
        vfs.remove(VfsRoot::User, "a/b/c").unwrap();
        vfs.remove(VfsRoot::User, "a/b/c").unwrap();
        assert!(!vfs.exists(VfsRoot::User, "a/b/c"));
    }

    #[test]
    fn case_insensitive_lookup() {
        let Fixture {
            _dir,
            mut vfs,
            root,
        } = fixture();
        fs::create_dir_all(format!("{root}/asset/Resource/Sub")).unwrap();
        fs::write(format!("{root}/asset/Resource/Empire.LFD"), b"lfd").unwrap();
        assert!(
            vfs.open(VfsRoot::Asset, "RESOURCE/EMPIRE.LFD", OpenMode::Read)
                .is_err()
        );

        vfs.set_root_options(VfsRoot::Asset, RootOptions::CASE_INSENSITIVE_LOOKUP);
        assert_eq!(
            vfs.read_all(VfsRoot::Asset, "resource\\empire.lfd", None)
                .unwrap(),
            b"lfd"
        );
        assert_eq!(
            vfs.stat(VfsRoot::Asset, "RESOURCE/EMPIRE.LFD")
                .unwrap()
                .size,
            3
        );
        assert_eq!(
            names(
                &mut vfs,
                VfsRoot::Asset,
                "RESOURCE",
                "*.lfd",
                GlobFlags::CASE_INSENSITIVE
            ),
            ["Empire.LFD"]
        );
        assert!(
            vfs.open(VfsRoot::Asset, "resource/../x", OpenMode::Read)
                .is_err()
        );

        // Writes reuse the on-disk case of existing components and keep the requested case for new ones.
        vfs.set_root_options(VfsRoot::User, RootOptions::CASE_INSENSITIVE_LOOKUP);
        vfs.create_directory(VfsRoot::User, "Saves").unwrap();
        let mut file = vfs
            .open(VfsRoot::User, "SAVES/Pilot.PLT", OpenMode::Write)
            .unwrap();
        file.write(b"p");
        drop(file);
        assert!(fs::metadata(format!("{root}/user/Saves/Pilot.PLT")).is_ok());
        vfs.write_all_atomic(VfsRoot::User, "saves/pilot.plt", b"q")
            .unwrap();
        assert_eq!(
            fs::read(format!("{root}/user/Saves/Pilot.PLT")).unwrap(),
            b"q"
        );
        vfs.create_directory(VfsRoot::User, "SAVES/New/Deeper")
            .unwrap();
        assert!(fs::metadata(format!("{root}/user/Saves/New/Deeper")).is_ok());
        vfs.rename(VfsRoot::User, "saves/PILOT.plt", "saves/Renamed.plt")
            .unwrap();
        assert!(fs::metadata(format!("{root}/user/Saves/Renamed.plt")).is_ok());
        vfs.remove(VfsRoot::User, "SAVES/RENAMED.PLT").unwrap();
        assert!(fs::metadata(format!("{root}/user/Saves/Renamed.plt")).is_err());
    }

    #[test]
    fn ambiguous_case_matches_fail() {
        let Fixture {
            _dir,
            mut vfs,
            root,
        } = fixture();
        let probe = format!("{root}/asset/probe");
        fs::write(&probe, b"").unwrap();
        if fs::metadata(format!("{root}/asset/PROBE")).is_ok() {
            return; // Case-insensitive host filesystem.
        }
        fs::write(format!("{root}/asset/Dup.txt"), b"1").unwrap();
        fs::write(format!("{root}/asset/DUP.txt"), b"2").unwrap();
        vfs.set_root_options(VfsRoot::Asset, RootOptions::CASE_INSENSITIVE_LOOKUP);
        assert!(vfs.open(VfsRoot::Asset, "dup.txt", OpenMode::Read).is_err());
        assert!(vfs.open(VfsRoot::Asset, "Dup.txt", OpenMode::Read).is_ok());
    }

    #[test]
    fn glob_filters() {
        let Fixture {
            _dir,
            mut vfs,
            root,
        } = fixture();
        fs::create_dir(format!("{root}/asset/DIR")).unwrap();
        fs::write(format!("{root}/asset/A.PLT"), b"").unwrap();
        fs::write(format!("{root}/asset/b.plt"), b"").unwrap();
        assert_eq!(
            names(&mut vfs, VfsRoot::Asset, "", "", GlobFlags::empty()),
            ["A.PLT", "DIR", "b.plt"]
        );
        assert_eq!(
            names(&mut vfs, VfsRoot::Asset, ".", "*", GlobFlags::DIRECTORIES),
            ["DIR"]
        );
        assert_eq!(
            names(&mut vfs, VfsRoot::Asset, "", "*.plt", GlobFlags::FILES),
            ["b.plt"]
        );
        assert_eq!(
            names(
                &mut vfs,
                VfsRoot::Asset,
                "",
                "*.plt",
                GlobFlags::FILES | GlobFlags::CASE_INSENSITIVE
            ),
            ["A.PLT", "b.plt"]
        );
        assert!(matches!(
            vfs.glob(VfsRoot::Asset, "", "*", GlobFlags::empty(), |_| false),
            Err(VfsError::Stopped)
        ));
        assert!(
            vfs.glob(VfsRoot::Asset, "missing", "*", GlobFlags::empty(), |_| true)
                .is_err()
        );
    }

    #[test]
    fn disc_root() {
        let Fixture {
            _dir,
            mut vfs,
            root,
        } = fixture();
        let image = to_raw(&build(&[
            Node::Directory("RESOURCE"),
            Node::File("RESOURCE/EMPIRE.LFD", b"empire"),
            Node::File("RESOURCE/FONTS.LFD", b"fonts"),
        ]));
        let path = format!("{root}/game.gog");
        fs::write(&path, image).unwrap();

        assert!(vfs.set_disc_root(VfsRoot::User, &path).is_err());
        assert!(matches!(
            vfs.set_disc_root(VfsRoot::Asset, &format!("{root}/missing.iso")),
            Err(VfsError::Disc(_))
        ));
        vfs.set_disc_root(VfsRoot::Asset, &path).unwrap();
        assert_eq!(
            vfs.read_all(VfsRoot::Asset, "RESOURCE/EMPIRE.LFD", None)
                .unwrap(),
            b"empire"
        );
        assert!(
            vfs.open(VfsRoot::Asset, "resource/empire.lfd", OpenMode::Read)
                .is_err()
        );
        assert!(
            vfs.open(VfsRoot::Asset, "RESOURCE/EMPIRE.LFD", OpenMode::Write)
                .is_err()
        );
        assert!(vfs.remove(VfsRoot::Asset, "RESOURCE/EMPIRE.LFD").is_err());

        vfs.set_root_options(VfsRoot::Asset, RootOptions::CASE_INSENSITIVE_LOOKUP);
        let mut file = vfs
            .open(VfsRoot::Asset, "resource/empire.lfd", OpenMode::Read)
            .unwrap();
        assert_eq!(file.size(), Some(6));
        assert!(file.seek(SeekFrom::Start(3)));
        let mut buffer = [0u8; 8];
        assert_eq!(file.read(&mut buffer), 3);
        assert!(file.eof());
        assert_eq!(file.write(b"x"), 0);
        assert!(file.has_error());
        assert!(vfs.stat(VfsRoot::Asset, "resource").unwrap().is_directory);
        assert_eq!(
            names(
                &mut vfs,
                VfsRoot::Asset,
                "resource",
                "*.LFD",
                GlobFlags::FILES
            ),
            ["EMPIRE.LFD", "FONTS.LFD"]
        );

        // A host root replaces the disc again.
        vfs.set_root(VfsRoot::Asset, &format!("{root}/asset"))
            .unwrap();
        assert!(!vfs.exists(VfsRoot::Asset, "RESOURCE/EMPIRE.LFD"));
    }
}
