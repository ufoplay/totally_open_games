# totally_open_games

This repository is a Rust + Bevy rewrite of the `elyosh` projects. There is one workspace crate per upstream repo.

- **Building blocks:** Aeron, imuse, landru.
- **Games:** OpenXW, OpenXvT, OpenTIE, OpenXWA.

## Read first

- **[docs/PLAN.md](docs/PLAN.md)** is the plan, the rules and the **decision log (§9)**, and the source of truth.
  Check new work against it. Record new decisions in §9 with a date.
- **[docs/analysis/](docs/analysis/)** holds detailed upstream analyses: module inventories, host APIs, file formats,
  integer-constant catalogues and hard-to-port patterns.
  - `landru.md`, `imuse.md`, `aeron.md`, `games.md`.
  - Each states the upstream commit it was made at.

## Non-negotiable rules (details in PLAN.md)

- **No `unsafe`, no C-library wrappers.** Use pure-Rust crates or port the code.
- **Port only the functions the games use**, plus whatever they reach. See `USAGE.toml` per crate (`just usage`).
- **Integer mappings become enums.** Values read from files use `TryFrom` with an unknown fallback. OR-able bit sets
  use `bitflags`.
- **Stay close to upstream structure.** One module per C file with the same name. `#[doc(alias = "<C name>")]` on every
  ported function whose Rust name differs from the C name. Address annotations (`FUNCTION: TIE95 0x…`) are kept. Intentional changes are marked
  `// DEVIATION:` and listed in the crate's `DEVIATIONS.md`.
- **Simulation and rendering are separate.**
  - State is changed only by `step` functions.
  - Renderers take `&State` and only write their own caches.
  - landru and imuse are split into `state/` and `render/` module trees, with no feature flag.
- **landru and imuse stay Bevy-free.** Bevy is used by `aeron` and the games.
- **No DirectX emulation layer.** Translate DirectX call sites to native Rust/Bevy.
- **Every command-line action is a `just` recipe.** Complex logic lives in `tools/devtool`, invoked only by recipes.

## Upstream tracking

- **Pinned commit:** each crate's `Cargo.toml` has `[package.metadata.upstream] repository/commit`, the last upstream
  commit fully reflected. `commit` is added with the first ported function; only `aeron` is pinned so far.
- **Clones:** upstream clones live in `.upstream/<Repo>` (gitignored); `just upstream-fetch` clones or updates all 7.
- **Tooling** (`tools/devtool`, through `just`): `upstream-status [crate]` reports commits since the pin as changed-ported
  / changed-unported / newly used functions; `upstream-diff <crate> [file|function]`; `upstream-bump <crate> <sha>`;
  `usage [crate]` regenerates `crates/<crate>/USAGE.toml`. Snapshot commits are in the root `Cargo.toml`
  (`[workspace.metadata.upstream.snapshot]`).
- **Ported detection:** a C function counts as ported when a Rust item carries its name in `#[doc(alias)]`, or a Rust
  fn has its exact name in the module whose `//! Port of` header lists its C file. A C helper absorbed by Rust
  idiom (string duplication, destructors, buffers) gets an alias on the absorbing item, so upstream changes to it
  are reported.
- **Snapshot commits analysed (2026-10-10):**

  | Repo | Commit |
  |---|---|
  | Aeron | `24e8489` |
  | imuse | `09d3da4` |
  | landru | `bbc73c3` |
  | OpenXW | `d487487` |
  | OpenXvT | `ed6e2c4` |
  | OpenTIE | `8b24fb9` |
  | OpenXWA | `4d71b7c` |

  Full hashes are in PLAN.md §0.
- **Licence:** everything upstream is GPL-3.0, so this workspace is too.

## Status

- **Done (2026-10-10):** analysis and plan. Virtual workspace with stub crates: libraries in `crates/{aeron,imuse,landru}`,
  games in `bin/{openxw,opentie,openxvt,openxwa}`; `[workspace.lints]` with `unsafe_code = "forbid"`.
- **Done (2026-10-10): M1 headless aeron** (`crates/aeron`): `vfs` + `vfs_iso9660`, `config_file`, `numeric` and the
  `asset` decoders (LFD, PNL, ACT, BMP, ABP, bitmap font, image coverage), with unit tests. `just test-data` decodes
  every asset of every edition under `games_dir` (hardcoded in the `Justfile`:
  `/home/ufo/Temp/Games/xwing/<OpenXW|OpenTIE|OpenXvT|OpenXWA>/games/<edition>`), GOG disc images included. The
  `bevy_asset` `AssetReader` adapter moves to M4 (PLAN §9 #18).
- **Done (2026-10-10): M0 scaffolding**: GPL-3.0 `LICENSE`, `tools/devtool`, the `USAGE.toml` of aeron, imuse and
  landru, and the `harness/` skeleton. `just harness-build` exports the pinned sources but needs CMake, which is not
  installed on the dev machine yet; the harness has no programs until M2/M3.
- **In progress: M2 imuse** (`crates/imuse`): every module and backend the games use is ported (FM4 + OPL3, the
  DOS93 AdLib driver, the SoundFont backend on rustysynth; MT-32/SC-55 dropped, streaming not ported; see
  `DEVIATIONS.md`). `just test-data` sequences every song of XW and TIE, synthesizes a sample of them and plays
  voices; `tests/engine.rs` covers markers, re-entrant callbacks and deferred commands. `just example imuse
  imuse_render <LFD> <[TYPE:]entry> <out.wav> [seconds] [backend]` renders a WAV. **Open:** the exit criterion,
  matching the C build's OPL register trace and PCM hash, needs the harness program (and CMake on the dev machine).
- **Then:** finish M2 ∥ M3 landru → M4 aeron Bevy runtime.
- **Toolchain:** Rust 1.98 (edition 2024), Bevy 0.20, just 1.54.
