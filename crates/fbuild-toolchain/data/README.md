# Pinned inputs for the delegated ESP-IDF path

No file here is generated at build time; all are reviewed pins
(FastLED/fbuild#1970). The two Python inputs are `include_str!`-ed by
`src/toolchain/esp_idf_python.rs`; `esp-idf-v5.5.1-tools.json` is read only by
the pin-verification tests.

## `esp-idf-v5.5.1-python.lock`

The Python environment a delegated `idf.py` build (#1971) runs in: the pinned
commit's `tools/requirements/requirements.core.txt`, resolved against the
release's published constraints file with

```sh
uv pip compile --universal --generate-hashes --python-version 3.12 \
  --constraints esp-idf-v5.5.1-constraints.txt requirements.core.txt
```

`--universal` means each line carries the sha256 of every platform's wheel (and
of `esptool`'s sdist), so one file is correct on all five hosts and
`pip install --require-hashes --no-deps` refuses anything that is not listed.

## `esp-idf-v5.5.1-constraints.txt`

The release's own `espidf.constraints.v5.5.txt` (served from
`dl.espressif.com` next to `tools.json`), committed verbatim so the lock can be
regenerated and so the environment can be checked with `-c` offline.

## `esp-idf-v5.5.1-tools.json`

The pinned release's own `tools/tools.json`, committed verbatim (sha256
`5c6a0453a9af7ee382681dab98ab9a3c51920cc68b57038d35848b0f08bc2793`) so the
`PINS` table in `src/toolchain/esp_idf_release.rs` can be *checked* instead of
trusted. `pins_are_verbatim_rows_of_the_release_tools_json` compares every row's
version, sha256, size and URL against this file; a drifted row fails with the
row named, so moving the pin (#2133) is a data diff rather than a
transcription exercise. Replace the snapshot **and** `IDF_RELEASE_VERSION`
together — `the_tools_json_snapshot_belongs_to_the_pinned_release` fails if only
one of them moves.
