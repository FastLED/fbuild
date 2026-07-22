# GitHub Actions Workflows

CI/CD workflows for the fbuild project, covering lint, test, documentation, and binary builds.

## CI Checks (push/PR)

- **`check-{macos,ubuntu,windows}.yml`** -- Clippy + tests per platform
- **`fmt.yml`** -- Rustfmt check | **`docs.yml`** -- Doc build with `-D warnings`
- **`msrv.yml`** -- MSRV 1.94.1 verification | **`validate-boards.yml`** -- Board JSON validation
- **`loc-gate.yml`** -- Reject `.rs` files over 1000 LOC | **`lint-subprocess.yml`** -- Forbid direct subprocess spawns
- **`crate-gate.yml`** -- Reject new workspace crates (monocrate policy, `ci/check_workspace_crates.py`)

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
