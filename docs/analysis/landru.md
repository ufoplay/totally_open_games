# landru — upstream analysis

These notes record a reading of upstream `elyosh/landru` at commit `bbc73c360f7ccf97b87687c3201abab2af0b0fac`, made on
2026-10-10. They are reference material for the port; [PLAN.md](../PLAN.md) holds the decisions.

## Size and shape

- **Size:** about 22k LOC. There are 56 `.c` files, 60 public headers in `include/landru/` and 8 private headers
  (`binio.h`, `canvas_internal.h`, `dialog_internal.h`, `host_video.h`, `host_internal.h`, `render_internal.h`,
  `timer_internal.h`, `vesa_internal.h`).
- **Missing pieces:** there are no tests, no sample data, and no SDL or Aeron dependency.
- **History:** 3 commits: Initial import, Annotations update and refactor, Improve matching.
- **Largest files:**

  | File | Lines |
  |---|---|
  | `film.c` | 1089 |
  | `fade.c` | 955 |
  | `actor.c` | 919 |
  | `font.c` | 867 |
  | `canvas.c` | 741 |
  | `inpcall.c` | 719 |
  | `pal.c` | 685 |

## Modules by layer (LOC including headers)

| Layer | LOC | Modules |
|---|---|---|
| Memory | ~230 | `memhdl` (32768-slot handle table of malloc'd blobs), `memptr` (malloc wrapper, fills 0xA3/0), `memcom` (compaction callbacks, which are stubs) |
| Files and IO | ~1750 | `file` (host IO + native-endian byte/word/long readers), `res` (LFD `RMAP` container, name lookup), `filedir`, `stream` (synchronous whole-file channels), `paragrp` (TEXT bundles), `fourcc.h`, `binio.h` |
| Graphics | ~7660 | `bitmap` (8-bit blits); `canvas` (screen/diff/buffer bitmaps, canvas stack, clipping, copy to video); `surface` (VGA/SVGA surface sets, presentation, effective palette); `vesa`, `video` (framebuffer, software vs platform backend); `delta` (DELT RLE decoder) with `color`/`flip`/`scale` variants; `remap` (37 fixed colours); `paint` (rect/line display list); `rect`; `dirty`; `pal` (palettes, colour cycling, src/dest/work/fade palettes, VGA retrace timing); `fade` (26 wipe modes, palette fades, fade task, plus XW timed caption text); `font`; `render` (optional render sink) |
| Actors and animation | ~4470 | `actor` (base, type registry, linked list + z-sort, movement), `actanim` (ANIM frame list), `actdelt` (single DELT), `actraw` (TIE RAW), `actcust` (custom draw callback), `actback` (XW background capture/restore), `actrect` (XW blank rect), `film` (FILM interpreter), `view` (4 parallax layers with tracking), `viewadd` (view clipping + per-frame view task) |
| UI | ~5180 | `input` (widget tree, built from resources in TIE), `inpattr` (flags, z-sort), `inpcall` (hit-test, key/mouse dispatch), `btnpush`, `btnchk`, `btnsldr`, `btnstr`, `btntext`, `dialog` (modal dialog task, sub-dialog channel), `dlg` (XW choices popup), `dlgjoy` (XW joystick calibration), `style` (bevels, icons), `cursor` |
| Input | ~820 | `io` (global mouse/key/joy state + polling), `mouse`, `joy` |
| Timing and tasks | ~450 | `timer` (250 Hz PIT emulation from the host clock, frame budgets), `task` (cooperative task stack) |
| Sound | ~730 | `sound` (Sound records, GMID/VOIC blobs; playback delegated to a game `SoundActionFunc`) |
| Host and support | ~590 | `host`, `host_timing.h`, `presentation.h`, `error` (exit/bail/escape, assertion groups), `config.h` |

## Game variants

- **CMake switch:** `LANDRU_GAME=XW|TIE`, default TIE.
  - XW drops `btnchk btnsldr btnstr btntext`.
  - TIE drops `actback actrect dlg dlgjoy`.
  - The variant-only modules total about 1.2k LOC.
- **How much differs:** 3473 of 20636 lines (16.8%) sit inside `LANDRU_GAME_*` blocks. Most variant-specific lines:

  | File | Lines | File | Lines | File | Lines |
  |---|---|---|---|---|---|
  | `input.c` | 368 | `stream.c` | 171 | `btnpush.c` | 122 |
  | `canvas.c` | 351 | `sound.c` | 164 | `surface.c`, `fade.c` | 120 each |
  | `font.c` | 304 | `inpcall.c` | 139 | `viewadd.c`, `res.c` | 96 each |
  | `dialog.c` | 182 | `cursor.c` | 133 | | |

- **Layout and signature differences:**
  - Struct layouts differ for `Input`, `ViewStruct`, `Sound`, `Palette`, `FontStruct` and `ResFile`.
  - Signatures differ too: XW passes an explicit `ResFile*` where TIE searches globally, and XW passes the list to its
    draw functions.
- **`LANDRU_MODERN`** (on in both games) adds:
  - actor interpolation fields (`prev_*`, `film_entry_index`);
  - the render-sink `*_emit_*` functions;
  - `logical_size_changed`;
  - presented-frame and effective-palette support;
  - `landru_port_Set_Native_Vga_Presentation`.

## Host seam

Each callback table carries a `void* userdata`.

**`LandruHost`** (`include/landru/host.h`, installed with `landru_set_host`)
- Required:
  - files: `file_open(root, path, mode)`, `file_read`, `file_write`, `file_seek`, `file_tell`, `file_close`, where
    `LandruFileRoot` is ASSET, AUXILIARY_ASSET, USER or TEMP;
  - directories: `dir_open`, `dir_next`, `dir_close`, `path_is_dir`;
  - keyboard: `key_pending`, `key_read` (returns DOS `scancode << 8 | ascii`), `modifier_keys`;
  - mouse: `mouse_position(buttons, x, y)`, `mouse_movement`, `mouse_set_position`, `mouse_show`;
  - joystick: `joystick_count`, `joystick_read(port, axes, n, buttons)`;
  - `palette_set(rgb, start, count)`, `now_us`.
- Optional: `log` (va_list), `joystick_read_xw`, `frontend_audio_pump`, `music_type`, `music_load`, `music_release`.
- Optional nested video table `LandruPlatformVideo`: `set_mode`, `lock`, `unlock`, `copy_to_present_surface`,
  `present`, `logical_size_changed`.

**`LandruRenderSink`** (`render.h`, installed with `landru_set_render_sink`)
- Retained publication callbacks: `actor`, `draw`, `film`, `text`, `paint`, `cursor`, `fade`,
  `scene_clock(frame, progress, period_us)`, `is_scene_transition`.
- Only OpenTIE installs it, for the remaster high-resolution redraw.

**`SoundActionFunc`** (`sound.h`, set with `xsound_Set_Sound_Action_Function`)
- Landru issues actions 1–10 (PAUSE … FADE_PAN) and never plays audio itself.

**Scheduler queries** (`host_timing.h`)
- `xpal_Next_VGA_Delay_Us`, `xtimer_Next_Frame_Delay_Us`, `xtimer_Elapsed_Delay_Us` (XW).

**Presented frame**
- `xsurface_Get_Presented_Frame` returns `LandruPresentedFrame`: 320×200 indexed pixels plus a 6-bit palette.

## Control flow

- **No hidden concurrency.** There is no setjmp/longjmp, no coroutines and no threads.
- **Cooperative task stack** (`task.c`):
  - `landru_task_push(vtable)` returns 256 bytes of zeroed storage.
  - `landru_task_pop` runs the task's `end`.
  - The other entry points are `landru_task_step_once`, `landru_task_service_wait`, `landru_task_next_wake_delay_us`
    and `landru_task_clear_all`.
  - The stack is at most 8 deep.
- **Per-frame entry point:** `landru_task_run_frame()` steps the top task up to 64 times. It stops on YIELD or
  FRAME_COMPLETE, or when a VGA retrace wait is pending.
- **Tasks:**
  - `viewadd_task_step` (`xviewadd_Push_Handle_View_Task`) is the per-frame pipeline. Phases: VIEW_PHASE_DRAW, WAIT,
    AFTER_FRAME_FADE, AFTER_SUB. The DRAW phase runs:
    1. `xview_Move_View`
    2. `xinput_Update_System_Inputs`
    3. `xfilm_Update_Films`, `xactor_Update_Actors`
    4. `xinput_User_System_Inputs`, `xfilm_User_Films`, `xfilm_Check_Film_ZPlanes`, `xactor_User_Actors`,
       `xactor_Check_Actor_ZPlanes`, `xsound_User_Sounds`
    5. **erase** (`xcanvas_Erase_Canvas_Rect` per view)
    6. **draw** (`xfilm_Draw_Films`, `xactor_Draw_Actors`, `xinput_Draw_System_Inputs`, `xdialog_Draw_System_Dialogs`)
    7. WAIT (`xtimer_Frame_Budget_Pending`/`Commit_Frame`)
    8. push the fade task → present → FRAME_COMPLETE.
  - `dialog_task_step` and `fade_task_step` are the other tasks.
  - Callers push a child task, return CONTINUE, and read the result (`dlg_exit_gbl`, `xerror_Get_Landru_Exit`) on
    their next step.
  - Sub-dialogs go through a single-slot channel: `xdialog_Schedule_Sub_Dialog`, `xdialog_Try_Push_Pending_Sub_Dialog`,
    `xdialog_Invoke_Sub_Handler`.
- **Where drawing mutates state** (relevant to PLAN §1.5):
  - `xactor_Draw_Actors` clears per-actor refresh flags after drawing (`xactor_Non_Refresh_Actor`). On TIE it also
    resets `refresh_actors_gbl`.
  - It calls `xtimer_Often()`, which polls the timer and input, inside the draw loop.

## Memory patterns and safe-Rust equivalents

1. **Global handle table** (`memhdl`): `void* handle_pointers[32768]` with 16-bit handles. Handles hold both raw blobs
   and typed arrays, e.g. a `FilmObject` header followed by command bytes. Rust: typed arena keys plus owned buffers.
2. **Struct-prefix inheritance.** `xactor_Alloc_Actor(extend)` allocates `sizeof(Actor)+extend`, and widgets embed an
   `Input` header as their first field and are cast. Rust: base struct + kind enum.
3. **Function pointers** in Actor, Film, Input and View (`draw`, `update`, `user`), `DialogUpdateFunc`,
   `DialogSubResultHandler(result, void* ctx)`, and `InputType::func1/func2` stored as `void*` and cast to different
   signatures. Rust: enum ids dispatched through a game trait (PLAN §1.4).
4. **Untyped user slots** `varptr/varhdl/var1/var2` on Actor, Film, Input, Sound and Palette.
5. **Intrusive `next`/`child` lists** with z-plane re-sorting, plus cross-references: `FilmObject::object` points to an
   Actor, Palette or Sound; `View::trackActor[]`; `Film::def_palette`; `ResFile::next`. Rust: arenas with typed ids.
6. **Native-endian casts** (`(int16_t*)data`) in film, actdelt, actraw and flip. `delta.c` uses memcpy. FOURCCs are
   big-endian, swapped through `xfile_Swap_DWord`.
7. **Packed structs** (`pshpack1` in 12 places; `Input` is 51 bytes). `InputResourceHeader_decode` (25 bytes) is the
   model for explicit decoding.
8. **Task storage:** a `union LandruTaskStorage` of 256 bytes, cast to `FadeTask`/`DialogTask`/`ViewAddTask`.
9. **About 276 `static`s plus many externs.** `io.h` alone has about 30 `*_gbl` (`canvas.h`: bitmaps,
   `draw_buff_gbl`, clip globals).
10. **Pixel pointer arithmetic** such as `draw_buff_gbl + x + y*draw_w_gbl`, with aliasing between `canvas_bm_gbl`,
    `draw_bm_gbl` and `screen_bm_gbl`.

## File formats

- **LFD:** an `RMAP` header (type, name[8], len), then 16-byte `ResEntry {type, name[8], size}`. Parsed in `res.c`
  (`xres_Fetch_Resource_Files`, `xres_Get_Resource_Offset`).
- **FILM** (`film.c`: `xfilm_Res_Callback_Film`, `xfilm_Res_Film_Data`):
  - Header: u16 magic (= 4), u16 cels, u16 numObjects.
  - Each object: type 4cc, name[8], u32, u16 typeId (`FilmTypeCode`), u16, u16 length, then commands.
  - Each command: i16 length, i16 opcode (`FCMD_*`), then i16 args.
- **DELT:** 4×i16 bounds, then lines {u16 count (bit 0 = RLE), i16 x, i16 y, data}. RLE control byte: bit 0 means run,
  bits 1–7 are the count. Decoded by `delta`/`color`/`flip`/`scale`.
- **ANIM:** u16 frameCount, then for each frame a u32 length followed by a DELT.
- **PLTT:** u8 start, u8 end, RGB triplets (>>2), u8 cycleCount, then cycles {u16 rate (stored as 4915/rate),
  u8 start, u8 end}.
- **FONT:** firstChar, numChars, width, height, baseline, isColor, width array, glyphs.
- **TEXT:** u16 count, then {u16 size, NUL-separated strings}.
- **TIE input/dialog trees:** recursive records {name[8], u16 resSize, res, u16 strSize, string}. Each res starts with a
  4cc (BUTN/SBTN/CHCK/SLDR/STRG/TEXT/FRAM/INPT) and a 25-byte `InputResourceHeader`.
- **GMID/VOIC/ADLB/RLND:** opaque blobs handed to the host. `music_type` defaults to `'GMID'`.

## Integer mappings → Rust types

- **Bitflags:**
  - `ActorFlags` `AF_*` (~13), `FilmFlags` `FF_*` (8).
  - `InputStateFlags` `INPUT_*` (~11; values differ XW vs TIE), `InputWindowFlags` (6).
  - `LandruActorRenderFlags` (5), `LandruVideoFlags` (3).
  - Sound flags, currently bare hex: 0x1 free, 0x2 discard, 0x4 user-keep, 0x1000 keep, 0x2000 keepable, 0x4000/0x8000
    flag1/2.
  - Modifier mask (`io.c` ~506–522): 0x80 insert, 0x40 caps, 0x20 num, 0x10 scroll, 0x08 alt, 0x04 ctrl, 0x02/0x01
    shift.
  - `BACK_HOLD_STATE` 0x80 combined with `BACK_STATE_MASK`.
- **Read from data (TryFrom with Unknown):**
  - `FilmCmd` `FCMD_*` (29); `FilmTypeCode` `FTC_*` (4; unknown values are skipped silently).
  - FOURCCs (~25, `fourcc.h`).
  - `FadeWipeMode` (26), `FadeColorMode` (6).
  - String filter type (0–3).
- **Plain enums:**
  - `SoundAction` (10), `SoundType` (3), `MouseUsage` (5), `InputActiveState` (5).
  - `RemapColor` (37), `StyleIcon` (14).
  - `LandruTaskStepResult` (4), `LandruFileRoot` (4), seek origin (3), `LandruLogLevel` (4).
  - `LandruRenderTarget`, `LandruPaintOp` (11), `LandruCursorKind`, `LandruFadeKind`, `LandruSurfaceSet`,
    `LandruPortVideoBackend`.
  - `FadeEndCursorAction` (3); phase enums (`FadePhase`, `DialogPhase` ~8, `ViewAddPhase` 4).
  - XW `LandruFrameRatePreset` (6; tick budgets 54/34/27/23/14/11).
  - `BACK_*` (4), dlgjoy steps (5).
  - Error groups 2–14 (`xerror_Display_Assert`).
  - VESA modes 0x13/0x101/0x103.
  - Key codes 0x0D, 8, 0x1800, 0x5200, 0x5300, 0x2E00 (`io.c` ~246–269, `btnstr.c` 156).
  - Paint ops 0–4, slider drag states 1–4.
- `LandruBoolResult` (`int16_t`) becomes `bool`.

## Address annotations

- **Format:** `// GLOBAL: <EXE> 0x<addr>` or `// FUNCTION: <EXE> 0x<addr>`, placed immediately before the definition
  (reccmp style).
- **EXE values:** `TIE95` (TIE 1995 DOS, linear offsets) and `TIE98` (1998 Windows, VAs).
- **Counts:** 104 GLOBAL TIE95, 19 GLOBAL TIE98, 4 FUNCTION TIE95, 32 FUNCTION TIE98.
- **Most annotated files:** `vesa.c` (37), `io.c` (16), `dirty.c` (13), `canvas.c` (11), `cursor.c` (10).
- X-Wing addresses appear only in prose comments.
