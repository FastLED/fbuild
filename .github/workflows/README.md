# GitHub Actions Workflows

CI/CD workflows for the fbuild project, covering lint, test, documentation, and binary builds.

## CI Checks (push/PR)

- **`check-{ubuntu,windows}.yml`** -- Clippy + tests per platform (no macOS runner: macOS
  binaries are cross-built from Linux in the release matrix, see below)
- **`fmt.yml`** -- Rustfmt check | **`docs.yml`** -- Doc build with `-D warnings`
- **`msrv.yml`** -- MSRV 1.95.0 verification | **`validate-boards.yml`** -- Board JSON validation
- **`platform-boundary-research.yml`** -- Windows/Linux reconciliation of the #1307 research inventory and RED fixture
- **`loc-gate.yml`** -- Reject `.rs` files over 1000 LOC | **`lint-subprocess.yml`** -- Forbid direct subprocess spawns
- **`crate-gate.yml`** -- Reject new workspace crates (monocrate policy, `ci/check_workspace_crates.py`)

## Concurrency (auto-cancel superseded PR runs)

Every `pull_request`-triggered workflow declares:

```yaml
concurrency:
  group: <this-file>.yml-${{ github.event_name == 'pull_request' && github.ref || github.run_id }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
```

Pushing again to a feature branch **cancels** the superseded runs instead of
queueing a second full copy of ~80 board builds behind them. Three details are
load-bearing:

- **Group keyed on the file name, not `github.workflow`.** Several board
  workflows share a display name on purpose (`build-due.yml` and
  `build-sam3x8e_due.yml` are both "Build Arduino Due"); keying on the display
  name would make those siblings cancel each other inside one PR.
- **Non-PR events fall back to `github.run_id`.** GitHub keeps at most **one
  pending run per group**, so a shared group would silently *drop* queued
  pushes to `main` -- the very SHAs that populate the soldr build caches and
  feed the release flow. With `run_id` each non-PR run is its own group and
  nothing on `main` is ever cancelled or dropped.
- **`cancel-in-progress` is PR-only** for the same reason.

Exempt, with the reason recorded in `ci/check_workflow_concurrency.py`:
`template_build.yml` / `template_native_build.yml` (in a reusable workflow the
`github` context is the *caller's*, so all ~80 nightly template invocations
would share one group and cancel each other), `hw-ci.yml` (cancelling mid-flash
can wedge real hardware), and `add-to-project.yml` (fires once on PR open).

Per-board files get the block from `ci/render_workflows.py`; everything else
carries it by hand. `ci-workflow-drift.yml` enforces both halves via
`ci/check_workflow_concurrency.py` -- a new PR-triggered workflow without the
block fails CI with a copy-paste fix.

## Scheduled Benchmarks

- **`benchmark-build-comparison.yml`** -- Arduino CLI vs PlatformIO vs fbuild Blink cold/warm benchmark; runs nightly, manually, and for relevant pushes to `main`, then force-publishes the one-commit `benchmark-stats` branch and deploys its site to GitHub Pages

## Per-Board Builds (push/PR)

- **`build-esp32{c2,c3,c5,c6,dev,h2,p4,s2,s3}.yml`** -- ESP32 variants
- **`build-esp8266.yml`** -- ESP8266
- **`build-uno.yml`**, **`build-leonardo.yml`**, **`build-atmega8a.yml`** -- AVR classic boards
- **`build-attiny{85,88,4313}.yml`** -- ATtiny AVR boards
- **`build-ATtiny{1604,1616}.yml`**, **`build-nano_every.yml`** -- MegaAVR boards
- **`build-uno_r4_wifi.yml`** -- Renesas RA
- **`build-teensy{30,31,32,35,36,40,41,lc}.yml`** -- Teensy variants
- **`build-stm32f{103c8,103cb,103tb,411ce}.yml`**, **`build-stm32h747xi.yml`**, **`build-nucleo_f{429,439}zi.yml`** -- STM32
- **`build-sam3x8e_due.yml`**, **`build-samd{21,21_zero,51j,51p}.yml`** -- SAM/SAMD
- **`build-rp{2040,2350}.yml`** -- RP2040/RP2350
- **`build-nrf52840_dk.yml`** -- Nordic nRF52
- **`build-apollo3_{red,thing_explorable}.yml`** -- Apollo3
- **`build-mgm240.yml`** -- Silicon Labs EFR32
- **`build-ch32v003.yml`** -- CH32V003 RISC-V (48MHz, 2KB RAM, 16KB Flash)
- **`build-ch32v103.yml`** -- CH32V103 RISC-V (72MHz, 20KB RAM, 64KB Flash)
- **`build-ch32v203.yml`** -- CH32V203 RISC-V (144MHz, 20KB RAM, 64KB Flash)
- **`build-ch32v208.yml`** -- CH32V208 RISC-V + BLE 5.3 (144MHz, 64KB RAM, 128KB Flash)
- **`build-ch32v303.yml`** -- CH32V303 RISC-V (144MHz, 64KB RAM, 256KB Flash)
- **`build-ch32v307.yml`** -- CH32V307 RISC-V (144MHz, 64KB RAM, 256KB Flash, ETH+USB HS)
- **`build-ch32x035.yml`** -- CH32X035 RISC-V + USB PD (48MHz, 20KB RAM, 62KB Flash)

## Native Binaries and Templates

- **`build.yml`** -- Manual dispatch: cross-platform native binary builds
- **`release-auto.yml`** -- Version-gated GitHub/PyPI release workflow with attestations
- **`template_build.yml`** -- Reusable workflow for per-board firmware builds
- **`template_native_build.yml`** -- Reusable workflow for native Rust binary builds

### Bumping soldr

All `zackees/setup-soldr@v0` steps pin the installed soldr binary version.
When bumping it, first confirm the proposed tag is a published, non-draft
release with the required platform assets. Update every setup-soldr call site
in one PR, retain the previous pin until that release exists, and require
representative CI to pass before merging. Keep the action reference at `@v0`;
the pin is for the binary it installs.

### Native Build Attestations

Manual `build.yml` native artifacts include `SHA256SUMS.txt` and GitHub Artifact
Attestations for every staged native file:

- `fbuild` / `fbuild.exe`
- `fbuild-daemon` / `fbuild-daemon.exe`
- `_native.abi3.so` / `_native.pyd`

After downloading and extracting a `binaries-${target}` workflow artifact:

```bash
sha256sum -c SHA256SUMS.txt
gh attestation verify fbuild --repo FastLED/fbuild
gh attestation verify fbuild-daemon --repo FastLED/fbuild
gh attestation verify _native.abi3.so --repo FastLED/fbuild
```

For Windows artifacts, verify `fbuild.exe`, `fbuild-daemon.exe`, and
`_native.pyd` instead.

### Autonomous Releases

`release-auto.yml` follows the attested release pattern used by `soldr`:

- reads the workspace/package version from `Cargo.toml` and `pyproject.toml`
- skips the run if the tag already exists or PyPI already has that version
- builds native artifacts through `template_native_build.yml`
- packages GitHub Release archives and creates `SHA256SUMS`
- attests the release archive checksums with GitHub Artifact Attestations
- builds fbuild wheels from the native artifacts
- publishes wheels to PyPI through Trusted Publishing

If GitHub release creation succeeds but PyPI publishing fails, run
`release-auto.yml` manually with `workflow_dispatch`. When the matching tag
already exists but PyPI has fewer than the expected wheel files, the workflow
rebuilds from that tag, skips GitHub release creation, and retries only the PyPI
publish path.

To verify a downloaded GitHub Release artifact:

```bash
gh attestation verify <path-to-release-archive> --repo FastLED/fbuild
```

To inspect the release checksums:

```bash
sha256sum -c fbuild-vX.Y.Z-SHA256SUMS.txt
```

PyPI publishing requires a Trusted Publisher configured on PyPI for:

- project: `fbuild`
- repository: `FastLED/fbuild`
- workflow: `.github/workflows/release-auto.yml`
- environment: `pypi`

The PyPI publish job declares the `pypi` GitHub environment so PyPI receives an
OIDC token with `environment: pypi`. The Trusted Publisher entry on PyPI must
match that environment exactly; otherwise PyPI rejects the exchange with
`invalid-publisher`.
