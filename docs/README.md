# Documentation

Documentation is organized by reader intent. Start with
[`INDEX.md`](INDEX.md) when you have a specific question.

## User Docs

- **`getting-started/`** -- install, first build, first deploy, first emulator run
- **`guides/`** -- task workflows such as emulator testing and CI caching
- **`reference/`** -- CLI, `platformio.ini`, compatibility, and other stable references
- **`platforms/`** -- board/platform support routing
- **`BOARD_STATUS.md`** -- canonical per-platform CI badges and supported boards
- **`WHY.md`** -- why fbuild exists, key benefits, performance benchmarks

## Contributor And Internal Docs

- **`development/`** -- human entry point for working on fbuild itself
- **`DEVELOPMENT.md`** -- testing, troubleshooting, local development setup
- **`ARCHITECTURE.md`** -- index of all architecture documents
- **`architecture/`** -- subsystem-specific architecture documents
- **`CLAUDE.md`** -- guide mapping crates to relevant architecture docs
- **`DESIGN_DECISIONS.md`** -- ADR-style decisions with rationale
- **`ROADMAP.md`** -- implementation phases for the Rust port
- **`RELEASING.md`** -- release workflow

## Operations And Performance

- **`CI_CACHE.md`** -- consumer-facing cross-run CI cache strategy
- **`CI_CACHING.md`** -- detailed save/restore design for CI runners
- **`PERF_WARM_BUILD.md`** -- warm-pass build performance investigation
- **`PERF_WARM_DEPLOY.md`** -- warm deploy and monitor timing results
- **`SOLDR_BUILD_PERF.md`** -- local soldr build benchmark for the Rust workspace
- **`symbols.md`** -- per-symbol bloat analysis reference
- **`sdkconfig.md`** -- ESP `sdkconfig` user-override design
