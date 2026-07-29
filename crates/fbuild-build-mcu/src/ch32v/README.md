# CH32V Platform Build Support

Build orchestrator for WCH CH32V RISC-V MCUs. Uses xPack riscv-none-elf-gcc toolchain and OpenWCH Arduino core framework.

The V303/V307 board definitions use the vendor hard-float profile
(`rv32imafcxw`/`ilp32f`, normalized to `rv32imafc_zicsr`). This intentionally
differs from Community-PIO-CH32V's soft-float defaults. The vendor builder also
uses `-msave-restore -msmall-data-limit=8 -fno-use-cxa-atexit`; fbuild keeps
`-msmall-data-limit=0` and omits those flags, so reference-build flash-size
comparisons should record that code-generation difference.

Some board packages reuse the nearest upstream pin-map variant because the
pinned OpenWCH core does not ship a dedicated map: V208 uses the V203 map,
X035 uses the G8U map, and V103 uses the R8T6 map. These are intentional
registry fallbacks, not claims that the physical pinouts are identical.

## Bench bring-up: WCH-LinkE probe setup

Flashing a CH32V goes through `wlink` and a WCH-LinkE probe. Getting the
probe recognised by the host is the single most common first-time
failure, and it is a host/driver problem rather than an fbuild one — the
steps below come out of the FastLED/fbuild#1208 bring-up.

### 1. Put the probe in RV mode

The WCH-LinkE has two personalities:

| Mode | Enumerates as | Use |
|---|---|---|
| **RV** | `1A86:8010` | CH32V RISC-V targets — **this is the one you want** |
| DAP | `1A86:8012` | ARM CMSIS-DAP targets |

Switch modes **in software** — no button press needed once `wlink` can
already reach the probe:

```bash
wlink mode-switch --rv     # or --dap
```

**Hold the button on the probe while plugging in the USB cable** to force
the toggle when `wlink` *can't* reach it (wrong mode and no driver bound
is the usual chicken-and-egg). Confirm the mode from the enumerated PID
before debugging anything else.

### 2. Bind WinUSB (Windows only)

`wlink` talks to the probe through libusb, which on Windows needs a
WinUSB-class driver bound to the interface. Without it `wlink` fails to
claim the device even though Device Manager shows it as working.

Use [Zadig](https://zadig.akeo.ie/): *Options → List All Devices*, select
the **WCH-Link** interface (interface 0 in RV mode — **not** the CDC
serial companion interface), pick **WinUSB**, then *Replace Driver*.

Replacing the driver on the wrong interface removes the probe's serial
port. If that happens, uninstall the device in Device Manager with
"delete the driver software" ticked and re-plug.

Linux needs no driver, only a udev rule granting access to `1A86:8010`.

### 3. Verify before building

```bash
wlink status
```

This must print a CH32V chip ID and the correct flash size. Record the
WCH-Link firmware version — the pinned `wlink` v0.1.2 is tested against
probe firmware 2.15.

**Use `status`, not `list`, to detect a probe.** Verified against wlink
v0.1.2 with no probe attached:

| Command | Exit | Output |
|---|---|---|
| `wlink status` | **1** | `Error: WCH-Link not found, please check your connection` |
| `wlink flash <file>` | **1** | `Error: WCH-Link not found, please check your connection` |
| `wlink list` | **0** | *(nothing)* |

`list` exiting 0 on an empty probe set is a false-negative trap for
scripts — presence checks must key off `status`. The non-zero exits on
`flash` are also why `WlinkDeployer`'s `result.success()` is trustworthy:
a missing probe can never be reported as a successful flash.

If the probe can power the target, you don't need an external 3V3 supply:

```bash
wlink set-power enable3v3     # also enable5v / disable3v3 / disable5v
```

If enumeration fails entirely — the device appears as *Unknown USB Device
(Device Descriptor Request Failed)*, or under a null VID such as
`0000:0002` — that is a cable, port, or hub fault, not a driver issue.
Many cheap USB cables are charge-only. Try a known-good data cable
directly into a root port before troubleshooting anything downstream.

### Notes for CH32V003 specifically

- **No USB peripheral and no factory USB-ISP bootloader.** `wchisp`
  correctly refuses this part; the probe is the only flashing path.
- Wire SWIO / GND / 3V3 to the target. SWIO is a single-wire debug line,
  not SWD.
- Any serial port present while a V003 is on the bench belongs to the
  *probe*, not the target.
- 16 KB flash / 2 KB SRAM, and RV32EC has **no hardware multiply** —
  every `*`, `/`, `%` becomes a libgcc call. Watch the flash budget.
- Measured footprint of the `tests/platform/ch32v003` blink with the
  OpenWCH core (FastLED/fbuild#1208 Phase 1): **flash 8896 / 16384 B
  (54.3%), RAM 1180 / 2048 B (57.6%)**. A bare blink already takes over
  half of both budgets — roughly 7.5 KB flash and 868 bytes of SRAM are
  left for application code.

## Bench verification sequence

Once the probe enumerates, run these in order. Each rules out a distinct
failure the previous step cannot see:

```bash
# 0. probe + target visible (use `status`, never `list` — see table above)
soldr cargo test -p fbuild-deploy wlink::tests::try_wlink_status_detects_ch32v003 -- --ignored --nocapture

# 1. build
soldr cargo run -p fbuild-cli -- build tests/platform/ch32v003 -e ch32v003

export CH32V003_FIRMWARE=tests/platform/ch32v003/.fbuild/build/release/firmware.bin

# 2. flash
soldr cargo test -p fbuild-deploy wlink::tests::try_flash_real_ch32v003 -- --ignored --nocapture

# 3. the bytes actually landed (catches partial write / wrong base / protected flash)
soldr cargo test -p fbuild-deploy wlink::tests::try_verify_flash_readback_ch32v003 -- --ignored --nocapture

# 4. the core is executing, not halted or fault-looping
soldr cargo test -p fbuild-deploy wlink::tests::try_ch32v003_core_is_executing -- --ignored --nocapture
```

**Steps 3 and 4 are proxies, not the milestone.** A clean run proves the
image is on-chip and the CPU is retiring instructions; it says nothing
about whether the GPIO toggles at the right rate. **Bring-up is complete
only when the blink is observed on a scope or LED** — that is the one
step no automation can perform, and `docs/BOARD_STATUS.md` should not be
promoted to hardware-verified until someone has watched it.

There is also a network-only test that catches a rotted `wlink` release
URL or stale checksum with no hardware attached:

```bash
soldr cargo test -p fbuild-deploy wlink::tests::try_install_wlink_from_pinned_release -- --ignored --nocapture
```

## Console output on a part with no USB

CH32V003 has no USB peripheral, so there is no native CDC port to open
and `fbuild monitor` has no target-side device to attach to. There are
two options, and they are not equivalent:

### SDI-print over the debug link (recommended)

The QingKe debug module can carry a virtual serial channel over the same
single-wire SWIO connection used for flashing, so it costs **no extra
pins and no USART peripheral**:

```bash
wlink sdi-print enable      # implies --no-detach; the probe stays attached
```

This is the right default for a 2 KB / 16 KB part — a USART driver plus
its buffers is real budget against the ~868 bytes measured above, and the
V003's pins are scarce.

Caveats worth knowing before relying on it:

- `enable` **implies `--no-detach`**, so the probe holds the chip
  attached. That conflicts with a flash operation on the same probe —
  expect to sequence print and flash, not run them concurrently.
- The channel is a debug-module facility, not a UART. There is no baud
  rate, no DTR/RTS, and therefore nothing for the control-line matrix in
  [`docs/usb-cdc-control-line-matrix.md`](../../../../docs/usb-cdc-control-line-matrix.md)
  to govern.
- It is **not** wired into `fbuild monitor` today. Consuming it means
  invoking `wlink sdi-print enable` directly. Integrating it would mean
  teaching the serial layer about a non-serial transport, which is a
  larger change than this bring-up.

### A USART pin to a separate bridge

Route a V003 USART TX pin to an external CH340/CP2102/FTDI bridge. That
yields an ordinary serial port `fbuild monitor` can already open, at the
cost of a pin, a peripheral, code space, and a second USB device on the
bench. Prefer this only when SDI-print's attach behaviour gets in the
way, or when the console must survive without the probe connected.

**Either way, do not expect `fbuild serial` to discover the target.** The
only USB device present is the probe.
