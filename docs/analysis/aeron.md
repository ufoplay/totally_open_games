# Aeron — upstream analysis

These notes record a reading of upstream `elyosh/Aeron` at commit `24e84894ed1d42f8045edbd025da3107f1571535`, made on
2026-10-10. They are reference material for the port; [PLAN.md](../PLAN.md) holds the decisions.

Aeron is about 60k LOC outside `third_party`. It had 131 commits between 2026-07-27 and 2026-10-10, is unversioned, and
its interfaces are still evolving.

## Modules and classification

Classes: **R** = replaced by Bevy, **P** = port as pure Rust, **B** = Bevy plugin/system/pass, **D** = defer or drop.

**Core (`src/*.c`, about 16k LOC)**

| Module | Lines | Responsibility | Class |
|---|---|---|---|
| aeron.c, window.c, events.c | 146+404+150 | init, frame loop, window, logical size, aspect fit, fullscreen, relative mouse | R; logical size + `PresentationMode {Stretch, AspectFit}` + letterbox math as B |
| input.c, gamepad.c | 246+313+538 | per-frame `AeronInputSnapshot` (scancodes, text, mouse, 4 controllers, raw axes/hats, gamepad mapping, rumble, digital-source binding) | R (bevy_input + gilrs); snapshot facade as B |
| render_backend.c, render.h | 4392+1038 | SDL_GPU HAL **plus the layer compositor and HDR output** | HAL R; compositor B; HDR D |
| surface.c | 323+89 | CPU surfaces (INDEX8/RGB555/565/32-bit, palette, colour key, blit) | P (unused directly by games) |
| vfs.c, vfs_iso9660.c | 1131+852 | roots ASSET/USER/TEMP/RESOURCE, case-insensitive lookup, ISO9660 disc roots, atomic writes, glob | P |
| config_file.c | 1444+163 | YAML tree on libyaml: path get/set, overlay, save, line/col | P |
| log.c, diagnostics.c | 232+110 | levelled categories, session log files | R (tracing) |
| crash_handler_* | 227+381 | minidumps | D |
| audio.c | 1467+147 | software mixer | B (cpal) |
| audio_decode.c, ffmpeg_vfs_io.c | 342+72 | FFmpeg audio decode | P via symphonia |
| video.c, video_decode.c | ~1.7k | FFmpeg A/V player as a layer (XvT, XWA) | D (pure-Rust SMUSH `.SNM` decoder) |
| dialog.c | 163 | native message boxes | R (rfd) |
| time, sync, random, paths, numeric | ~500 | clock, threads, RNG, double parsing | R (std) |
| image.c, atlas_pack.c, mesh_normals.c | ~700 | premultiply, scaling, rect packing, normals | P |
| debug_ui.cpp | 292 | ImGui tool registry | D (bevy_egui) |
| fsr3/* | ~2k | FSR 3.1.4 | D |

**Assets (`src/asset`, about 1.9k LOC): all P**
- `lfd.c` (63), `pnl.c` (211; cockpit RLE), `act.c` (155; Landru ACT frames).
- `bmp.c` (132; LucasArts BMP), `abp_font.c` (122), `bitmap_font.c` (88).
- `decode_types` (`AeronIndexedFrame` = indices + coverage + RGBA palette + anchor).
- `opt_model.c` (349; links `tools/opt2gltf/opt.c` + `opt_upgrade.c` as `lucas_opt`).
- `flight_model.c` (657; GLB + the `AERON_flight_model` extension: components, hardpoints, engine glows, pivots).
- Use per game:

  | Game | Decoders |
  |---|---|
  | XW | LFD, PNL, ACT, BitmapFont, OPT |
  | XvT | adds ABP and BMP |
  | TIE | LFD, PNL, ACT, OPT, FlightModel |
  | XWA | ABP, BMP, OPT, FlightModel |

**Scene (`src/scene`, about 11.7k LOC)**

| Module | Lines | Class |
|---|---|---|
| scene3d.c (+internal, util) | ~2.3k | B (submission → Bevy entities/PBR) |
| pbr.c, gltf_mesh.c, mesh.c | ~1.75k | mostly R; atlas UV / variants / mesh table via `ExtendedMaterial` |
| directional_shadow.c | 1347 | R (cascades); PCSS D |
| clustered_lights.c | 294 | R |
| post.c (SSAO, motion blur) | 913 | R |
| bloom.c, present.c (ACES/AgX tonemap, "punchy") | 621 | R; exact look would need a custom tonemap |
| billboards3d.c | 563 | B (SKY/OVERLAY/LENS stages; 5×5 depth-sampled lens occlusion) |
| mesh_overlay.c | 239 | B |
| temporal.c | 243 | D |
| draw_list2d, text2d, font_atlas, sprite_atlas, runtime_atlas, image_cache, rgba_upload | ~2.4k | B (custom 2D pipeline: trapezoids, projected lines, tint/bias, rounded rects) |
| ktx2_reader.c | 461 | R (Bevy ktx2) |
| ui/* | 4741 | B (immediate-mode settings UI, file picker, rebinding; used by XW/XvT/TIE, not XWA) |
| settings.c, blend_ramp.c, world.c | ~415 | P |

## Legacy layer compositor (`render.h`)

- **Frame model:** `Aeron_BeginFrame` → queue layers → `Aeron_Present`.
  - At most 64 layers (`AERON_MAX_RENDER_LAYERS`), composited **in submission order**.
  - `logical_rect` is in the app's logical resolution, mapped by `AeronPresentationMode` STRETCH/ASPECT_FIT
    (`Aeron_GetContentPixelRect`).
- **`Aeron_SubmitPixelLayer(AeronPixelLayerDesc*)`** returns an `AeronRenderSubmission` (u32 generation token).
  - Frame view: `AeronPixelFrameView{pixels,width,height,pitch,bpp,format,color_space,palette[256] RGBA8,generation}`.
  - Other fields: `blend_mode{OPAQUE,ALPHA,PREMULTIPLIED}`, `sampling{LINEAR,NEAREST,SHARP_BILINEAR}`,
    `preserve_encoded_values`, `color_key` (raw value/index → alpha 0), optional `coverage` plane, `tint`, `scissor`.
  - INDEX8 → RGBA, colour key and coverage are applied **on the CPU**. Generation caching keeps at most 8 uploads.
- **`Aeron_SubmitTextureLayer`:** texture + rect + blend + colour space + tint + `bias_rgba`
  (out = s·tint + bias.rgb·s.a).
- **`Aeron_SubmitSwapchainRenderLayer`:** a draw callback into the swapchain pass, used for tonemap/bloom present.
- **Composite shader:** `fullscreen.frag.hlsl` (sRGB decode, tint/bias, sharp-bilinear).
- **Games:** OpenXW's `landru_layer.c` expands the 6-bit landru palette and calls `Aeron_SubmitPixelLayer` (INDEX8,
  sharp-bilinear). That is the whole landru → Aeron bridge.

## Scene API (immediate mode, per frame)

1. `AeronScene_SetTemporal/SetPost`.
2. `AeronScene_Begin(scene, AeronSceneCamera{pos, quat, h/v half-FOV, near (reversed-Z, infinite), proj offsets,
   viewport})`.
3. Submissions:
   - `AddMeshInstance{mesh, transform, prev_transform, variant, emissive strength, mesh_table, lights≤16, flags}`;
   - `AddLight` (≤256), `SetDirectionalShadow` (≤4 cascades), `AddShadowCaster`;
   - `SetSkyCube`, `SetPbrEnvironmentMap`;
   - `SetPassHook(BEFORE_OPAQUE|AFTER_OPAQUE|AFTER_TRANSPARENT|BEFORE_POST|PREPASS|AFTER_UPSCALE)`;
   - `SetMotionContext`, `AddBillboard`, `AddMeshOverlay`.
4. `AeronScene_Render`.
5. Bloom + present chain inside a swapchain layer.

**Meshes:** `AeronScene_MeshCreate(AeronFlightModel*)` builds:
- one VBO/IBO with opaque/mask/blend ranges;
- 4 BC7 channel atlases;
- a variant table;
- `AeronMeshRot mesh_rot[]` for articulated parts (per-slot 3×4 affine + visibility/highlight/markings/emissive
  lanes, indexed by a per-vertex mesh index).

## Audio (`audio.h`)

- **Output:** one SDL output device with a pull callback into Aeron's own mixer.
- **Clips:** `ClipCreate`, `ClipCreateMutable` + `ClipWrite` (DirectSound semantics); formats U8 and S16.
- **Voices** (u64 generation handles): `VoicePlay(gain, pan, pitch, loop)`, `VoicePlay3D`, `GetCursors(play, write)`,
  `SetListener`, `SetDistanceModel` (inverse distance, doppler).
- **Streams:** SPSC FIFO (`AudioStreamOpen/Write/WaitWritable`, queued/consumed/audible clocks, underruns).
- **Who uses what:**
  - Only `compat/dsound.c` uses clips and voices.
  - OpenXW and OpenTIE run a producer thread in `*_runtime/audio/output.c` that pulls `imuse_mix_s16` into a stream.
- The mixer was reworked on 2026-09-29.

## Compat (`src/compat`, about 12.3k LOC, ~4.2k of it DirectPlay)

- **APIs reimplemented** (COM vtables, binary-exact structs):
  - DirectDraw (`ddraw.c` 1454), D3D5 execute buffers (`d3d.c` 1899), DirectSound (963), DirectInput (1228);
  - WinMM CD audio (493) + joystick (79);
  - DirectPlay (HTTPS lobby via libcurl + ICE via libjuice; `tools/netprobe`).
- **Used by:**

  | Game | COMPAT | DPLAY | Notes |
  |---|---|---|---|
  | OpenXW | on | off | |
  | OpenXvT | on | on | heaviest user |
  | OpenTIE | on | | |
  | OpenXWA | on | off | has its own `directplay_private.h` |

  All four `std3d` renderers include `compat/d3d.h`.
- **Decision:** not ported as an emulation layer (PLAN §4.4).

## Tools

- **opt2gltf** (opt.c 1324, opt2gltf.c 1702, opt_upgrade.c 670). `opt.c` is also runtime code.
  - OPT nodes are 24 bytes, with absolute pointers minus the header `globalBase` (v2 base 0x10000000).
  - Node types: GROUP 0, FACE_DATA 1, MESH_VERTICES 3, NODE_REFERENCE 7, VERTEX_NORMALS 11, TEXTURE_COORDINATES 13,
    TEXTURE 20, FACE_GROUPING 21, HARDPOINT 22, ROTATION_SCALE 23, NODE_SWITCH 24, MESH_DESCRIPTOR 25; XWA-only
    TEXTURE_ALPHA 26 and ENGINE_GLOW 28.
  - Texture palettes are 16×256×2 (16 brightness levels). Normals are Q15.
  - Units: 1 unit = 1600/65536 m. Axes: +Z up, −Y forward.
- **gltf_cook:** artist glTF → GLB with 4 KTX2 channel atlases.
- **imgbake:** a library of about 3.7k LOC.
- **fontbake:** TTF → PNG + `.fnt` (TFNT v2).
- **Dropped:** `scene_capture`, `netprobe`, `fsr3_shadergen`.

## Integer mappings → Rust types

- **render:**
  - `AeronPixelFormat` (INDEX8, RGB555, RGB565, XRGB8888, ARGB8888, RGBA8888, BGRA8888);
  - `AeronColorSpace` (SRGB, LINEAR_SRGB, LINEAR_DISPLAY);
  - `AeronLayerBlendMode`, `AeronPixelSamplingMode`, `AeronPresentationMode`, `AeronHdrOutputStatus`.
- **surface:** `AeronSurfaceFlags`, `AeronSurfaceBlitFlags` (bitflags).
- **input:**
  - `AeronKey` (sparse SDL scancodes ↔ `KeyCode` + a name table for config);
  - `AeronKeyModifiers`, `AeronMouseButton` (bitflags);
  - `AeronGamepadAxis` (6), `AeronGamepadButton` (26);
  - `AeronControllerKind`, `AeronControllerHat` (bitflags), `AeronControllerDigitalSourceKind`.
- **audio:** `AeronPcmFormat` (U8, S16).
- **log:** `AeronLogLevel`.
- **vfs:** `AeronVfsRoot`, `AeronVfsOpenMode`, `AeronVfsRootOptions`, `AeronVfsGlobFlags`.
- **config:** `AeronConfigNodeType`, `AeronConfigErrorCode`.
- **video:** `AeronVideoState`, `ScaleMode`, `Filter`.
- **other:** `AeronTemporalMode`, `AeronImageAlphaMode`, `AeronAtlasAddressMode`.
- **scene:**
  - `AeronScenePassSlot`, `ShadowFitMode`, `BillboardBlend`/`Stage`, `MeshOverlayBlend`;
  - `AeronGltfAlphaMode`, `AeronGltfEmissiveMode`;
  - tonemap operator / AgX look / bloom kernel;
  - `AeronBlit2DBlend`/`Filter`, `DrawList2DClearMode`, `TextAlign`;
  - UI enums.
- **assets:** OPT `mesh_type`, hardpoint type, LFD FOURCCs, OPT node types, decode errors.

## Shaders (about 5.3k LOC HLSL)

- **Covered by Bevy built-ins:** PBR, shadows, clustered lights, SSAO, bloom, motion blur, tonemap, sky, FSR.
- **Need a WGSL `ExtendedMaterial`:** mesh-slot transforms, atlas sub-rect sampling, legacy emissive.
- **Need a custom WGSL rewrite:**
  - `fullscreen` (compositor), `scene_blit*` (draw list), `scene_ui_rect`;
  - `scene_billboard3d*` (+velocity, lens), `scene_mesh_overlay`, `scene_pbr_prepass_stamp`;
  - `dx5_projected_triangle`.
- **Per-game shaders:** XW 12, XvT 11, TIE 11, XWA 8.

## Churn

Commits touching each area since 2026-08-10, out of all commits touching it:

| Area | Recent / total |
|---|---|
| compat | 15/15 |
| scene | 26/49 |
| ui | 12/17 |
| asset | 9/11 |
| tools | 14/22 |
| shaders | 8/28 |
| render_backend | 7/15 |
| audio | 4/8 |
| vfs | 2/7 |
| config | 2/7 |

VFS, config and the asset decoders are the stable areas.
