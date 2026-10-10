
# Run all tests
[group('test')]
test test_name="":
  cargo test {{test_name}}

# Run the tests that need game data: every XW and TIE edition under `games_dir`, as install folders and GOG disc images.
# TOG_DATA_XW / TOG_DATA_TIE override the lists (paths separated by `:`).
[group('test')]
test-data:
  TOG_DATA_XW="${TOG_DATA_XW:-{{xw_data}}}" TOG_DATA_TIE="${TOG_DATA_TIE:-{{tie_data}}}" \
    cargo test --workspace game_data -- --nocapture --test-threads=1

xw_games := games_dir / "OpenXW/games"
xw_data := xw_games / "X-Wing_1993" + ":" + xw_games / "X-Wing_1994_Collector_CD/game.ins" + ":" + xw_games / "X-Wing_1998_Collector_Edition"
tie_games := games_dir / "OpenTIE/games"
tie_data := tie_games / "TIE-Fighter_1994" + ":" + tie_games / "TIE-Fighter_1995_Collector_CD/game.ins" + ":" + tie_games / "TIE-Fighter_1998_Collector_Edition"
