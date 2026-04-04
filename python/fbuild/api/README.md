# fbuild.api package

Public Python API for serial monitoring, consumed by FastLED via `from fbuild.api import SerialMonitor`.

## Modules

- **`__init__.py`** -- Re-exports `SerialMonitor` from `fbuild._native`; supports context-manager usage (`with SerialMonitor(port, baud_rate) as mon`)
