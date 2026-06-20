# `tools/` — refresh pipeline for the `online-data` branch

Python scripts that produce the JSON files committed to this branch.
Invoked by `.github/workflows/update-data.yml` on `main` once a day plus
on demand via `workflow_dispatch`. All scripts are
`uv run --no-project --script` shebangs so they self-bootstrap any
dependencies.

| Script | Purpose |
|---|---|
| `dump_platformio.py` | `pio boards --json-output` → sorted id-keyed map. Inline dep on `platformio>=6`. |
| `merge_sources.py` | Union-merge USB vendor name sources (rs dump + linux-usb.org + github mirror) into nested-per-VID `data/usb-vid.json` (+ `data/usb-vid-conflicts.json` flat conflict log). Emits a per-dataset manifest fragment. |
| `merge_pio_boards.py` | Deep-union the new PIO dump with the previously committed `data/pio-boards.json` so transient field drops in `pio boards` output don't propagate. Also emits the slim `vendor_boards.json` view (just `{vendor, name, mcu}`). |
| `build_manifest.py` | **Auto-discovers** every `data/*.json` and emits `manifest.json` at the branch root. Per-dataset metadata (description, sources, key_format) is supplied via `--fragment NAME=PATH` files written by each merger. Drop any new JSON into `data/` and the manifest will pick it up next run. |
