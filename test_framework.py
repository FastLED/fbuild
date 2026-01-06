#!/usr/bin/env python3
"""Test ESP32 Framework download and management.

This script tests the ESP32Framework class by:
1. Downloading Arduino-ESP32 core for ESP32-C6
2. Downloading ESP-IDF precompiled libraries
3. Verifying framework structure
4. Listing cores, variants, and libraries
"""

from pathlib import Path
from src.zapio.packages.cache import Cache
from src.zapio.packages.esp32_platform import ESP32Platform
from src.zapio.packages.esp32_framework import ESP32Framework


def test_framework():
    """Test ESP32 framework download and extraction."""
    print("=" * 70)
    print("ESP32 Framework Download Test")
    print("=" * 70)
    print()

    # Initialize cache
    cache = Cache()
    print(f"Cache directory: {cache.cache_root}")
    print()

    # Initialize platform to get framework URLs
    platform_url = "https://github.com/pioarduino/platform-espressif32/releases/download/55.03.34/platform-espressif32.zip"
    platform = ESP32Platform(cache, platform_url, show_progress=True)

    print("Step 1: Ensure platform package is available")
    print("-" * 70)
    platform.ensure_platform()
    print()

    # Get framework URLs
    print("Step 2: Get framework URLs from platform")
    print("-" * 70)
    framework_url = platform.get_package_url("framework-arduinoespressif32")
    libs_url = platform.get_package_url("framework-arduinoespressif32-libs")

    print(f"Framework URL: {framework_url}")
    print(f"Libraries URL: {libs_url}")
    print()

    if not framework_url or not libs_url:
        print("ERROR: Could not find framework URLs in platform.json")
        return False

    # Initialize framework manager
    print("Step 3: Initialize framework manager")
    print("-" * 70)
    framework = ESP32Framework(
        cache=cache,
        framework_url=framework_url,
        libs_url=libs_url,
        show_progress=True
    )
    print(f"Framework version: {framework.version}")
    print(f"Framework path: {framework.framework_path}")
    print()

    # Ensure framework is downloaded and extracted
    print("Step 4: Download and extract framework")
    print("-" * 70)
    framework_path = framework.ensure_framework()
    print(f"Framework installed at: {framework_path}")
    print()

    # Verify framework structure
    print("Step 5: Verify framework structure")
    print("-" * 70)

    cores_dir = framework.get_cores_dir()
    print(f"Cores directory: {cores_dir}")
    print(f"Cores directory exists: {cores_dir.exists()}")

    variants_dir = framework.get_variants_dir()
    print(f"Variants directory: {variants_dir}")
    print(f"Variants directory exists: {variants_dir.exists()}")

    libraries_dir = framework.get_libraries_dir()
    print(f"Libraries directory: {libraries_dir}")
    print(f"Libraries directory exists: {libraries_dir.exists()}")

    sdk_dir = framework.get_sdk_dir()
    print(f"SDK directory: {sdk_dir}")
    print(f"SDK directory exists: {sdk_dir.exists()}")
    print()

    # List available cores
    print("Step 6: List available cores")
    print("-" * 70)
    cores = framework.list_cores()
    print(f"Available cores ({len(cores)}):")
    for core in cores:
        print(f"  - {core}")
    print()

    # List available variants
    print("Step 7: List available variants")
    print("-" * 70)
    variants = framework.list_variants()
    print(f"Available variants ({len(variants)}):")
    for variant in sorted(variants):
        if "c6" in variant.lower():
            print(f"  - {variant} ← ESP32-C6 variant")
        else:
            print(f"  - {variant}")
    print()

    # List available libraries
    print("Step 8: List available libraries")
    print("-" * 70)
    libraries = framework.list_libraries()
    print(f"Available libraries ({len(libraries)}):")
    for lib in sorted(libraries[:20]):  # Show first 20
        print(f"  - {lib}")
    if len(libraries) > 20:
        print(f"  ... and {len(libraries) - 20} more")
    print()

    # Test ESP32-C6 specific paths
    print("Step 9: Test ESP32-C6 specific paths")
    print("-" * 70)

    # Try to get esp32c6 variant
    try:
        c6_variant = framework.get_variant_dir("esp32c6")
        print(f"ESP32-C6 variant: {c6_variant}")
        print(f"Variant exists: {c6_variant.exists()}")

        # List files in variant
        if c6_variant.exists():
            variant_files = list(c6_variant.glob("*"))
            print(f"Files in ESP32-C6 variant ({len(variant_files)}):")
            for f in variant_files[:10]:
                print(f"  - {f.name}")
            if len(variant_files) > 10:
                print(f"  ... and {len(variant_files) - 10} more")
    except Exception as e:
        print(f"Error accessing ESP32-C6 variant: {e}")
    print()

    # Get SDK includes for ESP32-C6
    print("Step 10: Get SDK includes for ESP32-C6")
    print("-" * 70)
    sdk_includes = framework.get_sdk_includes("esp32c6")
    print(f"SDK include directories for ESP32-C6 ({len(sdk_includes)}):")
    for include in sdk_includes[:15]:
        print(f"  - {include.name}")
    if len(sdk_includes) > 15:
        print(f"  ... and {len(sdk_includes) - 15} more")
    print()

    # Get SDK libraries for ESP32-C6
    print("Step 11: Get SDK libraries for ESP32-C6")
    print("-" * 70)
    sdk_libs = framework.get_sdk_libs("esp32c6")
    print(f"SDK libraries for ESP32-C6 ({len(sdk_libs)}):")
    for lib in sdk_libs[:15]:
        print(f"  - {lib.name}")
    if len(sdk_libs) > 15:
        print(f"  ... and {len(sdk_libs) - 15} more")
    print()

    # Get core sources
    print("Step 12: Get ESP32 core sources")
    print("-" * 70)
    core_sources = framework.get_core_sources("esp32")
    print(f"ESP32 core source files ({len(core_sources)}):")
    for src in sorted(core_sources)[:20]:
        print(f"  - {src.name}")
    if len(core_sources) > 20:
        print(f"  ... and {len(core_sources) - 20} more")
    print()

    # Get framework info
    print("Step 13: Get framework information")
    print("-" * 70)
    info = framework.get_framework_info()
    print("Framework information:")
    for key, value in info.items():
        if isinstance(value, list):
            print(f"  {key}: {len(value)} items")
        else:
            print(f"  {key}: {value}")
    print()

    print("=" * 70)
    print("Framework download test completed successfully!")
    print("=" * 70)
    return True


if __name__ == "__main__":
    try:
        success = test_framework()
        exit(0 if success else 1)
    except Exception as e:
        print(f"\nERROR: {e}")
        import traceback
        traceback.print_exc()
        exit(1)
