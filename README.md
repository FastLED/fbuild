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

| phase               | what it measures                                              |
|---------------------|----------------------------------------------------------------|
| `checkout_fbuild`   | shallow-clone `fbuild` to pull the `setup` composite action     |
| `fbuild_cache_hit`  | `true`/`false` — did we restore the fbuild cache from a prior run |
| `clone_fastled`     | `git clone` FastLED at the requested ref                        |
| `uv_sync`           | FastLED's `./install` / `uv sync` to materialize the .venv      |
| `compile`           | `./compile --no-interactive --no-parallel <board> <examples>`   |
| `job_total`         | end-to-end wall-clock from job start to end                     |

`actions/setup-python` and `pip install fbuild` are inside the composite `setup` action so their wall-clock shows up in the Actions UI under that step rather than in the TSV.

## Interpreting results

The short-term target is to find the largest single phase and shrink it. Expected shape on `ubuntu-latest`:

- Cold AVR (`uno`): toolchain download dominates inside `compile`.
- Warm AVR (`uno`): `uv_sync` + checkout overheads dominate; actual compile should be cache-hit-heavy.
- Cold ESP32: toolchain + pioarduino bootstrap dominates.
- Warm ESP32: compile still non-trivial due to large object set; daemon and toolchain should be free.

Update the tracking issue with each run's numbers so progress is visible.
