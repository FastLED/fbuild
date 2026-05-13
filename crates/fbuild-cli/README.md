# fbuild-cli

Clap-based CLI for fbuild. Thin HTTP client that delegates most work to the daemon. Subcommands: build, deploy, test-emu, monitor, reset, purge, daemon, device, show, mcp, clang-tidy, iwyu, clang-query, compile-many, ci.

## Key Types

- `DaemonClient` -- async HTTP client wrapping reqwest; methods for build, deploy, monitor, health, device management, lock management
- `BuildRequest` / `DeployRequest` / `MonitorRequest` -- JSON request bodies sent to daemon endpoints
- `OperationResponse` -- parsed daemon response with success/exit_code/message

## Modules

- **daemon_client** -- `DaemonClient`, request/response types, `ensure_daemon_running` (spawn + stale detection), streaming NDJSON build output
- **mcp** -- stdio-based MCP (Model Context Protocol) JSON-RPC server for AI assistant integration

## Subcommands

- `build` -- compile firmware (supports streaming output, compiledb target, quick/release profiles)
- `deploy` -- flash firmware to device (optional post-deploy monitor, emulator support via `--to emu`)
- `test-emu` -- build firmware and run it in an emulator (avr8js, simavr, or QEMU) with pattern matching and timeout
- `monitor` -- serial monitor with halt-on-error/success and timeout
- `reset` -- reset device via DTR/RTS without re-flashing
- `purge` -- clear cached packages and build artifacts
- `daemon` -- start/stop/info/restart/logs management
- `device` -- list/status/lease/release/preempt connected devices
- `show` -- display daemon logs
- `mcp` -- start MCP server for AI assistants
- `compile-many` -- two-stage compile of many sketches against the same board (FastLED/fbuild#238, PR #241)
- `ci` -- PlatformIO-compatible CI command (drop-in for `pio ci`, FastLED/fbuild#242)

## `fbuild ci` -- PlatformIO-compatible CI command

`fbuild ci` is a drop-in replacement for [`pio ci`](https://docs.platformio.org/en/latest/core/userguide/cmd_ci.html) that delegates to the two-stage `compile-many` primitive shipped in PR [#241](https://github.com/FastLED/fbuild/pull/241). It lets existing CI workflows swap `s/pio ci/fbuild ci/` without other changes. Tracking issue: [#242](https://github.com/FastLED/fbuild/issues/242).

### Flag mapping

| `fbuild ci` flag | `pio ci` equivalent | Behavior |
|---|---|---|
| `--board <b>` / `-b <b>` | `--board` / `-b` | Required. Board id (e.g. `uno`, `teensy41`). |
| `--lib <path>` / `-l <path>` (repeatable) | `--lib` / `-l` | Mapped to `PLATFORMIO_LIB_EXTRA_DIRS`; joined with `;` on Windows, `:` elsewhere. |
| `--project-conf <path>` / `-c <path>` | `--project-conf` / `-c` | Mapped to `PLATFORMIO_PROJECT_CONFIG`. Canonicalized when possible. |
| `--keep-build-dir` | `--keep-build-dir` | Accepted for compatibility; no-op (fbuild always keeps build dirs under `.fbuild/build/...`). |
| `--build-dir <path>` | `--build-dir` | Accepted for compatibility; not yet honored. Emits a warning when set. |
| `--framework-jobs` / `--sketch-jobs` / `--quick` / `--release` / `--verbose` (-v) | n/a | fbuild-native; see `compile-many`. |
| positional sketches | positional | Each entry may be a project dir or a `.ino` file (its parent dir is used). |

### Example

```bash
fbuild ci --board uno --lib ./libs --lib ./more -c custom.ini \
  examples/Blink/Blink.ino examples/Fire2012/Fire2012.ino
```

Exits 0 when every sketch builds, non-zero on any failure.
