# fbuild-deploy

Firmware deployment to embedded devices via platform-specific upload tools (esptool, avrdude, teensy_loader_cli), and device reset sequences.

## Key Types

- `Deployer` -- trait for platform-specific firmware upload (`deploy` method)
- `DeploymentResult` -- success/failure with message and optional port
- `Esp32Deployer` -- esptool-based deployer with chip-specific flash offsets and modes
- `AvrDeployer` -- avrdude-based deployer for Arduino boards
- `TeensyDeployer` -- teensy_loader_cli-based deployer via USB HID

## Modules

- **esp32** -- `Esp32Deployer`, `EsptoolParams`; handles bootloader/partitions/firmware offsets per MCU
- **esp32_native** -- native espflash-backed `verify-flash` (issue #66); opt-in via `FBUILD_USE_ESPFLASH_VERIFY=1`, falls back to esptool subprocess by default
- **avr** -- `AvrDeployer`, `AvrdudeParams`; flashes firmware.hex via serial
- **teensy** -- `TeensyDeployer`, `TeensyLoaderParams`; flashes firmware.hex via USB
- **reset** -- `reset_device`, `detect_platform_for_reset`; DTR/RTS toggle sequences per platform

Skip-redeploy is handled authoritatively by the daemon's device-side `verify-flash` pre-check (see `handlers/operations.rs`), which asks the ESP32 stub flasher for a per-region MD5 via `FLASH_MD5SUM` before writing. The previous client-side `FirmwareLedger` was removed (issue #18) because it could not detect flashes performed outside fbuild.

### Native verify-flash (issue #66, opt-in)

Setting `FBUILD_USE_ESPFLASH_VERIFY=1` routes the ESP32 verify pre-check through the native [`espflash`](https://crates.io/crates/espflash) crate instead of the Python `esptool` subprocess, avoiding ~1 s of interpreter startup and ~0.5 s of subprocess spawn per invocation. Write-flash still goes through esptool until a follow-up PR lands. The daemon pre-empts any active serial monitor via `SharedSerialManager::preempt_for_deploy` before opening the port, so the native path never contends with the existing lease.

See `docs/architecture/deploy-preemption.md` for architecture details.
