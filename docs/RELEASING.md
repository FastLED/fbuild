# Releasing fbuild

fbuild is published to PyPI by the **Autonomous Release** GitHub Action (`.github/workflows/release-auto.yml`). There is **no local publish script** — the legacy `./publish` shell wrapper and the local-only entry point in `ci/publish.py` were removed once trusted publishing was wired up. `ci/publish.py` now exists only as a library the action imports to assemble per-platform wheels.

## Quick recipe

```bash
# 1. Decide the next version (semver patch / minor / major).
# 2. Bump it in BOTH files; the workflow refuses to release if they differ.
#    - Cargo.toml         [workspace.package].version
#    - pyproject.toml     [project].version
# 3. Commit and push to main.
#    DO NOT push a v<version> tag — the action only triggers when the
#    tag is absent, and it creates the tag itself after the upload
#    succeeds.
git commit -am "chore: bump version to X.Y.Z"
git push origin main
```

That's the entire happy path. Wait for the action to complete (~10-15 min for the full matrix + upload); the new wheels appear at `https://pypi.org/project/fbuild/X.Y.Z/`.

## What the action actually does

```
release-auto.yml
├── prepare              ── compute candidate version, check tag + PyPI state
├── build (matrix)       ── build native binaries for 4 targets
├── build-pypi           ── call `ci/publish.py::build_all_wheels` → 4 wheels
├── smoke test           ── pip-install one wheel, run `fbuild --version`
├── publish              ── create GitHub release + push v<version> tag
└── publish-pypi         ── upload wheels via trusted publishing (OIDC)
```

The `prepare` job is the trigger gate. It runs on every push that touches `Cargo.toml` or `pyproject.toml`, and decides whether the rest of the pipeline runs:

```
should_build = true  IF  (tag does not exist)
                     AND (version on disk >= newest known PyPI version)
                     AND (PyPI has < 4 files for this version)
```

The "< 4 files" guard exists because PyPI requires exactly 4 platform wheels (Linux x86_64, Linux aarch64, macOS aarch64, Windows x86_64) for an `fbuild` release. Anything less is a partial / stranded release and the prep job will refuse to "fix it" by uploading more files to the same version.

## Common failure modes

### "I pushed the version bump but nothing happened"

The most common cause is a manually-pushed tag. If `v<version>` already exists on the remote when the `prepare` job runs, `should_build` stays false and every downstream job is skipped — the action conclusion shows `success` because nothing failed, but no wheels were built. Symptom: action completed quickly (<1 min), no `build` jobs ran.

**Fix:** delete the tag and re-run the workflow.

```bash
git push --delete origin v<version>
gh workflow run release-auto.yml --repo FastLED/fbuild --ref main
```

Then `gh run watch` to confirm `should_build=true` this time.

### Cargo.toml and pyproject.toml versions disagree

The `prepare` job aborts with a non-zero exit if `[workspace.package].version` (Cargo.toml) ≠ `[project].version` (pyproject.toml). Fix both files in the same commit.

### A wheel built but never uploaded (partial release)

Re-run via `workflow_dispatch`. The fallback branch in `prepare` allows builds when the tag exists but PyPI has fewer than 4 files — but only on a manual dispatch:

```bash
gh workflow run release-auto.yml --repo FastLED/fbuild --ref main
```

The action will rebuild the missing wheels and upload, leaving the existing wheels in place. PyPI does not let you re-upload the same filename, so the new run produces fresh sdists/wheels only for the missing platforms.

### `prepare` says "no, already shipped"

If `pypi_file_count` is already 4, the release is complete. Bump to the next version.

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
4. Bump the "< 4 files" check in `prepare` to the new count.
