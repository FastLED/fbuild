# Pinned inputs for the delegated ESP-IDF path

No file here is generated at build time; all are reviewed pins
(FastLED/fbuild#1970). The two Python inputs are `include_str!`-ed by
`src/toolchain/esp_idf_python.rs`; `esp-idf-v5.5.6-tools.json` is read only by
the pin-verification tests.

## `esp-idf-v5.5.6-python.lock`

The hash-locked Python environment of the pinned release: `tools/requirements/
requirements.core.txt` of commit `880a203ae603d08a94d06aa01e0d6761958e5e9e`,
resolved against the release's published constraints. `pip --require-hashes
--no-deps` installs exactly this, so the delegated `idf.py` runs under the
packages Espressif tested and nothing else.

`--universal` (all platforms in one file) is what makes one file correct on
all five hosts: each line carries the sha256 of every wheel **and** sdist, so
pip picks the file matching the platform it runs on. There is no fallback and
no unhashed download, which is the whole point -- see `esp_idf_python.rs`.
`esptool` ships a pre-built Windows wheel and an sdist only, so one file is
correct on all five hosts.

## `esp-idf-v5.5.6-constraints.txt`

The release's own `espidf.constraints.v5.5.txt` (served from
`dl.espressif.com` next to `tools.json`), committed verbatim so the lock can be
regenerated and so the environment can be checked with `-c` offline. The
served file is regenerated per IDF release, so it moves with the pin.

## `esp-idf-v5.5.6-tools.json`

The pinned release's own `tools/tools.json`, committed verbatim so the `PINS`
table in `src/toolchain/esp_idf_release.rs` can be *checked* instead of
trusted. `pins_are_verbatim_rows_of_the_release_tools_json` compares every
row's version, sha256, size and URL against this file; a drifted row fails with
the row named, so moving the pin (#2133) is a data diff rather than a
transcription exercise. Replace the snapshot **and** `IDF_RELEASE_VERSION`
together — `the_tools_json_snapshot_belongs_to_the_pinned_release` fails if only
one of them moves.
