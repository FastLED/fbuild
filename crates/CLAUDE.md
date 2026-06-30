# Crates Architecture

> **Agents:** for task-specific routing (which `fbuild` subcommand,
> how deploy actually works, DTR/RTS rules), see the root
> [`../CLAUDE.md`](../CLAUDE.md) routing table and
> [`../agents/docs/`](../agents/docs/README.md). This doc covers
> crate boundaries and dependency policy — not "how to do task X".

## Monocrate policy — do not add new crates

fbuild is kept as close to a monocrate as possible. The set of crates below is
intentionally fixed. **New functionality is folded into an existing crate as a
module, never introduced as a new crate.** Examples that already follow this:
per-platform build orchestrators are modules under `fbuild-build`, and the
running-process v1 broker adoption is a module under `fbuild-daemon/src/broker/`
(it was deliberately *not* kept as a standalone `fbuild-broker` crate —
FastLED/fbuild#560).

When something is needed by two crates that cannot depend on each other (the
classic case: `fbuild-cli` is a thin HTTP client and must not depend on
`fbuild-daemon`), put the shared, dependency-free pieces in a crate both already
depend on — `fbuild-core` or `fbuild-paths` — and keep the heavy/transport code
in the consuming crate. (That is exactly how the broker's display constants and
`CacheRoots` live in `fbuild-paths::running_process` while the prost/session
machinery stays in `fbuild-daemon`.)

This is enforced by CI: `crate-gate.yml` runs `ci/check_workspace_crates.py`,
which fails if `[workspace] members` gains an entry outside the approved
allowlist. A genuinely-justified new crate requires adding it to that allowlist
in the same PR with a maintainer-reviewed rationale.

## Dependency Graph

```
fbuild-daemon (bin) ──────────────────────────────────────────┐
  ├─ fbuild-build ─── fbuild-config ─── fbuild-core          │
  │                ├── fbuild-paths ─── fbuild-core           │
  │                └── fbuild-packages ─── fbuild-paths       │
  ├─ fbuild-deploy ── fbuild-serial ─── fbuild-core           │
  │                ├── fbuild-config                          │
  │                └── fbuild-paths                           │
  ├─ fbuild-serial ── fbuild-core (DashMap, broadcast, tokio) │
  └─ fbuild-packages ── fbuild-config, fbuild-paths           │
                                                              │
fbuild-cli (bin: "fbuild") ──────────────────────────────────┤
  ├─ fbuild-core                                              │
  ├─ fbuild-config                                            │
  └─ fbuild-paths                                             │
                                                              │
fbuild-python (cdylib: PyO3 bindings) ───────────────────────┤
  ├─ fbuild-core                                              │
  ├─ fbuild-serial                                            │
  └─ fbuild-paths                                             │
                                                              │
fbuild-test-support (test utilities) ────────────────────────┘
```

## Crate Responsibilities

- **fbuild-core** — `FbuildError`/`Result`, `BuildProfile`, `Platform`, `SizeInfo`, `DaemonState`. Also ships the `dump_usb_ids` example (`examples/dump_usb_ids.rs`) — **tier-1 source** for the nightly `online-data` USB-VID merge (FastLED/fbuild#720). Do not delete; the `usb-ids` workspace dep exists *only* for that example, so removing it drops the aggregator from 4 → 3 independent sources.
- **fbuild-config** — `PlatformIOConfig` (INI parser with `extends` inheritance), `BoardConfig`, `McuSpec`
- **fbuild-paths** — Dev/prod path isolation (`~/.fbuild/{dev|prod}/`), port mapping (8765/8865), cache dirs
- **fbuild-packages** — URL-based package downloads, toolchain resolution, library manager, parallel pipeline
- **fbuild-serial** — `SharedSerialManager` (centralized serial I/O), deploy preemption protocol, WebSocket messages, USB-CDC retry logic
- **fbuild-build** — `BuildOrchestrator` trait, per-platform orchestrators (AVR, ESP32, ESP8266, RP2040, STM32, Teensy, WASM)
- **fbuild-deploy** — `Deployer` trait, esptool/avrdude/picotool invocation, firmware upload
- **fbuild-daemon** — Axum HTTP/WS server, request processors, device lease manager, lock manager, emulator runners (avr8js, simavr, QEMU)
- **fbuild-cli** — Clap CLI: build, deploy, test-emu, monitor, purge subcommands. Thin HTTP client to daemon.
- **fbuild-python** — PyO3 bindings: `SerialMonitor`, `Daemon`, `DaemonConnection`, `connect_daemon`
- **fbuild-test-support** — `create_test_project()`, temp dir fixtures

## Key Design Patterns

**Daemon-centric:** All serial access routes through `SharedSerialManager` in the daemon. No OS-level port locks. Multiple readers (broadcast), exclusive writer (condition variable).

**Deploy preemption:** Deploy forcibly closes serial sessions → flash via esptool → 2s USB re-enumeration delay → monitors auto-reconnect. Monitors with `auto_reconnect=true` pause during preemption.

**HTTP API boundary:** CLI sends JSON requests to daemon over HTTP. Build output streams via WebSocket. Serial monitor data streams via `/ws/serial-monitor`. All endpoints match the Python FastAPI daemon's contract.

**Diagnostic subcommand exception:** A small, growing set of `fbuild-cli` subcommands (`clang-tidy`, `clang-query`, `iwyu`, `mcp`, `lnk`, `lib-select`) run in-process and intentionally bypass the daemon. They are read-only diagnostics that don't need build orchestration, so a round-trip through the HTTP API would only add latency. The "thin HTTP client" rule still applies to every command that touches the build pipeline (`build`, `deploy`, `monitor`, `test-emu`, etc.).

**PyO3 consumer contract:** FastLED imports `SerialMonitor` as a Python context manager with `read_lines()`, `write()`, `write_json_rpc()`. The `fbuild-python` crate must preserve this API exactly.

## Current Status

Workspace scaffolded with all crate dependencies, types, and traits. All 11 crates compile clean (clippy `-D warnings`). Implementation starts with fbuild-serial (highest risk, most complex).
