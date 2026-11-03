# fbuild.api package

Public Python API for serial monitoring, consumed by FastLED via `from fbuild.api import SerialMonitor`.

## Modules

- **`__init__.py`** -- Re-exports `SerialMonitor` and `AsyncSerialMonitor` from `fbuild._native`; supports context-manager usage (`with SerialMonitor(port, baud_rate) as mon`)

## Workflow leases

`SerialMonitor(port, baud_rate, ..., lease_id="<lease>")` and
`AsyncSerialMonitor(..., lease_id="<lease>")` present an exclusive device lease
on the daemon's serial `Attach` frame, so a workflow that reserved the board can
attach its own session. The daemon refuses an attach that names no lease while
another client holds one (FastLED/fbuild#1864). Omit `lease_id` for the
historical unattached behaviour.
