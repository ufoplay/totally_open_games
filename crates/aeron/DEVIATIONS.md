# aeron deviations from upstream

Intentional behaviour changes against upstream Aeron. Each one is also marked `// DEVIATION:` (or `DEVIATION:` in a
doc comment) at the code site.

| Module | Deviation | Why |
|---|---|---|
| `sdl` | Case-insensitive comparisons fold ASCII only; SDL folds the full Unicode range. | Game file names are ASCII. |
| `vfs` | Roots and paths have no `AERON_MAX_PATH` (1024-byte) limit, so the "path too long" failures are gone. | Rust strings grow as needed. |
| `vfs` | Host entries whose names are not UTF-8 are skipped by case-insensitive lookup and glob. | Paths are `&str`; game files are ASCII. |
| `vfs` | Glob patterns match one path component. `SDL_GlobDirectory` would recurse for a pattern containing `/`. | No game uses such a pattern. |
| `vfs_iso9660` | Entry names that are not UTF-8 have the invalid bytes replaced with U+FFFD. | Paths are `&str`; ISO9660 names are ASCII d-characters. |
| `asset` | The allocation-failure errors (codes 24, 31, 44, 65, 71, 87, 112 and the ABP "allocation failed") do not exist. | Rust aborts on allocation failure; NULL inputs and outputs cannot be expressed. |
| `asset` | Error codes become one error enum per decoder; codes stay in the variant docs. Several BMP failures shared code 110. | Games read only the message. |
| `asset` | `Lfd::parse` has no 64-entry cap (upstream error code 23). | The cap came from OpenTIE's cockpit-panel parser; general LFDs (`PLATFORM.LFD`, `ROLAND.LFD`, …) exceed it. |
| `numeric` | `parse_ascii_double` rejects hexadecimal floats (`0x1.8p3`), which `strtod` accepts. Overflow and underflow (to zero or a subnormal) are rejected like glibc's `ERANGE`. | No configuration uses hex floats. |
| `config_file` | YAML is parsed by `saphyr-parser` (YAML 1.2) with a small composer, not libyaml (YAML 1.1). Aeron's own scalar typing is unchanged. Syntax error messages and some positions differ, input must be UTF-8 (libyaml also reads UTF-16), a redefined anchor is accepted, and a recursive alias is reported as an undefined alias. | No C libraries (PLAN §1.1). |
| `config_file` | Error `path` and `message` are not truncated to 512 bytes. | Rust strings grow as needed. |
| `config_file` | Getters return `Option` in place of taking a fallback argument. | Idiomatic; `.unwrap_or(fallback)` gives the upstream result. |
