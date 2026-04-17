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
- **avr** -- `AvrDeployer`, `AvrdudeParams`; flashes firmware.hex via serial
- **teensy** -- `TeensyDeployer`, `TeensyLoaderParams`; flashes firmware.hex via USB
- **reset** -- `reset_device`, `detect_platform_for_reset`; DTR/RTS toggle sequences per platform

Skip-redeploy is handled authoritatively by the daemon's device-side `verify-flash` pre-check (see `handlers/operations.rs`), which asks the ESP32 stub flasher for a per-region MD5 via `FLASH_MD5SUM` before writing. The previous client-side `FirmwareLedger` was removed (issue #18) because it could not detect flashes performed outside fbuild.

See `docs/architecture/deploy-preemption.md` for architecture details.
