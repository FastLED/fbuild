# Boards

PlatformIO board definitions — one JSON file per board.

- `manifest.json` — sorted list of all 1609 board IDs
- `json/{board_id}.json` — individual board configuration

## Enrichment

Board JSONs are enriched with `build` and `upload` sections from PlatformIO's
full board definitions. This provides accurate `core`, `variant`, `extra_flags`,
`upload.protocol`, `upload.speed`, `flash_mode`, `f_flash`, and linker/partition
info — eliminating hardcoded platform-family defaults.

To re-enrich (requires PlatformIO platforms installed locally):

```bash
uv run python ci/enrich_boards.py
```

Boards without a local PlatformIO platform install are left unenriched and fall
back to generic defaults at runtime.
