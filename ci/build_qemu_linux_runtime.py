#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Build the Linux runtime-library bundle that Espressif QEMU needs.

Why this exists
---------------
The Espressif QEMU tarballs (`qemu-{xtensa,riscv32}-softmmu-*.tar.xz`) ship
**only** the emulator binary, its ROM blobs, and a static `libfdt.a`. The
binaries carry no `RPATH`/`RUNPATH` and dynamically link against five
non-glibc libraries:

    libpixman-1.so.0  libgcrypt.so.20  libSDL2-2.0.so.0  libz.so.1  libslirp.so.0

`ubuntu-24.04` — what `ubuntu-latest` resolves to on GitHub Actions — carries
none of libslirp/libSDL2/libpixman, so `qemu-system-xtensa` dies at exec with
`error while loading shared libraries: libslirp.so.0`. Requiring every caller
to `apt-get install` the set first makes QEMU emulation depend on an external
bootstrap step, which is exactly what fbuild exists to remove.

This script produces the bundle fbuild downloads on demand instead. It runs
`ldd` over both real QEMU binaries inside a pinned container and copies the
**full transitive closure minus the glibc family** (libc/libm/libpthread/
librt/libdl/libutil/libresolv/libgcc_s and the loader itself — shipping those
without the matching `ld-linux` is how you get a segfault, not a fix).

The closure is walked mechanically rather than hand-listed on purpose: SDL2 on
Ubuntu pulls in X11, Wayland, PulseAudio and friends, and libslirp pulls glib.
A curated list silently rots the first time a dependency is added upstream.

Build host is **ubuntu:20.04**. The bundled libraries inherit the build image's
glibc floor, and the goal is for the bundle never to be the binding constraint:
the Espressif QEMU binaries themselves require up to `GLIBC_2.30`, and a
20.04-built closure requires exactly the same. Building on 22.04 would push the
floor to `GLIBC_2.34` and lock out hosts that could otherwise run QEMU.

Usage::

    uv run python ci/build_qemu_linux_runtime.py --arch x86_64
    uv run python ci/build_qemu_linux_runtime.py --arch aarch64 --out dist/

Both arches build in a container, but the aarch64 bundle needs a **native**
arm64 host (an `ubuntu-24.04-arm` runner, via `qemu-runtime-bundle.yml`):
dpkg's maintainer scripts fail under Docker Desktop's arm64 emulation.

Emits `qemu-esp-linux-runtime-<arch>-<tag>.tar.zst`, a `.manifest.txt`
listing what went in, and prints the SHA-256 to paste into
`crates/fbuild-toolchain/src/toolchain/esp_qemu_runtime.rs`.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import subprocess
import sys
from pathlib import Path


# Keep in sync with QEMU_RELEASE_TAG in
# crates/fbuild-toolchain/src/toolchain/esp_qemu.rs.
QEMU_RELEASE_TAG = "esp-develop-9.2.2-20250817"
QEMU_ARCHIVE_VERSION = "esp_develop_9.2.2_20250817"

BUILD_IMAGE = "ubuntu:20.04"

# Docker --platform value per target architecture.
DOCKER_PLATFORM = {
    "x86_64": "linux/amd64",
    "aarch64": "linux/arm64",
}

# Espressif's archive suffix per target architecture.
QEMU_ARCHIVE_SUFFIX = {
    "x86_64": "x86_64-linux-gnu",
    "aarch64": "aarch64-linux-gnu",
}

CONTAINER_SCRIPT = r"""
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

ARCH_SUFFIX="__ARCH_SUFFIX__"
TAG="__TAG__"
VERSION="__VERSION__"
OUT_NAME="__OUT_NAME__"
DROP="__DROP_DIR__"

# Private staging directory: this payload runs as root, so fixed /tmp names
# would let a local attacker pre-create them as symlinks and redirect
# privileged mkdir/cp/zstd writes.
WORK="$(mktemp -d)"
chmod 700 "$WORK"
trap 'rm -rf "$WORK"' EXIT

apt-get update -qq
apt-get install -y --no-install-recommends     ca-certificates curl xz-utils zstd     libsdl2-2.0-0 libslirp0 libpixman-1-0 libgcrypt20 zlib1g >/dev/null

cd "$WORK"
for a in xtensa riscv32; do
    url="https://github.com/espressif/qemu/releases/download/${TAG}/qemu-${a}-softmmu-${VERSION}-${ARCH_SUFFIX}.tar.xz"
    echo "downloading ${url}"
    curl -sSfL -o "q-${a}.tar.xz" "${url}"
    mkdir -p "x-${a}"
    tar xf "q-${a}.tar.xz" -C "x-${a}"
done

XTENSA="$WORK/x-xtensa/qemu/bin/qemu-system-xtensa"
RISCV="$WORK/x-riscv32/qemu/bin/qemu-system-riscv32"
test -x "$XTENSA"
test -x "$RISCV"

BUNDLE="$WORK/bundle"
mkdir -p "$BUNDLE/lib"

# Resolved shared-object paths for one ELF, one per line.
deps() {
    ldd "$1" 2>/dev/null | awk '{print $3}' | grep '^/' || true
}

# The glibc family and the dynamic loader are deliberately excluded: they
# must come from the host, and a bundled libc without its matching
# ld-linux is unloadable.
is_glibc_family() {
    case "$1" in
        libc.so.6|libm.so.6|libpthread.so.0|librt.so.1|libdl.so.2) return 0;;
        libutil.so.1|libresolv.so.2|libgcc_s.so.1|ld-linux*) return 0;;
        *) return 1;;
    esac
}

QUEUE="$(deps "$XTENSA"; deps "$RISCV")"
for _round in 1 2 3 4 5 6 7 8; do
    NEXT=""
    for f in $QUEUE; do
        b="$(basename "$f")"
        if is_glibc_family "$b"; then continue; fi
        if [ ! -f "$BUNDLE/lib/$b" ]; then
            cp -L "$f" "$BUNDLE/lib/$b"
            NEXT="$NEXT $(deps "$f")"
        fi
    done
    [ -z "$(echo $NEXT)" ] && break
    QUEUE="$NEXT"
done

# Fail loudly if the libraries this bundle exists for are absent.
for required in libslirp.so.0 libSDL2-2.0.so.0 libpixman-1.so.0 libgcrypt.so.20 libz.so.1; do
    if [ ! -f "$BUNDLE/lib/$required" ]; then
        echo "FATAL: closure is missing $required" >&2
        exit 1
    fi
done

{
    echo "# Espressif QEMU ${TAG} Linux runtime libraries (${ARCH_SUFFIX})"
    echo "# glibc family intentionally excluded; comes from the host."
    (cd "$BUNDLE/lib" && ls -1 | sort)
} > "$BUNDLE/MANIFEST.txt"

cd "$BUNDLE"
tar cf - lib MANIFEST.txt | zstd -19 -T0 -q -o "$WORK/${OUT_NAME}"

echo "=== bundle ==="
cat "$BUNDLE/MANIFEST.txt"
echo "libraries: $(ls -1 "$BUNDLE/lib" | wc -l)"
echo "archive:   $(stat -c %s "$WORK/${OUT_NAME}") bytes"

# Prove the bundle actually satisfies both binaries with no host packages in
# play beyond glibc: strip the apt-installed copies first, so a system library
# left behind cannot mask a gap in the closure.
apt-get remove -y libsdl2-2.0-0 libslirp0 libpixman-1-0 >/dev/null 2>&1 || true
for bin in "$XTENSA" "$RISCV"; do
    if ! LD_LIBRARY_PATH="$BUNDLE/lib" "$bin" --version >/dev/null; then
        echo "FATAL: $bin still cannot start with the bundle applied" >&2
        exit 1
    fi
done
echo "selftest: both QEMU binaries start with LD_LIBRARY_PATH=<bundle>/lib"

# Hand the artifacts to the caller through a fresh, root-owned drop directory.
rm -rf "$DROP"
mkdir -m 700 -p "$DROP"
cp "$WORK/${OUT_NAME}" "$DROP/${OUT_NAME}"
cp "$BUNDLE/MANIFEST.txt" "$DROP/${OUT_NAME}.manifest.txt"
"""

# Root-owned directory the payload copies finished artifacts into.
DROP_DIR = "/var/tmp/fbuild-qemu-runtime"


def out_name(arch: str) -> str:
    return f"qemu-esp-linux-runtime-{arch}-{QEMU_RELEASE_TAG}.tar.zst"


def render_script(arch: str) -> str:
    return (
        CONTAINER_SCRIPT.replace("__ARCH_SUFFIX__", QEMU_ARCHIVE_SUFFIX[arch])
        .replace("__TAG__", QEMU_RELEASE_TAG)
        .replace("__VERSION__", QEMU_ARCHIVE_VERSION)
        .replace("__OUT_NAME__", out_name(arch))
        .replace("__DROP_DIR__", DROP_DIR)
    )


def build(arch: str, out_dir: Path) -> Path:
    script = render_script(arch)
    payload = base64.b64encode(script.encode()).decode()
    container = f"fbuild-qemu-runtime-{arch}"

    subprocess.run(["docker", "rm", "-f", container], capture_output=True, check=False)
    proc = subprocess.run(
        [
            "docker",
            "run",
            "--name",
            container,
            "--platform",
            DOCKER_PLATFORM[arch],
            BUILD_IMAGE,
            "bash",
            "-c",
            f"echo {payload} | base64 -d > /tmp/build.sh && bash /tmp/build.sh",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    sys.stdout.write(proc.stdout)
    sys.stderr.write(proc.stderr)
    if proc.returncode != 0:
        raise SystemExit(f"container build failed with exit code {proc.returncode}")

    out_dir.mkdir(parents=True, exist_ok=True)
    archive = out_dir / out_name(arch)
    for remote, local in (
        (f"{DROP_DIR}/{out_name(arch)}", archive),
        (f"{DROP_DIR}/{out_name(arch)}.manifest.txt", Path(f"{archive}.manifest.txt")),
    ):
        subprocess.run(
            ["docker", "cp", f"{container}:{remote}", str(local)], check=True
        )
    subprocess.run(["docker", "rm", "-f", container], capture_output=True, check=False)
    return archive


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--arch",
        choices=sorted(DOCKER_PLATFORM),
        default="x86_64",
        help="target architecture of the bundle (default: x86_64)",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("dist/qemu-runtime"),
        help="output directory (default: dist/qemu-runtime)",
    )
    args = parser.parse_args()

    archive = build(args.arch, args.out)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    print()
    print(f"archive: {archive}")
    print(f"sha256:  {digest}")
    print(f"size:    {archive.stat().st_size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
