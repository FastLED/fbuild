#!/usr/bin/env bash
# Run inside the `fbuild-mac-arm64-cross` docker image (see Dockerfile).
# Cross-compiles fbuild + fbuild-daemon + the PyO3 extension to
# aarch64-apple-darwin using soldr + cargo-zigbuild + soldr's Apple SDK.
#
# Output layout (in $PWD/staging):
#   fbuild                  ← Mach-O 64-bit executable arm64
#   fbuild-daemon           ← Mach-O 64-bit executable arm64
#   _native.abi3.so         ← Mach-O 64-bit dylib arm64 (PyO3 extension)

set -euo pipefail

TARGET="aarch64-apple-darwin"
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

# Verify the output is actually a Mach-O ARM64 binary — this is the
# `NO CHEATING` gate. If file(1) reports anything other than
# `Mach-O 64-bit ... arm64` for all three artifacts, the cross-compile
# silently produced the host binary instead and we must fail loudly.
for f in fbuild fbuild-daemon _native.abi3.so; do
    desc="$(file "$STAGING/$f")"
    echo "  $desc"
    if ! echo "$desc" | grep -qE "Mach-O.*(arm64|aarch64)"; then
        echo "ERROR: $f is not Mach-O arm64 — got: $desc" >&2
        exit 1
    fi
done
echo "::endgroup::"

echo "All three artifacts are valid Mach-O arm64. Staging dir: $STAGING"
ls -lh "$STAGING"
