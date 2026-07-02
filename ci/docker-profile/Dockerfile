# Linux profiling harness for fbuild builds of NightDriverStrip
# (FastLED/fbuild#942 Phase 0).
#
# The image carries the *tooling* only: soldr (which bootstraps the
# pinned Rust toolchain on first run), perf, bpftrace, FlameGraph, and
# a pinned NightDriverStrip checkout. fbuild itself is built at run
# time from the bind-mounted source tree so the harness always profiles
# the current working copy — no COPY of fbuild source here.
#
# Caching contract (see README.md):
#   * PERSISTED across runs via named volumes: /work/target, ~/.cargo,
#     ~/.rustup, ~/.soldr — these only make the *harness* fast.
#   * NEVER persisted: ~/.fbuild (and ~/.platformio) — the system under
#     test. It lives in the container filesystem and dies with the
#     `--rm` container, so "cold cache" is genuinely cold.
#
# Build (from repo root):
#   docker build -f ci/docker-profile/Dockerfile -t fbuild-profile-linux ci/docker-profile
#
# Run: use `uv run python ci/docker-profile/run_profile.py` — it owns
# the volume flags and privileged-mode setup. Don't hand-type them.

# ubuntu:24.04 for the same reason as ci/docker-mac-cross: the
# published soldr linux-gnu binary requires glibc 2.39, and 24.04
# matches the GHA `ubuntu-latest` runners.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# Tooling set = docker-mac-cross baseline + profiling additions:
#   linux-tools-generic → perf (invoked via /usr/lib/linux-tools-*/perf;
#                         the /usr/bin/perf wrapper refuses to run when
#                         `uname -r` doesn't match an installed linux-tools,
#                         which is always the case on WSL2 kernels)
#   bpftrace            → off-CPU wait profiling (sched_switch sums)
#   time                → /usr/bin/time -v resource capture per build
#   procps              → pkill for daemon teardown between scenarios
#   perl                → FlameGraph stackcollapse/flamegraph scripts
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        ca-certificates curl git xz-utils bzip2 zstd \
        python3 python3-pip python3-venv \
        build-essential pkg-config file \
        linux-tools-generic bpftrace time procps perl \
 && rm -rf /var/lib/apt/lists/*

# uv for python ops (CLAUDE.md mandate).
RUN curl -LsSf https://astral.sh/uv/install.sh | sh \
 && cp /root/.local/bin/uv /usr/local/bin/

# Soldr binary tarball from GitHub Releases — same rationale and pin as
# ci/docker-mac-cross/Dockerfile (pip wheel is manylinux_2_39-tagged and
# falls back to a squatted placeholder on older glibc).
ARG SOLDR_VERSION=0.7.98
RUN mkdir -p /opt/soldr-bin \
 && curl -fsSL \
        "https://github.com/zackees/soldr/releases/download/v${SOLDR_VERSION}/soldr-v${SOLDR_VERSION}-x86_64-unknown-linux-gnu.tar.zst" \
    | zstd -d --stdout \
    | tar -xf - -C /opt/soldr-bin \
 && for bin in soldr soldr-clang-shim cargo-chef crgx; do \
        [ -f "/opt/soldr-bin/$bin" ] && cp "/opt/soldr-bin/$bin" "/usr/local/bin/$bin" && chmod +x "/usr/local/bin/$bin"; \
    done \
 && soldr --version

# Brendan Gregg's FlameGraph scripts (stackcollapse-perf.pl,
# stackcollapse-bpftrace.pl, flamegraph.pl). Shallow clone of master —
# these scripts have been stable for years and the repo is untagged.
RUN git clone --depth 1 https://github.com/brendangregg/FlameGraph.git /opt/FlameGraph

# Benchmark workload: NightDriverStrip pinned to a fixed upstream sha so
# every profile run builds the identical source. env:demo is the target
# (esp32dev / registry espressif32@^6.12.0, WiFi+audio disabled).
# The checkout is a pristine template; profile_entry.sh copies it to a
# throwaway per-run directory so build output never contaminates it.
ARG NIGHTDRIVER_SHA=59113319200a62ffd39c91f7372b47e86df38bcc
RUN git clone https://github.com/PlummersSoftwareLLC/NightDriverStrip.git /opt/nightdriver \
 && git -C /opt/nightdriver checkout --detach "${NIGHTDRIVER_SHA}" \
 && rm -rf /opt/nightdriver/.git

ENV HOME=/root \
    SOLDR_TRUST_MODE=permissive \
    CARGO_TERM_COLOR=always

WORKDIR /work
CMD ["bash", "ci/docker-profile/profile_entry.sh"]
