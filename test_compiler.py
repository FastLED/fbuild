#!/usr/bin/env python3
"""Test ESP32 Compiler Module.

This script tests the ESP32 compiler implementation by:
1. Setting up platform, toolchain, and framework
2. Compiling a simple sketch
3. Verifying object files are generated
"""

import sys
from pathlib import Path

# Add src to path
sys.path.insert(0, str(Path(__file__).parent / "src"))

from zapio.packages.cache import Cache
from zapio.packages.esp32_platform import ESP32Platform
from zapio.packages.esp32_toolchain import ESP32Toolchain
from zapio.packages.esp32_framework import ESP32Framework
from zapio.build.esp32_compiler import ESP32Compiler


def main():
    """Test the ESP32 compiler."""
    print("=" * 80)
    print("ESP32 Compiler Test")
    print("=" * 80)
    print()

    # Setup
    project_dir = Path(__file__).parent
    sketch_path = project_dir / "tests" / "esp32c6" / "esp32c6.ino"
    build_dir = project_dir / ".build" / "esp32c6"
    cache_dir = project_dir / ".zap" / "cache"

    # Create cache
    cache = Cache(cache_dir)

    # Platform URL
    platform_url = "https://github.com/pioarduino/platform-espressif32/releases/download/55.03.34/platform-espressif32.zip"

    print("Step 1: Initialize Platform")
    print("-" * 40)
    platform = ESP32Platform(cache, platform_url, show_progress=True)
    platform.ensure_platform()
    print()

    # Get required packages for ESP32-C6
    board_id = "esp32-c6-devkitm-1"
    board_json = platform.get_board_json(board_id)
    mcu = board_json.get("build", {}).get("mcu", "esp32c6")

    print(f"Board: {board_id}")
    print(f"MCU: {mcu}")
    print()

    # Get package URLs
    packages = platform.get_required_packages(mcu)

    print("Step 2: Initialize Toolchain")
    print("-" * 40)
    toolchain_url = packages.get("toolchain-riscv32-esp")
    if not toolchain_url:
        print("ERROR: Toolchain URL not found")
        return 1

    toolchain = ESP32Toolchain(
        cache,
        toolchain_url,
        "riscv32-esp",
        show_progress=True
    )
    toolchain.ensure_toolchain()
    print()

    print("Step 3: Initialize Framework")
    print("-" * 40)
    framework_url = packages.get("framework-arduinoespressif32")
    libs_url = packages.get("framework-arduinoespressif32-libs")

    if not framework_url or not libs_url:
        print("ERROR: Framework URLs not found")
        return 1

    framework = ESP32Framework(
        cache,
        framework_url,
        libs_url,
        show_progress=True
    )
    framework.ensure_framework()
    print()

    print("Step 4: Initialize Compiler")
    print("-" * 40)
    compiler = ESP32Compiler(
        platform,
        toolchain,
        framework,
        board_id,
        build_dir,
        show_progress=True
    )

    # Print compiler info
    info = compiler.get_compiler_info()
    print(f"Board: {info['board_id']}")
    print(f"MCU: {info['mcu']}")
    print(f"Variant: {info['variant']}")
    print(f"Toolchain: {info['toolchain_type']}")
    print(f"GCC: {info['gcc_path']}")
    print(f"G++: {info['gxx_path']}")
    print(f"Include paths: {info['include_count']} directories")
    print()

    print("Compile flags:")
    for key, flags in info['compile_flags'].items():
        print(f"  {key}: {len(flags)} flags")
        if len(flags) <= 5:
            for flag in flags:
                print(f"    {flag}")
    print()

    print("Step 5: Preprocess Sketch")
    print("-" * 40)
    if not sketch_path.exists():
        print(f"ERROR: Sketch not found: {sketch_path}")
        return 1

    cpp_path = compiler.preprocess_ino(sketch_path)
    print(f"Generated: {cpp_path}")
    print()

    # Show preprocessed content
    print("Preprocessed content (first 30 lines):")
    print("-" * 40)
    with open(cpp_path, 'r', encoding='utf-8') as f:
        lines = f.readlines()
        for i, line in enumerate(lines[:30], 1):
            print(f"{i:3}: {line}", end='')
    if len(lines) > 30:
        print(f"... ({len(lines) - 30} more lines)")
    print()

    print("Step 6: Compile Sketch")
    print("-" * 40)
    try:
        obj_files = compiler.compile_sketch(sketch_path)
        print(f"Success! Generated {len(obj_files)} object file(s):")
        for obj_file in obj_files:
            size = obj_file.stat().st_size if obj_file.exists() else 0
            print(f"  {obj_file.name}: {size:,} bytes")
        print()
    except Exception as e:
        print(f"ERROR: Compilation failed: {e}")
        import traceback
        traceback.print_exc()
        return 1

    print("Step 7: Compile Core (sample)")
    print("-" * 40)
    print("Compiling a few core files as a test...")
    try:
        # Get core sources
        core_sources = framework.get_core_sources("esp32")
        print(f"Found {len(core_sources)} core source files")

        # Compile just a few core files as a test
        test_sources = core_sources[:3]
        print(f"Testing with {len(test_sources)} files:")
        for src in test_sources:
            print(f"  - {src.name}")
        print()

        core_obj_dir = build_dir / "obj" / "core"
        core_obj_dir.mkdir(parents=True, exist_ok=True)

        compiled = 0
        for source in test_sources:
            try:
                obj_path = core_obj_dir / f"{source.stem}.o"
                compiler.compile_source(source, obj_path)
                compiled += 1
                size = obj_path.stat().st_size if obj_path.exists() else 0
                print(f"  ✓ {source.name} -> {obj_path.name} ({size:,} bytes)")
            except Exception as e:
                print(f"  ✗ {source.name}: {e}")

        print(f"\nCompiled {compiled}/{len(test_sources)} test files successfully")
        print()

    except Exception as e:
        print(f"ERROR: Core compilation test failed: {e}")
        import traceback
        traceback.print_exc()
        return 1

    print("=" * 80)
    print("COMPILER TEST COMPLETE!")
    print("=" * 80)
    print()
    print("Next steps:")
    print("  - Implement linker module (esp32_linker.py)")
    print("  - Link object files with ESP-IDF libraries")
    print("  - Generate firmware.elf and .bin")
    print()

    return 0


if __name__ == "__main__":
    sys.exit(main())
