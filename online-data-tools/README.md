# online-data-tools

Build-time helpers invoked by `.github/workflows/update-data.yml` to produce
the SQLite databases hosted on the `www` orphan branch.

Scripts here live on `main` (so they get unit-tested in CI), but their output
is committed to orphan branches:

| Script              | Reads from                                  | Writes to                              |
| ------------------- | ------------------------------------------- | -------------------------------------- |
| `build_sqlite.py`   | `online-data/data/*.json`                   | `www/<YYYY-MM-DD>.db`                  |
| `rotate_www_dbs.py` | `www/*.db`                                  | `www/` (deletes >2-day-old `.db`s)     |
| `build_www_manifest.py` | day-stable filenames                    | `www/manifest.json`                    |
| `fetch_espressif_usb_pids.py` | `espressif/usb-pids` official PID registry | merge-compatible `/tmp/espressif-usb-pids.json` |
| `fetch_raspberrypi_usb_pids.py` | `raspberrypi/usb-pid` official PID registry | merge-compatible `/tmp/raspberrypi-usb-pids.json` |
| `fetch_nordic_usb_pids.py` | Nordic nRF Connect Programmer / DFU sources | merge-compatible `/tmp/nordic-usb-pids.json` |
| `fetch_microchip_usb_pids.py` | Microchip pyedbglib/pykitinfo plus weak AVRDUDE/board-package supplements | merge-compatible `/tmp/microchip-*-usb-pids.json` |
| `fetch_arduino_usb_pids.py` | Official ArduinoCore `boards.txt` files | merge-compatible `/tmp/arduino-usb-pids.json` |
| `fetch_adafruit_usb_pids.py` | Adafruit Arduino cores + TinyUF2 + CircuitPython descriptors | merge-compatible `/tmp/adafruit-usb-pids.json` |
| `fetch_sparkfun_usb_pids.py` | SparkFun board packages plus weak PlatformIO/CircuitPython supplements | merge-compatible `/tmp/sparkfun-*-usb-pids.json` |
| `fetch_seeed_usb_pids.py` | Seeed package archives/platform JSON plus weak third-party board packages | merge-compatible `/tmp/seeed-*-usb-pids.json` |
| `fetch_ftdi_usb_pids.py` | Linux `ftdi_sio_ids.h` original-FTDI PID section | merge-compatible `/tmp/ftdi-usb-pids.json` |
| `fetch_wch_usb_pids.py` | WCH CH343 Linux driver + udev rules | merge-compatible `/tmp/wch-usb-pids.json` |
| `fetch_teensy_usb_pids.py` | PJRC Teensy core headers + loader CLI | merge-compatible `/tmp/teensy-usb-pids.json` |
| `fetch_stm_usb_pids.py` | ST/OpenOCD ST-LINK sources | merge-compatible `/tmp/stm-usb-pids.json` |
| `fetch_nxp_usb_pids.py` | NXP mfgtools/UUU config table | merge-compatible `/tmp/nxp-usb-pids.json` |
| `fetch_silabs_usb_pids.py` | Linux CP210x driver + SiliconLabsSoftware OpenOCD udev rule | merge-compatible `/tmp/silabs-usb-pids.json` |
| `fetch_renesas_usb_pids.py` | ArduinoCore-renesas `boards.txt` weak supplement | merge-compatible `/tmp/renesas-usb-pids.json` |

The merger scripts on the `online-data` orphan branch
(`merge_sources.py`, `merge_pio_boards.py`, `build_manifest.py`,
`dump_platformio.py`) are NOT moved here — they predate this directory and
the convention is documented in [issue #718](https://github.com/FastLED/fbuild/issues/718).

## USB VID:PID Supplements

The Espressif supplement ingests the official `espressif/usb-pids` registry:

- `allocated-pids.txt` for customer/product allocations under VID `0x303a`
- `allocated-pids-espressif-devboards.txt` for Espressif-built devboard PIDs

It emits the flat JSON shape consumed by `online-data/tools/merge_sources.py`
so product-level names such as `303a:8001` (`Unexpected Maker TinyS2 - Arduino`)
land in `usb-vid.json` and the www SQLite `vidpid` table. Board existence is
still governed by the board catalogs (`pio-boards` / FastLED board data); a PID
registry entry does not by itself prove that fbuild supports a board.

The Raspberry Pi supplement ingests the official `raspberrypi/usb-pid`
allocation table for VID `0x2e8a`. It emits product names from the upstream
`Product Description` column while keeping the VID owner as
`Raspberry Pi Foundation`; the per-row `Company` cell is allocation context,
not the USB vendor name. Blank placeholders, ranges, and reserved rows are
skipped.

The Nordic supplement ingests Nordic-maintained nRF Connect Programmer
USB-product arrays and the `pc-nrf-dfu-js` README. It emits current Nordic
DFU / MCUboot rows for VID `0x1915`, including the specific
`1915:521f` PCA10059 nRF52840 dongle SDFU bootloader label, while keeping
application-dependent IDs such as `cafe` generic rather than assigning them to
a single firmware role.

The Microchip supplement has explicit source tiers. `--tier first-party`
parses Microchip-maintained `pyedbglib` and `pykitinfo` rows for Atmel VID
`0x03eb` CMSIS-DAP tools and Microchip VID `0x04d8` MPLAB tools; the workflow
orders this before generic USB-ID sources so first-party tool names win.
`--tier supplemental` parses AVRDUDE plus selected Arduino/LowPowerLab board
package rows only after first-party and generic sources. The weak tier fills
gaps such as LowPowerLab SAMD VID/PIDs, but it must not override Microchip
first-party data. LowPowerLab's current package maps CurrentRanger to
`04d8:ee44`/`ee48`/`ee4c` and Moteino M0 to `04d8:eee4`/`eee5`/`eee8`;
local FastLED board JSON currently carries `current_ranger` as `04d8:eee5`,
so that local mismatch is noted rather than used as source truth.

The Arduino supplement parses official ArduinoCore `boards.txt` files from
Arduino-maintained AVR, SAM, SAMD, megaAVR, mbed, Renesas, and Zephyr cores.
Arduino does not publish a single public PID allocation registry; the core
board definitions are the authoritative public table for Arduino VID `0x2341`
and historical Arduino VID `0x2a03`. The workflow orders this source before
generic USB-ID feeds so current Arduino product names win. As with other
board-package data, a row from an ArduinoCore source improves VID/PID
resolution but does not prove the board exists under
`crates/fbuild-config/assets/boards`.

The Adafruit supplement parses Adafruit-maintained Arduino core `boards.txt`
files for SAMD, nRF52, AVR/32u4, and WICED rows, then fills newer gaps from
Adafruit TinyUF2 bootloader `board.h` descriptors and CircuitPython
`mpconfigboard.mk` descriptors for Adafruit-prefixed boards. These sources
cover VID `0x239a` products across SAMD, nRF52, ESP32-S2/S3, and RP2040
families. The workflow orders the supplement before generic USB-ID feeds so
Adafruit first-party product names win, but entries still describe USB
products rather than fbuild board support; support is governed by
`crates/fbuild-config/assets/boards`.

The SparkFun supplement has explicit source tiers for VID `0x1b4f`.
`--tier first-party` parses SparkFun-maintained Arduino board package files,
product-repo board files, and SparkFun product descriptors such as UF2
`board_config.h` and CircuitPython `mpconfigboard.mk` files. The workflow
orders this first-party tier before generic USB-ID feeds so SparkFun-owned
product names win. `--tier supplemental` parses third-party PlatformIO board
JSON `build.hwids` and Adafruit CircuitPython descriptors for SparkFun-named
boards only after first-party and generic USB-ID sources. Those weak rows fill
gaps such as newer SparkFun ESP32/RP/Teensy MicroMod products but must not
override first-party rows. SparkFun's Apollo3 package currently has no
`vid.N`/`pid.N` rows, so Artemis/Apollo3 board discovery remains a weak CH340
bridge hint rather than a SparkFun PID table. A SparkFun VID/PID row is USB
product metadata, not proof of fbuild board support; support still depends on
the board existing under `crates/fbuild-config/assets/boards`.

The Seeed supplement has explicit source tiers for Seeed VID `0x2886`.
`--tier first-party` reads the current Seeed Boards Manager package index,
downloads the newest Seeed-hosted archives for SAMD, nRF52, mbed nRF52,
Renesas RA, and i.MX RT packages, then parses each archive's `boards.txt`.
It also parses Seeed's own `platform-seeedboards` JSON files as a first-party
gap filler without replacing package-archive names. `--tier supplemental`
parses third-party Espressif, Silicon Labs, Arduino-Pico, PlatformIO,
CircuitPython, and TinyUF2 rows only after first-party and generic USB-ID
sources. The Seeed platform C6 row that reuses `2886:0046` is skipped because
that PID is already used by XIAO ESP32C3; the weak PlatformIO C6 rows
`2886:0048`/`8048` are allowed to fill the gap. XIAO RP2040 remains under
Raspberry Pi VID `0x2e8a` from first-party Seeed platform data, so the
CircuitPython `2886:0042` row is not used to remap it. Rows for boards absent
from `crates/fbuild-config/assets/boards` improve USB name resolution only;
they do not prove fbuild board support.

The FTDI supplement parses the upstream Linux `ftdi_sio_ids.h` header but only
the original FTDI PID section before the third-party marker. It emits a small
allowlist of missing FTDI-owned bridge/product rows such as `0403:6040` and
`0403:fbfa`; the workflow orders this source after generic USB-ID sources so
existing mature product names win for common bridge PIDs.

The WCH supplement parses OpenWCH's CH343 Linux driver and its udev rules for
newer VID `0x1a86` USB serial chips. Udev comments provide names for CH342,
CH343, CH344, CH347T, and CH9101-CH9104 rows; newer driver-only IDs such as
CH339, CH346, CH9105, CH911x, and CH9433 are named from the driver's
PID-to-chip switch cases. It does not infer board names from bridge-chip IDs.

The Teensy supplement parses PJRC's `teensy3/usb_desc.h` and
`teensy4/usb_desc.h` product descriptors plus `teensy_loader_cli` rebootor and
HalfKay bootloader references. Teensy PIDs identify USB personalities, not a
single board model; duplicate PIDs that differ by USB BCD version are collapsed
to conservative family labels such as `Teensyduino MIDI + Serial`.

The STM supplement parses ST's OpenOCD ST-LINK driver and `stlink.cfg` PID
list, then adds common ST DFU and virtual COM rows. These entries identify
debugger or USB function products (`STLINK-V3P`, `STM Device in DFU Mode`,
`Virtual COM Port`); they do not imply a specific Nucleo or Discovery board.

The NXP supplement parses NXP's mfgtools/UUU `config.cpp` table for VID
`0x1fc9` ROM downloader and fastboot protocol rows. Entries such as
`1fc9:0135` are labeled as NXP i.MX/i.MX RT serial downloader modes rather
than as a specific downstream board.

The Silicon Labs supplement parses Linux's CP210x driver for Silicon
Labs-owned bridge defaults under VID `0x10c4`, then parses a first-party
SiliconLabsSoftware Arduino example udev rule for Energy Micro VID
`0x2544`, PID `0001`. Silicon Labs does not publish a public PID registry,
and its current Arduino core only declares board-package rows under Arduino
and Seeed VIDs (`2341:0072`, `2886:0062`, `2886:8062`), so this supplement
does not infer Silicon Labs board names from bridge-chip or debug-interface
IDs.

Ambiq/Apollo3 currently has no wired PID supplement. The public USB-ID
sources already identify VID `0x2aec` as Ambiq Micro with PID `6011`
(`Converter`), while VID `0x1cbe` belongs to Luminary Micro/TI rather than
Ambiq. SparkFun's `Arduino_Apollo3` board package has no `vid.N`/`pid.N`
rows to ingest, and the Apollo3 boards present under
`crates/fbuild-config/assets/boards` are SparkFun Artemis boards uploaded via
serial loader rather than a documented Ambiq PID table. The `AM_APOLLO3`
MCU-to-VID seed therefore uses the weak CH340 bridge VID `0x1a86` only as a
board-search hint for those SparkFun boards; it does not add Ambiq product
PIDs without a first-party source. Third-party SDK or board-package rows may
be added later as supplemental data, but they should merge after first-party
and generic USB-ID sources so they fill gaps only.

The Renesas RA supplement parses Arduino's `ArduinoCore-renesas` `boards.txt`
for Arduino-owned VID rows used by UNO R4, Nano R4, Portenta C33, and related
RA-family board-package entries. Renesas-owned VID `0x045b` remains sourced
from the generic USB-ID feeds unless a first-party Renesas PID registry is
found. Because the parsed source is an Arduino board package rather than a
Renesas allocation table, the workflow merges it after generic USB-ID sources
and vendor-owned supplements. Rows from ArduinoCore-renesas may describe
boards that are not present under `crates/fbuild-config/assets/boards`; those
rows improve VID/PID resolution but do not prove fbuild board support.

## Tests

```bash
uv run --no-project --with pytest pytest online-data-tools/test_build_sqlite.py -v
uv run --no-project --with pytest pytest online-data-tools/test_espressif_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_raspberrypi_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_nordic_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_microchip_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_arduino_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_adafruit_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_sparkfun_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_seeed_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_ftdi_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_wch_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_teensy_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_stm_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_nxp_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_silabs_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_renesas_usb_pids.py -v
```

Each script declares its own PEP 723 dependencies and is runnable via
`uv run --no-project --script <script>.py`.
