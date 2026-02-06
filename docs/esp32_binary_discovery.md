# ESP32 Toolchain Binary Discovery

## Overview

As of version 1.4.4, fbuild automatically discovers the correct binary prefix for ESP32 toolchains by scanning the installed binaries, rather than relying solely on hardcoded mappings.

## Problem

Previously, fbuild used a hardcoded mapping to determine binary prefixes:

```python
TOOLCHAIN_NAMES = {
    "riscv32-esp": "riscv32-esp-elf",
    "xtensa-esp-elf": "xtensa-esp32-elf",
}
```

This approach had several issues:
1. **Not manifest-driven**: The mapping didn't come from any authoritative source
2. **Fragile**: If toolchain packaging changed, builds would fail
3. **Inconsistent**: The `tools.json` manifest says binaries should be named `xtensa-esp-elf-*` but they're actually `xtensa-esp32-elf-*`

## Solution

fbuild now **discovers the actual binary prefix** by scanning the extracted toolchain's bin directory for the gcc binary:

```python
def discover_binary_prefix(self, verbose: bool = False) -> Optional[str]:
    """Discover the actual binary prefix by scanning the bin directory.

    Searches for gcc binary and extracts its prefix (e.g., "xtensa-esp32-elf-gcc.exe" → "xtensa-esp32-elf").
    """
    # Scans bin_dir for files matching pattern: {prefix}-gcc[.exe]
    # Returns the discovered prefix or None if not found
```

### How It Works

1. **At construction time**: If toolchain exists, `_get_binary_prefix()` scans for gcc binary and caches the prefix
2. **After installation**: `ensure_toolchain()` calls `_update_binary_prefix_after_install()` to discover the prefix from newly installed files
3. **Fallback**: If discovery fails, falls back to hardcoded `TOOLCHAIN_NAMES` mapping

### Benefits

- **Manifest-driven**: Uses actual binary names from extracted packages (ground truth)
- **Robust**: Adapts to toolchain packaging changes automatically
- **Backward compatible**: Falls back to hardcoded mappings if discovery fails
- **No performance impact**: Single directory scan cached per toolchain instance
- **Future-proof**: Works for any toolchain variant without code changes

## Implementation Details

### Files Modified

1. **src/fbuild/packages/toolchain_binaries.py**
   - Added `discover_binary_prefix()` method to `ToolchainBinaryFinder` class

2. **src/fbuild/packages/toolchain_esp32.py**
   - Added `_discovered_prefix` cache variable
   - Added `_get_binary_prefix()` method
   - Added `_update_binary_prefix_after_install()` method
   - Modified `__init__()` to use discovery
   - Modified `ensure_toolchain()` to update prefix for cached installations
   - Updated `TOOLCHAIN_NAMES` comment

### Test Coverage

1. **tests/unit/packages/test_toolchain_binaries.py** (7 tests)
   - `test_discover_binary_prefix_riscv` - RISC-V toolchain discovery
   - `test_discover_binary_prefix_xtensa` - Xtensa toolchain discovery
   - `test_discover_binary_prefix_xtensa_no_extension` - Linux/macOS binary discovery
   - `test_discover_binary_prefix_not_found` - Missing bin directory
   - `test_discover_binary_prefix_no_gcc` - Missing gcc binary
   - `test_discover_binary_prefix_verbose` - Verbose output
   - `test_discover_binary_prefix_multiple_binaries` - Multiple gcc variants

2. **tests/unit/packages/test_toolchain_esp32_discovery.py** (5 tests)
   - `test_binary_prefix_discovery_after_install` - Discovery at construction
   - `test_binary_prefix_fallback_when_not_installed` - Fallback when not installed
   - `test_binary_prefix_discovery_for_riscv` - RISC-V integration
   - `test_binary_prefix_updated_on_ensure_toolchain` - Discovery after ensure_toolchain
   - `test_binary_prefix_discovery_failure_uses_fallback` - Fallback on discovery failure

## Edge Cases Handled

1. **Toolchain not yet installed**: Falls back to `TOOLCHAIN_NAMES` until first `ensure_toolchain()` call
2. **Binary discovery fails**: Falls back to `TOOLCHAIN_NAMES` mapping (same as current behavior)
3. **Malformed toolchain package**: `verify_installation()` catches missing binaries
4. **Multiple GCC binaries**: Returns first match (alphabetical from `iterdir()`)
5. **Cached old toolchains**: Discovery runs on every `ensure_toolchain()`, so cached prefix is always refreshed

## Verification

To verify the fix works:

```bash
# Clear cache to force fresh download
rm -rf ~/.fbuild/cache/toolchains/*

# Build should now succeed with verbose output showing discovery
fbuild build tests/esp32dev -e esp32dev -v
```

Expected output:
```
Discovered toolchain binary prefix: xtensa-esp32-elf
```

## Future Improvements

While this implementation solves the immediate problem, future enhancements could include:

1. **Parse tools.json for verification**: Compare discovered prefix against manifest (if provided) and warn on mismatches
2. **Cache discovery results globally**: Store discovered prefixes in a global cache to avoid rescanning
3. **Support custom binary patterns**: Allow users to specify expected binary patterns in platformio.ini

## References

- GitHub Issue: "Archiver (ar) path not found" CI failure for ESP32dev builds
- Implementation PR: [To be filled in]
- Related docs: `docs/architecture.dot`, `src/fbuild/packages/toolchain_binaries.py`
