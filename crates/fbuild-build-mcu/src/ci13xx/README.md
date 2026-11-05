# Native CI13xx support

Initial native compile/link support targets CI1306 with the Chipintelli Arduino
1.0.18 core and Nuclei GCC 9.2 on Linux x86_64. Package archives are SHA256-pinned.
The SDK source manifest and vendor platform recipes supply compiler/linker flags;
SDK sources compile through the shared engine, without executing vendor scripts.

Use `platform = ci13xx`, `board = ci1306`, `framework = arduino`.
For non-LTO compilation set `build_unflags = -flto` and
`build_flags = -fno-lto`. The cross-host vendor archives contain precompiled code;
these flags apply to sources compiled by fbuild, not those archives.

This initial path produces a linked ELF and raw user-code binary, not the vendor
combined two-image firmware. Deployment and resource-generation hooks are not
implemented. Unsupported hosts, variants, algorithms, and custom resources must
be rejected rather than silently building a different configuration.

`lib_deps` uses the shared library resolver, manifest source filters and archive
compiler. Named local dependencies use the normal syntax, for example
`lib_deps = MyLibrary=symlink:///absolute/path/to/MyLibrary`. Their discovered
include directories reach sketch compilation, and library archives are linked
inside the vendor library group. Vendor target defines and user no-LTO flags
also reach library compilation. Dependencies are built outside their source
checkout. Framework-bundled Arduino library selection is not implemented in this
initial port; applications requiring it need a separate supported dependency.

The native compile database currently records engine-compiled core, SDK and
sketch commands. External library compilation uses the shared library compiler;
its commands are not synthesized into that database. Inspect verbose build logs
and retained library objects/archives when evaluating dependency compilation.

Project sketches use the shared `SourceScanner` after dependency include discovery.
Ordinary `.ino` tabs receive Arduino header/prototype generation and tab line
mapping; a `main.cpp` that includes its own tabs follows the shared duplicate
avoidance policy. Source-directory overrides and `build_src_filter` use the
shared configuration/parser behavior. Generated `.ino.cpp` compilation remains
visible in the native GNU compile database.

The SDK source manifest is deliberately compiled without LTO, matching the
pinned vendor `compile_sdk_sources.py`. Its scheduler call must remain an
external relocation for GNU ld `--wrap,vTaskStartScheduler`: that wrapper creates
the Arduino task that runs `setup()`/`loop()`. Applying LTO across SDK sources
can inline the real scheduler before wrapping and discard the whole sketch.
Core, sketch and external-library defaults retain LTO, as does linking. Explicit
no-LTO configuration still applies to every source group.

The selected board supplies the CPU define and reported flash/RAM budgets.
Native provisioning/build validates its CPU/core/variant against the installed,
pinned vendor `boards.txt` recipe; unsupported clocks are rejected rather than
silently compiled as the default clock. Offline checks with a missing framework
report the missing package and defer recipe validation until it is installed.

When the default `src/` directory is absent, ordinary Arduino sketches at the
project root are preprocessed through the same scanner. An explicit source-dir
selection is respected and does not silently fall back to a different tree.

## Configuration ownership

The CI1306 descriptor uses the existing `BoardConfig` schema: nested `build`
fields supply MCU, clock, core, variant and ISA/ABI metadata; nested `upload`
capacity fields supply the flash/RAM budgets consumed by native size reporting.
Top-level clock/memory display aliases are omitted to avoid maintaining a second
independent authority for those values. No uploader protocol or speed is
invented: fbuild deployment for this platform remains unsupported.

The remaining Rust defines adapt the Arduino architecture/compatibility contract
and the vendor SDK source-hook contract (`ARDUINO_ARCH_CI13XX`, Arduino version,
`CI_ARDUINO_CORE`, and `CI_CONFIG_FILE`). They do not replace board clock,
capacity, core or variant configuration. Bulk compiler/linker flags and board
recipe values come from the pinned vendor package's
[`platform.txt`](https://github.com/coloz/arduino-ci130x/blob/6a7f7ab6e526d92d619d4e46923dbe38b9454b8f/platform.txt),
[`boards.txt`](https://github.com/coloz/arduino-ci130x/blob/6a7f7ab6e526d92d619d4e46923dbe38b9454b8f/boards.txt),
and SDK manifest. The explicit SDK non-LTO boundary reproduces that package's
source-build hook so the linker scheduler wrapper can reach the Arduino sketch.
