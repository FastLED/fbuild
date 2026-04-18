# `FastLED/fbuild/setup` — composite GitHub Action

One-line fbuild setup for consumer CI pipelines. Installs fbuild, wires `actions/cache@v4` with sensible defaults, and exports `FBUILD_CACHE_DIR` so subsequent steps Just Work.

For the underlying cache design (what's cached, why, and how to tune the key for your project) see [`../../docs/CI_CACHING.md`](../../../docs/CI_CACHING.md).

## Minimal usage

```yaml
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: FastLED/fbuild/.github/actions/setup@main
        with:
          cache-key-extra: ${{ hashFiles('platformio.ini') }}
      - run: fbuild build examples/Blink -e esp32dev
```

## Full matrix example

```yaml
jobs:
  build:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
        board: [uno, esp32dev, esp32s3, teensy41]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4

      - uses: FastLED/fbuild/.github/actions/setup@main
        id: fbuild
        with:
          cache-key-extra: ${{ matrix.board }}-${{ hashFiles('platformio.ini') }}

      - run: fbuild build examples/Blink -e ${{ matrix.board }}

      - name: Second build should be a near-no-op (smoke test cache warmth)
        shell: bash
        run: |
          start=$SECONDS
          fbuild build examples/Blink -e ${{ matrix.board }}
          elapsed=$((SECONDS - start))
          echo "second-build elapsed: ${elapsed}s"
          test "$elapsed" -lt 15 || {
            echo "::error::cache restore did not produce a warm build (took ${elapsed}s)"
            exit 1
          }
```

## Inputs

| Input | Default | Description |
|---|---|---|
| `fbuild-version` | `latest` | PyPI version spec. Pin to an exact version (`2.1.16`) for reproducible CI. |
| `python-version` | `3.12` | Python used to install fbuild. Must be ≥ 3.9. |
| `cache` | `true` | Set to `false` to install fbuild without wiring `actions/cache`. |
| `cache-key-extra` | `""` | String baked into the cache key. Use `hashFiles(...)` over your graph inputs so edits invalidate stale artifacts. |
| `cache-version` | `v1` | Manual cache bump. Increment when you want to force-invalidate across your matrix. |
| `cache-dir` | `$RUNNER_TEMP/fbuild-cache` | Override if you need a different cache root. |

## Outputs

| Output | Description |
|---|---|
| `cache-hit` | `true` if the cache was restored from a previous run, `false` on miss. |
| `cache-dir` | Resolved cache directory path. Useful for diagnostic steps. |
| `fbuild-hash` | sha256 prefix (16 hex chars) of the installed fbuild wheel's `RECORD` file. Baked into the cache key so any fbuild change — including a re-released wheel at the same version — invalidates stale cache artifacts. |

## What this action does

1. Sets up Python.
2. Resolves a stable cache directory under `$RUNNER_TEMP` (survives across steps of the same job; not a surprise `$HOME` path).
3. Exports `FBUILD_CACHE_DIR` via `$GITHUB_ENV` so every later step inherits it.
4. Installs fbuild from PyPI at the requested version.
5. **Computes the installed fbuild's content hash** (sha256 of its dist-info `RECORD`) and bakes it into the cache key. This guarantees the cache is tied to the exact fbuild you're running, not just the PyPI version string — so `latest` is safe and a re-released wheel won't poison the cache.
6. Restores (and on job-end, saves) the cache via `actions/cache@v4`.

### Why hash-pinning matters

The fbuild cache stores toolchains, frameworks, and build outputs whose internal layout is tied to fbuild's own fingerprint format, response-file generation, and path embedding. If fbuild itself changes without the cache key changing, the restored cache can encode stale paths or obsolete fingerprints — silent cache poisoning.

Baking the wheel's `RECORD` hash into the key means:

- `fbuild-version: latest` is reproducible-by-construction: a new release produces a new hash, which rolls the cache.
- A re-uploaded wheel (same version, different contents) invalidates correctly.
- Per-platform wheel differences (manylinux vs. macOS vs. Windows) produce different hashes and do not cross-pollute caches.

It does **not** cache `~/.fbuild/*/daemon/` — that's ephemeral runtime state and restoring it across runs causes broken daemon discovery on the next client call. The action sidesteps the whole `~/.fbuild/` tree by redirecting fbuild's cache to `$RUNNER_TEMP/fbuild-cache` via `FBUILD_CACHE_DIR`.

## Version pinning

For reproducible CI, reference a release tag rather than `@main`:

```yaml
- uses: FastLED/fbuild/.github/actions/setup@v1
```

At time of writing there is no `v1` tag — use `@main` and pin `fbuild-version` to an explicit PyPI version if reproducibility matters.

## Related

- [`docs/CI_CACHING.md`](../../../docs/CI_CACHING.md) — detailed design of the underlying cache, plus raw `actions/cache@v4` snippets for consumers who prefer to avoid the action dependency.
- [#101](https://github.com/FastLED/fbuild/issues/101) — the issue that tracked creation of this action.
