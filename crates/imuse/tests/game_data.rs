//! Tests on user-supplied game data (`TOG_DATA_XW`, `TOG_DATA_TIE`: `PATH`-style lists of install folders or GOG
//! disc images). Each test is skipped when its variable is unset.
//!
//! Every music resource is sequenced; a few are also synthesized and must make sound. Voices play through the wave
//! path.

use std::collections::HashMap;
use std::sync::Arc;

use aeron::asset::lfd::{FourCc, Lfd};
use aeron::vfs::{GlobFlags, RootOptions, Vfs, VfsConfig, VfsRoot};
use imuse::{Imuse, ImuseConfig, ImuseHost, ImuseRenderer, MidiBackendKind, Param, SoundId};

struct Host {
    sounds: HashMap<isize, Arc<[u8]>>,
}

impl ImuseHost for Host {
    fn sound(&mut self, sound: SoundId) -> Option<Arc<[u8]>> {
        self.sounds.get(&sound.0).cloned()
    }
}

/// The entries of the given types in every LFD archive of every data path named by `variable`.
fn entries(variable: &str, kinds: &[&[u8; 4]]) -> Vec<(String, [u8; 4], Vec<u8>)> {
    let Some(value) = std::env::var_os(variable) else {
        eprintln!("skipped: {variable} is not set");
        return Vec::new();
    };
    let mut out = Vec::new();
    for path in std::env::split_paths(&value).filter(|path| !path.as_os_str().is_empty()) {
        let path = path.to_str().expect("UTF-8 path").to_owned();
        let mut vfs = Vfs::new(&VfsConfig::default());
        if std::fs::metadata(&path).unwrap().is_dir() {
            vfs.set_root(VfsRoot::Asset, &path).unwrap();
        } else {
            vfs.set_disc_root(VfsRoot::Asset, &path).unwrap();
        }
        vfs.set_root_options(VfsRoot::Asset, RootOptions::CASE_INSENSITIVE_LOOKUP);
        let mut files = Vec::new();
        find_lfds(&mut vfs, "", &mut files);
        for file in files {
            let bytes = vfs.read_all(VfsRoot::Asset, &file, None).unwrap();
            let lfd = Lfd::parse(&bytes).unwrap();
            for entry in &lfd.entries {
                if kinds.iter().any(|kind| entry.kind == FourCc::new(**kind)) {
                    let name = format!("{path}:{file}:{}", String::from_utf8_lossy(entry.name()));
                    out.push((name, entry.kind.bytes(), entry.data.to_vec()));
                }
            }
        }
    }
    out
}

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

const FRAME_US: i32 = 16_667;
const RATE: usize = 44_100;

/// Plays one sound for `seconds` of engine time. With `render`, also mixes audio in step and returns its peak.
fn play(
    name: &str,
    backend: MidiBackendKind,
    xwing: bool,
    data: &[u8],
    seconds: u32,
    render: bool,
) -> (bool, i16) {
    let config = ImuseConfig {
        xwing_timing: xwing,
        ..ImuseConfig::default()
    };
    let (mut imuse, mut renderer): (Imuse, ImuseRenderer) = Imuse::new(config, backend).unwrap();
    let mut host = Host {
        sounds: HashMap::from([(1, Arc::from(data))]),
    };
    if let Err(error) = imuse.start_sound(&mut host, SoundId(1), 64) {
        panic!(
            "{name}: start_sound failed: {error} (header {:02x?})",
            &data[..data.len().min(16)]
        );
    }
    let mut peak = 0i16;
    let mut buffer = vec![0i16; RATE / 60 * 2];
    let mut alive = false;
    for _ in 0..seconds * 60 {
        imuse.advance(&mut host, FRAME_US);
        alive |= imuse.get_param(SoundId(1), Param::SoundPlayCount) > 0;
        if render {
            renderer.mix_s16(&mut buffer);
            peak = peak.max(buffer.iter().map(|s| s.saturating_abs()).max().unwrap_or(0));
        }
    }
    (alive, peak)
}

fn check_music(variable: &str, kind: &[u8; 4], backend: MidiBackendKind, xwing: bool) {
    let music = entries(variable, &[kind]);
    if music.is_empty() {
        return;
    }
    let mut audible = 0;
    // A few resources of music types are not iMUSE containers; the engine rejects them as upstream does.
    let (music, foreign): (Vec<_>, Vec<_>) = music.into_iter().partition(|(_, _, data)| {
        [b"MIDI", b"GMD ", b"ADL ", b"ROL "]
            .iter()
            .any(|magic| data.starts_with(*magic))
    });
    for (index, (name, _, data)) in music.iter().enumerate() {
        // Synthesis is slow in debug builds: render every 50th song only.
        let render = index % 50 == 0;
        let (alive, peak) = play(name, backend.clone(), xwing, data, 5, render);
        assert!(alive, "{name}: did not play");
        if render {
            assert!(peak > 0, "{name}: silent");
            audible += 1;
        }
    }
    eprintln!(
        "{variable}: {} {} songs sequenced, {audible} synthesized, {} foreign resources skipped",
        music.len(),
        String::from_utf8_lossy(kind),
        foreign.len()
    );
}

fn check_voices(variable: &str) {
    let voices = entries(variable, &[b"VOIC"]);
    for (name, _, data) in voices.iter().step_by(25) {
        let (alive, peak) = play(name, MidiBackendKind::None, false, data, 1, true);
        assert!(alive, "{name}: did not play");
        assert!(peak > 0, "{name}: silent");
    }
    eprintln!("{variable}: {} voices, every 25th played", voices.len());
}

#[test]
fn game_data_tie_music() {
    check_music("TOG_DATA_TIE", b"GMID", MidiBackendKind::Fm4Opl3, false);
}

#[test]
fn game_data_xw_general_midi_music() {
    check_music("TOG_DATA_XW", b"GMID", MidiBackendKind::Fm4Opl3Xwing, true);
}

#[test]
fn game_data_xw_adlib_music() {
    check_music("TOG_DATA_XW", b"ADLB", MidiBackendKind::XwingAdlib, true);
}

#[test]
fn game_data_tie_voices() {
    check_voices("TOG_DATA_TIE");
}

/// TIE's music on the SoundFont backend; skipped unless `TOG_SOUNDFONT` names a SoundFont.
#[test]
fn game_data_tie_music_soundfont() {
    let Some(path) = std::env::var_os("TOG_SOUNDFONT") else {
        eprintln!("skipped: TOG_SOUNDFONT is not set");
        return;
    };
    assert!(imuse::fluidsynth_soundfont_is_valid(path.as_ref()));
    let backend = MidiBackendKind::FluidSynth(imuse::FluidSynthConfig {
        soundfont: imuse::SoundFontSource::Data(Arc::from(std::fs::read(&path).unwrap())),
        polyphony: 0,
    });
    let music = entries("TOG_DATA_TIE", &[b"GMID"]);
    for (name, _, data) in music.iter().step_by(100) {
        let (alive, peak) = play(name, backend.clone(), false, data, 3, true);
        assert!(alive, "{name}: did not play");
        assert!(peak > 0, "{name}: silent");
    }
}
