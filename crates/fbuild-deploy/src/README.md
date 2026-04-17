# Source

## Modules

- **`lib.rs`** -- Crate root; defines `Deployer` trait, `DeploymentResult` struct, and `DeployOutcome` enum (full / verify-skip / selective) surfaced in the daemon's `/api/deploy` response message (issue #76)
- **`esp32.rs`** -- `Esp32Deployer`: esptool invocation with chip/port/baud/flash-mode/offsets, `EsptoolParams` config
- **`avr.rs`** -- `AvrDeployer`: avrdude invocation with MCU/programmer/baud, `AvrdudeParams` config
- **`teensy.rs`** -- `TeensyDeployer`: teensy_loader_cli invocation via USB HID, `TeensyLoaderParams` config
- **`reset.rs`** -- Platform-specific reset sequences: Teensy 134-baud magic, ESP32 DTR/RTS, AVR DTR toggle
