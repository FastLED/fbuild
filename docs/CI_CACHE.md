# Cross-run CI cache strategy

This is the consumer-facing checklist for carrying fbuild state across GitHub
Actions runs. For the deeper design notes and matrix examples, see
[CI_CACHING.md](CI_CACHING.md). For the composite action inputs and outputs, see
[`.github/actions/setup/README.md`](../.github/actions/setup/README.md).

## What to cache

Cache these paths together with a key that identifies the runner, fbuild build,
board, and build-graph inputs:

| Path | Why it matters |
|---|---|
| `FBUILD_CACHE_DIR` | fbuild's package cache: downloaded archives, extracted toolchains/frameworks/libraries, and `index.sqlite`. The setup action pins this to `$RUNNER_TEMP/fbuild-cache`. |
| `$ZCCACHE_DIR` | zccache's compiled-object store. This is required for cross-run per-translation-unit reuse. |
| `.fbuild/build` | Project build outputs and fingerprints. This is what lets a second build become a near no-op when inputs have not changed. |

Do not cache `~/.fbuild/*/daemon/`. It contains runtime-only PID, port, log,
and status files for a daemon that will not exist in the next CI run.

## Key composition

For a single board on one pinned runner image, compose keys from:

1. Cache schema version: a manual `v1`, `v2`, or date-stamped bump.
2. Runner identity: the fixed image label plus `${{ runner.os }}-${{ runner.arch }}`.
3. Installed fbuild content hash: use the setup action's `fbuild-hash` output.
4. Board or environment name: for example `esp32dev`.
5. Build-graph inputs: `platformio.ini`, board JSON, lock files, toolchain pins, or any file that changes libraries/frameworks/build flags.

The fbuild content hash is important. Version strings alone do not protect
against local installs, dev builds, or a wheel re-uploaded with the same version.
The setup action computes the hash for you and uses it in its built-in
`FBUILD_CACHE_DIR` cache key.

## Invalidation pattern

Keep normal invalidation automatic by hashing graph inputs. Keep forced
invalidation explicit by carrying a manual cache version in workflow `env`:

```yaml
env:
  FBUILD_CACHE_VERSION: v1
```

Bump that value when you change runner images, alter cache layout assumptions,
or suspect a poisoned cache. If you use the setup action's built-in cache, pass
the same value to `cache-version` so both fbuild's package cache and your
consumer-managed zccache/build-output cache roll together.

## Copy-paste GitHub Actions block

This example is for one board on one pinned runner image. It lets the setup
action cache fbuild's package/tool cache, then adds a second cache entry for
zccache and project build outputs.

```yaml
jobs:
  build:
    runs-on: ubuntu-24.04
    env:
      FBUILD_BOARD: esp32dev
      FBUILD_RUNNER_IMAGE: ubuntu-24.04
      FBUILD_CACHE_VERSION: v1
    steps:
      - uses: actions/checkout@v4

      - name: Setup fbuild
        id: fbuild
        uses: FastLED/fbuild/.github/actions/setup@main
        with:
          cache-version: ${{ env.FBUILD_CACHE_VERSION }}
          cache-key-extra: ${{ env.FBUILD_RUNNER_IMAGE }}-${{ env.FBUILD_BOARD }}-${{ hashFiles('platformio.ini', 'rust-toolchain.toml', '.fbuild-cache-version') }}

      - name: Restore zccache store and fbuild build outputs
        uses: actions/cache@v4
        with:
          path: |
            ${{ steps.fbuild.outputs.zccache-store-path }}
            .fbuild/build
          key: zccache-${{ env.FBUILD_CACHE_VERSION }}-${{ env.FBUILD_RUNNER_IMAGE }}-${{ runner.os }}-${{ runner.arch }}-${{ steps.fbuild.outputs.fbuild-hash }}-${{ env.FBUILD_BOARD }}-${{ hashFiles('platformio.ini', 'rust-toolchain.toml', '.fbuild-cache-version') }}
          restore-keys: |
            zccache-${{ env.FBUILD_CACHE_VERSION }}-${{ env.FBUILD_RUNNER_IMAGE }}-${{ runner.os }}-${{ runner.arch }}-${{ steps.fbuild.outputs.fbuild-hash }}-${{ env.FBUILD_BOARD }}-
            zccache-${{ env.FBUILD_CACHE_VERSION }}-${{ env.FBUILD_RUNNER_IMAGE }}-${{ runner.os }}-${{ runner.arch }}-${{ steps.fbuild.outputs.fbuild-hash }}-
            zccache-${{ env.FBUILD_CACHE_VERSION }}-${{ env.FBUILD_RUNNER_IMAGE }}-${{ runner.os }}-${{ runner.arch }}-

      - name: Build
        run: fbuild build examples/Blink -e "$FBUILD_BOARD"
```

Adjust the `hashFiles(...)` list to match your repository. If your board list,
library pins, or build flags live somewhere else, include those files in the
hash.

## Expected numbers

Use these as order-of-magnitude expectations, not hard pass/fail thresholds.
[CI_CACHING.md](CI_CACHING.md#expected-timings) has the current CI-oriented
summary; [PERF_WARM_BUILD.md](PERF_WARM_BUILD.md) records the local profiling
context behind the warm-path work.

| Scenario | Observed shape |
|---|---|
| First cache-miss ESP32 build | Can be dominated by downloads and full framework compile; the local benchmark saw about 14 minutes on Windows for `tests/platform/esp32dev`. |
| Warm small AVR build | About 250 ms wall-clock in the local benchmark once disk cache and build outputs were populated. |
| Steady-state warm ESP32 build | About 280 ms wall-clock for runs 3+ with the same daemon. |
| Current CI warm no-op target | Near-warm builds should be single-digit seconds including CLI and daemon overhead; the internal hot path can be much lower. |

CI cache restore should remove package download and extraction time. It does not
guarantee every restored build is as fast as a hot-daemon local no-op, especially
for ESP32 paths that still need warm-path validation. A restored build that still
downloads toolchains or frameworks usually means the package cache key/path is
wrong; a restored build that skips downloads but spends time validating project
outputs points at warm-path behavior instead.

## Common pitfalls

- Caching `~/.fbuild/prod/` wholesale restores daemon state. Cache
  `FBUILD_CACHE_DIR` instead.
- Sharing one key across OSes, architectures, or runner images can restore
  incompatible toolchains or path-embedded response files.
- Keying only on `platformio.ini` can miss board JSON, lock file, or toolchain
  changes that should invalidate stale outputs.
- Omitting `fbuild-hash` can keep stale artifacts after an fbuild upgrade or dev
  build change.
- Restoring zccache without `.fbuild/build` still helps compile reuse, but it
  will not preserve fbuild's project-level warm-build fast path.
- Letting cache entries grow past GitHub Actions limits can make saves fail. If
  needed, cap fbuild's package cache with `FBUILD_CACHE_BUDGET=8G`.

## See also

- [CI_CACHING.md](CI_CACHING.md) - detailed cache design, raw snippets, and
  matrix guidance.
- [`.github/actions/setup/README.md`](../.github/actions/setup/README.md) -
  setup action contract and examples.
- [PERF_WARM_BUILD.md](PERF_WARM_BUILD.md) - benchmark context behind the
  expected warm-build numbers.
