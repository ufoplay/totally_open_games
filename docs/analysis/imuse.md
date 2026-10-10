# imuse — upstream analysis

These notes record a reading of upstream `elyosh/imuse` at commit `09d3da46e004413635a58a8646fa01a27bc745b1` (v0.5.1,
ABI 5), made on 2026-10-10. They are reference material for the port; [PLAN.md](../PLAN.md) holds the decisions.

## Shape

- **Language:** C11, plus one C++23 file (SC-55).
- **Own code:** about 14k LOC.
- **State:** a single `imuse_t` per session holds everything (`internal/state.h`). There are no mutable globals.
- **Dependencies:** no SDL or Aeron, and no threads of its own.
- **Missing pieces:** no tests. `docs/imuse-state-format.md` is referenced but absent.

## Modules by layer

| Layer | LOC | Modules |
|---|---|---|
| Public API | ~2.1k | `state.c` (create/destroy), `commands.c` (Init/Terminate cascade, `imuse_advance`, start/stop/param/hook, `ImCommands_ExecOpcode`), `lolevel.c` (trigger/defer/fade/MIDI jump/scan/send/share/stream), `hilevel.c` (per-group volume/start wrappers, wave-fill setter), `groups.c` (16 group volumes), `debug.c`, `version.c`, `mix.c` (`imuse_mix_f32/s16`) |
| Sequencer | ~3.9k | `midi.c` (facade, re-entry lock counter, pause), `players.c` (904 lines; 8 players × 16 virtual channels, Set/GetParam, CCs, part allocator `MidiSetupParts`, save/restore replay), `slots.c` (32 voice pairs, note bitmaps, drum channel 9), `sustain.c` (jump bridge carrying ringing notes across jumps; 24-node pool), `seq.c` (SMF walker, tempo, jump/scan, LucasArts 0x7D SysEx), `triggers.c` (16 marker triggers, 8 deferred commands), `fades.c` (16 Bresenham ramps), `utils.c`, `timer.c` |
| X-Wing dialect | ~0.3k | `xwing.c`: DOS93 GMD/ADL/ROL container validation, legacy hooks, instrument SysEx, loops, MT-32 writes |
| MIDI backends | ~3.9k | `midi_backend.c` (vtable fan-out), `gmidi_driver.c` (GM policy + 12-voice allocator, shared by FluidSynth and SC-55), `fm4_driver.c` + `fm4_data.c` (TIE FM4 OPL driver + tone banks), `midi_fm4_opl3.c` (FM4 + Nuked-OPL3, with an X-Wing DOS94 bank variant), `midi_xwing_adlib.c` (DOS93 ADLIB.IMS 9-voice OPL2), `midi_mt32.c`, `midi_fluidsynth.c`, `midi_nuked_sc55.cpp`, `internal/synth_queue.h` (SPSC events) |
| Digital audio | ~2.7k | `wave.c`, `tracks.c` (16 voices, priority preemption), `dispatch.c` (VOC parser, cursor, resampling, stream crossfade), `streamer.c` (2 ring-buffered streams via host read/seek), `mixer.c` + `internal/mixer_tables.h` (int16 mix, pan/volume LUTs, 8-bit companding), `wave_renderer.c` (SPSC ring of u8 halves, zero-order hold, optional SB16 FIR) |
| Files | ~0.6k | `files.c` (host wrappers, IFF chunk walker), `filelist.c` (refcounted name→handle table of 10 entries) |
| State | ~0.4k | `state_io.c`: **not called by any game, so not ported** |

## Third party

- **Nuked-OPL3-fast:** 3.3k LOC, LGPL-2.1. Built with `OPL_WF_TABLE_RUNTIME=0`, `OPL_ENABLE_STEREOEXT=0`,
  `OPL_COMPAT_OLD_EG=0`, `OPL_COMPAT_DEFERRED_4OP_ALG=0`.
- **mt32emu (Munt):** about 17.5k LOC of C++. Dropped.
- **nuked-sc55:** a submodule of about 10k+ LOC of C++23, plus SpeexDSP. Dropped.
- **libfluidsynth:** a system library. Replaced by `rustysynth`.

## Public API and seam

- **Create:** `imuse_create(const ImuseHost*, const ImuseConfig*, ImuseMidiBackend*)`. The library takes ownership of
  the backend.
- **Backend factories:**
  - `imuse_fm4_opl3_backend_create`, `imuse_fm4_opl3_xwing_backend_create`;
  - `imuse_midi_xwing_adlib_create`;
  - `imuse_midi_mt32_create`, `imuse_fluidsynth_backend_create`, `imuse_nuked_sc55_backend_create`.
- **`ImuseConfig`:** `outputSampleRate` (44100), `waveSpeed` (11/22 kHz), `waveMixCount` (1..16, default 4),
  `xwingTiming`, `waveOutputFilter`.
- **Time:** the host calls `imuse_advance(im, usec_elapsed)` once per frame.
  - The core tick is 8000 µs, counted as 8060 µs of logical time. With `xwingTiming` it is 3433 µs (DOS93 PIT).
  - Fades and triggers run at 60 Hz and ducking at 10 Hz.
  - Catch-up is capped at 8 ticks.
- **Audio:** a pull model. `imuse_mix_f32/s16(im, buf, frames)` writes interleaved stereo: the MIDI backend overwrites
  the buffer, then the wave renderer adds on top. It is called from the audio thread, concurrently with `advance`.
- **`ImuseHost`:**
  - `getSoundPtrFunc(intptr_t id) -> void*` is required and has **no userdata**.
  - Optional: `seekFunc`, `readFunc`, `getBufInfoFunc`, `getSoundSizeFunc` (required for X-Wing),
    `logFunc(user, level, msg)`, and the filelist `load`/`unload`/`open`/`close` callbacks.
  - `imuse_set_wave_fill_cb` is stored but never called.
- **Trigger/defer callbacks:** function pointers passed as `ImuseCmd.opcode` when the value is ≥ 30.
- **Sound ids:** `soundId` is an `intptr_t` and may be a pointer. Buffers are re-fetched every tick and only offsets are
  kept.

## Backends used by the games

- **OpenXW** (`xw_runtime/audio/midi_backend.c`). The arrangement setting selects the backend:
  - `adlb` (1994 AdLib) → `adlib`;
  - `rlnd` (1994 Roland MT-32) → `mt32` (**dropped**: the Roland arrangement becomes unavailable);
  - otherwise (1994 General MIDI) → `fm4` (the X-Wing variant), `fluidsynth`, or `sc55`.
  - The 1998 version uses CD audio WAVs instead.
- **OpenTIE:** FM4 by default, FluidSynth or SC-55 optionally. MT-32 is disabled.

## Concurrency and unsafe-in-C patterns

- **Threads:** the game thread (`advance` + commands) and the audio thread (`mix`) share one `imuse_t`.
  - Synchronisation is lock-free SPSC: the wave renderer's halves (`_Atomic filledHalves`) and a 4096-entry event ring
    per backend.
  - Synth state (`opl3_chip` …) is touched only on the render side.
  - `ImMidi_Lock` and `ImWave_Lock` are re-entry counters, not mutexes.
- **Intrusive doubly-linked lists** through `ImUtils_ListAddItem(void* head)` (prev/next at offset 0) for players,
  tracks, sustain lists and filelist entries.
- **Back-pointer graph:** seq↔player, part→player/outChannel, part↔sibling, channel→part, track↔dispatch,
  dispatch→stream, and `activeNotes` pointing into module arrays.
- **Vtables** use struct-first downcasting. The 9-slot seq handler tables are `void*` cast to different signatures and
  compared by identity (`handlers[8] == ImParser_HandleMetaEvent`).
- **Upstream LP64 bugs:**
  - `imuse_defer_command` casts the opcode (a callback pointer) to `int`.
  - `dispatch.fadeBuf` is an `int32_t` holding a pointer, which breaks the stream-switch crossfade.
  - The SysEx sub-op 3 marker casts a payload pointer to `int`.
- **Host-endian read:** the VOC marker is read with an unaligned `*(int16_t*)`.
- **Borrowed buffers:** host-owned stream ring buffers are kept as raw pointers.

## Integer mappings → Rust types

- **Public:**
  - `ImuseOpcode` 0..29 (≥30 means a callback pointer).
  - `ImuseParam` 0x000..0x1800, including 0x777 (frequency). In 0x1100..0x1700 the low byte is the channel (kind mask
    0xFF00); `s_getParamCatLookup` is the reverse table.
  - `ImuseWaveStartFlags` are bitflags: LOOP=1, LOOP_FIRST_BLOCK=2.
  - `ImuseLogLevel` 0..3; `ImuseWaveOutputFilter` NONE/SB16.
  - `ImuseGroup` 0..4 (slots 5..15 reserved).
  - `IMUSE_SOUND_TYPE_*` -1/1/2.
  - Return codes: -1 general, -2 hook unsupported, -3 not found/starved, -4 no data, -5 bad argument, -6 table full;
    Init returns 48.
  - X-Wing legacy hook kinds 0..5 (jump, transpose, part enable, volume, program, part transpose); channel 16 means
    all.
- **Read from data:**
  - Container magics `'MIDI' 'GMD ' 'ADL ' 'ROL ' 'Crea'` give the dialect 0..3; chunk ids `'MTrk' 'MDhd'`.
  - MIDI status class `(status & 0x70) >> 4`; 0xF0/0xF7/0xFF; meta types 0x2F/0x51/0x58.
  - CCs: 1, 7, 10, 16 (pitch-bend range), 17 (note request / AdLib detune), 18 (priority), 64, 123.
  - LucasArts SysEx `F0 7D`:
    - TIE sub-ops: 0 marker, 1 jump-hook, 3 large marker;
    - X-Wing ops: 0x10/0x11 instruments (format byte 1/2/3), 0x30..0x35 hooks, 0x40 markers, 0x50/0x51 loop set/clear,
      0x60 global instrument slot.
  - Roland 0x41 DT1 0x12 model 0x16 (MT-32 addresses 0x40000, 0xC090, 0x10000, 0x22000).
  - Instrument sizes: 30 (AdLib), 246 (Roland).
  - FM4 patch SysEx `F0 7D 02`.
  - VOC blocks: 0 end, 1 audio, 4 marker, 6 loop start, 7 loop end, `'C'` header; anything else is an error. Rate =
    1e6/(256−d).
- **Internal:**
  - `SYNTH_*` (12 queue events); `ImFm4ToneField` (sparse 0, 1, 13, 14, 26, 27, 68).
  - Trigger marker: 0 any, 1..127, 128 large. Hook sentinels: 0 wildcard, 128 suppress, ≥128 persistent.
  - Packed sequencer time: tick bits 0–15, beat 16–19, measure 20–31.
  - Seq handler tables become a `match` on a `WalkMode` enum (live/jump/scan).

## Address annotations and recovered tables

- **Recovered tables:**
  - `mixer_tables.h`: `panTableBase @ 0x19960`, `volumeCurves256 @ 0x187E0` (IMUSE.ENG).
  - `fm4_data.c`, `internal/xwing_adlib_data.h` (DS 0x14d00).
- **Code addresses:**
  - `xwing.c`: DOS93_ADLIB/ROLAND.EXE 0x1204e..0x12243, 0x1421a.
  - `midi_xwing_adlib.c`: ADLIB.IMS 0x13f1e..0x14bca, 0x10A58, 0x144f6, 0x14092.
  - `midi_fm4_opl3.c`: DOS94 FM4.MDR banks at file offsets 0x10e0 / 0x18b4.
  - `players.c`: 0x13A12.
- **Other references:** IDA symbol names appear in comments, and patent US5315057A is cited in `sustain.h`.
