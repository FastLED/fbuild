# Build-path robustness refactor

## Problem

FastLED reported a build artifact path like:

```
.build/pio/teensy40/.fbuild/build/teensy40/release/firmware.hex
```

Two visible issues:

1. **`teensy40` is duplicated** — once as FastLED's per-board project sandbox (`.build/pio/teensy40/`) and once as fbuild's env-name layer (`.fbuild/build/teensy40/`).
2. **Nesting is gratuitously deep** — six directory levels (`.build/pio/<board>/.fbuild/build/<env>/<profile>/`) plus compile sub-trees (`core/`, `src/`, `libs/`) below it. On Windows this slams into the 260-char `MAX_PATH` limit, and other crates already work around it (`fbuild-build/src/zccache.rs:297`, `fbuild-packages/src/library/esp32_framework/libs.rs:136`).

## Investigation findings

### Where the layout comes from

`fbuild-paths::get_project_build_root(project_dir)` returns `<project_dir>/.fbuild/build/` (`crates/fbuild-paths/src/lib.rs:76`). `fbuild-packages::Cache::get_build_dir(env, profile)` appends `<env>/<profile>` (`crates/fbuild-packages/src/cache.rs:112`). So the canonical layout the orchestrators write to is:

```
<project_dir>/.fbuild/build/<env>/<profile>/{firmware.hex,firmware.elf,...}
                                         core/  src/  libs/  ...
```

`compile_many.rs:94` also hard-codes the same `<sketch>/.fbuild/build/<env>/<profile>` shape, *bypassing* `get_project_build_root` — that's a latent bug if `FBUILD_BUILD_DIR` is set.

### Dead parameter: `BuildParams.build_dir`

`crates/fbuild-build/src/lib.rs:151` declares `pub build_dir: PathBuf` on `BuildParams`. The daemon's HTTP handlers (`crates/fbuild-daemon/src/handlers/operations/build.rs:137`, `deploy.rs:197`, `emulator/select.rs:255`) carefully compute it via `get_project_build_root(&project_dir)` and pass it through.

**No orchestrator or pipeline step ever reads it.** Grep confirms zero `params.build_dir` references in `crates/fbuild-build/src/**`. The pipeline re-derives the build dir via `Cache::new(project_dir).get_build_dir(env, profile)` (`pipeline/context.rs:120`). The daemon's plumbing is a no-op.

This means today there is no path through which the daemon caller can actually override the build dir — only the `FBUILD_BUILD_DIR` env var works, and only because the env-var override lives inside `get_project_build_root` itself.

### Why FastLED ended up with `<board>/.fbuild/build/<board>/`

FastLED stages each board's PlatformIO project under `<repo>/.build/pio/<board>/` (`ci/compiler/pio.py:75`) and asks fbuild to build that directory. fbuild then automatically appends `.fbuild/build/<env>/<profile>/`, and on FastLED's matrix `env == board`. So the board name appears twice.

FastLED can't easily flatten this — its analysis tools (`ci/util/fbuild_compiledb.py:44`) already enumerate three candidate layouts (`<build_root>/pio/<board>/.fbuild/build/<env>/release`, `<project>/.fbuild/build/<env>/release`, `<build_root>/.fbuild/build/<env>/release`) and pick whichever exists. They've been compensating for path drift for a while.

### Why this matters beyond cosmetics

- **Windows `MAX_PATH`.** `<sketch>/.fbuild/build/<env>/<profile>/core/<lib>/<file>.cpp.o` already pushes close to 260 chars on long sketch names. esp32_framework explicitly extracts to a short temp dir to dodge this (`esp32_framework/libs.rs:136`).
- **Symbol-analysis / build-info consumers** look up paths by both an env-specific and a generic name (`build_info.rs:136`, FastLED `_find_build_info`). They're path-coupled; every layout change is a contract change.
- **The daemon API claims you can pass a build dir, but you can't.** That's a foot-gun for anyone integrating fbuild via HTTP.

## Goals

1. **One source of truth.** Build-dir resolution lives in `fbuild-paths`; everyone else calls into it. No hard-coded `.fbuild/build/<env>/<profile>` strings.
2. **`BuildParams.build_dir` is either load-bearing or removed.** No silently-ignored fields.
3. **Allow flattening the env-name layer** when the caller has already named the project after the env (the FastLED case), without breaking standalone callers who *do* want `<env>` separation.
4. **Make path length predictable and short.** Layout choice should be deterministic from inputs the caller can see.

## Proposed design

### A. Make `BuildParams.build_dir` load-bearing

Define the field as the **environment-rooted build directory** (i.e. the dir that contains `firmware.hex`, `core/`, `src/`, `libs/`). The daemon already computes the right value at the boundary; pipeline just needs to use it.

- `pipeline/context.rs:120`: replace `cache.get_build_dir(env_name, params.profile)` with `params.build_dir.clone()`.
- `fbuild-packages::Cache::get_build_dir` becomes a thin helper that the *daemon* uses to compute the default; the orchestrator side stops calling it.
- `compile_many.rs::project_build_dir` becomes a thin wrapper around `fbuild-paths`, not a string-literal duplicate.

### B. Single resolver in `fbuild-paths`

Add a single public function:

```rust
pub struct BuildLayout {
    pub project_dir: PathBuf,
    pub env_name: String,
    pub profile: BuildProfile,
    pub override_root: Option<PathBuf>,  // FBUILD_BUILD_DIR or HTTP override
    pub flatten_env: bool,               // skip the <env>/ layer when project basename already matches
}

impl BuildLayout {
    pub fn resolve(&self) -> PathBuf { ... }
}
```

Resolution precedence:

1. If `override_root` is set → use that, joined with `<env>/<profile>` unless `flatten_env`.
2. Else if `FBUILD_BUILD_DIR` env is set → same.
3. Else `<project_dir>/.fbuild/build/<env>/<profile>` (unchanged default).

`flatten_env = true` collapses to `<root>/<profile>/` — gives FastLED a way to dodge the `.build/pio/teensy40/.fbuild/build/teensy40/` duplication.

### C. Auto-detect duplication

When `project_dir.file_name() == env_name`, the resolver can log once and silently flatten (or emit a `tracing::warn!` recommending callers set `flatten_env`). This catches the FastLED case automatically without an opt-in.

### D. Stop hard-coding `.fbuild/build` in tests and docs

`crates/fbuild-build/tests/{avr,teensy,esp32,eh_frame_strip_esp32}_build.rs` constructs build paths as `project_dir.join(".fbuild/build/<env>/release")`. Migrate these to call the new resolver so a layout change updates tests for free.

### E. Update `find_firmware` to use the same resolver

`fbuild-paths::find_firmware` currently re-derives the directory via `get_project_build_root().join(env_name)` (`lib.rs:175`). Switch it to `BuildLayout::resolve()` so firmware discovery follows the same rules as firmware *production*.

### F. Document the contract

Update `crates/fbuild-paths/README.md` and `docs/architecture/overview.md` with the resolved precedence list and the duplication-collapse rule. Mention `flatten_env` as the public knob for embedders like FastLED.

## Step-by-step plan

- [ ] **Audit** (1 hr) — confirm `params.build_dir` is unused by orchestrators (already done: zero refs in `fbuild-build/src/**`). Catalogue every hard-coded `.fbuild/build/` literal across the workspace.
- [ ] **Introduce `BuildLayout`** in `fbuild-paths` with the resolver above, plus unit tests for: default layout, env-var override, `flatten_env=true`, duplication auto-detect.
- [ ] **Wire `BuildLayout` through `BuildParams`.** Either replace `build_dir: PathBuf` with `layout: BuildLayout`, or keep `build_dir` and demand the daemon sets it from `BuildLayout::resolve()`. Prefer the former — fewer chances for the field to drift.
- [ ] **Update `pipeline/context.rs:120`** to read from params instead of re-deriving via `Cache`.
- [ ] **Update `Cache::get_build_dir`** to accept a pre-resolved `BuildLayout` (or delete it — callers can compute directly).
- [ ] **Update `compile_many.rs::project_build_dir`** to call `BuildLayout::resolve`. Keep the same return type.
- [ ] **Update `find_firmware` / `find_firmware_dir`** to use `BuildLayout`. Preserve the legacy `.pio/build/<env>` tail fallback.
- [ ] **Update daemon HTTP handlers** (`build.rs:137`, `deploy.rs:197`, `emulator/select.rs:255`, `emulator/tests_process.rs:115`) to build a `BuildLayout` from request fields + env vars.
- [ ] **Add request-side knob.** Expose `flatten_env` (or equivalent) in the build/deploy/test-emu request models so FastLED can opt in over HTTP without setting a global env var.
- [ ] **Migrate tests** to the resolver; remove hard-coded `.fbuild/build/<env>/release` strings.
- [ ] **Add a regression test** for the FastLED-shaped invocation: project_dir = `<tmp>/.build/pio/teensy40`, env = `teensy40`. Assert the resolver collapses the duplicate.
- [ ] **Update docs** (`fbuild-paths/README.md`, `docs/architecture/overview.md`, `docs/CI_CACHE.md`).
- [ ] **Coordinate with FastLED.** `ci/util/fbuild_compiledb.py:44` currently enumerates three candidate layouts. Once flattening is on by default for the duplication case, that list collapses to two; update FastLED in lockstep (see `clud-extern-repos`).

## Out of scope

- Switching away from `.fbuild/` as the dotdir name (cross-cutting; would invalidate every consumer at once).
- Cache layout (`~/.fbuild/{dev|prod}/cache/`) — unaffected.
- Daemon dev/prod mode isolation — unaffected.

## Risks

- **Hidden assumption that `<env>/` exists.** Some consumer (FastLED's `_create_board_info`, our own `find_firmware`) may rely on the `<env>` directory layer. The auto-flatten case must keep firmware discoverable; `find_firmware` migration covers our side, but FastLED needs a heads-up.
- **`build_info_<env>.json` already lands at `project_dir/`**, not inside the build dir, so it's *not* affected — but worth double-checking when wiring `flatten_env` through.
- **Symbol-analysis path** (`symbol_analysis_path`) is an absolute caller-resolved path (`build.rs:142`); independent of the build-dir refactor.

## Acceptance

- `params.build_dir` (or `params.layout`) is the only source the orchestrators read.
- Re-running the FastLED teensy40 case produces a single `teensy40` segment in the path, not two.
- All workspace tests still pass; the path-length workaround in `esp32_framework/libs.rs:136` either becomes redundant or is documented as defense-in-depth.
