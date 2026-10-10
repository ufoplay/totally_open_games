# imuse deviations from upstream

Intentional behaviour changes against upstream imuse. Each one is also marked `// DEVIATION:` (or `DEVIATION:` in a
doc comment) at the code site.

| Module | Deviation | Why |
|---|---|---|
| `wave_renderer` (state) | The engine produces wave halves on its own clock (one per 512 wave frames of service time) and drops a half when the ring is full. Upstream produces one only while its 3-half ring has room, so wave playback is clocked by the audio device. | The simulation must not depend on audio-device consumption (PLAN §1.5). |
| `wave_renderer` (render) | The consumer waits for two halves before playing, drops one source frame when the ring runs ahead and repeats one when it runs dry. | Absorbs the drift between the engine clock and the audio device. |
| `commands` | A game callback is a `CallbackId` dispatched through `ImuseHost`, not a function pointer stored in the opcode field. | No function pointers in state; the callback receives the engine and can re-enter it, as upstream callbacks do. |
| `commands` | A large marker (SysEx sub-op 3) passes 128 to the callback; upstream passes the truncated payload address. | The address is meaningless to the game (an LP64 bug upstream). |
| `commands`, `dispatch`, `tracks` | Streaming and the stream-switch crossfade are not ported; the streaming opcodes return -1. | No game starts a stream; stream buffers are only reachable through `imuse_stream_start`. |
| `triggers` | A deferred callback id is kept whole; upstream truncates the opcode to `int`. | An LP64 bug upstream. |
| `seq` | The size of a DOS93 sound is the length of the host's data; upstream asks `getSoundSizeFunc`. | The host returns the bytes as a slice. |
| `files`, `seq`, `dispatch` | Every read of sound data is bounds-checked; reading past the end ends the sound or track. | Upstream trusts the container headers (undefined behaviour on bad data). |
| `dispatch` | The 16-bit VOC marker is read little-endian; upstream reads it in host order. | Equal on the x86 originals. |
| `mixer` | Mix values beyond the companding table read 0. | Upstream reads out of bounds. |
| `filelist` | Load and unload go through `ImuseHost`; the open/close half (streamed sounds) is not ported. | The host is passed per call; no game opens a streamed sound. |
| `xwing` | The Roland program, part selection, timbre and SysEx writes do nothing. | The MT-32 backend is dropped (PLAN §9 #4), so no backend has Roland hooks. |
| `midi_fluidsynth` | `rustysynth` replaces FluidSynth: SysEx (the GS init) is ignored and the output level is not calibrated. | No C libraries (PLAN §1.1, §4.1). |
| `midi_fm4_opl3` | Closing does not clear the chip's registers. | The renderer is dropped instead. |
| `opl3` | The OPL3 core is the pure-Rust core of `nuked-opl3` 0.1.0, with a write-buffer query added and a debug-build overflow fixed. | Pure Rust; the engine needs `write_space` (`internal/opl_buffer.h`). |
| `lib` | Diagnostics go to `tracing` instead of the host log callback. | PLAN §4.3: logging is `tracing`. |
