# fbuild-deploy

Firmware deployment to embedded devices via platform-specific upload tools (espflash/esptool, avrdude, teensy_loader_cli), and device reset sequences.

## Key Types

- `Deployer` -- trait for platform-specific firmware upload (`deploy` method)
- `DeploymentResult` -- success/failure with message and optional port
- `Esp32Deployer` -- ESP32 deployer with native espflash fast path and esptool fallback
- `AvrDeployer` -- avrdude-based deployer for Arduino boards
- `TeensyDeployer` -- teensy_loader_cli-based deployer via USB HID

## Modules

- **esp32** -- `Esp32Deployer`, `EsptoolParams`; handles bootloader/partitions/firmware offsets per MCU
- **esp32_native** -- native espflash-backed `verify-flash` and `write-flash` (issue #66); enabled by default when compiled in, with automatic esptool subprocess fallback
- **avr** -- `AvrDeployer`, `AvrdudeParams`; flashes firmware.hex via serial
- **teensy** -- `TeensyDeployer`, `TeensyLoaderParams`; flashes firmware.hex via USB
- **reset** -- `reset_device`, `detect_platform_for_reset`; DTR/RTS toggle sequences per platform

Skip-redeploy is handled authoritatively by the daemon's device-side `verify-flash` pre-check (see `handlers/operations.rs`), which asks the ESP32 stub flasher for a per-region MD5 via `FLASH_MD5SUM` before writing. The previous client-side `FirmwareLedger` was removed (issue #18) because it could not detect flashes performed outside fbuild.

### Native verify-flash and write-flash (issue #66)

The ESP32 verify pre-check uses the native [`espflash`](https://crates.io/crates/espflash) crate by default instead of the Python `esptool` subprocess, avoiding ~1 s of interpreter startup and ~0.5 s of subprocess spawn per invocation. `write-flash` uses the same in-process path for both full deploys and selective post-verify-mismatch rewrites. If native verify/write fails, the deployer logs a warning and retries the same operation through esptool. Set `FBUILD_USE_ESPFLASH_VERIFY=0` and/or `FBUILD_USE_ESPFLASH_WRITE=0` to force esptool for either phase.

The daemon pre-empts any active serial monitor via `SharedSerialManager::preempt_for_deploy` before opening the port, so neither path contends with the existing lease. Progress from espflash's `ProgressCallbacks` is bridged into `tracing` and throttled to roughly one log line per 10% of each region, which the daemon's existing log broadcaster surfaces. Structured WebSocket progress frames on the deploy channel are a follow-up.

See `docs/architecture/deploy-preemption.md` for architecture details.
