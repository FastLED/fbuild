# Deploy architecture

How firmware actually gets from `fbuild deploy` to a connected board.
The point of this doc is so an agent adding a new board family knows
**which pieces to extend** and which ones to leave alone, instead of
copy-pasting an ESP-shaped path onto a CDC-bridge board and losing an
afternoon to the FastLED/FastLED#3300 trap.

## High-level flow

```
fbuild-cli (`deploy` subcommand)
    │   thin HTTP client — no build/flash logic lives here
    ▼
fbuild-daemon (HTTP/WebSocket server)
    │   request validation, device lease, build orchestration
    ▼
fbuild-build::BuildOrchestrator (per-platform impl)
    │   produces firmware.elf, firmware.bin, build_info.json
    ▼
fbuild-deploy::Deployer trait
    │   board-family-specific flash path
    │     • ESP32: esptool / espflash via serial
    │     • LPC8xx: pyOCD / probe-rs via CMSIS-DAP USB
    │     • Teensy / SAMD / RP2040: 1200-bps touch + UF2 copy
    ▼
post_deploy_recovery (Deployer-supplied)
    │   serial-port re-enumeration wait, board-specific quirks
    ▼
fbuild-serial::manager (re-attach monitor)
    │   open_port with DTR=true, RTS=true per docs/usb-cdc-control-line-matrix.md
    ▼
Client receives stream over WebSocket
```

## The `Deployer` trait

`crates/fbuild-deploy/src/lib.rs::Deployer` is the contract every
board family implements:

```rust
trait Deployer {
    fn deploy(&self, ...) -> Result<()>;
    fn post_deploy_recovery(&self, port: &str) -> Result<()>;  // default: 3s sleep
}
```

`post_deploy_recovery` is the cross-family hook for "the board's USB
endpoint just disappeared because we reset it; here's how long /
how to wait for it to come back." Override when:

- The board uses **USB-bootloader** flashing (Teensy / RP2040 / SAMD)
  — the bootloader's VID:PID is different from the app's. The
  recovery has to poll for the *app's* VID:PID to reappear.
- The board uses a **CMSIS-DAP debug probe** (LPC845-BRK) — the
  debug probe stays enumerated through reset; the VCOM bridge may
  blink on Windows. Need a board-specific wait.
- The board is **LPC + CMSIS-DAP** specifically — wedge recovery
  needs extra retries; see FastLED/fbuild#605 Phase 1.

## Board-family dispatch

`BoardFamily` (in `crates/fbuild-serial/src/boards.rs`) is the
taxonomy that gates DTR/RTS conventions today. It will eventually
also gate the `Deployer` selection (FastLED/fbuild#687 — the
polymorphic `ResetMethod` dispatch registry) and per-family handoff
timing (FastLED/fbuild#691).

Today the dispatch is more ad-hoc:

- ESP variants → `fbuild-deploy::esp32_native` (espflash) or
  `fbuild-deploy::esp32_external_uart` (esptool over UART)
- LPC8xx → `fbuild-deploy::pyocd_cmsis_dap` (placeholder; bring-up
  in progress in the LPC845-BRK meta, FastLED/fbuild#586)
- Teensy / SAMD / RP2040 → 1200-bps-touch path (still partial)

The follow-up issues track the full polymorphic version:

- **FastLED/fbuild#687** — `BoardFamily` enum + polymorphic
  `ResetMethod` dispatch registry. The thing this doc will reference
  as the canonical source.
- **FastLED/fbuild#688** — `BootModeClassifier` registry (ESP-only
  today; generalize to per-family).
- **FastLED/fbuild#691** — `HandoffTiming` on `BoardFamily`.
- **FastLED/fbuild#692** — enumerate supported deploy protocols per
  board; fail fast on unsupported.
- **FastLED/fbuild#693** — USB-level bootloader re-enumeration
  detection (complement to #688).

## Worked example — "agent ports a new RP2350 board"

The right sequence today:

1. **VID:PID first.** Publish the board's bootloader and runtime USB
   endpoints in [FastLED/boards](https://github.com/FastLED/boards), then
   exercise fbuild's normal catalogue ingestion path. Never add a literal
   VID/PID to `BOARD_FINGERPRINTS`, `ENVIRONMENT_TO_VCOM`, generated Rust, or
   any other production fallback in this repository.
2. **Family classification.** Resolve the published FastLED/boards product
   identity and map that metadata to `CdcAcmBridge` (or the appropriate
   family behavior). If the catalogue cannot express a needed distinction,
   extend its published schema instead of embedding another ID table in
   fbuild. RP2350 normally uses 1200-bps touch and host-ready idle, matching
   the RP2040 convention.
3. **DTR/RTS matrix.** Add a row to
   [`docs/usb-cdc-control-line-matrix.md`](../../docs/usb-cdc-control-line-matrix.md)
   for the new chip, with a datasheet citation and capture date.
4. **Deployer impl.** If the existing `fbuild-deploy::pyocd_cmsis_dap`
   or RP2040 1200-bps-touch path is reusable, register the new
   board there. Otherwise add a new sibling module under
   `crates/fbuild-deploy/src/<family>/`. Implement `Deployer` +
   override `post_deploy_recovery` if the recovery timing differs.
5. **Test.** `fbuild serial probe list` should now show the new
   board's hint. `fbuild serial probe find --env <new-env>` should
   return the right port. `fbuild deploy --env <new-env>` should
   complete; `--monitor` should reattach without dropping bytes.

**Do NOT** start by copy-pasting an `esp32_native` deploy path. The
default ESP DTR/RTS state is `(false, false)` — fatal for any
CDC-ACM bridge.

## Worked example — "agent debugs deploy failure on COM20"

```
fbuild deploy -e lpc845brk
> error: pyocd CMSIS-DAP connect failed: device not found
```

The deploy path goes through the CMSIS-DAP USB endpoint
(`(0x1FC9, 0x0132)`), NOT the COM port. Two separate USB devices.

```bash
# Check what's actually enumerated
$ fbuild serial probe list
COM20      16C0:0483  ser=…  [LPC11U35 VCOM bridge (LPC845-BRK USART0) OR PJRC Teensy USB-Serial]
COM10      1FC9:0132  ser=…  [NXP CMSIS-DAP debug (LPC845-BRK / LPC11U35)]
```

Both endpoints present — good. pyOCD's failure is its own (probably
a driver / udev rule issue, not an fbuild bug). The point is:
**`COM20` is the data port, `COM10` is the deploy port.** Don't try
to flash via the wrong endpoint.

## See also

- [`commands-reference.md`](commands-reference.md) — every `fbuild`
  subcommand.
- [`../../docs/usb-cdc-control-line-matrix.md`](../../docs/usb-cdc-control-line-matrix.md)
  — DTR/RTS rules per board family (FastLED/fbuild#689).
- [`../../crates/CLAUDE.md`](../../crates/CLAUDE.md) — crate
  dependency graph + boundaries.
- [`../../docs/CLAUDE.md`](../../docs/CLAUDE.md) — architecture-doc
  routing table.

Filed in FastLED/fbuild#695.
