# Documentation Index

A grep-friendly FAQ that maps common questions to the file that answers them. Both humans and LLM agents should use this table as the entry point into the fbuild docs.

| Question                                                  | File                                                       |
|-----------------------------------------------------------|------------------------------------------------------------|
| How do I install fbuild?                                  | [../README.md](../README.md#installation)                  |
| How do I run my first build / deploy / monitor?           | [../README.md](../README.md#quick-start)                   |
| What does `platformio.ini` need to contain?               | [../README.md](../README.md#configuration)                 |
| Why does fbuild exist?                                    | [WHY.md](WHY.md)                                           |
| What are fbuild's key benefits and performance numbers?   | [WHY.md](WHY.md#key-benefits)                              |
| Is my board supported?                                    | [BOARD_STATUS.md](BOARD_STATUS.md)                         |
| How do I add a new board?                                  | [BOARD_STATUS.md](BOARD_STATUS.md#adding-a-new-board)      |
| What's the crate dependency graph?                        | [../crates/CLAUDE.md](../crates/CLAUDE.md)                 |
| How does fbuild's architecture fit together?              | [architecture/overview.md](architecture/overview.md)       |
| How does the daemon work?                                 | [architecture/runtime.md](architecture/runtime.md)         |
| How does the serial / monitor subsystem work?             | [architecture/serial.md](architecture/serial.md)           |
| How do the PyO3 Python bindings work?                     | [architecture/pyo3-bindings.md](architecture/pyo3-bindings.md) |
| How does deploy preemption work?                          | [architecture/deploy-preemption.md](architecture/deploy-preemption.md) |
| What are the cross-platform portability constraints?      | [architecture/portability.md](architecture/portability.md) |
| Why did we choose X over Y?                               | [DESIGN_DECISIONS.md](DESIGN_DECISIONS.md)                 |
| What's on the implementation roadmap?                     | [ROADMAP.md](ROADMAP.md)                                   |
| How do I run tests / lint / fmt?                          | [DEVELOPMENT.md](DEVELOPMENT.md#testing)                   |
| Why is my build failing?                                  | [DEVELOPMENT.md](DEVELOPMENT.md#troubleshooting)           |
| How do I use the emulator (QEMU / avr8js / simavr)?       | [../README.md](../README.md#emulator-testing)              |
| What CI cache block should consumers copy?                | [CI_CACHE.md](CI_CACHE.md#copy-paste-github-actions-block) |
| What cache keys and invalidation pattern should CI use?   | [CI_CACHE.md](CI_CACHE.md#key-composition)                 |
| How do I cache fbuild across CI runs?                     | [CI_CACHING.md](CI_CACHING.md)                             |
| What's safe to cache in GitHub Actions?                   | [CI_CACHING.md](CI_CACHING.md#what-the-cache-holds)        |
| Why does warm-pass build take ~30 s per sketch? (#91)     | [PERF_WARM_BUILD.md](PERF_WARM_BUILD.md)                   |
| What does `FBUILD_PERF_LOG=1` do?                          | [PERF_WARM_BUILD.md](PERF_WARM_BUILD.md#instrumentation)   |
| How fast is `soldr` when building fbuild itself?            | [SOLDR_BUILD_PERF.md](SOLDR_BUILD_PERF.md)                 |
| What architecture docs should I read for a given crate?   | [CLAUDE.md](CLAUDE.md)                                     |

## Conventions

- The top-level `README.md` is the install + Quick Start entry point for humans.
- `CLAUDE.md` at the repo root is the LLM-entry file; `docs/CLAUDE.md` is the LLM map into architecture docs.
- Architecture docs live under `docs/architecture/`, one file per subsystem.
- ADR-style decisions go in `docs/DESIGN_DECISIONS.md`.
- Per-directory `README.md` files are enforced by a pre-commit hook — every directory with files needs one.

## Keeping this index current

When a new doc is added under `docs/`, add a row to the table above. Prefer one FAQ-style question per row so readers can grep for the question they have.
