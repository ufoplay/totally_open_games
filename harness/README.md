# Reference harness

C programs built against the **pinned** upstream sources. Each one replays a deterministic scenario and writes one
trace line per tick or frame; the Rust tests compare the port against those traces (PLAN §6). This directory is not
part of cargo.

## Build

`just harness-build` exports each building block at its pinned commit (or its PLAN §0 snapshot) into
`target/harness/upstream/<Repo>`, then configures and builds this CMake project into `target/harness/build`.
It needs CMake ≥ 3.20, Ninja and a C compiler.

## Programs

Added with their milestone:

| Program | Milestone | Trace line |
|---|---|---|
| `imuse_trace` | M2 | OPL register writes and a PCM block hash per tick |
| `landru_trace` | M3 | presented frame hash and palette hash per frame |

`just traces <crate>` (with the programs) runs them on game data and writes the traces next to the Rust tests.
Only hashes are committed, never game data. Traces are regenerated at each pin bump.
