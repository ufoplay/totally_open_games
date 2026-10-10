//! Renders one sound of an LFD archive to a 16-bit stereo WAV, headless.
//!
//! `imuse_render <archive.LFD> <[TYPE:]entry name> <out.wav> [seconds] [fm4|fm4-xwing|adlib|none|<soundfont.sf2>]`
//!
//! An archive can hold one song in several arrangements (`GMID`, `ADLB`, `RLND`); `ADLB:rebels` picks one.
//!
//! The engine advances in 60 Hz frames and the renderer mixes the matching audio in step, as a game loop would.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use aeron::asset::lfd::Lfd;
use imuse::{
    FluidSynthConfig, Imuse, ImuseConfig, ImuseHost, MidiBackendKind, Param, SoundFontSource,
    SoundId,
};

const RATE: u32 = 44_100;
const FRAME_US: i32 = 16_667;

struct Host(HashMap<isize, Arc<[u8]>>);

impl ImuseHost for Host {
    fn sound(&mut self, sound: SoundId) -> Option<Arc<[u8]>> {
        self.0.get(&sound.0).cloned()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [archive, entry, out, rest @ ..] = args.as_slice() else {
        return Err(
            "usage: imuse_render <archive.LFD> <entry name> <out.wav> [seconds] [backend]".into(),
        );
    };
    let seconds: u32 = rest.first().map_or(Ok(10), |s| s.parse())?;
    let backend_arg = rest.get(1).map_or("fm4", String::as_str);

    let bytes = std::fs::read(archive)?;
    let lfd = Lfd::parse(&bytes)?;
    let (kind, name) = match entry.split_once(':') {
        Some((kind, name)) => (Some(kind.as_bytes()), name),
        None => (None, entry.as_str()),
    };
    let found = lfd
        .entries
        .iter()
        .find(|e| {
            e.name().eq_ignore_ascii_case(name.as_bytes())
                && kind.is_none_or(|kind| e.kind.bytes().eq_ignore_ascii_case(kind))
        })
        .ok_or_else(|| format!("{entry} is not in {archive}"))?;
    let data: Arc<[u8]> = Arc::from(found.data);

    let dialect =
        data.starts_with(b"GMD ") || data.starts_with(b"ADL ") || data.starts_with(b"ROL ");
    let backend = match backend_arg {
        "fm4" => MidiBackendKind::Fm4Opl3,
        "fm4-xwing" => MidiBackendKind::Fm4Opl3Xwing,
        "adlib" => MidiBackendKind::XwingAdlib,
        "none" => MidiBackendKind::None,
        path => MidiBackendKind::FluidSynth(FluidSynthConfig {
            soundfont: SoundFontSource::Path(path.into()),
            polyphony: 0,
        }),
    };
    let config = ImuseConfig {
        output_sample_rate: RATE as i32,
        // X-Wing's DOS93 dialects run on its PIT timing.
        xwing_timing: dialect || backend_arg == "adlib",
        ..ImuseConfig::default()
    };
    let (mut imuse, mut renderer) =
        Imuse::new(config, backend).map_err(|e| format!("cannot create the engine: {e}"))?;
    let mut host = Host(HashMap::from([(1, data)]));
    imuse
        .start_sound(&mut host, SoundId(1), 127)
        .map_err(|e| format!("cannot start {entry}: {e}"))?;

    let mut samples = Vec::new();
    let mut frame = Vec::new();
    let mut produced: u64 = 0;
    for tick in 1..=u64::from(seconds) * 60 {
        imuse.advance(&mut host, FRAME_US);
        // The audio frames due by the end of this video frame.
        let due = tick * u64::from(RATE) / 60 - produced;
        frame.resize(due as usize * 2, 0);
        renderer.mix_s16(&mut frame);
        samples.extend_from_slice(&frame);
        produced += due;
        if imuse.get_param(SoundId(1), Param::SoundPlayCount) == 0 && tick > 60 {
            break;
        }
    }

    let mut file = std::io::BufWriter::new(std::fs::File::create(out)?);
    let data_len = (samples.len() * 2) as u32;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_len).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&RATE.to_le_bytes())?;
    file.write_all(&(RATE * 4).to_le_bytes())?;
    file.write_all(&4u16.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    for sample in samples {
        file.write_all(&sample.to_le_bytes())?;
    }
    eprintln!("{out}: {:.1} s", produced as f64 / f64::from(RATE));
    Ok(())
}
