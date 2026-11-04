# prebuilt framework bench (pilot)

Measures the first-compile effect of prebuilt framework artefacts: a build
that installs a published `prebuilt.tar.zst` instead of compiling the
framework's translation units.

## What it measures

Two numbers, and they are not interchangeable:

1. **Framework translation units compiled** — deterministic, load-independent,
   the honest statement of what the feature removes. A source build compiles
   every framework TU; a hit compiles none.
2. **Wall clock** — context only. This machine runs other work (load average
   45–80 on 16 cores), so wall clock moves with the neighbours. Report it with
   that caveat or not at all.

`firmware.bin` byte equality is the correctness gate and is load-independent.

## Pilot results

Same host, same framework (`esp32-arduino` 3.3.12), framework caches cleared
before each arm, host load 45–80:

| board | framework TUs: source → hit | wall: source → hit | gain | firmware identical |
|---|---|---|---|---|
| `esp32-c3-devkitm-1` | 60 → 0 | 14.9 s → 10.7 s | 1.39x | yes |
| `esp32dev` | 60 → 0 | 12.7 s → 11.0 s | 1.15x | yes |
| `esp32-s3-devkitc-1` | 60 → 0 | 18.9 s → 11.6 s | 1.63x | yes |

Artefact sizes, against a 2.6 GB installed framework tree:

| board | archive | files |
|---|---|---|
| `esp32-c3-devkitm-1` | 3,453,642 B | 2,679 |
| `esp32dev` | 3,585,802 B | — |
| `esp32-s3-devkitc-1` | 4,075,144 B | — |

**Read the gain column with care.** A hit removes *all* framework compilation,
but the remaining ~10–11 s is fixed cost the feature does not touch (daemon
round-trip, sketch compile, link, objcopy), so the wall-clock ceiling at this
fixture size is bounded. The gain will be larger for projects with a heavier
framework configuration and smaller for sketches that barely use the framework.

This bench also does **not** measure the provisioning phase: a genuinely new
user additionally pays framework/toolchain download and extraction (measured
elsewhere at ~299 s on a cold cache for esp32c3, of which the framework tree is
2.6 of 4.7 GB). V1 still provisions framework sources before planning, so a hit
saves compilation only, not download bytes.

## Running it

1. Publish an artefact for the board (prints the upload commands; it does not
   upload):

   ```
   uv run --no-project --with zstandard python ci/build_prebuilt.py \
     --project tests/platform/esp32c3 --board esp32-c3-devkitm-1 \
     --environment esp32c3 --output /tmp/prebuilt-artifacts \
     --source-repo https://github.com/espressif/arduino-esp32 \
     --source-ref 3.3.12 --bucket s3://fbuild
   ```

2. Serve the artefact with an Index + shard on a local HTTP base, and point the
   client at it with `FBUILD_MIRROR_MANIFEST_URL`. The key layout is fixed:
   `prebuilt/<framework>/<version>/<arch>/fp-<first16>/prebuilt.tar.zst`.

3. Build each arm with the framework caches cleared in between:

   ```
   fbuild build <project> -e <env> --no-prebuilt
   FBUILD_MIRROR_MANIFEST_URL=http://127.0.0.1:8787/manifest.json \
     fbuild build <project> -e <env> --prebuilt
   ```

Note that a hit leaves the reusable framework caches warm for the *next* run,
so alternate the arms and clear between them or the source arm stops being a
source build.

The local-fixture variant (`bash bench/first_compile <gcc> <objcopy>`) exercises
the same code path on a synthetic single-TU framework; it is useful for
regression checks but reports a *loss* at that size because the fixed overhead
exceeds one trivial TU. Use the pilot boards above for the real statement.
