# Binary Targets

- **`enrich_boards.rs`** -- One-off maintenance tool that enriches stripped board JSONs with `build` and `upload` sections from PlatformIO's local platform installs (`~/.platformio/platforms/`). Extracts core, variant, f_cpu, mcu, extra_flags, VID/PID, Arduino sub-fields (ldscript, partitions), and upload protocol/speed. Upstream fields overwrite bundled ones, except `extra_flags`: upstream flags come first and win per macro, and hand-added bundled defines are kept after them (FastLED/fbuild#2039). Run manually with `soldr cargo run -p fbuild-config --bin enrich_boards`.
