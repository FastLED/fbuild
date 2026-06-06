# `fbuild bloat` — per-symbol bloat analysis

`fbuild bloat` produces a fine-grained, attributed report of every
live symbol in an ELF: name, size, region (`flash` / `ram`), source
archive, object file, and output section. It's the same analysis the
build orchestrator emits when you pass `--symbol-analysis` to
`fbuild build`, but as a standalone subcommand that works against any
ELF — fbuild-built, PlatformIO-built, or hand-compiled.

> **Note:** the legacy spelling `fbuild symbols` is still accepted as
> a hidden alias for back-compat through 2.3.x. Deprecation warning
> lands in 2.4.0; alias removal in 3.0.

## TL;DR

Point it at a project directory:

```bash
fbuild bloat .
```

Or at an ELF directly:

```bash
fbuild bloat .fbuild/build/uno/firmware.elf
fbuild bloat .pio/build/esp32s3/firmware.elf
```

No `--nm`. No `--cppfilt`. No manual ELF / map paths. Toolchain paths
come from the `build_info.json` fbuild (or PlatformIO) wrote next to
the ELF.

## Where toolchain paths come from

The analyzer needs four binaries: `nm` (lists symbol sizes), `c++filt`
(demangles C++ names), and optionally `readelf` / `objdump` for
downstream tools. These are looked up in this order:

1. **`--nm <path>` / `--cppfilt <path>` flags** (highest precedence;
   the user's explicit override always wins).
2. **`--build-info <path>`** — load `nm_path` / `cppfilt_path` from
   that file.
3. **Auto-discovery** — walk up from the ELF's directory looking for
   `build_info.json` or `build_info_<env>.json`. Both fbuild and
   PlatformIO write one next to `platformio.ini`.
4. **PATH lookup** of bare `nm` (and derive `c++filt` from its stem).
5. Hard error with a message pointing at all four sources.

`fbuild build` populates the `nm_path`, `cppfilt_path`, `readelf_path`,
and `objdump_path` fields (and a PIO-shape `aliases` block mirroring
them) in `build_info_<env>.json` so step 3 just works.

## Example: fbuild-built ESP32-S3

```bash
$ fbuild build .
# … link succeeds, writes .fbuild/build/esp32s3/firmware.elf +
# build_info.json with toolchain paths …

$ fbuild bloat .
# resolves ELF via discover_elf_in_project()
# resolves nm/c++filt via build_info.json
# emits text report to stdout
```

Pass `--output-dir <dir>` to drop both `report.json` and `report.md`
into the directory side-by-side. Pass `--json <path>` for JSON only.

## Example: PlatformIO-built ESP32-S3

PlatformIO emits its own `build_info_<env>.json` (with an `aliases`
block) next to `platformio.ini`. `fbuild bloat .pio/build/esp32s3/firmware.elf`
walks up from `.pio/build/esp32s3/` to the project root, finds it, and
reads the toolchain paths from there. No `--nm` needed.

## When auto-discovery falls back to PATH

- The ELF isn't under a project that contains a `build_info.json`
  (e.g. you copied just the ELF into `/tmp`).
- The `build_info.json` is older than the schema this fbuild expects
  (`nm_path` field missing).

In both cases the analyzer falls back to bare `nm` on `PATH`, which
works only for host ELFs. For cross-toolchain ELFs, pass `--nm` or
`--build-info` explicitly.

## Schema: the `aliases` block

`build_info.json` carries a top-level `aliases` block keyed by short
PIO tool names so PlatformIO consumers (FastLED
`ci/util/symbol_analysis.py`, `ci/inspect_binary.py`, etc.) can read
both PIO- and fbuild-built artifacts uniformly:

```json
{
  "esp32s3": {
    "prog_path": "...",
    "cc_path":   "/.../xtensa-esp32s3-elf-gcc",
    "cxx_path":  "/.../xtensa-esp32s3-elf-g++",
    "size_path": "/.../xtensa-esp32s3-elf-size",
    "nm_path":   "/.../xtensa-esp32s3-elf-nm",
    "cppfilt_path": "/.../xtensa-esp32s3-elf-c++filt",
    "readelf_path": "/.../xtensa-esp32s3-elf-readelf",
    "objdump_path": "/.../xtensa-esp32s3-elf-objdump",
    "aliases": {
      "gcc":      "/.../xtensa-esp32s3-elf-gcc",
      "g++":      "/.../xtensa-esp32s3-elf-g++",
      "size":     "/.../xtensa-esp32s3-elf-size",
      "nm":       "/.../xtensa-esp32s3-elf-nm",
      "c++filt":  "/.../xtensa-esp32s3-elf-c++filt",
      "readelf":  "/.../xtensa-esp32s3-elf-readelf",
      "objdump":  "/.../xtensa-esp32s3-elf-objdump",
      "ar":       "/.../xtensa-esp32s3-elf-ar",
      "objcopy":  "/.../xtensa-esp32s3-elf-objcopy"
    }
  }
}
```

Alias keys are only present when the corresponding path is non-empty;
consumers can rely on `"nm" in aliases` meaning the path is real.

## Related

- Issue [#428](https://github.com/FastLED/fbuild/issues/428) —
  toolchain-path schema and CLI auto-discovery (this doc).
- Issue [#434](https://github.com/FastLED/fbuild/issues/434) — the
  `fbuild bloat` meta that subsumes `symbols`.
- PR [#424] / PR [#427] — the fine-grained analyzer and map-derived
  rodata attribution this CLI drives.
