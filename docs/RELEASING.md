# Releasing fbuild

fbuild is published to PyPI by the **Autonomous Release** GitHub Action (`.github/workflows/release-auto.yml`). There is **no local publish script** — the legacy `./publish` shell wrapper and the local-only entry point in `ci/publish.py` were removed once trusted publishing was wired up. `ci/publish.py` now exists only as a library the action imports to assemble per-platform wheels.

## Quick recipe

```bash
# 1. Decide the next version (semver patch / minor / major).
# 2. Bump it in BOTH files; the workflow refuses to release if they differ.
#    - Cargo.toml         [workspace.package].version
#    - pyproject.toml     [project].version
# 3. Commit and push to main. This runs ordinary CI only.
git commit -am "chore: bump version to X.Y.Z"
git push origin main
# 4. After reviewing the exact candidate commit, dispatch a full dry run.
candidate_sha=$(git rev-parse HEAD)
gh workflow run release-auto.yml --repo FastLED/fbuild --ref main \
  -f candidate_sha="$candidate_sha" -f publish=false
# 5. After the dry run succeeds, publish that same candidate.
gh workflow run release-auto.yml --repo FastLED/fbuild --ref main \
  -f candidate_sha="$candidate_sha" -f publish=true
```

The dispatch requires a 40-character commit SHA equal to the head commit of the dispatch ref (`--ref main` in the example). This keeps the loaded workflow definitions and generated board matrix on the same revision as the code under test. If `main` has advanced, dispatch from a ref at the candidate commit. `publish=false` runs the native builds and the software jobs wired into `ci-full`, including fmt, docs, MSRV, board validation, and crate gates. Publication uses those same-SHA software checks plus release-binary smoke tests; physical hardware is not a release prerequisite.

## What the action actually does

```
release-auto.yml
├── prepare              ── compute candidate version, check tag + PyPI state
├── ci-full              ── validate exact candidate SHA and every board/platform job
├── build (matrix)       ── build native binaries for 6 targets
├── build-pypi           ── call `ci/publish.py::build_all_wheels` → 4 wheels
├── smoke test           ── pip-install one wheel, run `fbuild --version`
├── publish              ── create GitHub release + push v<version> tag
└── publish-pypi         ── upload wheels via trusted publishing (OIDC)
```

The `prepare` job runs only on an explicit `workflow_dispatch`. It verifies that checkout HEAD matches `candidate_sha`, refuses an existing version tag pointing to another commit, and decides which publication jobs are needed. A version-file push cannot start this workflow.

```
should_build = true  for an explicit candidate dispatch
should_publish_github = true  IF publish=true AND tag does not exist
should_publish_pypi = true    IF publish=true AND PyPI has fewer than expected wheels
```

The file-count guard exists because a complete `fbuild` release means one wheel per `PLATFORMS` entry in `ci/publish.py` (currently 4: Linux x86_64, Linux aarch64, macOS aarch64, Windows x86_64). Anything less is a partial / stranded release; an explicit retry on the same candidate SHA can rebuild and upload the missing wheels after full CI and release-binary smoke tests pass. Both this gate and the post-upload "Verify all wheels visible on PyPI" check derive their expected counts at run time (from `ci/publish.py::PLATFORMS` and the built wheels respectively) — a stale hardcoded 4 broke the verify gate during the 2.3.22-2.3.24 window. Note the build matrix has more lanes than wheels: the x86_64-apple-darwin and aarch64-pc-windows-msvc binaries ship via the GitHub release archives only (Intel Macs install the arm64 wheel via its dual macosx tag; ARM Windows uses the win_amd64 wheel via emulation).

## Common failure modes

### "I pushed the version bump but nothing happened"

That is expected: the version bump runs ordinary CI. Dispatch `release-auto.yml` with the exact candidate SHA when ready. Do not create the version tag manually.

### Cargo.toml and pyproject.toml versions disagree

The `prepare` job aborts with a non-zero exit if `[workspace.package].version` (Cargo.toml) ≠ `[project].version` (pyproject.toml). Fix both files in the same commit.

### A wheel built but never uploaded (partial release)

After full validation passes, re-run the same candidate SHA with `publish=true`. `prepare` verifies that the existing tag points to that SHA and rebuilds the missing wheels:

```bash
gh workflow run release-auto.yml --repo FastLED/fbuild --ref main \
  -f candidate_sha=<original-40-character-commit-sha> -f publish=true
```

The action will rebuild the missing wheels and upload, leaving the existing wheels in place. PyPI does not let you re-upload the same filename, so the new run produces fresh sdists/wheels only for the missing platforms.

### `prepare` says "no, already shipped"

If `pypi_file_count` already equals `len(PLATFORMS)` (currently 4), the release is complete. Bump to the next version.

## Library reference: `ci/publish.py`

The module is consumed by `release-auto.yml`'s `build-pypi` step. Public surface (anything else is implementation detail):

| Symbol | Used for |
|---|---|
| `DIST_DIR`, `WHEEL_DIR`, `PYTHON_SHIMS_DIR` | Layout constants |
| `ARTIFACT_MAP` | GH artifact name → `dist/<platform>` subdir |
| `PLATFORMS` | `dist/<platform>` subdir → wheel platform tags |
| `EXTENSION_NAMES` | Recognized PyO3 extension filenames |
| `read_project_meta()` | Parse `pyproject.toml` |
| `build_wheel(...)` | Assemble one platform wheel |
| `build_all_wheels(...)` | Assemble every configured platform; fail fast on missing |
| `log(msg)`, `record_hash(bytes)` | Internal helpers, also re-exported |

The module intentionally has no CLI entry point, no `argparse`, no upload code, and no PyPI auth logic. PyPI authentication is handled by GitHub's OIDC trusted publishing — there are no secrets to manage.

## Adding a new platform

1. Add the target to the `build` matrix in `release-auto.yml`.
2. Add the artifact-name → subdir entry in `ARTIFACT_MAP` (`ci/publish.py`).
3. Add the subdir → platform-tag entry in `PLATFORMS` (`ci/publish.py`).

The wheel-count gates in `prepare` and `publish-pypi` derive their expected counts from `ci/publish.py::PLATFORMS` and the built wheels, so no count needs bumping. Skip steps 2-3 if the new target should ship via the GitHub release archives only (no wheel) — every `binaries-*` artifact is packaged into the GitHub release regardless of `ARTIFACT_MAP`.
