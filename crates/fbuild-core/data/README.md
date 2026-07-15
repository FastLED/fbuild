# fbuild-core embedded data

This directory contains test fixtures and build-cache documentation. Board USB
VID/PID data is **not** a built-in fbuild runtime table: it is ingested from
the published FastLED/boards artifacts during the build/cache phase.

| File                    | Purpose                                                                                                                                                                                                                          |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `usb-vendors.tar.zst`   | Frozen test-only USB Vendor-ID fixture used by `crate::usb::embedded` under `cfg(test)`. It is never a production fallback. |
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

The vendor archive is intentionally frozen. New identities and corrections
must be published through FastLED/boards; never refresh this fixture to solve a
production lookup or deployment problem. Validate it only with `soldr cargo`
tests.
