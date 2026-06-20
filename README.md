# `online-data` — fbuild's orphan reference-data branch

This is the orphan branch carrying nightly-refreshed reference datasets
that `fbuild` reads at runtime as a fallback when its bundled offline
snapshots can't answer.

**Do not merge this branch into `main`.** It shares no history with the
source tree on purpose, so it can be force-pushed (history pruned to the
most recent 200 commits, or whole-branch orphaned on schema changes)
without ever touching the working repo's history.

## Layout

```
manifest.json                    # entry point — clients fetch this first
data/
  usb-vid.json                   # USB vendor catalog (nested per-VID)
  usb-vid-conflicts.json         # flat per-(vid,pid) source disagreement log
  pio-boards.json                # full PlatformIO board catalog
  vendor_boards.json             # slim {vendor, name, mcu} per board id
tools/
  README.md
  dump_platformio.py             # pio boards → JSON dump
  merge_sources.py               # union usb.ids sources → nested usb-vid.json
  merge_pio_boards.py            # deep-union pio dumps + emit slim view
  build_manifest.py              # auto-discover data/*.json → manifest.json
```

## URLs (raw GitHub)

Always start from the manifest — direct dataset URLs may change in the
future, but the manifest's `datasets.<name>.url` field is the contract.

- Manifest:           <https://raw.githubusercontent.com/fastled/fbuild/online-data/manifest.json>
- USB vendor catalog: <https://raw.githubusercontent.com/fastled/fbuild/online-data/data/usb-vid.json>
- Vendor conflicts:   <https://raw.githubusercontent.com/fastled/fbuild/online-data/data/usb-vid-conflicts.json>
- Full PIO boards:    <https://raw.githubusercontent.com/fastled/fbuild/online-data/data/pio-boards.json>
- Slim vendor_boards: <https://raw.githubusercontent.com/fastled/fbuild/online-data/data/vendor_boards.json>

## `usb-vid.json` schema

```json
{
  "0403": {
    "vendor": "Future Technology Devices International, Ltd",
    "products": [
      ["6001", "FT232 Serial (UART) IC"],
      ["6010", "FT2232C/D/H Dual UART/FIFO IC"]
    ]
  }
}
```

Top-level key is the 4-hex-digit VID (lowercase). Each entry carries the
canonical `vendor` name once + a `products` list of `[pid, product_name]`
tuples sorted by pid.

## How it gets refreshed

The nightly workflow `.github/workflows/update-data.yml` on `main`:

1. Builds `crates/fbuild-core/examples/dump_usb_ids.rs` with soldr and
   dumps the bundled `usb-ids` Rust crate (tier-1 USB-VID source).
2. `curl`s the two upstream `usb.ids` text mirrors:
   `http://www.linux-usb.org/usb.ids` and
   `https://raw.githubusercontent.com/usbids/usbids/master/usb.ids`.
3. Runs `pio boards --json-output` (full PlatformIO board catalog).
4. Runs `merge_sources.py` → nested `data/usb-vid.json` + flat
   `data/usb-vid-conflicts.json` + per-dataset manifest fragment.
5. Runs `merge_pio_boards.py` → `data/pio-boards.json` (deep-union with
   the previously committed dump) + `data/vendor_boards.json` (slim
   view) + per-dataset manifest fragment(s).
6. Runs `build_manifest.py` to auto-discover `data/*.json` and stitch
   the per-dataset fragments into the unified `manifest.json`.
7. Commits + pushes only if any file actually changed; prunes branch
   history to the most recent 200 commits.

Manual trigger: Actions → "Update data" → Run workflow.

## License

Source data: dual-licensed GPLv2+ / 3-clause BSD per linux-usb.org's
`usb.ids` upstream. PlatformIO board data: see the
[platformio/platform-* registries](https://registry.platformio.org/).
Scripts and this README are MIT OR Apache-2.0, same as the rest of
`fastled/fbuild`.
