# fbuild-deploy

Firmware deployment to embedded devices via platform-specific upload tools (esptool, avrdude, teensy_loader_cli). Includes a firmware ledger for skip-redeploy optimization and device reset sequences.

## Key Types

- `Deployer` -- trait for platform-specific firmware upload (`deploy` method)
- `DeploymentResult` -- success/failure with message and optional port
- `Esp32Deployer` -- esptool-based deployer with chip-specific flash offsets and modes
- `AvrDeployer` -- avrdude-based deployer for Arduino boards
- `TeensyDeployer` -- teensy_loader_cli-based deployer via USB HID
- `FirmwareLedger` -- persistent JSON ledger tracking deployed firmware hashes per port
- `FirmwareEntry` -- single deployment record with source/firmware hashes and staleness check

## Modules

- **esp32** -- `Esp32Deployer`, `EsptoolParams`; handles bootloader/partitions/firmware offsets per MCU
- **avr** -- `AvrDeployer`, `AvrdudeParams`; flashes firmware.hex via serial
- **teensy** -- `TeensyDeployer`, `TeensyLoaderParams`; flashes firmware.hex via USB
- **firmware_ledger** -- `FirmwareLedger`, source/firmware/build-flags hashing for skip-redeploy
- **reset** -- `reset_device`, `detect_platform_for_reset`; DTR/RTS toggle sequences per platform

See `docs/architecture/deploy-preemption.md` for architecture details.
