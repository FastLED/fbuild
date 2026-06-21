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

The merger scripts on the `online-data` orphan branch
(`merge_sources.py`, `merge_pio_boards.py`, `build_manifest.py`,
`dump_platformio.py`) are NOT moved here — they predate this directory and
the convention is documented in [issue #718](https://github.com/FastLED/fbuild/issues/718).

## Tests

```bash
uv run --no-project --with pytest pytest online-data-tools/test_build_sqlite.py -v
```

Each script declares its own PEP 723 dependencies and is runnable via
`uv run --no-project --script <script>.py`.
