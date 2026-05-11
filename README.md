# bench/fastled-examples

This is an **orphan branch** of the `FastLED/fbuild` repo. It shares no history with `main` and exists for a single purpose: run a self-contained CI benchmark that measures wall-clock time to compile the full FastLED `examples/` set for one board, broken out by phase.

Tracked by: [FastLED/fbuild#112](https://github.com/FastLED/fbuild/issues/112)

## Why an orphan branch?

We want to iterate on the benchmark (change caching strategy, change board, try different install paths) without polluting `main`'s history and without triggering `main`'s CI. An orphan branch gives us a clean namespace and its own CI surface.

## Running the benchmark

The workflow triggers on **every push** to `bench/fastled-examples`. `workflow_dispatch` would require the workflow to live on `main`, which defeats the point of the orphan branch.

Edit [`bench.config`](./bench.config), commit, push:

```bash
# change the parameters
$EDITOR bench.config

git commit -am "bench: try esp32dev warm"
git push
```

To re-run with the same parameters (e.g., for a warm-cache repeat), push an empty commit:

```bash
git commit --allow-empty -m "bench: rerun warm"
git push
```

### Cold vs warm

- **Cold** = `bench.config` has a non-empty `CACHE_BUST` value. Bump it (e.g., to a timestamp) to force a cache miss.
- **Warm** = `CACHE_BUST=` (empty). Run once to prime, then again to measure the warm number.

## Phases measured

Each phase writes one row into `phase-timings.tsv` (uploaded as an artifact):

| phase                       | what it measures                                              |
|-----------------------------|----------------------------------------------------------------|
| `clone_fastled`             | `git clone` FastLED at the requested ref                        |
| `warm_restore`              | seconds to look up + download + extract the prior-run cache artifact (or 0 if cold) |
| `warm_restore_hit`          | `true`/`false` — did we restore from a prior run's artifact      |
| `warm_restore_source_run`   | run id we restored from (or `<none>`)                            |
| `uv_sync`                   | FastLED's `./install` / `uv sync` to materialize the .venv      |
| `compile`                   | `./compile --no-interactive --no-parallel <board> <examples>`   |
| `pack_cache`                | tar+zstd the cache dirs into `bench-cache.tar.zst`               |
| `job_total`                 | end-to-end wall-clock from job start to end                     |

The `pip install fbuild` step is inside the FastLED `./install` script (which calls `uv sync`), so it shows up as part of `uv_sync` in the TSV.

## Cache strategy (iter7+)

This benchmark uses `actions/upload-artifact` + `actions/download-artifact` instead of `actions/cache`. Why: `FastLED/fbuild` has ~9.4GB of toolchain caches on `main` (close to the 10GB repo cap), and any new save triggers LRU eviction of our small 64MB bench cache within ~20 minutes. Artifacts have a separate, generous quota and 30-day retention here, so a warm cache survives between runs.

On each run:

1. Look up the most recent successful benchmark run on this branch.
2. Try to download the artifact named `bench-cache-${BOARD}-${REF}-${SOLDR_VER}-${CACHE_BUST}`.
3. If found, extract it (preserves absolute paths via `tar -xPf`). Sets `warm_restore_hit=true`.
4. If not found, run cold.
5. After compile, repack the cache dirs into a new artifact with the same name (overwrites the prior one).

## Interpreting results

The short-term target is to find the largest single phase and shrink it. Expected shape on `ubuntu-latest`:

- Cold AVR (`uno`): toolchain download dominates inside `compile`.
- Warm AVR (`uno`): `uv_sync` + checkout overheads dominate; actual compile should be cache-hit-heavy.
- Cold ESP32: toolchain + pioarduino bootstrap dominates.
- Warm ESP32: compile still non-trivial due to large object set; daemon and toolchain should be free.

Update the tracking issue with each run's numbers so progress is visible.
