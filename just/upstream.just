devtool := "cargo run --quiet --release -p devtool --"

# Clone or update the 7 upstream repositories in .upstream/
[group('upstream')]
upstream-fetch:
  {{devtool}} upstream-fetch

# Commits since the pin of one building block (or all), split into changed-ported, changed-unported and newly used functions
[group('upstream')]
upstream-status crate="":
  {{devtool}} upstream-status {{crate}}

# Upstream diff since the pin: everything, one file, or the history of one function
[group('upstream')]
upstream-diff crate target="":
  {{devtool}} upstream-diff {{crate}} {{target}}

# Move the pin of a crate to an upstream commit
[group('upstream')]
upstream-bump crate sha:
  {{devtool}} upstream-bump {{crate}} {{sha}}

# Regenerate USAGE.toml of one building block (or all) from the games at their pinned commits
[group('upstream')]
usage crate="":
  {{devtool}} usage {{crate}}
