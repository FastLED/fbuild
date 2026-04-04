#!/bin/bash
# Simple test for fbuild purge command using mock packages

set -e
export FBUILD_DEV_MODE=1

echo "=================================="
echo "fbuild Purge Command Simple Test"
echo "=================================="
echo ""

# Clean cache
echo "Step 1: Clean existing cache..."
rm -rf ~/.fbuild/cache_dev/
echo "✓ Cache cleaned"
echo ""

# Verify empty
echo "Step 2: Verify cache is empty..."
OUTPUT=$(uv run fbuild purge 2>&1 || true)
if echo "$OUTPUT" | grep -q "No packages cached"; then
    echo "✓ Cache is empty"
else
    echo "✗ Expected empty cache"
    echo "$OUTPUT"
    exit 1
fi
echo ""

# Create mock packages with manifests
echo "Step 3: Create mock packages with manifests..."
mkdir -p ~/.fbuild/cache_dev/toolchains/hash1/7.3.0/bin
mkdir -p ~/.fbuild/cache_dev/platforms/hash2/3.3.5

cat > ~/.fbuild/cache_dev/toolchains/hash1/7.3.0/manifest.json << 'EOF'
{
  "name": "AVR-GCC Toolchain",
  "type": "toolchains",
  "version": "7.3.0-atmel3.6.1",
  "url": "https://github.com/arduino/toolchain-avr/releases/download/7.3.0-atmel3.6.1/avr-gcc.tar.bz2",
  "install_date": "2026-02-14T12:00:00+00:00",
  "metadata": {"architecture": "avr"}
}
EOF

cat > ~/.fbuild/cache_dev/platforms/hash2/3.3.5/manifest.json << 'EOF'
{
  "name": "ESP32 Platform",
  "type": "platforms",
  "version": "3.3.5",
  "url": "https://github.com/platformio/platform-espressif32/archive/refs/tags/v3.3.5.zip",
  "install_date": "2026-02-14T13:30:00+00:00",
  "metadata": {"platform": "espressif32"}
}
EOF

# Add some files for size
dd if=/dev/zero of=~/.fbuild/cache_dev/toolchains/hash1/7.3.0/bin/avr-gcc bs=1M count=50 2>/dev/null
dd if=/dev/zero of=~/.fbuild/cache_dev/platforms/hash2/3.3.5/platform.json bs=1M count=30 2>/dev/null

echo "✓ Created 2 mock packages"
echo ""

# List packages
echo "Step 4: List cached packages..."
uv run fbuild purge 2>&1 || true
echo ""

# Test dry-run
echo "Step 5: Test dry-run..."
OUTPUT=$(uv run fbuild purge all --dry-run 2>&1)
if echo "$OUTPUT" | grep -q "Would delete:"; then
    echo "✓ Dry-run works correctly"
else
    echo "✗ Dry-run failed"
    echo "$OUTPUT"
    exit 1
fi
echo ""

# Verify packages still exist
if [ -f ~/.fbuild/cache_dev/toolchains/hash1/7.3.0/manifest.json ]; then
    echo "✓ Packages still exist after dry-run"
else
    echo "✗ Packages were deleted by dry-run!"
    exit 1
fi
echo ""

# Purge all
echo "Step 6: Purge all packages..."
uv run fbuild purge all 2>&1
echo ""

# Verify empty
echo "Step 7: Verify cache is empty after purge..."
OUTPUT=$(uv run fbuild purge 2>&1 || true)
if echo "$OUTPUT" | grep -q "No packages cached"; then
    echo "✓ Cache is empty (all packages deleted)"
else
    echo "✗ Cache is not empty"
    echo "$OUTPUT"
    exit 1
fi
echo ""

echo "=================================="
echo "ALL TESTS PASSED! ✓"
echo "=================================="
