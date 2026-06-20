# agents/docs/

Per-topic agent-facing docs. The root [`CLAUDE.md`](../../CLAUDE.md)
routing table sends an agent here by task.

## Files (FastLED/fbuild#695 MVP)

- [`commands-reference.md`](commands-reference.md) — every `fbuild`
  subcommand with one-line purpose + "use this when …" guidance.
- [`deploy-architecture.md`](deploy-architecture.md) — the
  `Deployer` trait, `post_deploy_recovery`, board-family dispatch
  model.

## Backlog

The full FastLED-equivalent set (rust-standards, crate-structure,
build-system, testing-commands, debugging, serial-architecture,
workflow) is intentionally not in the MVP. Each is its own future
PR; the routing entry in the root `CLAUDE.md` will gain rows as
they land.

Filed in FastLED/fbuild#695.
