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

**Hold the button on the probe while plugging in the USB cable** to
toggle between them. Confirm the mode from the enumerated PID before
debugging anything else.

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
  *probe*, not the target. Console output is either SDI-print over the
  debug link or a USART pin routed to a separate bridge.
- 16 KB flash / 2 KB SRAM, and RV32EC has **no hardware multiply** —
  every `*`, `/`, `%` becomes a libgcc call. Watch the flash budget.
