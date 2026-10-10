# Build the C reference programs against the building blocks at their pinned commits
[group('harness')]
harness-build:
  {{devtool}} harness-export
  @command -v cmake >/dev/null || { echo "error: harness-build needs CMake (>= 3.20)"; exit 1; }
  cmake -S harness -B target/harness/build -G Ninja -DTOG_UPSTREAM_DIR="{{justfile_directory()}}/target/harness/upstream"
  cmake --build target/harness/build
