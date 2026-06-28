# `ci/docker-mac-arm64-cross/` — Linux → `aarch64-apple-darwin` simulator

Reproduces fbuild's `aarch64-apple-darwin` release lane on **Linux x86_64**
with **no Apple-side tooling and no pre-installed Rust toolchain**. Soldr
bootstraps rustup, the pinned 1.94.1 channel, zig, the Apple SDK, and
`cargo-zigbuild` from a vanilla `ubuntu:24.04` base.

This is the proof-of-concept that lets fbuild's release pipeline drop
its `macos-latest` runner for the mac-arm64 lane and replace it with the
same `ubuntu-latest` lane every other target already uses.

## Build + run

```bash
# From the fbuild repo root:
docker build -f ci/docker-mac-arm64-cross/Dockerfile -t fbuild-mac-arm64-cross .
docker run --rm -v "$PWD:/src" -w /src fbuild-mac-arm64-cross \
    bash ci/docker-mac-arm64-cross/build.sh
```

`build.sh` produces three artifacts under `$PWD/staging/` and asserts via
`file(1)` that each is a `Mach-O 64-bit arm64` binary — the
`NO CHEATING` gate. If anything regressed and we accidentally produced
the host Linux binary, `file` reports `ELF 64-bit LSB pie executable,
x86-64` and the script fails loudly.

## Why `ubuntu:24.04` (not `debian:bookworm-slim`)

Soldr's published Linux-gnu binary requires `glibc 2.39`. Debian
bookworm-slim ships `glibc 2.36`, so `soldr --version` 404's its own
libc and a `pip install soldr` falls back to a squatted `soldr-0.1.0`
placeholder on PyPI. `ubuntu:24.04` matches what GitHub Actions'
`ubuntu-latest` resolves to today, so this is also the most-faithful
GHA-runner simulation.

The friction is tracked at zackees/soldr — Phase B-2 should lower the
soldr Linux-gnu binary to a `manylinux_2_17` floor so any distro from
the last ~5 years can run it directly.
