# `ci/docker-linux-verify/` — Local Linux check-ubuntu lane reproducer

Reproduces `.github/workflows/check-ubuntu.yml` (cargo check + clippy +
subprocess lint + cargo test) inside a Docker container so a Windows
host can prove the Linux lane is green before pushing to a GHA-bound
branch. Useful for fast iteration on async / cross-platform changes
where the only difference between platforms is what cfg-gated code
gets exercised.

## Why reuse `fbuild-mac-cross`

The image at `ci/docker-mac-cross/Dockerfile` already ships soldr +
zccache + uv + the Rust toolchain bootstrapper on a vanilla
`ubuntu:24.04` base — the exact same OS image
`ubuntu-latest` resolves to on GHA today. The mac-cross image's *job*
is cross-compiling for Apple, but its *bill of materials* is "what a
GHA Linux runner has on day one." That's exactly what
check-ubuntu.yml needs, so reuse it instead of building a second
image.

## Usage

```bash
# Full verify (first run: 5-8 min cold; subsequent: seconds-to-minutes)
uv run python ci/docker-linux-verify/verify.py

# Interactive shell (debugging)
uv run python ci/docker-linux-verify/verify.py --shell

# Force a cold rebuild (delete cargo target/ and CARGO_HOME volumes)
uv run python ci/docker-linux-verify/verify.py --wipe

# Force a rebuild of the docker image itself
uv run python ci/docker-linux-verify/verify.py --rebuild-image
```

## Volume conventions

Two named Docker volumes back the cargo state across runs:

| Volume                         | Mount      | Purpose                                |
|--------------------------------|------------|----------------------------------------|
| `fbuild-linux-verify-target`   | `/target`  | `CARGO_TARGET_DIR` — incremental cache |
| `fbuild-linux-verify-cargo-home` | `/cargo-home` | `CARGO_HOME` — registry + crate sources |

These are **named volumes, not host bind-mounts**. On Windows hosts,
WSL2's 9P translation rewrites mtimes per container start, which
defeats cargo's incremental fingerprint check (measured 4-6 min per
no-op rebuild). Named volumes live on Linux-native ext4 inside
Docker's VFS, so the same no-op rebuild is single-digit seconds.

The repo itself is bind-mounted at `/src` because we want source
edits to be visible immediately without copying.

## What gets verified

`verify.sh` runs exactly the four steps from `check-ubuntu.yml`:

1. `soldr cargo check --workspace --all-targets`
2. `soldr cargo clippy --workspace --all-targets -- -D warnings`
3. `uv run python ci/find_direct_subprocess.py --fail`
4. `soldr cargo test --workspace`

If all four pass, the GHA "Check (ubuntu-latest)" lane will be green
on the same commit. (Caveat: this image does not exercise the per-MCU
build matrix — those are separate jobs in their own workflows, not
part of check-ubuntu.)
