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

## Tests

```bash
uv run --no-project --with pytest pytest online-data-tools/test_build_sqlite.py -v
uv run --no-project --with pytest pytest online-data-tools/test_espressif_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_raspberrypi_usb_pids.py -v
uv run --no-project --with pytest pytest online-data-tools/test_nordic_usb_pids.py -v
```

Each script declares its own PEP 723 dependencies and is runnable via
`uv run --no-project --script <script>.py`.
