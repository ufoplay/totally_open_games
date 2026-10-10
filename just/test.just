
# Run all tests
[group('test')]
test test_name="":
  cargo test {{test_name}}

# The game data of every edition under `games_dir`: install folders and GOG disc images, separated by `:`.
# TOG_DATA_XW / TOG_DATA_TIE / TOG_DATA_XVT / TOG_DATA_XWA override these lists.

# Run the tests that need game data (every edition under `games_dir`, unless overridden)
[group('test')]
test-data:
  @TOG_DATA_XW="${TOG_DATA_XW:-{{xw_data}}}" TOG_DATA_TIE="${TOG_DATA_TIE:-{{tie_data}}}" \
  TOG_DATA_XVT="${TOG_DATA_XVT:-{{xvt_data}}}" TOG_DATA_XWA="${TOG_DATA_XWA:-{{xwa_data}}}" \
  TOG_SOUNDFONT="${TOG_SOUNDFONT:-{{soundfont}}}" \
    cargo test --workspace game_data -- --nocapture --test-threads=1

xw_games := games_dir / "OpenXW/games"
xw_data := xw_games / "X-Wing_1993" + ":" + xw_games / "X-Wing_1994_Collector_CD/game.ins" + ":" + xw_games / "X-Wing_1998_Collector_Edition"
tie_games := games_dir / "OpenTIE/games"
tie_data := tie_games / "TIE-Fighter_1994" + ":" + tie_games / "TIE-Fighter_1995_Collector_CD/game.ins" + ":" + tie_games / "TIE-Fighter_1998_Collector_Edition"
xvt_data := games_dir / "OpenXvT/games/X-Wing_vs_TIE-Fighter"
xwa_data := games_dir / "OpenXWA/games/X-Wing_Alliance"
# A General MIDI SoundFont for the SoundFont backend's test; TOG_SOUNDFONT overrides it.
soundfont := "/usr/share/sounds/sf2/default-GM.sf2"
