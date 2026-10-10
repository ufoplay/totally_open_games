# Games — how they use the building blocks

These notes record a reading of the four upstream games at the commits pinned in [PLAN.md](../PLAN.md) §0, made on
2026-10-10. The counts come from grep and are approximate.

## Layout

| Game | src/ dirs (LOC) | Total |
|---|---|---|
| OpenXW | xw 132.1k, xw_dos94 29.4k, xw_runtime 27.0k, xw_remaster 9.2k, xw_app 5.3k | ~203k |
| OpenXvT | xvt 154.8k, xvt_runtime 24.1k, xvt_remaster 8.8k, xvt_app 3.7k | ~191k |
| OpenTIE | tie 111.3k, tie_runtime 23.8k, tie_remaster 21.9k, tie_app 8.7k, tie_formats 1.1k | ~167k |
| OpenXWA | xwa 234.6k, xwa_remaster 18.3k, xwa_runtime 12.3k, xwa_app 3.3k, xwa_formats 0.9k | ~269k |

- **OpenXW and OpenTIE:**
  - `xw/` and `tie/` hold the recovered original DOS/Landru game code; `xw_dos94/` holds the DOS94 frontend, flight
    renderer and iMUSE scripts.
  - `*_runtime` holds the host adapters: landru host, iMUSE session, storage over the Aeron VFS, input, presentation.
  - `*_remaster` is the GPU scene path and `*_app` is setup, settings UI and the main loop.
  - Both build landru with `LANDRU_MODERN=ON`.
- **OpenXvT and OpenXWA:**
  - They are recovered Win32 DirectX games running on Aeron `compat/` (ddraw, d3d, dsound, dinput, dplay, mmsystem).
  - Neither uses landru or imuse.
  - OpenXWA has its own digital iMUSE in `src/xwa/audio/imuse/` (about 9.7k LOC: imcodec, implayer, imscript, imhost;
    `.IMC`/`.WAV`).

## Building-block usage

**landru** (about 1124 declared functions)
- **OpenXW:** 359 distinct functions, 63 headers. Usage is spread over 154 files in `xw/`, 174 in `xw_runtime` and 39 in
  `xw_dos94`.
- **OpenTIE:** 377 functions. TIE also uses buttons (`btnchk`, `btnsldr`, `btnstr`, `btntext`), `render.h` and `remap`.
- **Most-used calls:** `xrect_*`, `xactor_Set_Actor_User_Function`, `xcanvas_Get_Drawing_Canvas_Bounds`,
  `xres_Open/Close_Resource`, `xfilm_Res_Callback_Film`, `xview_*`, `xinpattr_Set_Input_Draw_Function`,
  `xfont_Print_Clipped/Centered_Text`, `xmemhdl_*`, `xfade_AddTimedText`, `xinput_Alloc_Input`,
  `xactcust_Alloc_Custom_Actor`, `xbtnpush_Alloc_Button`, `xpaint_*Clipped*`, `xdirty_Dirty_Rect`, `landru_task_top`.
- **Callback registrations** (XW + TIE combined), which need auditing under PLAN §1.5:

  | Call | Count |
  |---|---|
  | `xactor_Set_Actor_User_Function` | 385 |
  | `xinpattr_Set_Input_Draw_Function` | 225 |
  | `xactor_Set_Actor_Draw_Function` | 208 |
  | `xactcust_Alloc_Custom_Actor` | 162 |
  | `xinpattr_Set_Input_Update_Function` | 153 |
  | `xinpattr_Set_Input_User_Function` | 103 |
  | `xactor_Set_Actor_Update_Function` | 5 |

**imuse** (76 public functions)
- **OpenXW:** 37 functions. Top calls: `set_param`/`get_param`, `stop_sound`, `set_group_volume`, `forget_sound`,
  `pause`/`resume`, `imuse_xwing_set_hook`, `set`/`clear_trigger`, `start_sound`, `fade_param`,
  `imuse_xwing_validate_sound`, `share_parts`, `start_wave`, `set_music_ducking_factor`, the SC-55 romset functions.
- **OpenTIE:** 45 functions: the same core plus master/music/sfx/voice volume, `imuse_mix_s16` and the backend
  factories.
- **Not called by any game:** `imuse_state_save/load` and the filelist save/restore functions.

**Aeron** (about 610 functions): distinct calls per header

| Header | XW | XvT | TIE | XWA |
|---|---|---|---|---|
| render.h | 59 | 59 | 58 | 61 |
| scene3d | 27 | 26 | 28 | 28 |
| draw_list2d | 8 | 10 | 9 | 12 |
| present | 4 | 4 | 18 | 18 |
| scene/ui | 48 | 45 | 46 | 0 |
| config_file | 31 | 33 | 33 | 24 |
| vfs | 19 | 21 | 17 | 16 |
| input | 10 | 10 | 10 | 7 |
| compat/host | 13 | 15 | 11 | 10 |
| mmsystem | 8 | 8 | 5 | 2 |
| audio.h | 7 | 1 | 7 | 1 |
| video.h | 0 | 12 | 0 | 11 |
| dplay | 0 | 16 | 0 | 1 |

- **Unused by every game:** `surface.h` (direct use), `atlas_pack`, `gltf_mesh`, `text2d`, `random`.
- **Used by one game only:** `ktx2_reader` (TIE), `mesh_overlay` and `temporal` (XWA), `audio_decode` (XW).

## Adapter files (the C reference for our glue)

- **`OpenXW/src/xw_runtime/integration/landru_adapter.c`** (355 lines) fills `LandruHost`:
  - log → Aeron log;
  - files → the Aeron VFS via XwStorage, with routing to the 1994/1998 install or mission storage;
  - input → XwInput;
  - video and palette → FlightDisplay/XwPresentation.
  - It also calls `landru_port_Set_Native_Vga_Presentation`.
- **`integration/landru_sound.c`** fills `frontend_audio_pump`, `music_type`, `music_load` and `music_release`.
- **`runtime/landru_layer.c`** turns `xsurface_Get_Presented_Frame` (320×200 INDEX8 + 6-bit palette) into an
  8-bit-palette `Aeron_SubmitPixelLayer`.
- **`audio/imuse_host.c`:**
  - `getSoundPtrFunc` is `Dos94_gamesnd_GetSoundAddr`;
  - `getSoundSizeFunc` covers flight music ids 500–624 and the landru Sound list;
  - it also holds the music-marker callback.
  - `imuse_create` is called in `xw_dos94/audio/gamesnd.c`.
- **`audio/output.c`** (identical in TIE) is a worker thread that pulls the iMUSE render callback into an
  `AeronAudioStream`.
- **`OpenTIE/src/tie_runtime/integration/landru_adapter.c`** (527 lines) does the same plus a `LandruRenderSink`
  producing snapshot records (TieDraw2D, TieFilm2DState, TieUIText) for the `tie_remaster` high-resolution redraw.
- **`tie_runtime/audio/imuse_session.c`** creates iMUSE at 44.1 kHz, waveSpeed=1, with optional SB16 filtering.
  `imuse_api.c` maps `lolevel_Im*` onto `imuse_*`.

## Runtime data

- **VFS roots:** ASSET (the install, case-insensitive), USER, TEMP, RESOURCE. Install paths are stored in the user
  `config.yaml`; defaults come from `resources/config.yaml`.
- **GOG disc images** (`game.ins`/`game.gog`/`game.fil`) are mounted with `AeronVfs_SetDiscRoot` (ISO9660). XvT and
  XWA use `--game-data <path>`.
- **Per game:**
  - **OpenXW:**
    - LFDs: MAINMENU, BLAST, SFX, SPEECH, XWING, SPECIES, MISSIONS/BMISSION, plus the music LFDs
      ADLIB/ROLAND/GMIDI/LGMUSIC/TLMUSIC/INMUSIC (GMID/ADLB music and VOIC sound live inside LFDs);
    - CP/*.PNL, *.INT, CAMERA.LFD, FILM.LFD, TINY.FNT, MICRO.FNT, VGA.PAC;
    - 1998: `X-Wing Data/…`, `XwingCD/MUSIC/*.WAV`.
  - **OpenTIE:** many LFDs (scene*, awards*, emperor, tiemus2, tiespch2, species…) and PNLs.
  - **OpenXvT / OpenXWA:** .LST, .OPT, .WAV, .IMC, .SNM video, some LFD/CBM/ACT, strings.dat.
- **Test data for landru + imuse:** the XW and TIE LFDs, reached through an install folder or a GOG disc image.

## Minimal examples upstream

- `Aeron/examples/ui_demo/main.c` (264 lines) is the only example: window + frame loop + every `AeronUi` widget, given
  a font atlas.
- landru and imuse have none.
