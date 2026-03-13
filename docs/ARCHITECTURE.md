# Architecture Documentation

## Subsystem Documents

| Document | Scope |
|---|---|
| [overview.md](architecture/overview.md) | System diagram, component descriptions, key interfaces |
| [data-flow.md](architecture/data-flow.md) | Build, deploy, and monitor request flows |
| [serial.md](architecture/serial.md) | SharedSerialManager, broadcast/writer model, USB-CDC quirks |
| [deploy-preemption.md](architecture/deploy-preemption.md) | Preemption state machine, auto-reconnect, timing |
| [pyo3-bindings.md](architecture/pyo3-bindings.md) | PyO3 Python API contract, FastLED consumer |
| [runtime.md](architecture/runtime.md) | Concurrency model, tokio tasks, lock strategy |
| [portability.md](architecture/portability.md) | Platform differences (Windows USB-CDC, path handling) |
