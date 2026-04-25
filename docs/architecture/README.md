# Architecture Documents

Subsystem-specific architecture documentation. See [../CLAUDE.md](../CLAUDE.md) for which doc to read based on the crate you are working on.

## Contents

- **`overview.md`** -- High-level system architecture and crate responsibilities
- **`data-flow.md`** -- Build pipeline data flow from CLI to output
- **`deploy-preemption.md`** -- Deploy preemption and serial port management
- **`runtime.md`** -- Daemon runtime, async model, and task lifecycle
- **`serial.md`** -- Serial port communication and USB-CDC handling
- **`pyo3-bindings.md`** -- PyO3 Python bindings for the fbuild-python crate
- **`portability.md`** -- Platform-specific considerations (Windows, macOS, Linux)
- **`library-selection.md`** -- LDF-style scanner / walker / resolver subsystem (`fbuild-header-scan`, `fbuild-library-select`)
