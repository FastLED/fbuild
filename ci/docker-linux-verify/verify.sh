#!/usr/bin/env bash
# Run inside the `fbuild-mac-cross` docker image — exercises the same
# steps as `.github/workflows/check-ubuntu.yml` so a local Linux pass
# gives confidence the GHA Linux lanes will be green after a Windows-host
# admin-merge.
#
# Pre-built image: see ci/docker-mac-cross/Dockerfile. It already ships
# soldr + zccache + uv + the Rust toolchain bootstrapper, which is
# exactly what we need.
#
# Volumes (managed by verify.py for cross-run reuse):
#   /src        ← repo bind-mount (read-only would be nicer, but cargo
#                 writes Cargo.lock + the build emits dotfiles into the
#                 workspace, so RW is required)
#   /target     ← named volume `fbuild-linux-verify-target`
#   /cargo-home ← named volume `fbuild-linux-verify-cargo-home`

set -euo pipefail

export CARGO_TARGET_DIR=/target
export CARGO_HOME=/cargo-home
export RUSTFLAGS="-D warnings"

cd /src

echo "::group::soldr version + rust toolchain"
soldr --version
soldr toolchain ensure
echo "::endgroup::"

echo "::group::cargo check --workspace --all-targets"
soldr cargo check --workspace --all-targets
echo "::endgroup::"

echo "::group::cargo clippy --workspace --all-targets -- -D warnings"
soldr cargo clippy --workspace --all-targets -- -D warnings
echo "::endgroup::"

echo "::group::lint subprocess spawns"
uv run python ci/find_direct_subprocess.py --fail
echo "::endgroup::"

echo "::group::cargo test --workspace"
soldr cargo test --workspace
echo "::endgroup::"

echo "ALL GREEN — Linux check-ubuntu lane is satisfied."
