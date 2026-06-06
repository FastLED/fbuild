# Teensy deployer

State machine for `fbuild deploy -e teensyXX` that takes a wedged Windows host
back to unattended flashing without manual button presses or USB reseats.

Implements the design from [issue
#433](https://github.com/FastLED/fbuild/issues/433) (which supersedes #432). The
old single-file `teensy.rs` shipped only the bare `teensy_loader_cli` invocation
and inherited every failure mode the issue catalogues.

## Module layout

- **`mod.rs`** — `TeensyDeployer` + `TeensyLoaderParams`; orchestrates the state
  machine below.
- **`soft_reboot.rs`** — opens the device's CDC ACM port at **baud 134**, the
  Teensyduino USB stack's magic-baud signal to drop into HalfKay. Replaces the
  Windows-only `teensy_loader_cli` reboot which prints
  `Soft reboot is not implemented for Win32`.
- **`halfkay_probe.rs`** — confirms the CDC port vanished from
  `serialport::available_ports()` (HalfKay proxy: the device left CDC class for
  HID), or waits up to `wait_for_halfkay_timeout_secs` for the user to press the
  program button.
- **`flash.rs`** — bounded retry loop around `teensy_loader_cli`. Each attempt
  is a fresh subprocess; stops on first success; surfaces per-attempt diagnostic
  on the way to exhaustion.
- **`port_discovery.rs`** — pre-flash port snapshot + post-flash detection of
  the newly enumerated CDC ACM port. Filled into `DeploymentResult.port` so the
  post-deploy monitor can attach to the right device.
- **`first_byte_probe.rs`** — advisory probe that opens the post-flash port and
  reports whether any byte arrived inside `first_byte_timeout_secs`. Silent
  firmware is surfaced as a structured diagnostic, not a deploy failure.
- **`usb_type.rs`** — best-effort read of `usb_type` from the build artifact
  directory; advises the monitor when the device was built without a Serial
  endpoint (`USB_MIDI_SERIAL`, `USB_RAWHID`).

## State machine

```
pre-snapshot → (CDC at port? → baud-134 trigger) → wait-for-HalfKay
            → flash with retry → wait-for-new-CDC → first-byte probe
            → DeploymentResult { port: Some(new_port), … }
```

Failure at any stage returns a `DeploymentResult { success: false, message:
<stage-specific>, stderr: <last loader output> }` — same envelope the daemon
already propagates to the CLI verbatim.

## Env escape hatches

- `FBUILD_TEENSY_FLASH_RETRIES` — override `flash_retries` (default 5).
- `FBUILD_TEENSY_FIRST_BYTE_TIMEOUT_SECS` — override
  `first_byte_timeout_secs` (default 10, `0` disables).
- `FBUILD_TEENSY_DISABLE_BAUD_134_TRIGGER` — opt out of the baud-134 trigger
  (debug aid for the few hosts where `SerialPortBuilder::baud_rate(134)` is not
  honored).
