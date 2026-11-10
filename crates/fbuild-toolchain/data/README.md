# Pinned inputs for the delegated ESP-IDF path

Neither file is generated at build time; both are reviewed pins
(FastLED/fbuild#1970) and are `include_str!`-ed by
`src/toolchain/esp_idf_python.rs`.

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
