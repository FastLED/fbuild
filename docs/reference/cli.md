# CLI Reference

`fbuild` accepts either `fbuild <subcommand> <project-dir>` or
`fbuild <project-dir> <subcommand>` for project-oriented commands.

Global options:

| Option | Description |
|---|---|
| `-e`, `--environment <env>` | Select a `platformio.ini` environment. |
| `-v`, `--verbose` | Print more detail. |
| `-p`, `--port <port>` | Select a serial port. |
| `-c`, `--clean` | Clean before deploy when used by deploy-compatible paths. |
| `--monitor[=<flags>]` | Attach monitor after deploy. |
| `--timeout <secs>` | Set monitor/emulator timeout where supported. |
| `--halt-on-error <regex>` | Stop monitor/emulator on an error pattern. |
| `--halt-on-success <regex>` | Stop monitor/emulator on a success pattern. |
| `--expect <regex>` | Require a pattern in monitor/emulator output. |
| `--platformio` | Use PlatformIO compatibility mode where supported. |
| `--shrink[=<mode>]` | Flash-size reduction mode: `auto`, `off`, `safe`, `aggressive`, or `printf`. |
| `--no-shrink` | Disable all shrink optimizations. |

## Core Workflows

### `fbuild build`

Compile firmware.

```bash
fbuild build
fbuild build -e uno
fbuild build --clean
fbuild build --verbose
fbuild build --project-dir /path/to/project
```

Common options include `--clean`, `--jobs`, `--quick`, `--release`,
`--platformio`, `--dry-run`, `--target compiledb`, `--symbol-analysis`,
`--no-timestamp`, `--output-dir`, `--shrink`, and `--no-shrink`.

### `fbuild clean`

Remove project outputs without compiling or deploying. `sketch` removes only
the selected environment/profile build directory. `all` also removes the exact
matching reusable framework-cache entries. `cache` stops the current-version
daemon and any other live `fbuild-daemon` processes that verifiably own the
same cache root — including legacy pre-#1159 daemons — then takes exclusive
ownership of the cache root, deletes only the active dev/prod mode's
`<fbuild_root>/zccache` compiler-object store, and restarts the daemon.
Installed packages, platforms, frameworks, toolchains, and downloaded fbuild
archives are retained.

The command refuses `cache` while a build operation is active, so the compiler
store is never removed from underneath a build, and refuses if the running
daemon's identity or mode doesn't match the target cache root. It attempts to
restore daemon availability even if cache removal fails.

```bash
fbuild clean sketch
fbuild clean sketch examples/Blink -e uno --quick
fbuild clean all -e esp32dev --release
fbuild clean cache examples/Blink -e uno --release
```

The scope is required; `--quick` and `--release` are mutually exclusive and
default to the release profile. Unlike `sketch` and `all`, `cache` has global
scope within the active fbuild mode and affects compiler-cache hits for every
project using that mode.

### `fbuild deploy`

Build and flash firmware, or deploy an existing build with `--skip-build`.

```bash
fbuild deploy
fbuild deploy -e esp32s3 --port COM5
fbuild deploy --clean
fbuild deploy --monitor
fbuild deploy --monitor="--timeout 60 --halt-on-success \"TEST PASSED\""
```

Common options include `--port`, `--clean`, `--monitor`, `--timeout`,
`--halt-on-error`, `--halt-on-success`, `--expect`, `--no-timestamp`,
`--skip-build`, `--baud`, `--to device|emu|emulator`, `--emulator`, and
`--output-dir`.

`--transport picotool|uf2` selects the RP2040/RP2350 deploy transport
(FastLED/fbuild#1162). Before either transport runs, fbuild tries the normal
1200-bps CDC touch. If no BOOTSEL endpoint appears and the selected runtime
port supplied an exact VID, PID, and USB serial, fbuild asks the Pico SDK
application reset interface to enter BOOTSEL, then reacquires it. Windows uses
the exact healthy WinUSB reset interface mapped to the selected CDC identity;
other hosts fall back to managed
`picotool reboot -u --vid 0x<vid> --pid 0x<pid> -f`. Picotool derives the
runtime serial from the opened application device, and refuses a forced command
when that VID/PID is ambiguous. fbuild never emits an unscoped reset request.

`picotool` is the default transport: fbuild tries the target-bound PICOBOOT
vendor interface first (Windows preflight checks for a missing WinUSB driver,
then a bounded `picotool info` probe and `picotool load -x`) and falls back to
BOOTSEL mass-storage. `uf2` preserves the mass-storage-first order with
picotool as the fallback. The picotool load timeout defaults to 60s and is
configurable with `FBUILD_RP2040_PICOTOOL_TIMEOUT_SECS`.

### `fbuild monitor`

Attach to serial output.

```bash
fbuild monitor
fbuild monitor -e uno --port COM3 --baud 115200
fbuild monitor --timeout 60 --halt-on-error "TEST FAILED" --halt-on-success "TEST PASSED"
```

### `fbuild test-emu`

Build and run firmware in an emulator, then exit with the emulator result.

```bash
fbuild test-emu . -e uno
fbuild test-emu tests/platform/esp32s3 -e esp32s3 --emulator qemu --timeout 10
```

See [emulator testing](../guides/emulator-testing.md) for backend rules and
known limitations.

## Device And Daemon Operations

| Command | Purpose |
|---|---|
| `fbuild reset` | Reset a device without flashing. |
| `fbuild device list` | List connected devices. |
| `fbuild device status <port>` | Show detailed device status. |
| `fbuild device lease <port>` | Acquire a device lease. |
| `fbuild device release <port>` | Release a device lease. |
| `fbuild device take <port> --reason <text>` | Preempt the current holder. |
| `fbuild daemon status` | Show daemon status. |
| `fbuild daemon stop` | Stop the daemon gracefully. |
| `fbuild daemon restart` | Restart the daemon. |
| `fbuild daemon list` | List running daemon instances. |
| `fbuild daemon kill [--pid <pid>] [--force]` | Kill one daemon process. |
| `fbuild daemon kill-all [--force]` | Kill all daemon processes. |
| `fbuild daemon locks` | Show project and serial locks. |
| `fbuild daemon clear-locks` | Clear stale locks. |
| `fbuild daemon cache-stats` | Show disk cache statistics. |
| `fbuild daemon gc` | Run disk cache garbage collection. |
| `fbuild daemon monitor` | Tail daemon logs. |
| `fbuild show daemon` | Show daemon logs. |
| `fbuild purge` | Purge cached packages or run `--gc`. |

## Diagnostics And Analysis

| Command | Purpose |
|---|---|
| `fbuild symbols <elf-or-project>` | Per-symbol bloat analysis. See [symbols.md](../symbols.md). |
| `fbuild bloat graph <input> --symbol <name>` | Render a Graphviz back-reference graph. |
| `fbuild bloat lookup <input> --symbol <name>` | Inspect one symbol's size and references. |
| `fbuild lib-select` | Debug LDF-style library selection. |
| `fbuild clangd-config [--editor vscode\|zed] [--refresh]` | Emit `.clangd` (editor-neutral) plus per-editor project config (`.vscode/*` or `.zed/*`). `--editor` selects the emitter (default `vscode`); `--refresh` forces `compile_commands.json` regeneration even if it already exists. |
| `fbuild ide [project_dir] [-e <env>] [--no-launch]` | Open the project as an IDE workspace on stock Zed. See [`fbuild ide`](#fbuild-ide) below. |
| `fbuild ide select [project_dir] [-e <env>]` | Interactively (or with `-e`) choose the environment used for the IDE config, persist it, and regenerate. |
| `fbuild plotter [-p <port>]` | Open the daemon-served Serial Plotter web page in the default browser. See [`fbuild plotter`](#fbuild-plotter) below. |
| `fbuild build-progress` | Open the daemon-served Build Progress web page in the default browser. See [`fbuild build-progress`](#fbuild-build-progress) below. |
| `fbuild boards` | Open the daemon-served, read-only Board Manager web page in the default browser. See [`fbuild boards`](#fbuild-boards) below. |
| `fbuild libraries [project_dir] [-e <env>]` | Open the daemon-served, read-only Library Manager web page for this project/environment's declared `lib_deps`. See [`fbuild libraries`](#fbuild-libraries) below. |
| `fbuild clang-tidy` | Run clang-tidy against project sources. |
| `fbuild iwyu` | Run include-what-you-use analysis. |
| `fbuild clang-query` | Run a clang-query matcher. |
| `fbuild lnk pull` | Fetch `.lnk` resource blobs into the cache. |
| `fbuild lnk check` | Verify cached `.lnk` resources. |
| `fbuild lnk add <url>` | Create a `.lnk` manifest for a remote blob. |
| `fbuild mcp` | Start the MCP server for AI assistant integration. |

### `fbuild ide`

Open (or configure) a project as an IDE workspace on stock Zed
(FastLED/fbuild#1076 Phase 1). This installs the project's declared
dependencies, refreshes `compile_commands.json`, writes the same
editor-neutral `.clangd` as `fbuild clangd-config --editor zed`, merges an
fbuild-owned `.zed/tasks.json`, and — unless `--no-launch` is given —
launches Zed.

```bash
fbuild ide                       # current dir, resolved env, launches Zed
fbuild ide tests/platform/uno -e uno
fbuild ide --no-launch            # generate/refresh config only
fbuild ide select                 # interactive environment picker
fbuild ide select -e esp32dev     # non-interactive: pick esp32dev directly
```

Environment resolution, in order: an explicit `-e`, then the environment
persisted by a previous `fbuild ide` / `fbuild ide select` run
(`<project>/.fbuild/ide_state.json`), then `platformio.ini`'s default
environment.

Generated/updated files:

- `compile_commands.json` — regenerated on every `fbuild ide` invocation
  (equivalent to `fbuild build -t compiledb`). Machine-specific (absolute
  paths) — **recommend adding it to `.gitignore`**.
- `.clangd` — editor-neutral, safe to commit.
- `.zed/settings.json` — merge-don't-clobber (maps `.ino` to C++, sets
  `lsp.clangd.binary.arguments`); safe to commit.
- `.zed/tasks.json` — merge-don't-clobber: fbuild only replaces tasks whose
  label starts with `"fbuild: "` (Build, Build (clean), Deploy, Deploy +
  Monitor, Monitor, Reset, Serial Plotter, Build Progress, Board Manager,
  Library Manager, Select environment); any other
  task you've added is left untouched. Safe to commit.
- `.fbuild/ide_state.json` — the persisted environment choice. Local
  developer state; recommend `.gitignore`.
- `.zed/debug.json` — merge-don't-clobber, **only written when the
  environment's board resolves to a probe-rs-supported chip** (see
  "Debugging" below). Safe to commit.

If `zed` isn't found on `PATH` or in the usual per-OS install locations,
`fbuild ide` still generates/refreshes every file above and exits
successfully — it just prints install guidance (`winget install Zed.Zed`,
`brew install --cask zed`, or <https://zed.dev/download>) instead of
launching the editor. Config generation is the product; launching Zed is a
convenience on top of it.

#### Debugging (FastLED/fbuild#1076 Phase 3, milestone 1)

`fbuild ide` also tries to wire up Zed's debugger for the current
environment. **Milestone 1 covers probe-rs-supported targets only** — RP2040
and RP2350 (probe-rs chip `RP235x`), and a small, deliberately conservative
set of ARM Cortex-M chips (currently: Teensy 4.x — iMXRT1062, probe-rs chip
`MIMXRT1060`, requires the debug pads wired to a probe — plus nRF52840 and
the STM32F103C8 "Blue Pill"; names verified against probe-rs's target files).
ESP32 and AVR are **not** probe-rs targets (ESP32 debugging goes through
OpenOCD, which speaks GDB-remote, not DAP; AVR has no comparable open
debug-adapter story) and are explicitly out of scope for this milestone —
for those environments `fbuild ide` prints a one-line note (`debug config
not supported for <board/mcu> (milestone 1 is probe-rs targets)`) and moves
on. This is a normal, expected outcome, not a failure.

When the environment's board resolves to a mapped chip, `fbuild ide`:

- Writes `.zed/debug.json` with an fbuild-owned entry
  (`"fbuild: Debug (probe-rs attach)"`) that attaches Zed's debugger to a
  TCP DAP server at `127.0.0.1:50101` — the same merge-don't-clobber
  ownership convention as `.zed/tasks.json` (fbuild only ever touches
  entries whose label starts with `"fbuild: "`).
- Adds a `"fbuild: Debug server (probe-rs)"` task to `.zed/tasks.json` that
  runs `probe-rs dap-server --port 50101 --chip <CHIP>`. Run this task
  first (it's a long-lived server), then start the "Debug (probe-rs
  attach)" debug config to connect.

fbuild does **not** install `probe-rs` itself in milestone 1 — install it
yourself (`cargo install probe-rs-tools`, see
<https://probe.rs/docs/getting-started/installation/>). If it's missing,
the debug-server task fails in Zed's terminal panel with probe-rs's own
"command not found" message.

Port `50101` is fixed today (not yet configurable via a flag). The
`program` field in the generated debug entry points at the expected
`fbuild build -e <env>` ELF output location, whether or not it exists yet —
build once before attaching.

### `fbuild debug`

No-probe GDB debugging orchestration (FastLED/fbuild#1144): build → flash →
find the port → launch `gdb` against the exact build's ELF. This is a
separate command from `fbuild ide`'s probe-rs-based `.zed/debug.json`
generation above — `fbuild debug` targets GDB-remote-speaking on-device
stubs (ESP32's native gdbstub) rather than probe-rs's DAP server, and it
launches `gdb` directly instead of wiring up an editor.

```bash
fbuild debug -e esp32dev                  # build, flash, attach gdb
fbuild debug -e esp32dev --no-flash       # attach to the existing build
fbuild debug -e esp32dev --port COM5      # skip port auto-detection
```

#### Capability matrix

| Target | Mechanism | Status in `fbuild debug` |
|---|---|---|
| ESP32 family (`espressif32`) | Orchestrates the framework's native IDF/ROM gdbstub — no code injection needed. | **Implemented.** |
| CH32V (`ch32v`) | Injected stub (EBREAK + trap handler, CDC transport). | Planned, not yet implemented — depends on the on-device side in FastLED/soundwave#38. Prints an explanatory message and exits nonzero-but-clean. |
| AVR (`atmelavr`, `atmelmegaavr`) | None. | Honestly unsupported: AVR has no trap architecture to host a debug monitor. Prints an explanatory message and exits nonzero-but-clean. |
| Everything else (ARM Cortex-M families, RP2040, ...) | Per-family DebugMon/BKPT stubs exist but haven't been evaluated for this command. | Not yet supported by `fbuild debug`; see `fbuild ide`'s probe-rs path above for RP2040/RP2350 and the small ARM chip set it covers. |

Unsupported/planned targets never attempt to build or flash — the
capability check runs first and the command exits cleanly (no stack
trace, no partial build).

#### What works today on ESP32, and what's still pending sdkconfig support

The ROM/IDF gdbstub component itself is a stock part of every Arduino-ESP32
build — `fbuild debug` doesn't need to inject anything for it to exist.
What's still pending is *panic-triggers-gdbstub* behavior
(`CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y`): fbuild's sdkconfig-override story
(`docs/sdkconfig.md`) is a **design proposal, not yet implemented**, so
`fbuild debug` cannot flip that flag for you today. Every invocation
prints an honest readiness note:

- If the project's `sdkconfig`/`sdkconfig.defaults` already sets
  `CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=y`, the note says panics will drop
  straight into gdb.
  - Otherwise (the common case — Arduino-ESP32's framework default is
  `panic=print`), the note says panics only print a backtrace and reboot;
  gdb can still attach and control a *live* break (Ctrl+C in gdb, or a
  deliberate breakpoint), and the note points at the existing
  general-purpose escape hatch — `build_flags = -D
  CONFIG_ESP_SYSTEM_PANIC_GDBSTUB=1` in the environment's
  `platformio.ini` section — as the way to get panic-triggered gdbstub
  today, ahead of the proper sdkconfig-override layer landing.

The orchestration half (build/flash/port-discovery/ELF-and-toolchain
resolution/gdb launch) is what this command implements; #1144 stays open
for CH32V's injected stub and `fbuild crashdump`.

#### Mechanics

- **ELF resolution** reuses `fbuild symbols`' discovery
  (`build_info.json` → `.fbuild/build/**/firmware.elf` →
  `.pio/build/**/firmware.elf` → loose `*.elf`), so gdb always gets the
  exact build's symbols.
- **Port discovery**: `--port` wins; otherwise `fbuild debug` asks the
  daemon for the device list (`POST /api/devices/list`) and uses the port
  automatically if exactly one serial device is present, else it asks you
  to pass `--port`.
- **gdb resolution**: prefers deriving `gdb` from the build's own `gcc`
  path (`build_info.json`'s `cc_path`, GCC cross-toolchain naming
  convention: `<prefix>gcc` → `<prefix>gdb` in the same directory), then
  falls back to searching `PATH` for the architecture's conventional name
  (`riscv32-esp-elf-gdb` for RISC-V ESP32 chips, `xtensa-<mcu>-elf-gdb` /
  `xtensa-esp32-elf-gdb` for Xtensa chips). If neither resolves, the error
  names the toolchain prefix so you know what to install/add to `PATH` —
  the ESP32 GCC toolchain ships `gdb` alongside `gcc`.
- **Launch**: `gdb -ex "set serial baud 115200" -ex "target remote <port>"
  <elf>`, spawned interactively with the parent's stdio (so it behaves
  exactly like running `gdb` yourself). On Windows, bare `COMx` names are
  rewritten to `\\.\COMx` for the `target remote` argument.

#### Hardware validation caveat

The capability matrix, argv construction, gdb/ELF resolution, and CLI
parsing are covered by unit tests (`crates/fbuild-cli/src/cli/debug.rs`)
against fake toolchain/project fixtures — no hardware or real `gdb`
required. The interactive `gdb` attach itself has not been validated
against real ESP32 hardware as part of this change; treat it as
implemented-but-unvalidated until it's been run against a physical board.

### `fbuild plotter`

Open the daemon-served Serial Plotter web page in the default browser
(FastLED/fbuild#1076 Phase 2). The page (`GET /plotter` on the daemon,
`crates/fbuild-daemon/web/plotter/index.html`) is a single self-contained
HTML file with no external dependencies: it connects to the existing
`/ws/serial-monitor` WebSocket (the same one `fbuild monitor` uses),
populates its port selector from `POST /api/devices/list`, parses numeric
series out of incoming lines Arduino-Serial-Plotter style
(whitespace/comma-separated numbers, optional `name:value` labels), and
renders a live scrolling line chart on a `<canvas>` — pause/resume, clear,
and a raw-output tail are all built in.

```bash
fbuild plotter                # opens the page; pick a port in the UI
fbuild plotter --port COM3    # pins the page to a port and auto-connects
```

`fbuild plotter` itself does no parsing or rendering — its only job is to
make sure the daemon is running and open `http://127.0.0.1:<daemon
port>/plotter[?port=<port>]` in the OS default browser. `fbuild ide`
generates a `"fbuild: Serial Plotter"` Zed task that just runs `fbuild
plotter` (see [`fbuild ide`](#fbuild-ide) above), so the same command works
whether or not you're using Zed.

### `fbuild build-progress`

Open the daemon-served Build Progress web page in the default browser
(FastLED/fbuild#1076 Phase 2, second panel). The page (`GET
/build-progress` on the daemon,
`crates/fbuild-daemon/web/build-progress/index.html`) is a single
self-contained HTML file with no external dependencies. It is built
entirely out of existing, unmodified daemon endpoints:

- `GET /api/daemon/info`, polled every ~2s, for a status header (idle /
  building / deploying / ..., the current operation description, and any
  in-progress dependency install).
- `GET /ws/logs`, the existing daemon-wide broadcast log WebSocket, for a
  live scrolling activity pane with an autoscroll toggle and a clear
  button.

Note this is daemon-wide activity, not a literal line-by-line tail of one
build's compiler output: the build's own NDJSON log stream
(`POST /api/build`) is scoped to the HTTP request that started it and
isn't broadcast to other clients, so a second page can't attach to
"someone else's" in-flight build output without new server-side broadcast
plumbing. `/ws/logs` and `/api/daemon/info` are the observable,
already-broadcast alternative this page uses instead — see the
"Observability reality" comment in
`crates/fbuild-daemon/src/handlers/build_progress.rs` for the full
rationale.

```bash
fbuild build-progress   # opens the page
```

`fbuild ide` generates a `"fbuild: Build Progress"` Zed task that just runs
`fbuild build-progress`, so the same command works whether or not you're
using Zed.

### `fbuild boards`

Open the daemon-served Board Manager web page in the default browser
(FastLED/fbuild#1076 Phase 2, third panel). **Read-only first cut**:
browsing/inspection only, no install/mutation actions. The page (`GET
/boards` on the daemon, `crates/fbuild-daemon/web/boards/index.html`) is a
single self-contained HTML file with no external dependencies. It fetches
the full board list once from `GET /api/ide/boards` — backed by
`fbuild_config::search_boards` over the same embedded PlatformIO-registry
board database `fbuild build`/`fbuild deploy` already use — then filters
and expands rows entirely client-side. `?query=<substring>` also narrows
the endpoint's response server-side (case-insensitive match on id/name),
for programmatic callers.

```bash
fbuild boards   # opens the page
```

Daemon-global — no project context needed, so this command takes no
arguments. `fbuild ide` generates a `"fbuild: Board Manager"` Zed task
that just runs `fbuild boards`.

### `fbuild libraries`

Open the daemon-served Library Manager web page in the default browser
(FastLED/fbuild#1076 Phase 2, fourth panel). **Read-only first cut**:
browsing/inspection only, no install/mutation actions. Unlike `fbuild
boards`/`fbuild plotter`/`fbuild build-progress`, this page is not
daemon-global — it needs a project directory and environment to know
which `platformio.ini` to read.

```bash
fbuild libraries                    # current dir, resolved env
fbuild libraries tests/platform/uno -e uno
```

`fbuild libraries` resolves the project dir the same way `fbuild
build`/`fbuild ide` do (subcommand arg, else the top-level positional,
else `.`) and the environment the same way (`-e`, else
`platformio.ini`'s default environment), then opens `http://127.0.0.1:<daemon
port>/libraries?project=<dir>&env=<env>` — the page reads those two query
params straight back out of its own URL to call `GET
/api/ide/libraries?project=&env=`. Opening the page without `?project=`
(e.g. navigating to `/libraries` directly) renders a short how-to pointing
back at this command.

The data endpoint parses the environment's `lib_deps`, classifies each
entry with the same classifier `fbuild sync` uses (registry / GitHub /
git / HTTP archive / symlink / `file://` / local path —
`fbuild_config::classify_lib_dep`, moved there from `fbuild-cli::sync` so
the daemon can reuse it), and reports a best-effort installed/not-installed
flag per entry: it checks the *release* build profile's `<project>/.fbuild/
build/<env>/release/libs/` directory for a same-named subdirectory. That
directory only exists after a build that needed dependencies has actually
run, so on a fresh checkout every entry reports "not installed" even
though the source is perfectly resolvable — the response's
`install_state_note` field (also rendered on the page) spells this out.

`fbuild ide` generates a `"fbuild: Library Manager"` Zed task that runs
`fbuild libraries -e <env>` for the environment the IDE config was
generated for.

## Batch And CI Commands

### `fbuild compile-many`

Build many sketches against one board using a two-stage pipeline: framework and
library archives are built once, then sketch compile/link jobs fan out.

```bash
fbuild compile-many --board uno examples/Blink examples/Fire2012
fbuild compile-many --board teensy41 --framework-jobs 2 --sketch-jobs 8 --release sketches/*
```

### `fbuild ci`

PlatformIO-compatible CI entry point. It maps common `pio ci` flags onto
`compile-many`.

```bash
fbuild ci --board uno --lib ./libs --lib ./more -c custom.ini \
  examples/Blink/Blink.ino examples/Fire2012/Fire2012.ino
```

| `fbuild ci` flag | `pio ci` equivalent | Behavior |
|---|---|---|
| `--board <b>`, `-b <b>` | `--board`, `-b` | Required board id. |
| `--lib <path>`, `-l <path>` | `--lib`, `-l` | Repeatable extra library search path. |
| `--project-conf <path>`, `-c <path>` | `--project-conf`, `-c` | Use a shared `platformio.ini`. |
| `--keep-build-dir` | `--keep-build-dir` | Accepted; fbuild always keeps `.fbuild/build/...`. |
| `--build-dir <path>` | `--build-dir` | Accepted but not yet honored. |
| `--framework-jobs`, `--sketch-jobs`, `--quick`, `--release`, `--verbose` | n/a | fbuild-native controls. |

## Keeping This Reference Current

This file is the user-facing CLI reference. Crate-local README files under
`crates/` describe implementation internals. When the Clap command surface
changes, update this reference or add generation/CI validation so it cannot
drift.
