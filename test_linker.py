#!/usr/bin/env python3
"""Test ESP32 Linker with complete build pipeline.

This script tests the complete ESP32 build pipeline:
1. Compile Arduino core sources
2. Create core.a archive
3. Compile simple blink sketch
4. Link everything into firmware.elf
5. Generate firmware.bin
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
from zapio.build.esp32_linker import ESP32Linker


def main():
    """Test the complete ESP32 build pipeline."""
    print("=" * 80)
    print("ESP32 Complete Build Pipeline Test")
    print("=" * 80)
    print()

    # Setup
    project_dir = Path(__file__).parent
    sketch_path = project_dir / "tests" / "esp32c6_blink" / "esp32c6_blink.ino"
    build_dir = project_dir / ".build" / "esp32c6_full_build"
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
    print(f"Compiler ready for {board_id}")
    print()

    print("Step 5: Compile Arduino Core")
    print("-" * 40)
    try:
        core_obj_files = compiler.compile_core()
        print(f"✓ Compiled {len(core_obj_files)} core object files")
        print()
    except Exception as e:
        print(f"✗ ERROR: Core compilation failed: {e}")
        import traceback
        traceback.print_exc()
        return 1

    print("Step 6: Create Core Archive")
    print("-" * 40)
    try:
        core_archive = compiler.create_core_archive(core_obj_files)
        print()
    except Exception as e:
        print(f"✗ ERROR: Core archive creation failed: {e}")
        import traceback
        traceback.print_exc()
        return 1

    print("Step 7: Compile Sketch")
    print("-" * 40)
    if not sketch_path.exists():
        print(f"ERROR: Sketch not found: {sketch_path}")
        return 1

    try:
        sketch_obj_files = compiler.compile_sketch(sketch_path)
        print(f"✓ Compiled {len(sketch_obj_files)} sketch object file(s)")
        print()
    except Exception as e:
        print(f"✗ ERROR: Sketch compilation failed: {e}")
        import traceback
        traceback.print_exc()
        return 1

    print("Step 8: Initialize Linker")
    print("-" * 40)
    linker = ESP32Linker(
        platform,
        toolchain,
        framework,
        board_id,
        build_dir,
        show_progress=True
    )

    # Print linker info
    linker_info = linker.get_linker_info()
    print(f"Linker scripts: {linker_info.get('linker_script_count', 0)}")
    print(f"SDK libraries: {linker_info.get('sdk_library_count', 0)}")
    print()

    print("Step 9: Link Firmware")
    print("-" * 40)
    try:
        firmware_elf = linker.link(sketch_obj_files, core_archive)
        print()
    except Exception as e:
        print(f"✗ ERROR: Linking failed: {e}")
        import traceback
        traceback.print_exc()
        return 1

    print("Step 10: Generate Firmware Binary")
    print("-" * 40)
    try:
        firmware_bin = linker.generate_bin(firmware_elf)
        print()
    except Exception as e:
        print(f"✗ ERROR: Binary generation failed: {e}")
        import traceback
        traceback.print_exc()
        return 1

    print("=" * 80)
    print("COMPLETE BUILD PIPELINE SUCCESSFUL!")
    print("=" * 80)
    print()
    print(f"✓ Compiled {len(core_obj_files)} core source files")
    print(f"✓ Created core.a archive")
    print(f"✓ Compiled {len(sketch_obj_files)} sketch file(s)")
    print(f"✓ Linked firmware.elf")
    print(f"✓ Generated firmware.bin")
    print()
    print("Build artifacts:")
    print(f"  Core archive: {core_archive}")
    print(f"  Firmware ELF: {firmware_elf}")
    print(f"  Firmware BIN: {firmware_bin}")
    print()
    print("Build directory:", build_dir)
    print()

    return 0


if __name__ == "__main__":
    sys.exit(main())
