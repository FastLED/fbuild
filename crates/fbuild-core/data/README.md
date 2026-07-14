# fbuild-core embedded data

This directory contains test fixtures and build-cache documentation. Board USB
VID/PID data is **not** a built-in fbuild runtime table: it is ingested from
the published FastLED/boards artifacts during the build/cache phase.

| File                    | Purpose                                                                                                                                                                                                                          |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `usb-vendors.tar.zst`   | USB Vendor-ID → vendor-name map (long-tail fallback, ~2.2k VIDs). Produced by `online-data-tools/build_vendor_archive.py`. See `crate::usb::embedded`. |
| `usb-vids.proto.zstd`   | Test fixture for the compact VID:PID overlay produced by **FastLED/boards**. It must never be used as a production built-in catalogue. Production ingestion fetches/consumes the published boards artifact. |

## How to refresh the VID:PID overlay (`usb-vids.proto.zstd`)

This is the ingested product/board resolution. The source of truth is the
[FastLED/boards](https://github.com/FastLED/boards) repo; its `site.yml`
workflow regenerates the artifact on every push to a data branch
(`platformio`/`arduino`/`vendors`/`other`).

```bash
# The production path consumes the published artifact directly; do not copy
# it into a runtime source file or commit a refreshed built-in VID/PID table:
curl -fsSLo <cache>/usb-vids.proto.zstd https://fastled.github.io/boards/usb-vids.proto.zstd
soldr cargo test -p fbuild-core usb::
```

To add a new board/probe resolution (e.g. a debug-probe VID:PID), edit the
data on the FastLED/boards `vendors` (or `other`) branch — **never** a
hardcoded table or embedded runtime blob in fbuild — and re-run the boards
pipeline. The published artifact then carries it through to fbuild's
`usb::resolve` on the next ingestion.

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
