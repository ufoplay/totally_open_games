//! Engine behaviour on synthetic sounds: markers, game callbacks re-entering the engine, deferred commands and
//! sequencer time.

use std::collections::HashMap;
use std::sync::Arc;

use imuse::{
    CallbackId, Command, Imuse, ImuseCmd, ImuseConfig, ImuseHost, MidiBackendKind, Opcode, Param,
    SoundId, SoundType,
};

/// An iMUSE MIDI container with one track: a program change and a note on channel 0, a LucasArts marker SysEx
/// after `marker_delay` ticks, then the note off and the end of the track.
fn song(marker: u8, marker_delay: u8) -> Vec<u8> {
    let track = [
        0x00,
        0xC0,
        0x05, // program 5 on channel 0
        0x00,
        0x90,
        60,
        100, // note on
        marker_delay,
        0xF0,
        0x03,
        0x7D,
        0x00,
        marker, // marker SysEx
        0x60,
        0x80,
        60,
        0, // note off
        0x00,
        0xFF,
        0x2F,
        0x00, // end of track
    ];
    let mut chunk = b"MTrk".to_vec();
    chunk.extend_from_slice(&(track.len() as u32).to_be_bytes());
    chunk.extend_from_slice(&track);
    let mut file = b"MIDI".to_vec();
    file.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
    file.extend(chunk);
    file
}

#[derive(Default)]
struct Host {
    sounds: HashMap<isize, Arc<[u8]>>,
    fired: Vec<(u32, i32)>,
}

impl ImuseHost for Host {
    fn sound(&mut self, sound: SoundId) -> Option<Arc<[u8]>> {
        self.sounds.get(&sound.0).cloned()
    }

    fn trigger_callback(
        &mut self,
        imuse: &mut Imuse,
        callback: CallbackId,
        marker: i32,
        _args: &[isize; 10],
    ) {
        self.fired.push((callback.0, marker));
        // Like the games' music callbacks: start the next song from inside the tick.
        imuse.start_sound(self, SoundId(2), 64).unwrap();
    }
}

fn engine() -> (Imuse, Host) {
    let (imuse, _renderer) = Imuse::new(ImuseConfig::default(), MidiBackendKind::None).unwrap();
    let host = Host {
        sounds: HashMap::from([(1, Arc::from(song(5, 0x30))), (2, Arc::from(song(9, 0x7F)))]),
        ..Host::default()
    };
    (imuse, host)
}

#[test]
fn marker_fires_a_callback_that_starts_the_next_song_mid_tick() {
    let (mut imuse, mut host) = engine();
    let cmd = ImuseCmd::new(Command::Callback(CallbackId(7)), &[]);
    imuse.set_trigger(SoundId(1), 5, &cmd).unwrap();
    imuse.start_sound(&mut host, SoundId(1), 64).unwrap();
    assert_eq!(
        imuse.get_param(SoundId(1), Param::SoundType),
        SoundType::Midi as i32
    );
    // The marker comes 0x30 ticks in (~50 ms at 120 BPM), the end of track at 0x90 (~150 ms).
    for _ in 0..5 {
        imuse.advance(&mut host, 16_667);
    }
    assert_eq!(
        host.fired,
        [(7, 5)],
        "the trigger fires once, with its marker"
    );
    assert_eq!(imuse.get_param(SoundId(1), Param::SoundMarker), 5);
    assert_eq!(
        imuse.get_param(SoundId(2), Param::SoundPlayCount),
        1,
        "the callback started song 2"
    );
    assert_eq!(
        imuse.check_trigger(SoundId(1), -1, None),
        0,
        "triggers are one-shot"
    );
}

#[test]
fn songs_end_at_their_end_of_track() {
    let (mut imuse, mut host) = engine();
    imuse.start_sound(&mut host, SoundId(1), 64).unwrap();
    assert_eq!(imuse.next_sound(SoundId(0)), SoundId(1));
    // 120 BPM at 480 ticks per beat: the song lasts 0x30 + 0x60 ticks, well under a second.
    for _ in 0..60 {
        imuse.advance(&mut host, 16_667);
    }
    assert_eq!(imuse.get_param(SoundId(1), Param::SoundPlayCount), 0);
    assert_eq!(imuse.next_sound(SoundId(0)), SoundId(0));
}

#[test]
fn deferred_commands_run_after_their_ticks() {
    let (mut imuse, mut host) = engine();
    imuse.start_sound(&mut host, SoundId(2), 64).unwrap();
    let stop = ImuseCmd::new(Command::Op(Opcode::StopSound), &[2]);
    imuse.defer_command(3, &stop).unwrap();
    // 33 ms of host time is four services, 32240 µs of logical time: one 60 Hz tick.
    imuse.advance(&mut host, 33_000);
    assert_eq!(imuse.get_param(SoundId(2), Param::SoundPlayCount), 1);
    // Four more services: two more ticks, and the command runs.
    imuse.advance(&mut host, 34_000);
    assert_eq!(imuse.get_param(SoundId(2), Param::SoundPlayCount), 0);
}

#[test]
fn time_parameters_follow_the_sequencer() {
    let (mut imuse, mut host) = engine();
    imuse.start_sound(&mut host, SoundId(2), 64).unwrap();
    assert_eq!(imuse.get_param(SoundId(2), Param::MidiChunk), 1);
    assert_eq!(imuse.get_param(SoundId(2), Param::MidiMeasure), 1);
    assert_eq!(imuse.get_param(SoundId(2), Param::MidiBeat), 1);
    imuse.advance(&mut host, 8_000);
    // One service at 120 BPM: 480 * 8060 / 500000 ≈ 7.7 ticks.
    assert_eq!(imuse.get_param(SoundId(2), Param::MidiTick), 7);
}
