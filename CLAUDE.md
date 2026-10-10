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
  ported function. Address annotations (`FUNCTION: TIE95 0x…`) are kept. Intentional changes are marked
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
  commit fully reflected.
- **Clones:** upstream clones live in `.upstream/<Repo>` (gitignored). Until `just upstream-fetch` exists, clone them
  by hand: `git clone https://github.com/elyosh/<Repo> .upstream/<Repo>` for Aeron, imuse, landru, OpenXW, OpenXvT,
  OpenTIE and OpenXWA.
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
- **Next: M0 scaffolding** (PLAN §7):
  - GPL-3.0 `LICENSE` file;
  - `justfile`;
  - `tools/devtool` (upstream fetch/status/diff/bump + usage scan);
  - the first `USAGE.toml` files;
  - the `harness/` skeleton;
  - CI.
- **Then:** M1 headless aeron (vfs + iso9660, config, decoders) → M2 imuse ∥ M3 landru → M4 aeron Bevy runtime.
- **Toolchain:** Rust 1.98 (edition 2024), Bevy 0.20, just 1.54.
