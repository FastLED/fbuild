#!/usr/bin/env python3
"""Test script for ESP32 toolchain download and verification."""

import sys
from pathlib import Path

# Add src to path
sys.path.insert(0, str(Path(__file__).parent / "src"))

from zapio.packages.cache import Cache
from zapio.packages.esp32_platform import ESP32Platform
from zapio.packages.esp32_toolchain import ESP32Toolchain

def test_toolchain():
    """Test ESP32-C6 toolchain download and verification."""

    # Initialize cache
    cache = Cache()

    # Platform URL from LOOP.md
    platform_url = "https://github.com/pioarduino/platform-espressif32/releases/download/55.03.34/platform-espressif32.zip"

    # Initialize platform manager
    print("=" * 60)
    print("Step 1: Initialize ESP32 Platform")
    print("=" * 60)
    platform = ESP32Platform(cache, platform_url, show_progress=True)

    # Ensure platform is downloaded
    platform.ensure_platform()

    # Get required packages for ESP32-C6
    print("\n" + "=" * 60)
    print("Step 2: Get Required Packages for ESP32-C6")
    print("=" * 60)
    required_packages = platform.get_required_packages("esp32c6")

    print("\nRequired packages:")
    for package_name, package_url in required_packages.items():
        print(f"  - {package_name}")
        print(f"    URL: {package_url}")

    # Get RISC-V toolchain URL
    toolchain_url = required_packages.get("toolchain-riscv32-esp")
    if not toolchain_url:
        print("\nERROR: RISC-V toolchain URL not found!")
        return False

    # Initialize toolchain manager
    print("\n" + "=" * 60)
    print("Step 3: Download and Extract RISC-V Toolchain")
    print("=" * 60)
    toolchain = ESP32Toolchain(
        cache,
        toolchain_url,
        "riscv32-esp",
        show_progress=True
    )

    # Ensure toolchain is downloaded
    toolchain_path = toolchain.ensure_toolchain()
    print(f"\nToolchain installed at: {toolchain_path}")

    # Verify installation
    print("\n" + "=" * 60)
    print("Step 4: Verify Toolchain Installation")
    print("=" * 60)

    try:
        toolchain.verify_installation()
        print("✓ Toolchain verification successful!")
    except Exception as e:
        print(f"✗ Toolchain verification failed: {e}")
        return False

    # Get tool paths
    print("\n" + "=" * 60)
    print("Step 5: Tool Paths")
    print("=" * 60)
    tool_paths = toolchain.get_all_tool_paths()

    for tool_name, tool_path in tool_paths.items():
        if tool_path:
            exists_mark = "✓" if tool_path.exists() else "✗"
            print(f"{exists_mark} {tool_name:12} : {tool_path}")
        else:
            print(f"✗ {tool_name:12} : NOT FOUND")

    # Get toolchain info
    print("\n" + "=" * 60)
    print("Step 6: Toolchain Info")
    print("=" * 60)
    info = toolchain.get_toolchain_info()

    for key, value in info.items():
        if key != "tools":  # Skip tools dict, we already printed it
            print(f"  {key}: {value}")

    # Test GCC version
    print("\n" + "=" * 60)
    print("Step 7: Test GCC Version")
    print("=" * 60)
    gcc_path = toolchain.get_gcc_path()
    if gcc_path and gcc_path.exists():
        import subprocess
        try:
            result = subprocess.run(
                [str(gcc_path), "--version"],
                capture_output=True,
                text=True,
                timeout=5
            )
            print(result.stdout)
            print("✓ GCC is executable!")
        except Exception as e:
            print(f"✗ Failed to run GCC: {e}")
            return False
    else:
        print("✗ GCC not found!")
        return False

    print("\n" + "=" * 60)
    print("SUCCESS: All tests passed!")
    print("=" * 60)
    return True

if __name__ == "__main__":
    success = test_toolchain()
    sys.exit(0 if success else 1)
