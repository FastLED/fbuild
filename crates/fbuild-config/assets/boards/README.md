# Boards

PlatformIO board definitions — one JSON file per board.

- `manifest.json` — sorted list of all 1609 board IDs
- `json/{board_id}.json` — individual board configuration

## Enrichment

Board JSONs are enriched with `build` and `upload` sections from PlatformIO's
full board definitions. This provides accurate `core`, `variant`, `extra_flags`,
`upload.protocol`, `upload.speed`, `flash_mode`, `f_flash`, and linker/partition
info — eliminating hardcoded platform-family defaults.

Enrichment is a **one-off maintenance step**, not part of the build or CI
pipeline. Run it manually when adding new boards or upgrading PlatformIO
platform packages (requires PlatformIO platforms installed locally):

```bash
uv run python ci/one_off_enrich_boards.py
```

Boards without a local PlatformIO platform install are left unenriched and fall
back to generic defaults at runtime.
