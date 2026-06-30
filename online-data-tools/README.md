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

## Tests

```bash
uv run --no-project --with pytest pytest online-data-tools/test_build_sqlite.py -v
uv run --no-project --with pytest pytest online-data-tools/test_espressif_usb_pids.py -v
```

Each script declares its own PEP 723 dependencies and is runnable via
`uv run --no-project --script <script>.py`.
