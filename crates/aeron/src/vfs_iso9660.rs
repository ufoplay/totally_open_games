//! Port of `src/vfs_iso9660.c`, `src/vfs_iso9660.h`.
//!
//! Read-only ISO9660 disc images used as VFS roots: direct MODE1/2048 and MODE1/2352 images, plus single-track
//! CUE-compatible descriptors (`.cue`, and the GOG `.ins`).

use std::cmp::Ordering;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};

use crate::sdl;
use crate::vfs::{FileInfo, GlobFlags, IoStatus, VfsEntry};

const ISO_LOGICAL_SECTOR_SIZE: u64 = 2048;
const ISO_RAW_SECTOR_SIZE: u64 = 2352;
const ISO_RAW_PAYLOAD_OFFSET: u64 = 16;
const ISO_READ_BATCH_SECTORS: u64 = 16;
const ISO_MAX_ENTRIES: usize = 8192;
const ISO_MAX_DIRECTORY_BYTES: u32 = 32 * 1024 * 1024;
const ISO_MAX_DIRECTORY_DEPTH: u32 = 32;
const ISO_MAX_DESCRIPTOR_BYTES: u64 = 64 * 1024;

/// Why a disc image could not be mounted. The messages are the ones upstream passes to `SDL_SetError`.
#[derive(Debug, thiserror::Error)]
pub enum IsoError {
    #[error("disc image path is empty")]
    EmptyPath,
    #[error("disc descriptor has an invalid size")]
    DescriptorSize,
    #[error("disc descriptor has an invalid FILE statement")]
    DescriptorFile,
    #[error("disc descriptor has an invalid TRACK statement")]
    DescriptorTrack,
    #[error("only single-track MODE1 disc descriptors are supported")]
    DescriptorLayout,
    #[error("disc descriptor track is not MODE1/2048 or MODE1/2352")]
    DescriptorMode,
    #[error("disc descriptor references an unsupported image path")]
    DescriptorImagePath,
    #[error("disc image has an invalid size")]
    ImageSize,
    #[error("disc data track begins beyond the image")]
    TrackBeyondImage,
    #[error("disc data track does not contain an ISO9660 primary volume descriptor")]
    NoPrimaryDescriptor,
    #[error("disc image is not a supported ISO9660 MODE1 image")]
    UnsupportedImage,
    #[error("disc image has an invalid ISO9660 volume descriptor")]
    InvalidVolumeDescriptor,
    #[error("disc image has an unsupported ISO9660 primary volume descriptor")]
    UnsupportedPrimaryDescriptor,
    #[error("ISO9660 volume extends beyond the disc image")]
    VolumeBeyondImage,
    #[error("ISO9660 image contains too many entries")]
    TooManyEntries,
    #[error("ISO9660 directory cycle or alias detected")]
    DirectoryCycle,
    /// Upstream fails these checks without setting an SDL error.
    #[error("ISO9660 directory has an invalid depth, size or extent")]
    InvalidDirectory,
    #[error("ISO9660 directory contains an invalid record")]
    InvalidRecord,
    #[error("ISO9660 directory contains a truncated record")]
    TruncatedRecord,
    #[error("ISO9660 image uses unsupported multi-extent or interleaved files")]
    MultiExtent,
    #[error("ISO9660 image contains invalid entry metadata")]
    InvalidEntry,
    #[error("ISO9660 image contains ambiguous case-insensitive paths")]
    AmbiguousPaths,
    /// A logical read fell outside the image or came back short.
    #[error("disc image read failed")]
    Read,
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[doc(alias = "AeronIsoSource")]
#[derive(Debug, Clone, Default)]
struct IsoSource {
    image_path: String,
    image_size: u64,
    track_offset: u64,
    physical_sector_size: u64,
    payload_offset: u64,
}

#[doc(alias = "AeronIsoEntry")]
#[derive(Debug)]
struct IsoEntry {
    path: String,
    extent: u32,
    size: u32,
    is_directory: bool,
}

/// A mounted disc image: the immutable lookup table of every entry, sorted case-insensitively.
#[doc(alias = "AeronIso9660")]
#[derive(Debug)]
pub(crate) struct Iso9660 {
    source: IsoSource,
    entries: Vec<IsoEntry>,
}

/// An open file inside a disc image.
#[doc(alias = "AeronIsoFile")]
#[derive(Debug)]
pub(crate) struct IsoFile {
    source: IsoSource,
    image: fs::File,
    logical_offset: u64,
    size: u64,
    position: u64,
    scratch: Vec<u8>,
    cached_first_sector: u64,
    cached_sector_count: u64,
}

fn iso_le16(value: &[u8]) -> u16 {
    u16::from_le_bytes([value[0], value[1]])
}

fn iso_be16(value: &[u8]) -> u16 {
    u16::from_be_bytes([value[0], value[1]])
}

fn iso_le32(value: &[u8]) -> u32 {
    u32::from_le_bytes([value[0], value[1], value[2], value[3]])
}

fn iso_be32(value: &[u8]) -> u32 {
    u32::from_be_bytes([value[0], value[1], value[2], value[3]])
}

fn iso_seek_and_read(stream: &mut fs::File, offset: u64, destination: &mut [u8]) -> bool {
    offset <= i64::MAX as u64
        && stream.seek(SeekFrom::Start(offset)).is_ok()
        && stream.read_exact(destination).is_ok()
}

/// Reads logical (2048-byte sector) data, unwrapping MODE1/2352 sectors when needed.
fn iso_read_logical(
    source: &IsoSource,
    stream: &mut fs::File,
    mut offset: u64,
    destination: &mut [u8],
    scratch: Option<&mut [u8]>,
) -> bool {
    if source.physical_sector_size == ISO_LOGICAL_SECTOR_SIZE {
        let size = destination.len() as u64;
        let Some(start) = source.track_offset.checked_add(offset) else {
            return false;
        };
        if start > source.image_size || size > source.image_size - start {
            return false;
        }
        return iso_seek_and_read(stream, start, destination);
    }

    // MODE1/2352 stores each 2048-byte logical block between a physical sector header and its error-correction
    // tail.
    let mut scratch = scratch;
    let mut output = destination;
    while !output.is_empty() {
        let size = output.len() as u64;
        let sector = offset / ISO_LOGICAL_SECTOR_SIZE;
        let inside = offset % ISO_LOGICAL_SECTOR_SIZE;
        if let Some(scratch) = scratch.as_deref_mut()
            && inside == 0
            && size >= ISO_LOGICAL_SECTOR_SIZE
        {
            let sectors = (size / ISO_LOGICAL_SECTOR_SIZE).min(ISO_READ_BATCH_SECTORS);
            let physical_size = sectors * ISO_RAW_SECTOR_SIZE;
            let physical = source.track_offset + sector * ISO_RAW_SECTOR_SIZE;
            if physical > source.image_size
                || physical_size > source.image_size - physical
                || !iso_seek_and_read(stream, physical, &mut scratch[..physical_size as usize])
            {
                return false;
            }
            let logical_size = (sectors * ISO_LOGICAL_SECTOR_SIZE) as usize;
            let (blocks, _) =
                output[..logical_size].as_chunks_mut::<{ ISO_LOGICAL_SECTOR_SIZE as usize }>();
            for (index, block) in blocks.iter_mut().enumerate() {
                let from = index * ISO_RAW_SECTOR_SIZE as usize + ISO_RAW_PAYLOAD_OFFSET as usize;
                block.copy_from_slice(&scratch[from..from + ISO_LOGICAL_SECTOR_SIZE as usize]);
            }
            offset += logical_size as u64;
            output = &mut output[logical_size..];
            continue;
        }

        let chunk = size.min(ISO_LOGICAL_SECTOR_SIZE - inside);
        let physical =
            source.track_offset + sector * ISO_RAW_SECTOR_SIZE + ISO_RAW_PAYLOAD_OFFSET + inside;
        if physical > source.image_size
            || chunk > source.image_size - physical
            || !iso_seek_and_read(stream, physical, &mut output[..chunk as usize])
        {
            return false;
        }
        offset += chunk;
        output = &mut output[chunk as usize..];
    }
    true
}

fn iso_path_extension_is(path: &str, extension: &str) -> bool {
    path.rfind('.')
        .is_some_and(|dot| path[dot..].eq_ignore_ascii_case(extension))
}

fn iso_read_small_file(path: &str) -> Result<Vec<u8>, IsoError> {
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size == 0 || size > ISO_MAX_DESCRIPTOR_BYTES {
        return Err(IsoError::DescriptorSize);
    }
    let mut data = vec![0; size as usize];
    file.read_exact(&mut data)?;
    Ok(data)
}

fn iso_trim_line(line: &[u8]) -> &[u8] {
    line.trim_ascii()
}

fn iso_join_descriptor_file(descriptor: &str, filename: &str) -> Result<String, IsoError> {
    if filename.is_empty() || filename.contains(['/', '\\']) || filename == "." || filename == ".."
    {
        return Err(IsoError::DescriptorImagePath);
    }
    let prefix = descriptor.rfind(['/', '\\']).map_or(0, |slash| slash + 1);
    Ok(format!("{}{filename}", &descriptor[..prefix]))
}

/// The `sscanf` `%u` conversion: skips leading whitespace and parses an optionally signed decimal number, wrapping
/// like the C library does.
fn scan_unsigned(cursor: &mut &[u8]) -> Option<u32> {
    let mut rest = cursor.trim_ascii_start();
    let negative = match rest.first() {
        Some(b'-') => {
            rest = &rest[1..];
            true
        }
        Some(b'+') => {
            rest = &rest[1..];
            false
        }
        _ => false,
    };
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let value = rest[..digits].iter().fold(0u32, |value, &digit| {
        value.wrapping_mul(10).wrapping_add(u32::from(digit - b'0'))
    });
    *cursor = &rest[digits..];
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

/// The `sscanf` `%31s` conversion.
fn scan_word<'a>(cursor: &mut &'a [u8]) -> Option<&'a [u8]> {
    let rest = cursor.trim_ascii_start();
    let length = rest
        .iter()
        .take_while(|byte| !byte.is_ascii_whitespace())
        .count()
        .min(31);
    if length == 0 {
        return None;
    }
    *cursor = &rest[length..];
    Some(&rest[..length])
}

fn scan_literal(cursor: &mut &[u8], literal: u8) -> Option<()> {
    let (&first, rest) = cursor.split_first()?;
    (first == literal).then(|| *cursor = rest)
}

fn starts_with_keyword(line: &[u8], keyword: &str) -> bool {
    line.len() > keyword.len()
        && line[..keyword.len()].eq_ignore_ascii_case(keyword.as_bytes())
        && line[keyword.len()].is_ascii_whitespace()
}

fn iso_parse_descriptor(descriptor: &str, source: &mut IsoSource) -> Result<(), IsoError> {
    let bytes = iso_read_small_file(descriptor)?;
    let mut image_name = String::new();
    let mut mode: &[u8] = &[];
    let (mut index_minutes, mut index_seconds, mut index_frames) = (0u32, 0u32, 0u32);
    let (mut file_count, mut track_count) = (0, 0);
    let mut have_index = false;

    for line in bytes.split(|&byte| byte == b'\n' || byte == b'\r') {
        // The C parser stops each line at an embedded NUL.
        let line = line.split(|&byte| byte == 0).next().unwrap_or_default();
        let line = iso_trim_line(line);
        if starts_with_keyword(line, "FILE") {
            let rest = &line[4..];
            let quote = rest.iter().position(|&byte| byte == b'"');
            let end_quote = quote.and_then(|quote| {
                rest[quote + 1..]
                    .iter()
                    .position(|&byte| byte == b'"')
                    .map(|end| quote + 1 + end)
            });
            let (Some(quote), Some(end_quote)) = (quote, end_quote) else {
                return Err(IsoError::DescriptorFile);
            };
            image_name = String::from_utf8_lossy(&rest[quote + 1..end_quote]).into_owned();
            file_count += 1;
        } else if starts_with_keyword(line, "TRACK") {
            let mut cursor = &line[5..];
            let track_number = scan_unsigned(&mut cursor);
            let word = track_number.and_then(|_| scan_word(&mut cursor));
            if let Some(word) = word {
                mode = word;
            }
            if word.is_none() || track_number == Some(0) {
                return Err(IsoError::DescriptorTrack);
            }
            track_count += 1;
        } else if starts_with_keyword(line, "INDEX") {
            // Like `sscanf`, every conversion that succeeds is stored, even when a later one fails.
            let mut cursor = &line[5..];
            let mut scan = || {
                let index_number = scan_unsigned(&mut cursor)?;
                index_minutes = scan_unsigned(&mut cursor)?;
                scan_literal(&mut cursor, b':')?;
                index_seconds = scan_unsigned(&mut cursor)?;
                scan_literal(&mut cursor, b':')?;
                index_frames = scan_unsigned(&mut cursor)?;
                Some(index_number)
            };
            if scan() == Some(1) {
                have_index = true;
            }
        }
    }
    // The storefront descriptors describe one data-only track. Deliberately reject audio and multi-file layouts
    // rather than guessing their offsets.
    if file_count != 1
        || track_count != 1
        || !have_index
        || index_seconds >= 60
        || index_frames >= 75
    {
        return Err(IsoError::DescriptorLayout);
    }
    if mode.eq_ignore_ascii_case(b"MODE1/2352") {
        source.physical_sector_size = ISO_RAW_SECTOR_SIZE;
        source.payload_offset = ISO_RAW_PAYLOAD_OFFSET;
    } else if mode.eq_ignore_ascii_case(b"MODE1/2048") {
        source.physical_sector_size = ISO_LOGICAL_SECTOR_SIZE;
        source.payload_offset = 0;
    } else {
        return Err(IsoError::DescriptorMode);
    }
    source.image_path = iso_join_descriptor_file(descriptor, &image_name)?;
    let index_sector =
        (u64::from(index_minutes) * 60 + u64::from(index_seconds)) * 75 + u64::from(index_frames);
    source.track_offset = index_sector * source.physical_sector_size;
    Ok(())
}

fn iso_open_source(path: &str) -> Result<(IsoSource, fs::File), IsoError> {
    let mut source = IsoSource::default();
    if iso_path_extension_is(path, ".cue") || iso_path_extension_is(path, ".ins") {
        iso_parse_descriptor(path, &mut source)?;
    } else {
        source.image_path = path.to_owned();
    }

    let stream = fs::File::open(&source.image_path)?;
    let image_size = stream.metadata()?.len();
    if image_size == 0 {
        return Err(IsoError::ImageSize);
    }
    source.image_size = image_size;
    if source.track_offset >= source.image_size {
        return Err(IsoError::TrackBeyondImage);
    }
    Ok((source, stream))
}

fn is_primary_descriptor(descriptor: &[u8; 7]) -> bool {
    descriptor[0] == 1 && &descriptor[1..6] == b"CD001" && descriptor[6] == 1
}

fn iso_probe_layout(source: &mut IsoSource, stream: &mut fs::File) -> Result<(), IsoError> {
    let mut descriptor = [0u8; 7];
    if source.physical_sector_size != 0 {
        let offset = source.track_offset + 16 * source.physical_sector_size + source.payload_offset;
        if !iso_seek_and_read(stream, offset, &mut descriptor)
            || !is_primary_descriptor(&descriptor)
        {
            return Err(IsoError::NoPrimaryDescriptor);
        }
        return Ok(());
    }

    let layouts = [
        (ISO_LOGICAL_SECTOR_SIZE, 0),
        (ISO_RAW_SECTOR_SIZE, ISO_RAW_PAYLOAD_OFFSET),
    ];
    for (sector_size, payload_offset) in layouts {
        let offset = 16 * sector_size + payload_offset;
        if offset <= source.image_size
            && descriptor.len() as u64 <= source.image_size - offset
            && iso_seek_and_read(stream, offset, &mut descriptor)
            && is_primary_descriptor(&descriptor)
        {
            source.physical_sector_size = sector_size;
            source.payload_offset = payload_offset;
            return Ok(());
        }
    }
    Err(IsoError::UnsupportedImage)
}

/// Strips root separators and `.` components and joins the rest with `/`. A `..` component fails.
fn iso_normalize_path(path: &str) -> Option<String> {
    let mut normalized = String::with_capacity(path.len());
    for component in path.split(['/', '\\']) {
        match component {
            "" | "." => continue,
            ".." => return None,
            _ => {
                if !normalized.is_empty() {
                    normalized.push('/');
                }
                normalized.push_str(component);
            }
        }
    }
    Some(normalized)
}

fn iso_record_values(record: &[u8]) -> Option<(u32, u32)> {
    if record.len() < 34
        || record[1] != 0
        || iso_le32(&record[2..]) != iso_be32(&record[6..])
        || iso_le32(&record[10..]) != iso_be32(&record[14..])
    {
        return None;
    }
    Some((iso_le32(&record[2..]), iso_le32(&record[10..])))
}

/// The entry name without its `;version` suffix and one trailing `.`.
///
/// DEVIATION: bytes that are not UTF-8 are replaced with U+FFFD; upstream keeps raw bytes.
fn iso_entry_name(record: &[u8]) -> Option<String> {
    let name_length = usize::from(record[32]);
    if name_length == 0 || 33 + name_length > record.len() {
        return None;
    }
    let mut name = &record[33..33 + name_length];
    // The C code works on a NUL-terminated copy.
    if let Some(nul) = name.iter().position(|&byte| byte == 0) {
        name = &name[..nul];
    }
    if let Some(version) = name.iter().position(|&byte| byte == b';') {
        name = &name[..version];
    }
    if let [rest @ .., b'.'] = name {
        name = rest;
    }
    if name.is_empty()
        || name
            .iter()
            .any(|&byte| byte == b'/' || byte == b'\\' || byte < 0x20 || byte == 0x7f)
    {
        return None;
    }
    Some(String::from_utf8_lossy(name).into_owned())
}

/// The entry table being built while the directory tree is walked.
struct IsoBuild<'a> {
    source: &'a IsoSource,
    stream: &'a mut fs::File,
    volume_sectors: u32,
    entries: Vec<IsoEntry>,
    visited_directories: Vec<u32>,
}

impl IsoBuild<'_> {
    #[doc(alias = "iso_add_entry")]
    fn add_entry(
        &mut self,
        path: String,
        extent: u32,
        size: u32,
        is_directory: bool,
    ) -> Result<(), IsoError> {
        if self.entries.len() >= ISO_MAX_ENTRIES {
            return Err(IsoError::TooManyEntries);
        }
        self.entries.push(IsoEntry {
            path,
            extent,
            size,
            is_directory,
        });
        Ok(())
    }

    #[doc(alias = "iso_mark_directory_visited")]
    fn mark_directory_visited(&mut self, extent: u32) -> Result<(), IsoError> {
        if self.visited_directories.contains(&extent) {
            return Err(IsoError::DirectoryCycle);
        }
        self.visited_directories.push(extent);
        Ok(())
    }

    #[doc(alias = "iso_extent_valid")]
    fn extent_valid(&self, extent: u32, size: u32) -> bool {
        let sectors = u64::from(size).div_ceil(ISO_LOGICAL_SECTOR_SIZE);
        extent <= self.volume_sectors && sectors <= u64::from(self.volume_sectors - extent)
    }

    #[doc(alias = "iso_parse_directory")]
    fn parse_directory(
        &mut self,
        parent: &str,
        extent: u32,
        directory_size: u32,
        depth: u32,
    ) -> Result<(), IsoError> {
        if depth > ISO_MAX_DIRECTORY_DEPTH
            || directory_size == 0
            || directory_size > ISO_MAX_DIRECTORY_BYTES
            || !self.extent_valid(extent, directory_size)
        {
            return Err(IsoError::InvalidDirectory);
        }
        self.mark_directory_visited(extent)?;
        let mut data = vec![0u8; directory_size as usize];
        let mut scratch = vec![0u8; (ISO_READ_BATCH_SECTORS * ISO_RAW_SECTOR_SIZE) as usize];
        if !iso_read_logical(
            self.source,
            self.stream,
            u64::from(extent) * ISO_LOGICAL_SECTOR_SIZE,
            &mut data,
            Some(&mut scratch),
        ) {
            return Err(IsoError::Read);
        }

        // Directory records never span logical-sector padding. Recursing here builds the immutable lookup table
        // used by open, stat, and glob.
        let sector_size = ISO_LOGICAL_SECTOR_SIZE as usize;
        let mut offset = 0usize;
        while offset < data.len() {
            let record_size = usize::from(data[offset]);
            if record_size == 0 {
                offset = (offset / sector_size + 1) * sector_size;
                continue;
            }
            if record_size > data.len() - offset || record_size > sector_size - offset % sector_size
            {
                return Err(IsoError::InvalidRecord);
            }
            let record = &data[offset..offset + record_size];
            if record_size < 34 || 33 + usize::from(record[32]) > record_size {
                return Err(IsoError::TruncatedRecord);
            }
            if record[32] == 1 && (record[33] == 0 || record[33] == 1) {
                offset += record_size;
                continue;
            }
            if record[25] & 0x80 != 0 || record[26] != 0 || record[27] != 0 {
                return Err(IsoError::MultiExtent);
            }
            let (Some((child_extent, child_size)), Some(name)) =
                (iso_record_values(record), iso_entry_name(record))
            else {
                return Err(IsoError::InvalidEntry);
            };
            if !self.extent_valid(child_extent, child_size) {
                return Err(IsoError::InvalidEntry);
            }
            let path = if parent.is_empty() {
                name
            } else {
                format!("{parent}/{name}")
            };
            let is_directory = record[25] & 0x02 != 0;
            self.add_entry(path.clone(), child_extent, child_size, is_directory)?;
            if is_directory {
                self.parse_directory(&path, child_extent, child_size, depth + 1)?;
            }
            offset += record_size;
        }
        Ok(())
    }
}

fn iso_compare_entries(left: &IsoEntry, right: &IsoEntry) -> Ordering {
    sdl::strcasecmp(&left.path, &right.path).then_with(|| left.path.cmp(&right.path))
}

fn iso_parse_volume(source: &IsoSource, stream: &mut fs::File) -> Result<Vec<IsoEntry>, IsoError> {
    let mut descriptor = [0u8; ISO_LOGICAL_SECTOR_SIZE as usize];
    let mut scratch = vec![0u8; (ISO_READ_BATCH_SECTORS * ISO_RAW_SECTOR_SIZE) as usize];
    let mut found_primary = false;
    for sector in 16..80u64 {
        if !iso_read_logical(
            source,
            stream,
            sector * ISO_LOGICAL_SECTOR_SIZE,
            &mut descriptor,
            Some(&mut scratch),
        ) {
            return Err(IsoError::Read);
        }
        if &descriptor[1..6] != b"CD001" || descriptor[6] != 1 {
            return Err(IsoError::InvalidVolumeDescriptor);
        }
        if descriptor[0] == 1 {
            found_primary = true;
            break;
        }
        if descriptor[0] == 255 {
            break;
        }
    }
    if !found_primary
        || iso_le32(&descriptor[80..]) != iso_be32(&descriptor[84..])
        || iso_le16(&descriptor[128..]) != iso_be16(&descriptor[130..])
        || u64::from(iso_le16(&descriptor[128..])) != ISO_LOGICAL_SECTOR_SIZE
    {
        return Err(IsoError::UnsupportedPrimaryDescriptor);
    }
    let volume_sectors = iso_le32(&descriptor[80..]);
    let available_sectors = (source.image_size - source.track_offset) / source.physical_sector_size;
    if volume_sectors == 0 || u64::from(volume_sectors) > available_sectors {
        return Err(IsoError::VolumeBeyondImage);
    }

    let mut build = IsoBuild {
        source,
        stream,
        volume_sectors,
        entries: Vec::new(),
        visited_directories: Vec::new(),
    };
    let root_record_size = usize::from(descriptor[156]);
    let root_record = &descriptor[156..(156 + root_record_size).min(descriptor.len())];
    let Some((root_extent, root_size)) = iso_record_values(root_record) else {
        return Err(IsoError::InvalidDirectory);
    };
    if !build.extent_valid(root_extent, root_size) {
        return Err(IsoError::InvalidDirectory);
    }
    build.add_entry(String::new(), root_extent, root_size, true)?;
    build.parse_directory("", root_extent, root_size, 0)?;

    let mut entries = build.entries;
    entries.sort_by(iso_compare_entries);
    if entries
        .windows(2)
        .any(|pair| sdl::strcasecmp(&pair[0].path, &pair[1].path) == Ordering::Equal)
    {
        return Err(IsoError::AmbiguousPaths);
    }
    Ok(entries)
}

/// `iso_wildcard_match`: `*` and `?` wildcards with ASCII case folding.
fn iso_wildcard_match(pattern: &[u8], value: &[u8], case_insensitive: bool) -> bool {
    let mut pattern = pattern;
    let mut value = value;
    while let Some((&p, pattern_rest)) = pattern.split_first() {
        if p == b'*' {
            let stars = pattern_rest
                .iter()
                .take_while(|&&byte| byte == b'*')
                .count();
            let rest = &pattern_rest[stars..];
            if rest.is_empty() {
                return true;
            }
            return (0..value.len())
                .any(|start| iso_wildcard_match(rest, &value[start..], case_insensitive));
        }
        let Some((&v, value_rest)) = value.split_first() else {
            return false;
        };
        let matches = if case_insensitive {
            p.eq_ignore_ascii_case(&v)
        } else {
            p == v
        };
        if p != b'?' && !matches {
            return false;
        }
        pattern = pattern_rest;
        value = value_rest;
    }
    value.is_empty()
}

impl Iso9660 {
    /// Mounts a disc image or a single-track `.cue`/`.ins` descriptor.
    #[doc(alias = "AeronIso9660_Open")]
    pub(crate) fn open(source_path: &str) -> Result<Self, IsoError> {
        if source_path.is_empty() {
            return Err(IsoError::EmptyPath);
        }
        let (mut source, mut stream) = iso_open_source(source_path)?;
        iso_probe_layout(&mut source, &mut stream)?;
        let entries = iso_parse_volume(&source, &mut stream)?;
        Ok(Self { source, entries })
    }

    #[doc(alias = "iso_find_entry")]
    fn find_entry(&self, path: &str, case_insensitive: bool) -> Option<&IsoEntry> {
        let normalized = iso_normalize_path(path)?;
        let low = self
            .entries
            .partition_point(|entry| sdl::strcasecmp(&entry.path, &normalized) == Ordering::Less);
        self.entries[low..]
            .iter()
            .take_while(|entry| sdl::strcasecmp(&entry.path, &normalized) == Ordering::Equal)
            .find(|entry| case_insensitive || entry.path == normalized)
    }

    /// Opens a file for reading. Directories and missing paths give `None`.
    #[doc(alias = "AeronIso9660_OpenFile")]
    pub(crate) fn open_file(&self, path: &str, case_insensitive: bool) -> Option<IsoFile> {
        let entry = self.find_entry(path, case_insensitive)?;
        if entry.is_directory {
            return None;
        }
        let image = fs::File::open(&self.source.image_path).ok()?;
        let scratch = if self.source.physical_sector_size == ISO_RAW_SECTOR_SIZE {
            vec![0; (ISO_READ_BATCH_SECTORS * ISO_RAW_SECTOR_SIZE) as usize]
        } else {
            Vec::new()
        };
        Some(IsoFile {
            source: self.source.clone(),
            image,
            logical_offset: u64::from(entry.extent) * ISO_LOGICAL_SECTOR_SIZE,
            size: u64::from(entry.size),
            position: 0,
            scratch,
            cached_first_sector: 0,
            cached_sector_count: 0,
        })
    }

    #[doc(alias = "AeronIso9660_Stat")]
    pub(crate) fn stat(&self, path: &str, case_insensitive: bool) -> Option<FileInfo> {
        let entry = self.find_entry(path, case_insensitive)?;
        Some(FileInfo {
            size: u64::from(entry.size),
            exists: true,
            is_directory: entry.is_directory,
        })
    }

    /// Calls `callback` for each direct child of `directory` matching `pattern`, in case-insensitive path order.
    /// Returns `false` when the directory is missing or the callback stops the iteration.
    #[doc(alias = "AeronIso9660_Glob")]
    pub(crate) fn glob(
        &self,
        directory: &str,
        pattern: &str,
        flags: GlobFlags,
        root_case_insensitive: bool,
        callback: &mut dyn FnMut(&VfsEntry<'_>) -> bool,
    ) -> bool {
        let directory = if directory.is_empty() { "." } else { directory };
        let pattern = if pattern.is_empty() { "*" } else { pattern };
        let Some(parent) = self.find_entry(directory, root_case_insensitive) else {
            return false;
        };
        if !parent.is_directory {
            return false;
        }
        let prefix_length = if parent.path.is_empty() {
            0
        } else {
            parent.path.len() + 1
        };
        let match_case_insensitive = flags.contains(GlobFlags::CASE_INSENSITIVE);
        let mut want_files = flags.contains(GlobFlags::FILES);
        let mut want_directories = flags.contains(GlobFlags::DIRECTORIES);
        if !want_files && !want_directories {
            want_files = true;
            want_directories = true;
        }

        for entry in &self.entries {
            if std::ptr::eq(entry, parent) || entry.path.len() <= prefix_length {
                continue;
            }
            let path = entry.path.as_bytes();
            if prefix_length != 0
                && (!path[..prefix_length - 1]
                    .eq_ignore_ascii_case(&parent.path.as_bytes()[..prefix_length - 1])
                    || path[prefix_length - 1] != b'/')
            {
                continue;
            }
            let name = &entry.path[prefix_length..];
            let wanted = if entry.is_directory {
                want_directories
            } else {
                want_files
            };
            if name.contains('/')
                || !iso_wildcard_match(pattern.as_bytes(), name.as_bytes(), match_case_insensitive)
                || !wanted
            {
                continue;
            }
            let public_entry = VfsEntry {
                name,
                size: u64::from(entry.size),
                is_directory: entry.is_directory,
            };
            if !callback(&public_entry) {
                return false;
            }
        }
        true
    }
}

impl IsoFile {
    #[doc(alias = "iso_file_size")]
    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    #[doc(alias = "iso_file_seek")]
    pub(crate) fn seek(&mut self, position: SeekFrom) -> Option<u64> {
        let (base, offset) = match position {
            SeekFrom::Start(offset) => (0, i64::try_from(offset).ok()?),
            SeekFrom::Current(offset) => (self.position, offset),
            SeekFrom::End(offset) => (self.size, offset),
        };
        let position = base.checked_add_signed(offset)?;
        if position > self.size {
            return None;
        }
        self.position = position;
        Some(position)
    }

    pub(crate) fn tell(&self) -> u64 {
        self.position
    }

    /// Reads MODE1/2352 data through a short cached window of physical sectors.
    #[doc(alias = "iso_file_read_raw")]
    fn read_raw(&mut self, mut offset: u64, destination: &mut [u8]) -> bool {
        // Small recovered-code reads commonly stay within the same archive area. Cache a short physical-sector
        // window per file handle.
        let mut output = destination;
        while !output.is_empty() {
            let sector = offset / ISO_LOGICAL_SECTOR_SIZE;
            let inside = offset % ISO_LOGICAL_SECTOR_SIZE;
            if self.cached_sector_count == 0
                || sector < self.cached_first_sector
                || sector - self.cached_first_sector >= self.cached_sector_count
            {
                let total_sectors =
                    (self.source.image_size - self.source.track_offset) / ISO_RAW_SECTOR_SIZE;
                if sector >= total_sectors {
                    return false;
                }
                let sectors = (total_sectors - sector).min(ISO_READ_BATCH_SECTORS);
                let physical = self.source.track_offset + sector * ISO_RAW_SECTOR_SIZE;
                let physical_size = (sectors * ISO_RAW_SECTOR_SIZE) as usize;
                if !iso_seek_and_read(
                    &mut self.image,
                    physical,
                    &mut self.scratch[..physical_size],
                ) {
                    return false;
                }
                self.cached_first_sector = sector;
                self.cached_sector_count = sectors;
            }
            let cached_index = sector - self.cached_first_sector;
            let chunk = (output.len() as u64).min(ISO_LOGICAL_SECTOR_SIZE - inside) as usize;
            let from =
                (cached_index * ISO_RAW_SECTOR_SIZE + ISO_RAW_PAYLOAD_OFFSET + inside) as usize;
            output[..chunk].copy_from_slice(&self.scratch[from..from + chunk]);
            offset += chunk as u64;
            output = &mut output[chunk..];
        }
        true
    }

    #[doc(alias = "iso_file_read")]
    pub(crate) fn read(&mut self, destination: &mut [u8]) -> (usize, IoStatus) {
        let available = self.size - self.position;
        let count = (destination.len() as u64).min(available) as usize;
        let offset = self.logical_offset + self.position;
        let read_ok = if self.source.physical_sector_size == ISO_RAW_SECTOR_SIZE {
            self.read_raw(offset, &mut destination[..count])
        } else {
            iso_read_logical(
                &self.source,
                &mut self.image,
                offset,
                &mut destination[..count],
                None,
            )
        };
        if count != 0 && !read_ok {
            return (0, IoStatus::Error);
        }
        self.position += count as u64;
        let status = if count < destination.len() {
            IoStatus::Eof
        } else {
            IoStatus::Ready
        };
        (count, status)
    }
}

/// Builds synthetic ISO9660 images for tests.
#[cfg(test)]
pub(crate) mod test_image {
    use std::collections::BTreeMap;

    const SECTOR: usize = 2048;

    /// One entry of a synthetic image: a directory (no data) or a file.
    pub(crate) enum Node<'a> {
        Directory(&'a str),
        File(&'a str, &'a [u8]),
    }

    struct Directory {
        children: Vec<(String, bool, usize)>,
        extent: u32,
    }

    fn record(name: &[u8], extent: u32, size: u32, directory: bool) -> Vec<u8> {
        let mut record = vec![0u8; 33];
        record[2..6].copy_from_slice(&extent.to_le_bytes());
        record[6..10].copy_from_slice(&extent.to_be_bytes());
        record[10..14].copy_from_slice(&size.to_le_bytes());
        record[14..18].copy_from_slice(&size.to_be_bytes());
        record[25] = if directory { 0x02 } else { 0 };
        record[28] = 1;
        record[31] = 1;
        record[32] = name.len() as u8;
        record.extend_from_slice(name);
        if record.len() % 2 == 1 {
            record.push(0);
        }
        record[0] = record.len() as u8;
        record
    }

    /// A MODE1/2048 image holding `nodes`. File names get a `;1` version suffix. Every directory fits one sector.
    pub(crate) fn build(nodes: &[Node<'_>]) -> Vec<u8> {
        let mut directories: BTreeMap<String, Directory> = BTreeMap::new();
        directories.insert(
            String::new(),
            Directory {
                children: Vec::new(),
                extent: 0,
            },
        );
        for (index, node) in nodes.iter().enumerate() {
            let (path, is_directory) = match node {
                Node::Directory(path) => (*path, true),
                Node::File(path, _) => (*path, false),
            };
            let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
            directories
                .get_mut(parent)
                .expect("parent directory listed before its children")
                .children
                .push((name.to_owned(), is_directory, index));
            if is_directory {
                directories.insert(
                    path.to_owned(),
                    Directory {
                        children: Vec::new(),
                        extent: 0,
                    },
                );
            }
        }

        let mut next = 18u32;
        for directory in directories.values_mut() {
            directory.extent = next;
            next += 1;
        }
        let mut file_extents = BTreeMap::new();
        for (index, node) in nodes.iter().enumerate() {
            if let Node::File(_, data) = node {
                file_extents.insert(index, next);
                next += data.len().div_ceil(SECTOR).max(1) as u32;
            }
        }
        let total = next as usize;
        let mut image = vec![0u8; total * SECTOR];

        let root_extent = directories[""].extent;
        let pvd = &mut image[16 * SECTOR..17 * SECTOR];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        pvd[80..84].copy_from_slice(&(total as u32).to_le_bytes());
        pvd[84..88].copy_from_slice(&(total as u32).to_be_bytes());
        pvd[128..130].copy_from_slice(&(SECTOR as u16).to_le_bytes());
        pvd[130..132].copy_from_slice(&(SECTOR as u16).to_be_bytes());
        let root = record(&[0], root_extent, SECTOR as u32, true);
        pvd[156..156 + root.len()].copy_from_slice(&root);
        let terminator = &mut image[17 * SECTOR..18 * SECTOR];
        terminator[0] = 255;
        terminator[1..6].copy_from_slice(b"CD001");
        terminator[6] = 1;

        for (path, directory) in &directories {
            let parent_extent = match path.rsplit_once('/') {
                Some((parent, _)) => directories[parent].extent,
                None if path.is_empty() => directory.extent,
                None => root_extent,
            };
            let mut bytes = record(&[0], directory.extent, SECTOR as u32, true);
            bytes.extend(record(&[1], parent_extent, SECTOR as u32, true));
            for (name, is_directory, index) in &directory.children {
                if *is_directory {
                    let child = format!("{}{}{name}", path, if path.is_empty() { "" } else { "/" });
                    bytes.extend(record(
                        name.as_bytes(),
                        directories[&child].extent,
                        SECTOR as u32,
                        true,
                    ));
                } else {
                    let Node::File(_, data) = nodes[*index] else {
                        unreachable!()
                    };
                    let versioned = format!("{name};1");
                    bytes.extend(record(
                        versioned.as_bytes(),
                        file_extents[index],
                        data.len() as u32,
                        false,
                    ));
                }
            }
            assert!(bytes.len() <= SECTOR);
            let start = directory.extent as usize * SECTOR;
            image[start..start + bytes.len()].copy_from_slice(&bytes);
        }
        for (index, extent) in file_extents {
            let Node::File(_, data) = nodes[index] else {
                unreachable!()
            };
            let start = extent as usize * SECTOR;
            image[start..start + data.len()].copy_from_slice(data);
        }
        image
    }

    /// Wraps each 2048-byte sector in a 2352-byte MODE1 raw sector.
    pub(crate) fn to_raw(image: &[u8]) -> Vec<u8> {
        let mut raw = Vec::with_capacity(image.len() / SECTOR * 2352);
        for sector in image.chunks(SECTOR) {
            raw.extend_from_slice(&[0xff; 16]);
            raw.extend_from_slice(sector);
            raw.extend_from_slice(&[0xee; 288]);
        }
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::test_image::{Node, build, to_raw};
    use super::*;

    fn nodes() -> Vec<Node<'static>> {
        vec![
            Node::File("README.TXT", b"hello"),
            Node::Directory("RESOURCE"),
            Node::File("RESOURCE/EMPIRE.LFD", &[7u8; 5000]),
            Node::File("RESOURCE/FONTS.LFD", b"font"),
            Node::Directory("RESOURCE/SUB"),
            Node::File("RESOURCE/SUB/DEEP.DAT", b"deep"),
        ]
    }

    fn write(dir: &tempfile::TempDir, name: &str, data: &[u8]) -> String {
        let path = dir.path().join(name);
        fs::write(&path, data).unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn read_all(file: &mut IsoFile) -> Vec<u8> {
        let mut data = vec![0; file.size() as usize];
        assert_eq!(file.read(&mut data), (data.len(), IoStatus::Ready));
        data
    }

    fn check_image(iso: &Iso9660) {
        let mut file = iso.open_file("resource/empire.lfd", true).unwrap();
        assert_eq!(read_all(&mut file), vec![7u8; 5000]);
        assert!(iso.open_file("resource/empire.lfd", false).is_none());
        assert!(iso.open_file("/RESOURCE//./EMPIRE.LFD", false).is_some());
        assert!(iso.open_file("RESOURCE/../README.TXT", true).is_none());
        assert!(iso.open_file("RESOURCE", true).is_none());

        let info = iso.stat("RESOURCE/SUB", false).unwrap();
        assert!(info.exists && info.is_directory);
        assert_eq!(iso.stat("README.TXT", false).unwrap().size, 5);

        let mut names = Vec::new();
        assert!(iso.glob(
            "resource",
            "*.lfd",
            GlobFlags::CASE_INSENSITIVE,
            true,
            &mut |entry| {
                names.push(entry.name.to_owned());
                true
            }
        ));
        assert_eq!(names, ["EMPIRE.LFD", "FONTS.LFD"]);

        names.clear();
        assert!(
            iso.glob("", "", GlobFlags::DIRECTORIES, false, &mut |entry| {
                names.push(entry.name.to_owned());
                true
            })
        );
        assert_eq!(names, ["RESOURCE"]);
        assert!(!iso.glob("RESOURCE", "*", GlobFlags::empty(), false, &mut |_| false));
    }

    #[test]
    fn mode1_2048_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "game.iso", &build(&nodes()));
        check_image(&Iso9660::open(&path).unwrap());
    }

    #[test]
    fn mode1_2352_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "game.bin", &to_raw(&build(&nodes())));
        let iso = Iso9660::open(&path).unwrap();
        check_image(&iso);

        // Reads that straddle sectors and the cached window.
        let mut file = iso.open_file("RESOURCE/EMPIRE.LFD", false).unwrap();
        assert_eq!(file.seek(SeekFrom::Start(2040)), Some(2040));
        let mut chunk = [0u8; 16];
        assert_eq!(file.read(&mut chunk), (16, IoStatus::Ready));
        assert_eq!(chunk, [7u8; 16]);
        assert_eq!(file.seek(SeekFrom::End(-4)), Some(4996));
        let mut tail = [0u8; 8];
        assert_eq!(file.read(&mut tail), (4, IoStatus::Eof));
        assert_eq!(file.seek(SeekFrom::Current(1)), None);
        assert_eq!(file.seek(SeekFrom::End(0)), Some(5000));
    }

    #[test]
    fn cue_descriptor_with_pregap() {
        let dir = tempfile::tempdir().unwrap();
        let mut raw = vec![0u8; 2 * 2352];
        raw.extend(to_raw(&build(&nodes())));
        write(&dir, "game.gog", &raw);
        let cue = write(
            &dir,
            "game.ins",
            b"FILE \"game.gog\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:02\r\n",
        );
        check_image(&Iso9660::open(&cue).unwrap());
    }

    #[test]
    fn descriptor_errors() {
        let dir = tempfile::tempdir().unwrap();
        let cue = |text: &str| write(&dir, "x.cue", text.as_bytes());
        assert!(matches!(
            Iso9660::open(&cue("FILE game BINARY\n")),
            Err(IsoError::DescriptorFile)
        ));
        assert!(matches!(
            Iso9660::open(&cue("FILE \"a\" BINARY\nTRACK 0 MODE1/2048\n")),
            Err(IsoError::DescriptorTrack)
        ));
        assert!(matches!(
            Iso9660::open(&cue("FILE \"a\" BINARY\nTRACK 1 MODE1/2048\n")),
            Err(IsoError::DescriptorLayout)
        ));
        assert!(matches!(
            Iso9660::open(&cue(
                "FILE \"a\" BINARY\nTRACK 1 AUDIO\nINDEX 01 00:00:00\n"
            )),
            Err(IsoError::DescriptorMode)
        ));
        assert!(matches!(
            Iso9660::open(&cue(
                "FILE \"../a\" BINARY\nTRACK 1 MODE1/2048\nINDEX 01 00:00:00\n"
            )),
            Err(IsoError::DescriptorImagePath)
        ));
        assert!(matches!(Iso9660::open(""), Err(IsoError::EmptyPath)));
        let junk = write(&dir, "junk.iso", &[0u8; 40000]);
        assert!(matches!(
            Iso9660::open(&junk),
            Err(IsoError::UnsupportedImage)
        ));
    }

    #[test]
    fn ambiguous_paths_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let image = build(&[Node::File("A.TXT", b"1"), Node::File("a.txt", b"2")]);
        let path = write(&dir, "dup.iso", &image);
        assert!(matches!(
            Iso9660::open(&path),
            Err(IsoError::AmbiguousPaths)
        ));
    }

    #[test]
    fn wildcard_matching() {
        assert!(iso_wildcard_match(b"*.LFD", b"EMPIRE.LFD", false));
        assert!(iso_wildcard_match(b"**", b"", false));
        assert!(iso_wildcard_match(b"e?pire.lfd", b"EMPIRE.LFD", true));
        assert!(!iso_wildcard_match(b"*.lfd", b"EMPIRE.LFD", false));
        assert!(!iso_wildcard_match(b"EMPIRE", b"EMPIRE.LFD", false));
    }

    #[test]
    fn entry_names() {
        let mut record = vec![0u8; 33];
        record.extend_from_slice(b"FILE.;1");
        record[32] = 7;
        assert_eq!(iso_entry_name(&record).as_deref(), Some("FILE"));
        record[33] = 0x01;
        assert_eq!(iso_entry_name(&record), None);
    }
}
