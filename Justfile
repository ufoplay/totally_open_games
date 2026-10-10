import "./just/build.just"
import "./just/code_check.just"
import "./just/test.just"

export RUST_BACKTRACE := "full"

# Original game installations, one subfolder per game (OpenXW, OpenTIE, OpenXvT, OpenXWA)
games_dir := "/home/ufo/Temp/Games/xwing"

# Lists all the available commands
default:
  @just --list
