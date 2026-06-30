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
| `fetch_ftdi_usb_pids.py` | Linux `ftdi_sio_ids.h` original-FTDI PID section | merge-compatible `/tmp/ftdi-usb-pids.json` |
| `fetch_wch_usb_pids.py` | WCH CH343 Linux driver + udev rules | merge-compatible `/tmp/wch-usb-pids.json` |
| `fetch_teensy_usb_pids.py` | PJRC Teensy core headers + loader CLI | merge-compatible `/tmp/teensy-usb-pids.json` |
| `fetch_stm_usb_pids.py` | ST/OpenOCD ST-LINK sources | merge-compatible `/tmp/stm-usb-pids.json` |
| `fetch_nxp_usb_pids.py` | NXP mfgtools/UUU config table | merge-compatible `/tmp/nxp-usb-pids.json` |

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

## Tests

```bash
uv run --no-project --with pytest pytest online-data-tools/test_build_sqlite.py -v
uv run --no-project --with pytest pytest online-data-tools/test_espressif_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_raspberrypi_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_nordic_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_ftdi_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_wch_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_teensy_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_stm_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_nxp_usb_pids.py -v
```

Each script declares its own PEP 723 dependencies and is runnable via
`uv run --no-project --script <script>.py`.
