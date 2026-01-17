"""
Integration test with actual FastLED platformio.ini structure.
"""

from pathlib import Path

import pytest

from fbuild.config.ini_parser import PlatformIOConfig


def test_fastled_platformio_ini_parsing():
    """Test parsing of FastLED's actual platformio.ini structure."""
    # Use the actual FastLED platformio.ini
    fastled_ini = Path("C:/Users/niteris/dev/fastled/platformio.ini")

    if not fastled_ini.exists():
        pytest.skip("FastLED repository not found at expected path")

    config = PlatformIOConfig(fastled_ini)

    # Test that we can get the generic-esp base environment (without validation since it's abstract)
    generic_esp = config.get_env_config("generic-esp", _validate=False)
    assert generic_esp["framework"] == "arduino"
    assert "FastLED=symlink://./" in generic_esp.get("lib_deps", "")
    assert "build_type" in generic_esp
    assert generic_esp["build_type"] == "debug"

    # Test that ESP32-C6 inherits from generic-esp
    esp32c6 = config.get_env_config("esp32c6")
    assert esp32c6["board"] == "esp32-c6-devkitc-1"
    assert esp32c6["framework"] == "arduino"  # Inherited
    assert "FastLED=symlink://./" in esp32c6.get("lib_deps", "")  # Inherited
    assert "-DDEBUG" in esp32c6["build_flags"]  # Inherited

    # Test board build overrides
    overrides = config.get_board_overrides("esp32c6")
    assert overrides.get("flash_mode") == "dio"
    assert overrides.get("flash_size") == "4MB"
    assert overrides.get("upload_flash_size") == "4MB"
    assert overrides.get("partitions") == "huge_app.csv"

    # Test that ESP32-S3 inherits correctly
    esp32s3 = config.get_env_config("esp32s3")
    assert esp32s3["board"] == "seeed_xiao_esp32s3"
    assert esp32s3["framework"] == "arduino"  # Inherited
    assert "-DDEBUG" in esp32s3["build_flags"]  # Inherited from generic-esp

    # Test board overrides for ESP32-S3
    s3_overrides = config.get_board_overrides("esp32s3")
    assert s3_overrides.get("flash_mode") == "dio"
    assert s3_overrides.get("flash_size") == "4MB"
    assert s3_overrides.get("partitions") == "huge_app.csv"

    # Test src_dir override
    src_dir = config.get_src_dir()
    assert src_dir == "examples/Blink"

    # Test default environment
    default_env = config.get_default_environment()
    assert default_env == "esp32c6"

    print("\n✅ FastLED platformio.ini parsing successful!")
    print("  - generic-esp base environment: OK")
    print("  - esp32c6 inheritance: OK")
    print("  - esp32s3 inheritance: OK")
    print("  - board_build overrides: OK")
    print(f"  - src_dir override: {src_dir}")
    print(f"  - default_envs: {default_env}")


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
