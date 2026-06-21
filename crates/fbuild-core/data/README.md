# fbuild-core embedded data

Binary blobs `include_bytes!`'d into `fbuild-core` at compile time.

| File                    | Purpose                                                                                                                                                                                                                          |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `usb-vendors.tar.zst`   | USB Vendor-ID → vendor-name map. Produced by `online-data-tools/build_vendor_archive.py` from the merged `online-data/data/usb-vid.json` (which already incorporates the curated `vendor_names_inlined.py` overlay). See `crate::usb::embedded`. |

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
