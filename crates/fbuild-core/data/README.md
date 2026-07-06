# fbuild-core embedded data

Binary blobs `include_bytes!`'d into `fbuild-core` at compile time.

| File                    | Purpose                                                                                                                                                                                                                          |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `usb-vendors.tar.zst`   | USB Vendor-ID → vendor-name map (long-tail fallback, ~2.2k VIDs). Produced by `online-data-tools/build_vendor_archive.py`. See `crate::usb::embedded`. |
| `usb-vids.proto.zstd`   | Compact VID:PID → {vendor, product} overlay **and** a VID → vendor map, in one artifact. Produced by the **FastLED/boards** data pipeline (`builders/build_usb_ids.py` over the `platformio`/`arduino`/`vendors`/`other` branches). Baked in so full VID + VID:PID resolution works OFFLINE with no hardcoded per-board tables. See `crate::usb::data` (`embedded()`), consumed by `usb::resolve`. |

## How to refresh the VID:PID overlay (`usb-vids.proto.zstd`)

This is the ingested product/board resolution. The source of truth is the
[FastLED/boards](https://github.com/FastLED/boards) repo; its `site.yml`
workflow regenerates the artifact on every push to a data branch
(`platformio`/`arduino`/`vendors`/`other`).

```bash
# Regenerate from the boards repo (all four data branches), then copy in:
#   cd ../boards && python builders/site.py ...   # or fetch the published artifact
cp <boards-out>/usb-vids.proto.zstd crates/fbuild-core/data/usb-vids.proto.zstd
soldr cargo test -p fbuild-core usb::
```

To add a new board/probe resolution (e.g. a debug-probe VID:PID), edit the
data on the FastLED/boards `vendors` (or `other`) branch — NOT a hardcoded
table in fbuild — and re-run the boards pipeline. The proto then carries it
through to fbuild's `usb::resolve` on the next refresh.

## How to refresh the vendor archive

The nightly `Update data` workflow on `main` produces a fresh
`usb-vendors.tar.zst` under `online-data/data/`. To bump the embedded
copy here (a deliberate manual step — see issue #718):

```bash
# 1. Pull the latest from the online-data branch.
curl -sSLo crates/fbuild-core/data/usb-vendors.tar.zst \
  https://raw.githubusercontent.com/FastLED/fbuild/online-data/data/usb-vendors.tar.zst

# 2. Run the fbuild-core tests to confirm the archive parses + the
#    well-known entries still resolve.
soldr cargo test -p fbuild-core usb::embedded
```

`fbuild-core` will refuse to load the archive if its embedded
`manifest.json` reports a schema version newer than the consumer knows
about — bump `EMBEDDED_SCHEMA_VERSION` in `src/usb/embedded.rs` whenever
the archive format changes (in lock-step with
`online-data-tools/build_vendor_archive.py::SCHEMA_VERSION`).
