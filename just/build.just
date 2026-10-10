
# Builds all artifacts
[group('build')]
build:
  cargo build

# Run a crate example, e.g. `just example imuse imuse_render <archive.LFD> <entry> <out.wav>`
[group('build')]
example crate name *args:
  cargo run --release -p {{crate}} --example {{name}} -- {{args}}
