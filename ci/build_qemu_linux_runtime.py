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

What the build *proves* before it publishes anything
----------------------------------------------------
The archive's name and the API it must satisfy live in Rust
(`crates/fbuild-toolchain/src/toolchain/esp_qemu_runtime.rs`), and the only
thing that used to connect the two sides was a comment saying "keep in sync".
Four checks now run before a single byte is copied out of the container:

  * the closure walk must SETTLE — a walk that hits the round cap used to end
    silently, and a truncated closure still passes the five-name check;
  * the bundle must be a **complete closure**: every `DT_NEEDED` of every
    bundled library is either the glibc family or itself in `lib/`. The
    `--version` selftest alone cannot see behind libgcrypt or zlib, because
    the build image necessarily still carries both while it runs (`zlib1g` is
    `priority: required`);
  * the archive must contain `MANIFEST.txt` and at least one `lib/` entry, so a
    truncated `tar | zstd` cannot be published as a 30 MB success;
  * both emulators must still start with `LD_LIBRARY_PATH=<bundle>/lib` after
    the apt-provided copies of the three removable libraries are taken away.

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
import re
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

# Every non-glibc library the Espressif QEMU binaries link directly. Mirrors
# `REQUIRED_LIBS` in
# crates/fbuild-toolchain/src/toolchain/esp_qemu_runtime.rs, which refuses to
# treat an extracted tree as installed unless all of them are present; a test
# asserts the two lists stay equal. Injected into the payload below so there is
# exactly one copy in this file.
REQUIRED_LIBS = (
    "libslirp.so.0",
    "libSDL2-2.0.so.0",
    "libpixman-1.so.0",
    "libgcrypt.so.20",
    "libz.so.1",
)

# The glibc family plus the dynamic loader: always resolved from the host.
# A bundled `libc.so.6` without its matching `ld-linux` is unloadable, so these
# are never copied in and never counted as a missing dependency.
GLIBC_FAMILY = (
    "libc.so.6",
    "libm.so.6",
    "libpthread.so.0",
    "librt.so.1",
    "libdl.so.2",
    "libutil.so.1",
    "libresolv.so.2",
    "libgcc_s.so.1",
)
GLIBC_FAMILY_GLOBS = ("ld-linux*",)

# Breadth-first closure rounds allowed. One round copies one dependency level,
# so this caps the graph depth; real closures here are 4-5 deep. Hitting the cap
# is a hard failure rather than a short archive.
MAX_CLOSURE_ROUNDS = 8

# Markers delimiting the shell logic `ci/test_build_qemu_linux_runtime.py`
# extracts and runs against a fake `ldd`. The tests fail if a marker goes away.
LOGIC_BEGIN = "# --- closure-logic (exercised natively by ci/test_build_qemu_linux_runtime.py) ---"
LOGIC_END = "# --- end closure-logic ---"

CONTAINER_SCRIPT = r"""
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

ARCH_SUFFIX="__ARCH_SUFFIX__"
TAG="__TAG__"
VERSION="__VERSION__"
OUT_NAME="__OUT_NAME__"
DROP="__DROP_DIR__"
MAX_CLOSURE_ROUNDS=__MAX_ROUNDS__

# Private staging directory: this payload runs as root, so fixed /tmp names
# would let a local attacker pre-create them as symlinks and redirect
# privileged mkdir/cp/zstd writes.
WORK="$(mktemp -d)"
chmod 700 "$WORK"
trap 'rm -rf "$WORK"' EXIT

apt-get update -qq
apt-get install -y --no-install-recommends \
    ca-certificates curl xz-utils zstd \
    libsdl2-2.0-0 libslirp0 libpixman-1-0 libgcrypt20 zlib1g >/dev/null

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

__CLOSURE_LOGIC__

collect_closure
verify_closure

# Fail loudly if the libraries this bundle exists for are absent.
for required in __REQUIRED_LIBS__; do
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

# A truncated archive still copies out of the container happily, so inspect it
# instead of trusting the exit code: the manifest and the libraries must be
# inside.
# `zstd -dc | tar -t` rather than `tar --zstd`: the build image ships tar 1.30,
# which predates `--zstd` support (added in 1.31).
ENTRIES="$(zstd -dc "$WORK/${OUT_NAME}" | tar -tf -)"
case "$ENTRIES" in
    *MANIFEST.txt*) ;;
    *)
        echo "FATAL: archive carries no MANIFEST.txt" >&2
        exit 1
        ;;
esac
LIBS_IN_ARCHIVE="$(printf '%s\n' "$ENTRIES" | grep -c '^lib/' || true)"
if [ "$LIBS_IN_ARCHIVE" -lt __REQUIRED_LIB_COUNT__ ]; then
    echo "FATAL: archive carries ${LIBS_IN_ARCHIVE} lib/ entries, fewer than the __REQUIRED_LIB_COUNT__ required libraries" >&2
    exit 1
fi

echo "=== bundle ==="
cat "$BUNDLE/MANIFEST.txt"
echo "libraries: $(ls -1 "$BUNDLE/lib" | wc -l)"
echo "archive:   $(stat -c %s "$WORK/${OUT_NAME}") bytes"

# Prove the bundle actually satisfies both binaries with no host packages in
# play beyond glibc: strip the apt-installed copies first, so a system library
# left behind cannot mask a gap in the closure. `verify_closure` above already
# covers the transitively reachable gap this cannot see (libgcrypt and zlib
# themselves cannot be removed from a working shell).
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

# The closure walk, extracted verbatim by the test module so the bash logic is
# executed for real (against a stubbed `ldd`) instead of merely grepped.
CLOSURE_LOGIC = r"""
# Resolved shared-object paths for one ELF, one per line.
deps() {
    local out
    if ! out="$(ldd "$1" 2>/dev/null)"; then
        echo "FATAL: cannot read the dynamic dependencies of $1" >&2
        return 1
    fi
    printf '%s\n' "$out" | awk '{print $3}' | grep '^/' || true
}

# Sonames one ELF declares, INCLUDING entries `ldd` could not resolve. `deps`
# drops "not found" lines, which is right for copying but wrong for judging
# completeness: an unresolvable transitive library must be reported as a gap,
# not silently omitted from the graph.
needed() {
    ldd "$1" 2>/dev/null | awk '$2 == "=>" { print ($3 == "not" ? $1 : $3) }' || true
}

# The glibc family and the dynamic loader are deliberately excluded: they must
# come from the host, and a bundled libc without its matching ld-linux is
# unloadable.
is_glibc_family() {
    case "$1" in
        __GLIBC_FAMILY_PATTERN__) return 0;;
        *) return 1;;
    esac
}

# Breadth-first walk of both emulators' transitive closure into $BUNDLE/lib.
# One round copies one dependency level. The walk ends when a round finds no
# new library; a queue that is still non-empty at the cap means a level was
# never expanded, and the archive would be quietly incomplete.
collect_closure() {
    local queue next f b dep round
    queue="$(deps "$XTENSA")"
    queue="$queue $(deps "$RISCV")"
    round=0
    while [ -n "$queue" ]; do
        if [ "$round" -ge "$MAX_CLOSURE_ROUNDS" ]; then
            echo "FATAL: dependency closure did not settle in ${MAX_CLOSURE_ROUNDS} rounds; still unexpanded:${queue}" >&2
            return 1
        fi
        round=$((round + 1))
        next=""
        for f in $queue; do
            b="$(basename "$f")"
            if is_glibc_family "$b"; then continue; fi
            if [ -f "$BUNDLE/lib/$b" ]; then continue; fi
            cp -L "$f" "$BUNDLE/lib/$b"
            for dep in $(deps "$f"); do
                if is_glibc_family "$(basename "$dep")"; then continue; fi
                next="$next $dep"
            done
        done
        queue="$next"
    done
}

# Every library in the bundle must be complete on its own: each dependency it
# declares is either the glibc family or present in $BUNDLE/lib. This is the
# check the `--version` selftest cannot make, because the build image still
# carries libgcrypt and zlib while that selftest runs.
verify_closure() {
    local lib b dep gaps
    gaps=""
    for lib in "$BUNDLE/lib"/*; do
        b="$(basename "$lib")"
        if is_glibc_family "$b"; then continue; fi
        for dep in $(needed "$lib"); do
            dep="$(basename "$dep")"
            if is_glibc_family "$dep"; then continue; fi
            if [ ! -f "$BUNDLE/lib/$dep" ]; then
                gaps="$gaps ${b}->${dep}"
            fi
        done
    done
    if [ -n "$gaps" ]; then
        echo "FATAL: bundle is not a complete dependency closure:${gaps}" >&2
        return 1
    fi
}
"""

# Root-owned directory the payload copies finished artifacts into.
DROP_DIR = "/var/tmp/fbuild-qemu-runtime"

# Any `__PLACEHOLDER__` left in the rendered payload is a bug: the container
# would either crash or, worse, act on the literal text.
_PLACEHOLDER = re.compile(r"__[A-Z][A-Z_]*__")


def out_name(arch: str) -> str:
    return f"qemu-esp-linux-runtime-{arch}-{QEMU_RELEASE_TAG}.tar.zst"


def architecture_tokens() -> list[str]:
    return sorted(DOCKER_PLATFORM)


def _glibc_family_pattern() -> str:
    return "|".join(GLIBC_FAMILY + GLIBC_FAMILY_GLOBS)


def render_script(arch: str) -> str:
    """Fill the container payload in for `arch`, refusing to leave a hole."""
    logic = CLOSURE_LOGIC.replace(
        "__GLIBC_FAMILY_PATTERN__", _glibc_family_pattern()
    ).strip("\n")
    logic = f"{LOGIC_BEGIN}\n{logic}\n{LOGIC_END}"
    rendered = (
        CONTAINER_SCRIPT.replace("__CLOSURE_LOGIC__", logic)
        .replace("__ARCH_SUFFIX__", QEMU_ARCHIVE_SUFFIX[arch])
        .replace("__TAG__", QEMU_RELEASE_TAG)
        .replace("__VERSION__", QEMU_ARCHIVE_VERSION)
        .replace("__OUT_NAME__", out_name(arch))
        .replace("__DROP_DIR__", DROP_DIR)
        .replace("__MAX_ROUNDS__", str(MAX_CLOSURE_ROUNDS))
        .replace("__REQUIRED_LIB_COUNT__", str(len(REQUIRED_LIBS)))
        .replace("__REQUIRED_LIBS__", " ".join(REQUIRED_LIBS))
    )
    leftover = _PLACEHOLDER.search(rendered)
    if leftover is not None:
        raise ValueError(f"unsubstituted placeholder {leftover.group(0)} in payload")
    return rendered


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
    try:
        _docker_copy(container, f"{DROP_DIR}/{out_name(arch)}", archive)
        _docker_copy(
            container,
            f"{DROP_DIR}/{out_name(arch)}.manifest.txt",
            Path(f"{archive}.manifest.txt"),
        )
        _assert_payload(archive, Path(f"{archive}.manifest.txt"))
    finally:
        subprocess.run(
            ["docker", "rm", "-f", container], capture_output=True, check=False
        )
    return archive


def _docker_copy(container: str, remote: str, local: Path) -> None:
    proc = subprocess.run(
        ["docker", "cp", f"{container}:{remote}", str(local)],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise SystemExit(
            f"docker cp {remote} failed with exit code {proc.returncode}: "
            f"{proc.stderr.strip()}"
        )


def _assert_payload(archive: Path, manifest: Path) -> None:
    """Refuse to report a digest for an artifact that is absent or empty.

    `docker cp` succeeds for a zero-byte file, so a build killed after the
    archive was created but before it was filled would otherwise print a
    perfectly good-looking SHA-256 for nothing.
    """
    for path in (archive, manifest):
        if not path.is_file():
            raise SystemExit(f"container build produced no {path.name}")
        if path.stat().st_size == 0:
            raise SystemExit(f"container build produced an empty {path.name}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--arch",
        choices=architecture_tokens(),
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
