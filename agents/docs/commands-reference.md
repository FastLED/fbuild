# `fbuild` commands reference (agent-facing)

Every `fbuild` subcommand with one-line purpose and "use this when …"
guidance. Pulled from `fbuild --help` and the in-source docs. **Keep
this short — it's a routing table, not a manual.** Each row's "see
more" link goes to the canonical place for that command's deep
contract (issue tracker, crate-level README, or per-subcommand
help text).

## Build & deploy

| Command | Use this when | See more |
|---|---|---|
| `fbuild build` | You want to compile firmware for the env specified by `-e <env>` and `<project_dir>`. The default path; cache via daemon. | `fbuild help build` |
| `fbuild clean sketch|all|cache` | You want to remove one target's project outputs, optionally its reusable framework objects; `cache` additionally resets the active mode's global compiler-object cache while retaining installed packages/toolchains. | `fbuild help clean`, FastLED/fbuild#1089, FastLED/fbuild#1154 |
| `fbuild deploy` | You want to build AND flash to a connected board. Pass `--monitor` to attach the monitor after flash. | `fbuild help deploy` |
| `fbuild monitor` | You want to attach the serial monitor to an already-running board without re-flashing. | `fbuild help monitor` |
| `fbuild reset` | You want to reset the device without re-flashing. | `fbuild help reset` |
| `fbuild test-emu` | You want to build + run in an emulator (CI-friendly, exits with emulator exit code). | `fbuild help test-emu` |

## Build-graph + size analysis

| Command | Use this when | See more |
|---|---|---|
| `fbuild symbols` | You want per-symbol bloat analysis of an ELF or project. Default path for "what's eating my flash?" | `fbuild help symbols`, `docs/symbols.md` |
| `fbuild bloat graph` | You want a Graphviz back-reference dump for a specific symbol's reachability. | FastLED/fbuild#463 |
| `fbuild compile-many` | You're CI: building the framework + library archives once and fanning out per-sketch compile + link in parallel. | FastLED/fbuild#238 / #241 / #242 |
| `fbuild ci` | You're CI and want `pio ci` compatibility — same shape, fbuild backend. | FastLED/fbuild#242 |

## Toolchain & tooling

| Command | Use this when | See more |
|---|---|---|
| `fbuild clang-tidy` | Run clang-tidy static analysis on project sources. | `fbuild help clang-tidy` |
| `fbuild iwyu` | Run `include-what-you-use` analysis. | `fbuild help iwyu` |
| `fbuild clang-query` | Run a clang-query matcher script over the project. | `fbuild help clang-query` |
| `fbuild clangd-config` | Emit `.clangd` / `.vscode/settings.json` for the default env. | `fbuild help clangd-config` |
| `fbuild ide` / `fbuild ide select` | You want to open a project as an IDE workspace on stock Zed: installs declared deps, refreshes the compile DB, emits `.clangd` + `.zed/settings.json` + `.zed/tasks.json`, and — for probe-rs-supported boards only (RP2040/RP2350, a small ARM Cortex-M set) — `.zed/debug.json` plus a `probe-rs dap-server` task, then launches Zed. Unsupported targets (ESP32, AVR) get a one-line "not supported" note, not a failure. `ide select` interactively (or via `-e`) switches the persisted environment and regenerates. | `fbuild help ide`, FastLED/fbuild#1076 Phase 1 & Phase 3 milestone 1, `docs/reference/cli.md#fbuild-ide` |
| `fbuild lib-select` | Drive the LDF-style library-selection resolver and print the selected library set. Use this when debugging "library not found" without a full build. | FastLED/fbuild#202 / #204 |
| `fbuild debug [project_dir] [-e <env>] [--no-flash] [--port <port>]` | No-probe GDB debugging (FastLED/fbuild#1144): build (unless `--no-flash`) → flash via the existing deploy path → auto-detect the CDC port → launch `gdb` against the exact build's ELF, attaching to the ESP32 family's native IDF/ROM gdbstub. Non-ESP32 targets print a first-class capability-matrix message (CH32V: planned, depends on FastLED/soundwave#38; AVR: honestly unsupported, no trap architecture) and exit nonzero-but-clean instead of attempting orchestration. `fbuild crashdump` and CH32V's injected stub remain out of scope until soundwave#38 lands. | `fbuild help debug`, FastLED/fbuild#1144, `docs/reference/cli.md#fbuild-debug` |
| `fbuild plotter [-p <port>]` | Open the daemon-served Serial Plotter web page (`GET /plotter`) in the default browser: a self-contained, dependency-free `<canvas>` chart over the existing `/ws/serial-monitor` WebSocket, port list from `/api/devices/list`. `fbuild ide` wires this up as the `"fbuild: Serial Plotter"` Zed task. | `fbuild help plotter`, FastLED/fbuild#1076 Phase 2, `docs/reference/cli.md#fbuild-plotter` |
| `fbuild build-progress` | Open the daemon-served Build Progress web page (`GET /build-progress`) in the default browser: status polled from the existing `/api/daemon/info` every ~2s plus a live activity tail over the existing `/ws/logs` broadcast WebSocket — no new daemon endpoints. `fbuild ide` wires this up as the `"fbuild: Build Progress"` Zed task. | `fbuild help build-progress`, FastLED/fbuild#1076 Phase 2, `docs/reference/cli.md#fbuild-build-progress` |
| `fbuild boards` | Open the daemon-served, **read-only** Board Manager web page (`GET /boards`) in the default browser: fetches `GET /api/ide/boards?query=` (new endpoint, backed by `fbuild_config::search_boards`) and renders a filterable table client-side. No install/mutation actions in this first cut. `fbuild ide` wires this up as the `"fbuild: Board Manager"` Zed task. | `fbuild help boards`, FastLED/fbuild#1076 Phase 2, `docs/reference/cli.md#fbuild-boards` |
| `fbuild libraries [project_dir] [-e <env>]` | Open the daemon-served, **read-only** Library Manager web page (`GET /libraries?project=&env=`) for one project/env's declared `lib_deps`: resolves project dir + env the same way `fbuild build`/`fbuild ide` do, then calls `GET /api/ide/libraries?project=&env=` (new endpoint) which classifies each dep (registry/GitHub/git/HTTP-archive/local, via `fbuild_config::classify_lib_dep` — moved out of `fbuild-cli::sync` so the daemon can share it) and reports a best-effort installed flag (checks `<project>/.fbuild/build/<env>/release/libs/`). No install/mutation actions in this first cut. `fbuild ide` wires this up as the `"fbuild: Library Manager"` Zed task. | `fbuild help libraries`, FastLED/fbuild#1076 Phase 2, `docs/reference/cli.md#fbuild-libraries` |

## Daemon & cache

| Command | Use this when | See more |
|---|---|---|
| `fbuild daemon` | Start, stop, or query the long-lived fbuild daemon. | `fbuild help daemon` |
| `fbuild show` | Show daemon logs or other introspection. | `fbuild help show` |
| `fbuild device` | List / inspect connected devices the daemon knows about. | `fbuild help device` |
| `fbuild purge` | Purge cached packages — full purge or LRU-only via `--gc`. | `fbuild help purge` |
| `fbuild lnk` | Manage `.lnk` resource pointers (fetch / verify / add). | `fbuild help lnk` |

## Serial-port introspection (FastLED/fbuild#686)

| Command | Use this when | See more |
|---|---|---|
| `fbuild serial probe list` | You want every visible serial port with VID:PID + board hint annotated. **First thing to run** when an agent's debugging "is COM12 the right port?" | FastLED/fbuild#686 |
| `fbuild serial probe find --vid-pid V:P` | You want one device path on stdout for a literal VID:PID pair, or exit 1 when not found. Useful from scripts. | FastLED/fbuild#686 |
| `fbuild serial probe find --env <name>` | You want the right port for a PlatformIO env name (e.g. `lpc845brk` → the LPC11U35 VCOM bridge, NOT the CMSIS-DAP debug probe). Disambiguates multi-USB-endpoint boards. | FastLED/fbuild#686 |
| `fbuild serial probe read <port>` | You want to open a port with the **correct DTR/RTS for the board family** and dump bytes. Use this instead of ad-hoc `pyserial` / PowerShell `SerialPort` calls. See [docs/usb-cdc-control-line-matrix.md](../../docs/usb-cdc-control-line-matrix.md) for the why. | FastLED/fbuild#686, FastLED/fbuild#689 |

## AI integration

| Command | Use this when | See more |
|---|---|---|
| `fbuild mcp` | Start an MCP server for AI-assistant integration. | `fbuild help mcp` |

## Worked example — "agent needs to debug a silent device on COM20"

The right sequence today:

```bash
# 1. What's actually on COM20?
$ fbuild serial probe list
COM20      16C0:0483  ser=15821020          USB Serial Device  [LPC11U35 VCOM bridge (LPC845-BRK USART0) OR PJRC Teensy USB-Serial]

# 2. Confirm that's the LPC bridge (not the CMSIS-DAP debug port)
$ fbuild serial probe find --env lpc845brk
COM20

# 3. Read with correct DTR/RTS for a CDC-ACM bridge
$ fbuild serial probe read COM20 --seconds 4
…bytes…
```

**Do NOT reach for** ad-hoc PowerShell `SerialPort` or `python -c "import serial"`
probes — those default to DTR=False, which the LPC11U35 bridge treats as
"host not ready" and silently drops every byte. That's the
FastLED/FastLED#3300 / fbuild#684 trap. The `fbuild serial probe`
helpers exist specifically so agents stop rediscovering it.
