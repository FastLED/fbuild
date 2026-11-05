# fbuild-build integration tests

All integration tests for `fbuild-build` compile into this single `it` binary; each
`*.rs` here is a module listed in `main.rs`. Add new integration tests
as a module here, not as a new top-level `tests/*.rs` file: every top-level
file is a separate binary that re-links the crate's whole dependency graph
(zackees/ci.yml RUST-005).

`samd_acceptance` builds real SAMD21 and SAMD51 firmware with a sketch-provided
interrupt handler (including an LTO build), plus ClearCore vendor syscall linkage. Run it explicitly with
`soldr cargo test -p fbuild-build --test it samd_acceptance:: -- --ignored`.

`renesas_acceptance` builds UNO R4 firmware using `String` and `IPAddress`
and verifies that the supported default core supplied both source files.
Run with `soldr cargo test -p fbuild-build --test it renesas_acceptance:: -- --ignored`.

`rp2040_acceptance` builds a real default `pico` project, including boot2
assembly and USB string definitions. Run it with
`soldr cargo test -p fbuild-build --test it rp2040_acceptance:: -- --ignored`.

`link_overflow` invokes native GCC and the production linker to check failed
GNU links report each overflowing region's actual map capacity (#1658).

`ci13xx_fastled_acceptance` requires `FBUILD_FASTLED_SOURCE` to name an actual
FastLED Git checkout. It uses native `lib_deps = FastLED=symlink://...`, the
production CI1306 descriptor, and retained software SPI probes on PA2/PA3 at
1 MHz. Run explicitly with:

```bash
FBUILD_FASTLED_SOURCE=/absolute/path/to/FastLED \
FBUILD_CI13XX_ARTIFACT_DIR=/absolute/path/to/artifacts \
soldr cargo test -p fbuild-build --test it \
  ci13xx_fastled_acceptance::ci1306_real_fastled_software_spi_no_lto \
  -- --exact --ignored --nocapture
```

The probe also calls out-of-line `FastLED.count()` and requires its linked
definition. The emitted native link command must contain a real FastLED archive;
its object members must target RISC-V and contain no `.gnu.lto_*` sections.
The optional artifact directory retains the ELF, native GNU compile database,
link command, map, linked archives, probe, INI, size report and Git/source-byte
provenance under `fastled-ci1306/`.
This is compile/link evidence, not a measurement of physical GPIO timing.

`ci13xx_fastled_acceptance::ci1306_real_fastled_software_spi_default_lto` is a
separate real build using vendor defaults, with no LTO override. It requires
`-flto` in core/sketch/link commands and actual GCC LTO sections in the linked
FastLED archive. SDK commands must preserve the vendor's non-LTO scheduler-wrap
boundary. Actual Nuclei disassembly must connect SDK main to the Arduino
scheduler wrapper, task creation and sketch setup/loop. It retains evidence under `fastled-ci1306-default-lto/` using
the same explicit source/artifact environment variables. Run this selector and
the no-LTO selector sequentially: both own the native compile backend.
