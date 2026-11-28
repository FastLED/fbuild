# CI and Development Tools

Python scripts for CI, packaging, and development tooling. All invoked via `uv run`.

`gate_script_coverage.py --budget <n>` guards the gate scripts themselves: every
`ci` module that a workflow runs must appear in `gate_script_tests.toml` with the
test that covers it, and a module with no test must be one of the entries in
that registry's `untested` list -- which is empty, and the guard fails if it
grows. That list is a budget, not a shipping lane --
the guard fails when it grows, when a declared test disappears or stops
mentioning its subject, and when an entry is left behind after its test lands.
Run it with `uv run --no-project python -m unittest ci.test_gate_script_coverage`.

`reclaim_android_ndks.py` provisions cold Linux acceptance, glibc-floor, Dylint,
and native release builders by uninstalling only installed Android NDK package IDs
through their SDK manager. These jobs do not build Android targets. It records
workspace free bytes before and after; Rust/fbuild caches and disk guards stay
unchanged. Run its focused checks with
`uv run --no-project python -m unittest ci.test_reclaim_android_ndks`.

`./push` is a thin delegate (forge-ci/ci.yml GATE-014): it execs
`prooft --push` with its arguments, resolved from the one pin in
`prooft_verify.py`. The prooft runner refuses a dirty tree or the base branch,
runs the lanes that declare `run:` in `ci-attestations.yml` and stamps their
gates on HEAD, then pushes with a lease and opens or reuses the PR (`--no-pr`
skips it). A lane already attested on HEAD is not rerun.

Local lanes are host-only, for speed. The `lint` lane always runs first, on
the host (lint never runs under bosn): `soldr lint rust --check --host-only`,
which stamps formatting plus host Clippy and Dylint. `./push --test` adds the
`linux-minimal` test lane, which replays the checks and unit tests from
`check-ubuntu.yml` on the native Linux x64 host through bosn, with build and
test concurrency sized to the host's current load. Cross-target Clippy and
Dylint and the `dylint` lane declare no `run:`, so they stay unattested and
CI runs them. Bare `prooft` (lint only) and `prooft --test` run the same lanes
without pushing.

`prooft_verify.py` reads the policy and tool pin from the trusted base commit.
Default PR runs with valid proof can skip formatting, both jobs in the
`ci-minimal.yml` Linux call, and the Dylint call. Expanded CI and main pushes
retain hosted coverage. A PR changing a workflow or the attestation policy
requires hosted validation. Merge groups ask `prooft queue` to compare the PR
proofs with the queue head's inputs. For a two-parent MERGE entry, fbuild
uses the base-owned PR plan when the entry directly merges the recorded PR
head into the event base and has the same tree as the PR. Other two-parent
entries run hosted checks until prooft supports their topology.

The optional ci-lint verifier still reports proofs in shadow mode using the
pin from `ci.toml`. The local wrappers verify the exact committed source and
the terminal Bosn reports. The migration is tracked in
[FastLED/fbuild#1635](https://github.com/FastLED/fbuild/issues/1635) and
[FastLED/fbuild#1643](https://github.com/FastLED/fbuild/issues/1643).

## Optional local proof and repository copies

Bosn freezes source into a separate Git snapshot so concurrent edits cannot change
what CI tests and workflow Git commands can inspect HEAD and history. Running
`ci.local_gate` therefore creates an additional repository copy. Respect session
constraints against copies/worktrees: skip this optional runner when prohibited.
Do not request a copy exception or stall the work; use hosted CI instead.

Local proof verification is advisory and has no dependency edge to hosted tests,
board planning, or extended/full selections. Missing proof, verifier failure, or
runner unavailability must not block those jobs. Their actual test failures still
fail coverage. This policy is tracked in [#1698](https://github.com/FastLED/fbuild/issues/1698).

## Contents

- **`build_backend.py`** -- The in-tree PEP 517/660 backend that every `pip install .`
  and `uv sync` goes through (`build-backend` in `pyproject.toml`): runs
  `soldr cargo build` for `fbuild-cli`, `fbuild-daemon` and the PyO3 `_native`
  extension, stages them in `ci/bin/` + `python/fbuild/`, and writes a wheel whose
  `.data/scripts/` carry the raw binaries. Reuse of staged artifacts keys on the
  cargo input mtimes, the **profile** recorded in `ci/bin/.build-profile` (so
  `FBUILD_BUILD_RELEASE=1` can never adopt a debug binary, or the reverse) and
  non-emptiness; staging goes through `os.replace`, so an interrupted install cannot
  leave a truncated artifact that later runs adopt as current. Tested by
  `test_build_backend.py`.
  The cargo target dir itself lives in `~/.fbuild/cargo-target/wheel-build/<name>-<sha256>`,
  one per checkout, and the digest is one-way -- so a deleted worktree used to leave an
  unreclaimable 0.06-4 GB target behind forever ([#2062](https://github.com/FastLED/fbuild/issues/2062):
  31.8 GB of orphans on a real dev box). Each target now records its owning checkout in
  `.fbuild-wheel-source`, and a build sweeps sibling targets whose owner is gone (exact
  evidence) or, for unstamped legacy targets, whose newest file is older than
  `FBUILD_WHEEL_BUILD_IDLE_DAYS` (default 14). `FBUILD_WHEEL_BUILD_SWEEP=0` disables the
  sweep. To reclaim from outside a build -- e.g. from a disk-cleanup cron --
  `uv run --no-project python -m ci.build_backend --reclaim-stale-targets [--dry-run]`.
- **`build_dist.py`** -- Triggers GitHub Actions native builds, downloads artifacts, and assembles `dist/` for PyPI packaging
- **`build_qemu_linux_runtime.py`** -- Builds the Linux runtime-library bundle Espressif QEMU needs
  (`ldd` closure minus glibc, built in `ubuntu:20.04` so the bundle's glibc floor stays at 2.30).
  Published to the `qemu-linux-runtime-v1` release and downloaded on demand by `fbuild-toolchain`'s
  `esp_qemu_runtime` module; run in CI by `qemu-runtime-bundle.yml`. The bundle's tag and its five
  required libraries are declared once here and compared against their `esp_qemu*.rs` counterparts
  by `test_build_qemu_linux_runtime.py`, which also runs the bash closure walk against a stubbed
  `ldd`: an unsettled walk, a gap behind a bundled library, or a truncated `tar.zst` now fails the
  build instead of being published. Covers the same seams through a fake `docker` for the Python half.
- **`check_workspace_crates.py`** -- Monocrate guard: fails if the root `Cargo.toml` `[workspace] members` list gains a crate outside the approved allowlist (run by `crate-gate.yml`). Its `APPROVED_MEMBERS` and `ci/hooks/crate_guard.py`'s `APPROVED_CRATE_DIRS` are one policy in two files; `test_crate_guard_parity.py` asserts both against the real `Cargo.toml` -- and drives the hook itself -- so a drifted list fails a test instead of blocking a legitimate `Cargo.toml` edit with a "new crate" error. It also enforces the RUST-018 half of FastLED/fbuild#1639: `[workspace.lints.clippy]` must keep `cognitive_complexity`/`too_many_lines` at `warn` or stricter, and every member must declare `[lints] workspace = true` -- a member that drops that one line silently escapes both ratchets, so the guard names it.
- **`check_workflow_concurrency.py`** -- Requires every `pull_request`-triggered workflow to declare an auto-cancel `concurrency:` block, so pushing again to a feature branch supersedes its in-flight runs instead of queueing ~80 more board builds, and requires a `schedule:`-triggered workflow to keep its group out of the push/PR slot: GitHub keeps only one *pending* run per group, so a queued nightly is cancelled by the next merge (`cancel-in-progress: false` does not help). Run by `ci-workflow-drift.yml`; exemptions (reusable templates, `hw-ci.yml`, `add-to-project.yml`) carry a reason in the script. Tested by `test_workflow_concurrency.py`. See [.github/workflows/README.md](../.github/workflows/README.md#concurrency-auto-cancel-superseded-pr-runs).
- **`publish_bench_site.py`** -- The benchmark site publisher: assembles the last
  published `benchmark-stats` tree plus this run's inputs and pushes it under
  `--force-with-lease`, retrying a bounded number of times. Two publishers (the nightly
  owns its own `concurrency:` slot since FastLED/fbuild#2005, and a push run can overlap it)
  used to close the race with `git push --force`, which cannot fail: a rival that landed
  between the fetch and the push was silently reverted, and `history.jsonl` is append-only,
  so that row was a permanent hole (FastLED/fbuild#2006). Every attempt now re-reads the
  branch first, so the union below sees what is published *now*; a lease rejection is
  simply another attempt, and exhausting the budget fails the step loudly. Tested by
  `test_publish_bench_site.py` against real git repositories (bare remote, two publish
  dirs), where the interleaved case is red under the old force push.
- **`merge_bench_history.py`** -- Unions the published `history.jsonl` with the run's fresh rows inside the benchmark publisher. The nightly can overlap a push run, and each run's history was snapshotted when its measurement *started*, so an older snapshot landing last would revert the newer run's row. Identity is `(ts, sha)`, output stays chronological with the newest line last and trimmed to the same 365-line bound as `write_history`, and an unparseable line is passed through rather than deleted. Tested by `test_merge_bench_history.py`.
- **`artifact_serving_bench.py`** -- The artifact-serving half of decision 6 in
  `docs/design/artifact-serving-decisions.md` (the last cell of FastLED/fbuild#1882,
  unblocked by the #2196 consumer): `--measure` runs the reference cell twice from a
  cold `FBUILD_CACHE_DIR` per trial -- the served arm with the hidden `--prebuilt-core`
  opt-in, the control arm with `--no-prebuilt-libs` -- and writes `<YYYY-MM-DD>.json`
  plus a recency-gated `latest.json`, so `cold_first_compile_wall_s` and its control arm
  are one document. It reads what the build itself recorded
  (`prebuilt_libs` in `build_info_<env>.json`, #2196 cell 4) and REFUSES to report a
  served arm that fell back to source: a ratio from two source builds reads as a 1.00x
  speedup and measures nothing, which decision 6 makes a hard failure. `--check`
  applies the 10 % regression gate to the newest run against `latest.json` and exits 3
  under `--fail`. Tested by `test_artifact_serving_bench.py` against a real `fbuild`
  shim. The nightly step that invokes it, plus `bytes_downloaded` and
  `fallback_rung_histogram` (the ladder exposes no rung numbers yet), remain open.
- **`cancel_superseded.py`** -- The merge-queue janitor: on every `merge_group` run it
  cancels the in-flight queue runs for the same PR that a newer queue run replaced, so
  they stop holding runners when `main` moves. Runs are ordered by `(created_at, id)` --
  parsed as datetimes, because GitHub's fractional seconds (`.500Z` sorts before `Z` as a
  string) used to make a NEWER run look older and get cancelled, and a same-second tie
  matched neither run. A triggering ref that is not a `gh-readonly-queue/.../pr-<N>-<sha>`
  ref is a hard error rather than a `PR #None: cancelling 0` success. Tested by
  `test_cancel_superseded.py`.
- **`check_dylint_allowlists.py`** -- Fails when a `dylints/*/src/allowlist.txt`
  entry cannot excuse anything: the path is not a file in this repo, the line
  carries an inline `#` comment (every lint matches the WHOLE trimmed line, so
  `crates/x.rs  # why` never applies -- the reason goes on its own comment
  line), the entry holds a backslash (source paths are `/`-normalized first) or
  is absolute / contains `..`, or the allowlist is an orphan whose `lib.rs`
  never `include_str!`s it. It also fails when it finds no allowlists at all,
  so a renamed directory cannot print "paths are current". Tested by
  `test_check_dylint_allowlists.py`.
- **`check_rust_toolchain_pins.py`** -- Prevents fbuild-owned Rust 1.95.0 MSRV, toolchain, workflow, and bootstrap declarations from drifting
- **`enforce_platform_boundary.py`** -- Independent whole-tree and manifest checker for the exact host-platform occurrence ledger. It has no regeneration mode: the ledger and the Dylint baseline are reviewed artifacts, so a delta must be explained occurrence by occurrence.
- **`env.py`** -- Centralized PATH activation ensuring the Rust tool bin directory is on PATH before invoking Rust tools
- **`extract_pio_build_flags.py`** -- Extracts compiler/linker flags from PlatformIO for each board and writes reference JSONs
- **`lint.py`** -- Workspace linting (rustfmt + clippy), supports single-file and auto-fix modes
- **`platform_boundary_research.py`** -- Host-independent phase-1 inventory and cross-host drift check for FastLED/fbuild#1307
- **`render_workflows.py`** -- Re-renders the `on:` and `concurrency:` blocks of `.github/workflows/build-*.yml` and the full `nightly-platforms.yml` from `board_families.json` + `ci_common_paths.txt`. CI invokes `--check` to enforce no drift. See [docs/DEVELOPMENT.md](../docs/DEVELOPMENT.md#ci-per-board-build-triggers) and FastLED/fbuild#835.
- **`select_boards.py`** -- Picks which `build-<board>.yml` workflows `nightly-platforms.yml` dispatches: path-selected from a push diff, or `--all` for the nightly badge refresh. Tested by `test_select_boards.py`.
- **`test_setup_soldr_cache_keys.py`** -- Every saving `zackees/setup-soldr` step must set a `cache-key-suffix` unique per runner; jobs sharing a key race to own one immutable cache entry.
- **`board_families.json`** -- SOT: per-board metadata (workflow / test_dir / env_name / family) plus the family → crate-path mapping consumed by `render_workflows.py`. A `"matrix_only": true` board has no `build-*.yml`; it runs only from the CI tier matrices.
- **`ci_common_paths.txt`** -- SOT: paths whose changes force-run *every* per-board build workflow.
- **`test.py`** -- Workspace test runner with `--full` (stress + integration) and per-crate filtering
- **`trampoline.py`** -- Development helpers that run fbuild workspace binaries through soldr-managed Cargo
- **`validate_boards.py`** -- Validates fbuild board JSON assets against PlatformIO board definitions.
  Fails when no board could be compared, and on any board whose platform is installed but ships no upstream
  `<id>.json` unless it is declared in `validate_boards_baseline.json` (the 61 known-unmatched ids, pinned to the
  platform versions in `validate-boards.yml`; delete entries as the pins grow)
- **`zccache_setup.py`** -- Optional local wrapper-mode setup for zccache; not used by the standard soldr build path

## Subdirectories

- **`dev-tools/`** -- Pip-installable package that provides soldr and repo-local development helper scripts
- **`hooks/`** -- Claude Code hook scripts (tool guard, lint, readme guard, session lifecycle)
- **`mirror/`** -- The PlatformIO-registry-to-bucket mirror tool (`mirror.py`, `rotate.py` for full passes, `update.py` for incremental ones) whose state DB `gen_mirror_manifest.py` reads (FastLED/fbuild#2040). Tested by `test_mirror_tool.py`.

- **`test_cache_status.py`** streams the unchanged Cargo test selection with JSON compiler evidence; only a completed assertion failure enables setup-soldr failure saves. Compiler errors, incomplete logs and cancellation remain blocked. It also names a shared-soldr-daemon kill as infra (FastLED/fbuild#2178) and, when `FBUILD_CI_DAEMON_DEATH_RETRY=1`, re-runs the same command **once** if the daemon is the only culprit and the spent wall clock still fits `FBUILD_CI_DAEMON_RETRY_BUDGET_SECONDS` (default 600). A failed test or a rustc diagnostic is never retried. Every `soldr cargo test` in `check-ubuntu.yml` -- including the targeted `--ignored` policy steps, which is where two #2197 merge-queue deaths landed -- runs through it, so the retry cannot be armed for a step that does not consume it.

- **`find_direct_subprocess.py`** -- Linter for direct `Command::new` spawns outside the `fbuild-core::subprocess` wrappers (FastLED/fbuild#141), run with `--fail` by `check-ubuntu.yml` and `lint-subprocess.yml`. An `allow-direct-spawn` marker now needs a reason after the colon -- a bare marker no longer allowlists the site -- and mentions inside `//`, nested `/* ... */` comments or string/raw-string literals are not spawns. `test_find_direct_subprocess.py` drives both directions plus the live workspace scan.

- **`audit_ignored_tests.py`** -- Weekly inventory of every `#[ignore]` attribute, published as the body of tracking issue #880 (FastLED/fbuild#839). Detection runs on a blanked copy of the source, so a mention inside a `//` or nested `/* ... */` comment, a string/raw-string fixture or a `'"'` char literal is not an ignored test, while `#[ignore = "reason"]` (normal or raw string) and bare `#[ignore]` both are. `test_audit_ignored_tests.py` covers each of those faces plus the live workspace, and cross-checks every reported line against the file it names.

- **`check_python_complexity.py`** -- Runs configured C901/RUF100 checks on every tracked Python file, including scripts excluded from normal discovery (FastLED/fbuild#1639). `test_check_python_complexity.py` drives the same seams, so a raised `max-complexity`, a dropped rule, or a file-selection regression fails a test instead of silently loosening the ratchet.
- **`check_python_types.py`** -- Runs the pinned basedpyright over every tracked Python file in a uv environment that also holds the scripts' third-party imports, passing that interpreter as `--pythonpath` so they resolve. Policy lives in `[tool.basedpyright]` in `pyproject.toml`. `test_check_python_types.py` drives the same command against synthetic modules, so a loosened mode, a disabled rule, a stale ignore or an unresolvable dependency fails a test.

ESP libs slices: `uv run --no-project ci/slice_esp_libs.py SOURCE --version 3.3.5
--out-dir OUTPUT` reads the xz once and spools payloads to temporary disk to sort
entries. Each checksum-enabled zstd-19 slice includes its chip and all shared
content under the upstream root. `--check` recomputes and verifies without
writing OUTPUT. `uncompressed_bytes` counts file payload bytes; `entries`
counts tar members. The stdout JSON includes `manifest_slices` for pasting into
an existing libs asset's optional `slices` map. Upload keys must match
`--slice-prefix` (default `slices/`), relative to the manifest index directory.
The slicer never writes `site/mirror`. `publish_esp_slices.py` owns the
`@esp-libs` shard: it upserts one version's component (whole bundle for older
clients plus `slices`) and its Index descriptor.

`gen_mirror_manifest.py` regenerates only the registry owners and `@names-*`.
Every other `@` Index entry (`@esp-libs`, `@esp-toolchain`) is another tool's:
its descriptor and shard are kept byte for byte, after the generated entries in
committed order, and a listed shard that is missing or no longer hashes to its
descriptor stops the run before anything is written (FastLED/fbuild#2040). The
former `--prebuilt FILE` flag is gone; it rewrote `@esp-libs` wholesale with the
registry shards' version.

Toolchain slices (#1944): `uv run --no-project ci/slice_toolchain.py
<extracted-toolchain-dir> --out-dir OUTPUT --chips esp32s3 --verify` writes one
deterministic `tar.zst` per chip. Selection is by path component -- a member is
chip-specific when a component of its path names a chip (`lib/esp32s3`,
`lib/gcc/<triple>/<version>/esp32s3`) -- so `bin`, `libexec`, `include` and the
top-level multilib archives stay in every slice. It fails closed on a requested
chip with no directory, a tree with no per-chip directories, an escaping
symlink, and a symlink whose in-tree target the slice drops; an in-tree `..`
link (`lib/lto_plugin -> ../../libexec/...`) is kept. The install marker is
never archived. `--verify` extracts each slice and runs that chip's compiler
from it: `--version`, a `-print-libgcc-file-name` that must resolve *inside*
the extraction, and a real `-std=gnu++17 -Os -c`. `--check` recomputes without
writing. Measured on the cached `xtensa-esp-elf` 13.2.0 tree: 660 MB extracted
-> a 75.3 MB esp32s3 slice (vendor download is 168 MB) in 4m50s including
verification. Uploading is `publish_toolchain_slices.py`, which owns the
`@esp-toolchain` shard: `--metadata <tools.json> --toolchain
toolchain-xtensa-esp-elf --host linux-amd64` picks the version entry the
platform recommends (or `--version-name esp-<v>`), slices `--tree`, uploads each
slice under `v1/espressif/<toolchain>/release/<version>/<host>/<chip>/`, and
writes the shard component plus its Index descriptor. One invocation publishes
ONE host package, because a slice carries that host's compilers; chips are
additive, so publishing `esp32s3` first is a strict improvement for ESP32-S3
and a no-op elsewhere (a chip with no slice keeps the whole-archive fallback).
Integrity pins come from the vendor metadata, never from a local archive, so a
mismatched tree cannot relabel the asset. `--archive` is required: the whole
bundle is verified against those pins, mirrored to the bucket, and listed FIRST
with the vendor URL as the fallback -- every entry in the committed manifest sits
behind the mirror (`committed_manifest_is_accepted_and_prefers_the_bucket`
enforces it), so a slice-less chip never depends on the vendor being up. Files
are written only after every upload passed a HEAD size check, so an entry never
points at a missing object. `--check --shard ... --index ...` needs no network and fails
when the Index descriptor no longer hashes the committed shard -- the Index is a
generated document; `gen_mirror_manifest.py` keeps this entry on a
regeneration (FastLED/fbuild#2040), and `--check` still catches a hand edit or
another writer that drops it. Published linux-amd64 slices of 14.2.0_20260121 (1.1 GB extracted,
verified in 6m24s + 11m for the other two): `esp32s3` 106,462,349 B, `esp32s2`
106,655,161 B, `esp32` 141,361,956 B, against the vendor archive's 176,559,292 B.

Offline slice tests: `uv run --no-project --with zstandard python -m unittest
ci.test_slice_esp_libs -v`. Archives are retained under the framework's
`tools/.libs-archives/` in
vendor-identity / package@version+chip directories. An already complete MCU
install still skips fetching; a cached whole archive serves any MCU. Slice
cache hits verify their size and SHA-256; integrity errors never trigger a
whole-bundle retry. Artifacts and manifest publication remain separate steps.

Sliced packages: SDK libs (`@esp-libs`, keyed `esp32s3`...) and the unified
Xtensa toolchain `toolchain-xtensa-esp-elf` (`@esp-toolchain`). The RISC-V
toolchain is never sliced -- its multilibs are ISA variants, not chips. A client
that knows its chip (`Esp32Toolchain::with_slice_variant`) installs into a
chip-scoped cache directory `<version>+<chip>`, and prefers the shared
whole-archive directory when one is already installed, so a host that predates
slicing never re-downloads. A slice the manifest lists is authoritative: a
corrupt one fails the install rather than silently fetching the whole archive,
and one chip's slice never satisfies a different chip. The whole archive remains
the fallback when no slice is listed for the chip.
