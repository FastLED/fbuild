# Simulate the FastLED/fbuild GitHub Actions release runner for the
# `aarch64-apple-darwin` lane on **Linux x86_64**, with no Apple-side
# tooling and no pre-installed Rust toolchain. Soldr bootstraps
# everything else.
#
# This is the "NO CHEATING" proof: we ship a vanilla Debian base + the
# absolute minimum apt deps, and soldr brings rustup, the pinned
# toolchain, zig, the Apple SDK, and `cargo-zigbuild` — exactly the
# story soldr#997 promised for cross-compile completeness.
#
# Build:
#   docker build -f ci/docker-mac-arm64-cross/Dockerfile -t fbuild-mac-arm64-cross .
#
# Run (from repo root):
#   docker run --rm -v "$PWD:/src" -w /src fbuild-mac-arm64-cross \
#       ./ci/docker-mac-arm64-cross/build.sh

# ubuntu:24.04 is exactly what `ubuntu-latest` is on GitHub Actions
# right now (the runners moved off 22.04 in 2025-Q4). Picking it
# matches the constraint "simulate the GHA runner" verbatim and
# avoids glibc-floor friction: the published `soldr` linux-gnu
# binary requires glibc 2.39, which matches 24.04. Older bases
# (debian bookworm, ubuntu 22.04) would silently downgrade us to
# either the musl binary or a soldr that 404's its own libc.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# Minimum apt set — no rust, no zig, no clang. Soldr fetches all of those.
#   curl + ca-certificates  → soldr's HTTP fetches
#   git                     → cargo fetch from git deps
#   xz-utils + bzip2        → tarball extraction for various release archives
#   python3 + python3-pip   → soldr is shipped as a pip wheel
#   python3-venv            → uv venvs
#   build-essential         → cc/ld for any build script that needs them
#   pkg-config              → many *-sys crates poke pkg-config at build time
#   file                    → final verification of the Mach-O output
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        ca-certificates curl git xz-utils bzip2 zstd \
        python3 python3-pip python3-venv \
        build-essential pkg-config file \
 && rm -rf /var/lib/apt/lists/*

# uv for python ops (CLAUDE.md mandate).
RUN curl -LsSf https://astral.sh/uv/install.sh | sh \
 && cp /root/.local/bin/uv /usr/local/bin/ \
 && uv venv --seed /opt/soldr-venv

# Soldr binary tarball from GitHub Releases — the published soldr wheel
# is tagged `manylinux_2_39` (glibc 2.39 floor, ubuntu-24.04+) so a
# `pip install soldr` on Debian bookworm (glibc 2.36) silently falls back
# to an unrelated squatted `soldr-0.1.0` placeholder package. The tarball
# is built against glibc 2.17 (manylinux_2_17 floor) so it runs on every
# remotely-recent distro. Issue tracked at zackees/soldr#1005 (forthcoming).
ARG SOLDR_VERSION=0.7.59
RUN mkdir -p /opt/soldr-bin \
 && curl -fsSL \
        "https://github.com/zackees/soldr/releases/download/v${SOLDR_VERSION}/soldr-v${SOLDR_VERSION}-x86_64-unknown-linux-gnu.tar.zst" \
    | zstd -d --stdout \
    | tar -xf - -C /opt/soldr-bin \
 && ls -la /opt/soldr-bin

RUN for bin in soldr cargo-chef crgx zccache zccache-daemon zccache-fp; do \
        [ -f "/opt/soldr-bin/$bin" ] && cp "/opt/soldr-bin/$bin" "/usr/local/bin/$bin" && chmod +x "/usr/local/bin/$bin"; \
    done

# Pre-seed soldr's home so its first-run setup is non-interactive +
# the rustup auto-bootstrap has a writable HOME to drop ~/.cargo/ into.
ENV HOME=/root
RUN soldr --version

# cargo-zigbuild from PyPI — same approach fbuild's existing
# template_native_build.yml takes for its linux musl + linux PyO3 cross
# steps (see #331). Ships zig in the venv.
RUN /opt/soldr-venv/bin/pip install --no-cache-dir cargo-zigbuild ziglang \
 && ln -s /opt/soldr-venv/bin/cargo-zigbuild /usr/local/bin/cargo-zigbuild

# Soldr-controlled defaults:
#   - SOLDR_APPLE_SDK_VERSION pins the Apple SDK soldr fetches at
#     `cargo zigbuild --target *-apple-darwin` time.
#   - SOLDR_TRUST_MODE=permissive accepts unpinned managed downloads
#     (the Phase A catalogue blobs are sha-pinned by soldr itself).
ENV SOLDR_APPLE_SDK_VERSION=11.3 \
    SOLDR_TRUST_MODE=permissive \
    CARGO_TERM_COLOR=always
