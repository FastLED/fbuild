# agents/docs/

Per-topic agent-facing docs. The root [`CLAUDE.md`](../../CLAUDE.md)
routing table sends an agent here by task.

## Files (FastLED/fbuild#695 MVP)

- [`commands-reference.md`](commands-reference.md) — every `fbuild`
  subcommand with one-line purpose + "use this when …" guidance.
- [`path-conventions.md`](path-conventions.md) — prefix roots
  (`~/.fbuild/{dev|prod}/cache/…` vs `<project>/.fbuild/build/…`), the
  factory functions that pick them, and why a mis-spelled/absolute path
  silently defeats a cache key. **Read before touching any cache dir,
  build dir, or cache-key/signature code** (FastLED/fbuild#952).
- [`deploy-architecture.md`](deploy-architecture.md) — the
  `Deployer` trait, `post_deploy_recovery`, board-family dispatch
  model.
- [`hardware-ci-setup.md`](hardware-ci-setup.md) — how the
  hardware-in-the-loop CI rigs are wired (existing doc).
- [`serial-testing.md`](serial-testing.md) — Docker/WSL real-device
  harness for `port_class` + `family_for_port` validation against an
  actual ESP32. **Reach for this** when you touch
  `crates/fbuild-serial/src/port_class.rs`,
  `crates/fbuild-serial/src/boards.rs::family_for_vid_pid`, or any
  DTR/RTS handling in `SharedSerialManager::open_port`. See
  FastLED/fbuild#899 (resolved).

## Backlog

The full FastLED-equivalent set (rust-standards, crate-structure,
build-system, testing-commands, debugging, serial-architecture,
workflow) is intentionally not in the MVP. Each is its own future
PR; the routing entry in the root `CLAUDE.md` will gain rows as
they land.

Filed in FastLED/fbuild#695.
