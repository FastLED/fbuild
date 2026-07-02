# Docker profiling harness (FastLED/fbuild#942)

Reproducible Linux profiling of `fbuild build` against a pinned
[NightDriverStrip](https://github.com/PlummersSoftwareLLC/NightDriverStrip)
checkout (`env:demo`, esp32dev). Produces wall-clock timings, on-CPU and
off-CPU flamegraphs, and `FBUILD_PERF_LOG` phase timelines for cold-,
warm-, and hot-cache builds.

## Usage

```bash
# from repo root — full matrix: cold+warm+hot, 3 iterations each
uv run python ci/docker-profile/run_profile.py

# quick single cold run
uv run python ci/docker-profile/run_profile.py -n 1 --scenarios cold

# housekeeping
uv run python ci/docker-profile/run_profile.py --status   # harness volumes
uv run python ci/docker-profile/run_profile.py --wipe     # drop them (next run rebuilds fbuild from scratch)
```

Artifacts land in `ci/docker-profile/out/<timestamp>/` (gitignored):
`timings.jsonl` + `summary.md` (median wall clock per scenario), and per
run `oncpu.svg` / `offcpu.svg` flamegraphs, `.folded` stacks, raw
`perf.data` files, CLI logs, `daemon.log`, and extracted
`perf-log-lines.txt`.

## Scenarios

| scenario | fbuild cache (`~/.fbuild`) | project dir | measures |
|---|---|---|---|
| `cold` | wiped (+ `~/.platformio`), daemon killed | fresh copy | downloads, installs, full compile |
| `warm` | intact, daemon running | fresh copy | first build of a new project with a hot global cache — uncached work shows up here |
| `hot`  | intact, daemon running | unchanged | no-op rebuild / fast-path overhead |

Scenarios run in the order given per iteration; `warm`/`hot` assume
`cold` ran first in the same sequence (the default `cold,warm,hot` does
this per iteration).

## Caching contract

- **Persisted across runs** (named Docker volumes; only make the
  *harness* fast): `/work/target`, `~/.cargo`, `~/.rustup`, `~/.soldr`.
  Named volumes, not bind mounts, because Windows/WSL2 bind mounts
  rewrite mtimes on every container start and bust cargo fingerprints.
- **Never persisted** (the system under test): `~/.fbuild` and
  `~/.platformio` live in the container filesystem and die with the
  `--rm` container. Cold means genuinely cold — fresh downloads.

fbuild + fbuild-daemon are built inside the container from the
bind-mounted working copy with
`RUSTFLAGS="-C force-frame-pointers=yes -C debuginfo=2"` (clean perf
stacks; firmware compile flags are untouched — see the #942 constraint
that compile settings stay stock).

## Profilers

- **On-CPU**: `perf record -F 99 -e cpu-clock -g --call-graph fp -a`
  (software clock event — WSL2 exposes no hardware PMU), rendered with
  [FlameGraph](https://github.com/brendangregg/FlameGraph).
- **Off-CPU**: `offcpu.bt` (bpftrace, sums blocked µs per stack via
  `sched:sched_switch`) rendered as `offcpu.svg`; plus a concurrent raw
  `perf record -e sched:sched_switch` capture as fallback for WSL2
  kernels where bpftrace stack lookups come back empty.
- **Phase timeline**: `FBUILD_PERF_LOG=1` — summaries are emitted by
  the daemon, so the harness harvests
  `~/.fbuild/prod/daemon/daemon.log` per run.

The container runs `--privileged` (perf_event_open + BPF on Docker
Desktop) and relaxes `kernel.perf_event_paranoid` best-effort.

## Files

- `Dockerfile` — ubuntu:24.04 + soldr (pinned release tarball, same
  rationale as `ci/docker-mac-cross`), perf/bpftrace/FlameGraph, and
  the NightDriverStrip checkout pinned to a fixed sha.
- `profile_entry.sh` — in-container scenario loop + samplers.
- `offcpu.bt` — bpftrace off-CPU program.
- `run_profile.py` — host orchestrator (image build, volumes,
  `docker run`, summary table).
