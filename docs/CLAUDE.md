# Documentation Guide

Architecture docs are split by subsystem. Read only what's relevant to your current work.

## Which doc to read

| Working on crate | Read these |
|---|---|
| `fbuild-cli` | [overview.md](architecture/overview.md), [data-flow.md](architecture/data-flow.md) |
| `fbuild-daemon` | [overview.md](architecture/overview.md), [runtime.md](architecture/runtime.md) |
| `fbuild-serial` | [serial.md](architecture/serial.md), [deploy-preemption.md](architecture/deploy-preemption.md) |
| `fbuild-python` | [serial.md](architecture/serial.md), [pyo3-bindings.md](architecture/pyo3-bindings.md) |
| `fbuild-build` | [overview.md](architecture/overview.md), [data-flow.md](architecture/data-flow.md) |
| `fbuild-deploy` | [deploy-preemption.md](architecture/deploy-preemption.md) |
| `fbuild-config` | [overview.md](architecture/overview.md) |
| `fbuild-packages` | [overview.md](architecture/overview.md) |
| `fbuild-paths` | [overview.md](architecture/overview.md) |
| `fbuild-core` | [overview.md](architecture/overview.md) |
| Platform-specific issues | [portability.md](architecture/portability.md) |

## Other docs

- **[DESIGN_DECISIONS.md](DESIGN_DECISIONS.md)** — ADR-style decisions with rationale
- **[ROADMAP.md](ROADMAP.md)** — Implementation phases
- **[ARCHITECTURE.md](ARCHITECTURE.md)** — Index of all architecture documents
