#!/usr/bin/env bash
# Run inside the `fbuild-mac-cross` docker image (see Dockerfile).
# Cross-compiles fbuild + fbuild-daemon + the PyO3 extension to one of
# the Apple targets using soldr + cargo-zigbuild + soldr's Apple SDK.
#
# Usage:
#   ./build.sh                         # default aarch64-apple-darwin
#   TARGET=x86_64-apple-darwin ./build.sh  # mac intel
#   ./build.sh aarch64-apple-darwin    # explicit positional arg
#
# Output layout (in $PWD/staging):
#   fbuild                  ← Mach-O 64-bit executable <arch>
#   fbuild-daemon           ← Mach-O 64-bit executable <arch>
#   _native.abi3.so         ← Mach-O 64-bit dylib <arch> (PyO3 extension)

set -euo pipefail

TARGET="${1:-${TARGET:-aarch64-apple-darwin}}"
case "$TARGET" in
    aarch64-apple-darwin) MACHO_ARCH_PATTERN="arm64|aarch64" ;;
    x86_64-apple-darwin)  MACHO_ARCH_PATTERN="x86_64" ;;
    *)
        echo "ERROR: unsupported TARGET=$TARGET (expected aarch64-apple-darwin or x86_64-apple-darwin)" >&2
        exit 2
        ;;
esac
echo "::notice::TARGET=$TARGET (expect Mach-O $MACHO_ARCH_PATTERN)"
STAGING="${STAGING:-$PWD/staging}"
mkdir -p "$STAGING"

echo "::group::soldr version + rust toolchain"
soldr --version
# `soldr toolchain ensure` installs rustup (if missing) + the pinned
# channel from rust-toolchain.toml + the requested target stdlib + the
# components soldr's bootstrap matrix needs.
soldr toolchain ensure
soldr rustup target add "$TARGET"
echo "::endgroup::"

echo "::group::cargo-zigbuild + soldr-managed apple SDK"
which cargo-zigbuild
cargo-zigbuild --version
# Force a pre-fetch of the Apple SDK before the real build so a
# slow / failing SDK download is debuggable separately from the cargo
# build itself.
soldr prepare --target "$TARGET"
echo "::endgroup::"

echo "::group::Build fbuild-cli + fbuild-daemon"
soldr cargo zigbuild --release --target "$TARGET" \
    -p fbuild-cli -p fbuild-daemon
echo "::endgroup::"

echo "::group::Build fbuild-python PyO3 extension"
PYO3_NO_PYTHON=1 soldr cargo zigbuild --release \
    --target-dir target/python-extension \
    --target "$TARGET" -p fbuild-python \
    --features extension-module
echo "::endgroup::"

echo "::group::Stage + verify artifacts"
cp "target/$TARGET/release/fbuild"        "$STAGING/fbuild"
cp "target/$TARGET/release/fbuild-daemon" "$STAGING/fbuild-daemon"

EXT_SRC="target/python-extension/$TARGET/release/lib_native.dylib"
if [ ! -f "$EXT_SRC" ]; then
    echo "ERROR: PyO3 extension not found at $EXT_SRC" >&2
    find target/python-extension -name "lib_native.*" -o -name "_native.*" >&2 || true
    exit 1
fi
cp "$EXT_SRC" "$STAGING/_native.abi3.so"

# Verify the output is actually a Mach-O <arch> binary — this is the
# `NO CHEATING` gate. If file(1) reports anything other than
# `Mach-O 64-bit ... <arch>` for all three artifacts, the cross-compile
# silently produced the host binary instead and we must fail loudly.
for f in fbuild fbuild-daemon _native.abi3.so; do
    desc="$(file "$STAGING/$f")"
    echo "  $desc"
    if ! echo "$desc" | grep -qE "Mach-O.*($MACHO_ARCH_PATTERN)"; then
        echo "ERROR: $f is not Mach-O matching '$MACHO_ARCH_PATTERN' — got: $desc" >&2
        exit 1
    fi
done
echo "::endgroup::"

echo "All three artifacts are valid Mach-O for $TARGET. Staging dir: $STAGING"
ls -lh "$STAGING"
