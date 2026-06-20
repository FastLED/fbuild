# USB CDC DTR/RTS control-line semantics — per board family

USB CDC ACM `SET_CONTROL_LINE_STATE` (USB CDC PSTN spec §6.3.12, request
`0x22`) gives host software two bits: **DTR** and **RTS**. Both are
plumbed all the way to the device-side CDC peripheral, **but what the
device does with them is vendor- and board-specific.** Get it wrong on
the host side and the device either looks dead (CDC bridge silently
drops every transmit byte because DTR=False is read as "host not
ready") or gets stuck in the bootloader (DTR/RTS pulse triggered an
auto-reset the host didn't intend).

This document pins those rules down in one place. **It exists because
the LPC845-BRK bring-up incident** ([FastLED/FastLED#3300] / [#3325] /
[#3339]) **burned two debugging sessions chasing a fictional Arduino-
core regression**, where the actual root cause was three host-side
sites leaving DTR=False on a CDC-ACM bridge.

It's referenced from:

- `crates/fbuild-serial/src/esp_reset.rs` — module-level warning block.
- `crates/fbuild-serial/src/manager.rs::open_port` — the DTR/RTS-assert
  block (FastLED/fbuild#532).
- Per-family config sites that need to know which idle state to use.

## TL;DR — the cheat sheet

| Scenario | DTR | RTS | Effect |
|---|---|---|---|
| **Default safe open (any unknown board)** | True | True | "host ready" for CDC bridges; ESP no-op (peripheral consumes bits at re-enum, not on live toggle) |
| ESP32 reset hold | False | True | EN low (reset hold) |
| ESP32 release reset → run firmware | True | False | EN high, BOOT low |
| **ESP32 post-reset idle** | False | False | run firmware (BOOT high, EN high). ESP-correct, **fatal for CDC bridges** |
| LPC845-BRK / LPCXpresso845-MAX reset | (do NOT touch) | (do NOT touch) | reset via pyOCD/probe-rs CMSIS-DAP SWD |
| **LPC USB-VCOM monitor open** | True | True | bridge sees "host ready" — forwards target MCU bytes |
| Teensy / SAMD51 / RP2040 bootloader entry | (open 1200 baud, close) | — | bootloader engages on disconnect |
| Bootloader → app idle | True | True | safe default; idle CDC consumes bits, no reset |

**The one rule you can't go wrong with: `DTR=True, RTS=True` after open
unless you specifically need an ESP reset.** This is what
[FastLED/FastLED#3339] picked up on the Python side and what
`crates/fbuild-serial/src/manager.rs::open_port` already does on the
Rust side per FastLED/fbuild#532.

## Per-chip / per-bridge matrix

Per-row sources are pinned to a capture date so the table doesn't rot
silently when datasheets move; verify against the live URL before
treating an entry as load-bearing for new code.

### Espressif — ESP32-S3 / C3 / C6 / H2 / P4 native USB CDC

| Bit | Effect on device | Comment |
|---|---|---|
| DTR | drives **BOOT** pin via the SOC USB peripheral's inverter pair | DTR=False ⇒ BOOT high ⇒ boot from flash (run firmware). DTR=True ⇒ BOOT low ⇒ enter ROM bootloader on next reset. |
| RTS | drives **EN/RESET** pin | RTS=True ⇒ EN low ⇒ reset hold. RTS=False ⇒ EN high ⇒ release reset. |

- **Post-reset idle: `DTR=False, RTS=False`** (boot from flash).
- The CDC peripheral is the SOC's own USB endpoint; there is no
  separate "host ready" gate — bytes flow regardless of DTR.
- The bits are sampled by the peripheral at re-enumeration time as
  well as on live toggle, so the post-reset idle state matters.

Sources (captured 2026-06):

- esptool `reset.py::ClassicReset` (`esptool/reset.py`).
- ESP-IDF USB CDC docs §"USB Serial/JTAG Controller" — RM "USB
  Serial/JTAG Controller" chapter.

### NXP — LPC11U35 USB-VCOM bridge (LPC845-BRK, LPCXpresso845-MAX, LPCXpresso804)

| Bit | Effect on device | Comment |
|---|---|---|
| DTR | **host-ready gate** | DTR=False ⇒ bridge drops every byte the target MCU's USART emits. DTR=True ⇒ bridge forwards bytes. **This is the FastLED/FastLED#3300 failure mode.** |
| RTS | ignored on most LPC11U35 firmware revisions | mbed DAPLink builds expose it as a separate UART line; the on-board LPC845-BRK build does not. Safe to always send `True`. |

- **Post-open idle: `DTR=True, RTS=True`**.
- **Reset path is SWD via CMSIS-DAP, NOT DTR/RTS.** Use pyOCD or
  probe-rs to talk to the second USB endpoint (`(0x1FC9, 0x0132)` —
  NXP CMSIS-DAP debug). The first endpoint (`(0x16C0, 0x0483)`) is
  the LPC11U35 VCOM bridge — data only.

Sources (captured 2026-06):

- ARM CMSIS-DAP / DAPLink documentation —
  <https://github.com/ARMmbed/DAPLink/blob/main/docs/MSC_COMMANDS.md>.
- NXP UM10473 — LPC11U3x Manual, §"USB-to-Serial bridge" (specifically
  the LPC11U35 bridge logic on LPC845-BRK is the application-note
  version of this).

### FTDI — FT232R / FT231X / FT232H

| Bit | Effect on device | Comment |
|---|---|---|
| DTR | exposed as RS-232 DTR pin | Board-dependent. On Arduino classic auto-reset boards: DTR=False→True transition triggers ATmega reset via 100nF capacitor. On bare-FT232 USB-UART adapters: no electrical effect on the target unless the board wires it. |
| RTS | exposed as RS-232 RTS pin | Same: board-dependent. Some Arduino clones wire RTS to RESET too; some don't. |

- **Post-open safe default: `DTR=True, RTS=True`**.
- Avoid pulsing DTR low during normal monitor open — that's the
  Arduino auto-reset trigger and will reset the target every time
  the port is opened, losing the first ~2s of output.
- Some FT232H designs (FTDI MPSSE mode) don't expose CDC at all;
  this row is for the FT232R / FT231X CDC mode.

Sources (captured 2026-06):

- FTDI AN232B-04 — "Data Throughput, Latency and Handshaking" —
  <https://ftdichip.com/Documents/AppNotes/AN232B-04_DataLatencyFlow.pdf>.
- FTDI Programming Guide D2xx — §"FT_SetDtr / FT_SetRts".

### Silicon Labs — CP2102 / CP2104 / CP2105

| Bit | Effect on device | Comment |
|---|---|---|
| DTR | routable to any GPIO, not gating by default | OEM EEPROM config decides whether DTR drives a GPIO. ESP32 classic DevKits wire it to RESET (via the same auto-reset transistor pair as the FTDI Arduino design). |
| RTS | routable to any GPIO | Same — OEM-config-dependent. |

- **Post-open safe default: `DTR=True, RTS=True`** unless the board's
  schematic / silkscreen says otherwise.
- Some CP2102 firmware revisions hold the GPIO output static after
  USB enumeration completes — DTR/RTS toggles on a live port have no
  effect on the target chip in those revs.

Sources (captured 2026-06):

- Silicon Labs AN144 — "USB Customization of CP210x" —
  <https://www.silabs.com/documents/public/application-notes/AN144.pdf>.

### WCH — CH340 / CH341 / CH343 / CH9102

| Bit | Effect on device | Comment |
|---|---|---|
| DTR | exposed as a configurable output, typically wired to RESET via a 100nF cap on Arduino clones and most NodeMCU-style ESP boards | Same auto-reset behavior as FTDI/CP2102. |
| RTS | exposed as a configurable output, often wired to BOOT/IO0 on ESP boards | DTR pulse + RTS pulse together drive the ESP classic-reset sequence on a CH340-fronted DevKit-V1. |

- **Post-open safe default: `DTR=True, RTS=True`** for monitor open.
- On an ESP32 classic DevKit-V1 with CH340, the host-side `esptool`
  `hard_reset` sequence is identical to the native USB CDC case —
  the CH340 just passes the DTR/RTS bits through to the same
  transistor pair.

Sources (captured 2026-06, primary source in Mandarin):

- WCH CH340/CH341 datasheet —
  <https://www.wch.cn/downloads/CH340DS1_PDF.html>.

### PJRC — Teensy USB-Serial (Teensy 3.x / 4.x)

| Bit | Effect on device | Comment |
|---|---|---|
| DTR | host signal — Teensy firmware can read it via `Serial.dtr()` | No direct hardware effect; the Teensy user code reads it. |
| RTS | host signal — Teensy firmware can read it via `Serial.rts()` | Same. |

- **Reset trigger: open the port at 1200 baud, then close.** The
  Teensy bootloader watches for that exact baud rate change on a
  CDC disconnect and engages HalfKay (the Teensy bootloader). This
  is the "1200-bps touch" idiom.
- **Post-open at app baud: `DTR=True, RTS=True`** — defensive default.

Sources (captured 2026-06):

- PJRC Teensy USB Serial reference —
  <https://www.pjrc.com/teensy/td_serial.html>.

### Raspberry Pi — RP2040 native USB CDC

| Bit | Effect on device | Comment |
|---|---|---|
| DTR | informational only — TinyUSB stack exposes it via `tud_cdc_get_line_state` | No direct hardware effect unless user firmware acts on it. |
| RTS | informational only | Same. |

- **Reset trigger: 1200-bps touch, same idiom as Teensy.** RP2040
  pico-sdk's stock CDC handler watches for 1200-baud + DTR=False
  and reboots into BOOTSEL mode. Open `(vid=0x2E8A, pid=0x000A)` at
  1200 baud, close; the device re-enumerates at `(0x2E8A, 0x0003)`
  in BOOTSEL.
- **Post-open at app baud: `DTR=True, RTS=True`**.

Sources (captured 2026-06):

- RP2040 datasheet §"USB Controller" —
  <https://datasheets.raspberrypi.com/rp2040/rp2040-datasheet.pdf>.
- pico-sdk `pico_stdio_usb` source.

### Atmel/Microchip — SAMD21 / SAMD51 native USB CDC

| Bit | Effect on device | Comment |
|---|---|---|
| DTR | host signal; UF2 bootloader watches for 1200-baud + disconnect | Same 1200-bps touch idiom — UF2 boards enter bootloader mode on that sequence. |
| RTS | host signal | No standard hardware effect on Atmel SAM CDC. |

- **Reset trigger: 1200-bps touch.** Same idiom as Teensy and RP2040.
- **Post-open at app baud: `DTR=True, RTS=True`**.
- The UF2 bootloader re-enumerates at a separate VID:PID
  (board-specific; Adafruit boards use 0x239A:0xXXXX where the PID
  has a `_BOOT` variant).

Sources (captured 2026-06):

- Atmel SAM USB CDC stack — Microchip ASF library
  `common/services/usb/class/cdc/`.
- UF2 Bootloader spec —
  <https://github.com/microsoft/uf2/blob/master/README.md>.

## Worked example: how FastLED/FastLED#3339 would have been a 15-minute investigation

Without this matrix, the LPC845-BRK bring-up incident played out as:

1. Agent observes "device looks dead" on COM20 after firmware flash.
2. Agent assumes Arduino-core regression (false trail — there had
   been an ArduinoCore-LPC8xx version bump that week). [#3325] gets
   filed to bisect Arduino-core commits. 4+ hours of bisection work,
   no smoking gun.
3. A separate agent eventually notices the host-side `serial_probe.py`
   helper was leaving `DTR=False`. Setting `DTR=True` makes the COM20
   stream visible immediately — the LPC845 had been emitting fine the
   whole time, the LPC11U35 bridge was dropping every byte.
4. [#3325] closed as a false trail. [#3339] lands the host-side fix
   across three independent code paths plus a `serial_probe.py`
   helper plus a `BOARD_FINGERPRINTS` table.

With this matrix, the same incident plays out as:

1. Agent observes "device looks dead" on COM20.
2. Agent runs `fbuild serial probe list` (FastLED/fbuild#686) and
   sees `COM20  16C0:0483  [LPC11U35 VCOM bridge (LPC845-BRK …)]`.
3. Agent opens this doc, reads the LPC11U35 row, sees "DTR=False ⇒
   bridge drops every byte". Sets DTR=True. Bytes appear.
4. **Total elapsed time: ~15 minutes.** No false-trail issue filed,
   no bisection, no agent-rule debate.

## Empirical probe (future work)

A `fbuild serial probe probe-dtr-rts <port>` subcommand — sweep the
four combinations, log bytes seen in each, identify which combo gates
data and which triggers reset — would let a new board's row be
filled in empirically without datasheet archaeology. Mentioned in
[FastLED/fbuild#686]'s discussion; not yet implemented.

## Flash → monitor handoff timing (FastLED/fbuild#691)

| BoardFamily | `post_reset_settle_ms` | `boot_drain_ms` | `port_reappear_timeout_ms` | `open_retry_count` |
|---|---:|---:|---:|---:|
| `Esp32NativeUsbCdc` | 200 | 0 | 3000 | 5 |
| `Esp32ExternalUart` | 200 | 0 | 3000 | 5 |
| `CdcAcmBridge` (LPC11U35) | 500 | 2000 | 3000 | 3 |
| `Teensy` | 100 | 500 | 5000 | 10 |
| `NativeUsbCdcReset1200Bps` (RP2040/SAMD) | 100 | 500 | 5000 | 10 |
| `ArduinoAutoReset` | 1500 | 0 | 0 | 1 |

Source: `crates/fbuild-serial/src/boards.rs::BoardFamily::handoff_timing`. The LPC11U35 row is from FastLED/FastLED#3339 (the bring-up incident). 1200-bps-touch rows tolerate the double-enumeration window (bootloader VID/PID then app VID/PID). Arduino has zero `port_reappear_timeout_ms` because the USB endpoint lives on the bridge chip and never drops.

## When to update this doc

- A new board family lands in `crates/fbuild-build/src/<family>/`.
- A new VID:PID entry lands in `crates/fbuild-serial/src/boards.rs`.
- A FastLED bring-up incident involves DTR/RTS guessing.
- A datasheet URL above 404s — update both URL and capture date in
  the same PR.

[FastLED/FastLED#3300]: https://github.com/FastLED/FastLED/issues/3300
[FastLED/FastLED#3325]: https://github.com/FastLED/FastLED/issues/3325
[FastLED/FastLED#3336]: https://github.com/FastLED/FastLED/issues/3336
[#3300]: https://github.com/FastLED/FastLED/issues/3300
[#3325]: https://github.com/FastLED/FastLED/issues/3325
[#3336]: https://github.com/FastLED/FastLED/issues/3336
[#3339]: https://github.com/FastLED/FastLED/pull/3339
[FastLED/FastLED#3339]: https://github.com/FastLED/FastLED/pull/3339
[FastLED/fbuild#684]: https://github.com/FastLED/fbuild/issues/684
[FastLED/fbuild#686]: https://github.com/FastLED/fbuild/issues/686
[FastLED/fbuild#689]: https://github.com/FastLED/fbuild/issues/689
