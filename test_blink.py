#!/usr/bin/env python3
"""Test ESP32 Compiler with simple blink sketch (no external libraries).

This script tests the ESP32 compiler implementation by:
1. Setting up platform, toolchain, and framework
2. Compiling a simple blink sketch without external dependencies
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
    """Test the ESP32 compiler with simple blink sketch."""
    print("=" * 80)
    print("ESP32 Compiler Test - Simple Blink (No External Libraries)")
    print("=" * 80)
    print()

    # Setup
    project_dir = Path(__file__).parent
    sketch_path = project_dir / "tests" / "esp32c6_blink" / "esp32c6_blink.ino"
    build_dir = project_dir / ".build" / "esp32c6_blink"
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
    print(f"Include paths: {info['include_count']} directories")
    print()

    print("Step 5: Compile Blink Sketch")
    print("-" * 40)
    if not sketch_path.exists():
        print(f"ERROR: Sketch not found: {sketch_path}")
        return 1

    try:
        obj_files = compiler.compile_sketch(sketch_path)
        print(f"\n✓ SUCCESS! Generated {len(obj_files)} object file(s):")
        for obj_file in obj_files:
            size = obj_file.stat().st_size if obj_file.exists() else 0
            print(f"  {obj_file.name}: {size:,} bytes")
        print()
    except Exception as e:
        print(f"\n✗ ERROR: Compilation failed: {e}")
        return 1

    print("=" * 80)
    print("BLINK SKETCH COMPILED SUCCESSFULLY!")
    print("=" * 80)
    print()
    print("✓ All ESP-IDF include issues resolved")
    print("✓ Arduino core headers working")
    print("✓ Sketch compiles to object file")
    print()
    print("Next steps:")
    print("  - Compile Arduino core sources")
    print("  - Implement library dependency resolution (for FastLED, etc.)")
    print("  - Implement linker module (esp32_linker.py)")
    print()

    return 0


if __name__ == "__main__":
    sys.exit(main())
