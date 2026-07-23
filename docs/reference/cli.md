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
matching reusable framework-cache entries. `cache` first stops the daemon,
clears the active dev/prod mode's entire global zccache compiler-object store,
restarts the daemon, and then performs `all` for the selected target. Installed
packages, platforms, frameworks, and toolchains are retained.

The daemon refuses `cache` while an operation is active, so the compiler store
is never removed from underneath a build. The command attempts to restore the
daemon even if cache removal fails.

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
| `fbuild clang-tidy` | Run clang-tidy against project sources. |
| `fbuild iwyu` | Run include-what-you-use analysis. |
| `fbuild clang-query` | Run a clang-query matcher. |
| `fbuild lnk pull` | Fetch `.lnk` resource blobs into the cache. |
| `fbuild lnk check` | Verify cached `.lnk` resources. |
| `fbuild lnk add <url>` | Create a `.lnk` manifest for a remote blob. |
| `fbuild mcp` | Start the MCP server for AI assistant integration. |

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
