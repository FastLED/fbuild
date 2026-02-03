"""Test to expose the header trampoline bug in parallel compilation mode.

The bug: When using parallel compilation (jobs > 1), ConfigurableCompiler bypasses
the trampoline logic in CompilationExecutor, resulting in long command lines that
hit Windows' 32K character limit.

Serial mode (jobs=1): ✅ Uses compilation_executor.compile_source() which applies trampolines
Parallel mode (jobs>1): ❌ Builds command directly without trampolines

This test demonstrates that parallel mode doesn't use trampolines, causing command
lines to be much longer than necessary.
"""

import platform
from pathlib import Path
from unittest.mock import Mock, patch

import pytest

from fbuild.build.build_context import BuildContext
from fbuild.build.build_profiles import BuildProfile, get_profile
from fbuild.build.compilation_executor import CompilationExecutor
from fbuild.build.configurable_compiler import ConfigurableCompiler


@pytest.mark.skipif(platform.system() != "Windows", reason="Windows-specific command-line length issue")
def test_parallel_compilation_uses_trampolines():
    """Test that parallel compilation uses header trampolines to reduce command length.

    This test creates a scenario with many long include paths (like ESP32 builds)
    and verifies that:
    1. Trampolines are generated
    2. The final compilation command uses short trampoline paths, not long original paths
    3. Command line stays under Windows' 32K limit
    """
    # Create mock components
    mock_platform = Mock()
    mock_platform.get_board_json.return_value = {"build": {"mcu": "esp32s3", "variant": "esp32s3", "core": "esp32"}}

    mock_toolchain = Mock()
    mock_toolchain.get_gcc_path.return_value = Path("C:/toolchain/xtensa-esp32s3-elf/bin/xtensa-esp32s3-elf-gcc.exe")
    mock_toolchain.get_gxx_path.return_value = Path("C:/toolchain/xtensa-esp32s3-elf/bin/xtensa-esp32s3-elf-g++.exe")

    mock_framework = Mock()
    mock_framework.version = "3.0.0"

    # Create realistic long include paths (like ESP32 builds)
    long_include_paths = [
        Path("C:/Users/developer/.platformio/packages/framework-arduinoespressif32@3.20014.231204/tools/sdk/esp32s3/include/freertos/include"),
        Path("C:/Users/developer/.platformio/packages/framework-arduinoespressif32@3.20014.231204/tools/sdk/esp32s3/include/freertos/include/esp_additions/freertos"),
        Path("C:/Users/developer/.platformio/packages/framework-arduinoespressif32@3.20014.231204/tools/sdk/esp32s3/include/freertos/port/xtensa/include"),
        Path("C:/Users/developer/.platformio/packages/framework-arduinoespressif32@3.20014.231204/tools/sdk/esp32s3/include/freertos/include/esp_additions"),
        Path("C:/Users/developer/.platformio/packages/framework-arduinoespressif32@3.20014.231204/tools/sdk/esp32s3/include/esp_hw_support/include"),
        Path("C:/Users/developer/.platformio/packages/framework-arduinoespressif32@3.20014.231204/tools/sdk/esp32s3/include/esp_hw_support/include/soc"),
        Path("C:/Users/developer/.platformio/packages/framework-arduinoespressif32@3.20014.231204/tools/sdk/esp32s3/include/esp_hw_support/include/soc/esp32s3"),
        Path("C:/Users/developer/.platformio/packages/framework-arduinoespressif32@3.20014.231204/tools/sdk/esp32s3/include/esp_hw_support/port/esp32s3"),
        Path("C:/Users/developer/.platformio/packages/framework-arduinoespressif32@3.20014.231204/tools/sdk/esp32s3/include/heap/include"),
        Path("C:/Users/developer/.platformio/packages/framework-arduinoespressif32@3.20014.231204/tools/sdk/esp32s3/include/log/include"),
    ] * 20  # Repeat to simulate ESP32's ~200 include paths

    build_dir = Path("C:/tmp/test_build")
    build_dir.mkdir(parents=True, exist_ok=True)

    # Create mock cache with trampoline support
    mock_cache = Mock()
    mock_cache.trampolines_dir = Path("C:/tmp/trampolines")
    mock_cache.ensure_directories = Mock()

    # Create compilation executor with trampoline cache
    compilation_executor = CompilationExecutor(build_dir=build_dir, show_progress=False, cache=mock_cache, mcu="esp32s3", framework_version="3.0.0")

    # Verify trampoline cache was initialized
    assert compilation_executor.trampoline_cache is not None, "Trampoline cache should be initialized on Windows"

    # Create mock compilation queue for parallel mode
    mock_queue = Mock()
    mock_queue.submit_job = Mock()

    # Mock platform config to avoid file loading
    platform_config = {
        "compiler_flags": {"common": ["-Os", "-Wall"], "c": ["-std=gnu11"], "cxx": ["-std=gnu++17"]},
        "defines": [],
        "linker_flags": [],
        "linker_scripts": [],
    }
    board_config = {"build": {"mcu": "esp32s3", "variant": "esp32s3", "core": "esp32"}}

    # Create mock BuildContext with all required fields
    mock_context = BuildContext(
        project_dir=build_dir.parent,
        env_name="esp32-s3-devkitc-1",
        clean=False,
        profile=BuildProfile.RELEASE,
        profile_flags=get_profile(BuildProfile.RELEASE),
        queue=mock_queue,
        build_dir=build_dir,
        verbose=False,
        platform=mock_platform,
        toolchain=mock_toolchain,
        mcu="esp32s3",
        framework_version="3.0.0",
        cache=mock_cache,
        compilation_executor=compilation_executor,
        # New consolidated fields
        framework=mock_framework,
        board_id="esp32-s3-devkitc-1",
        board_config=board_config,
        platform_config=platform_config,
        variant="esp32s3",
        core="esp32",
        user_build_flags=[],
        env_config={},
    )

    # Create ConfigurableCompiler with BuildContext
    compiler = ConfigurableCompiler(mock_context)

    # Mock get_include_paths to return our long paths
    compiler.get_include_paths = Mock(return_value=long_include_paths)

    # Mock trampoline generation to return short paths
    short_trampoline_paths = [Path(f"C:/inc/{i:03d}") for i in range(len(long_include_paths))]

    with patch.object(compilation_executor.trampoline_cache, "generate_trampolines", return_value=short_trampoline_paths) as mock_generate:
        # Compile a source file in parallel mode
        source_file = build_dir / "test.cpp"
        source_file.write_text("#include <Arduino.h>\nvoid setup() {}\nvoid loop() {}")

        # This should use trampolines!
        compiler.compile_source(source_file)

        # Verify job was submitted to queue
        assert mock_queue.submit_job.called, "Job should be submitted to queue in parallel mode"

        # Get the submitted job
        submitted_job = mock_queue.submit_job.call_args[0][0]
        cmd = submitted_job.compiler_cmd

        # Build the command string
        cmd_str = " ".join(cmd)

        # Calculate command length
        cmd_length = len(cmd_str)
        print("\n=== Parallel Mode Command Analysis ===")
        print(f"Command length: {cmd_length} characters")
        print(f"Number of include paths: {len(long_include_paths)}")
        print("Windows limit: 32,767 characters")

        # Check if trampolines were called
        if mock_generate.called:
            print("✅ Trampolines WERE generated")

            # Print first few include flags to debug
            include_flags = [arg for arg in cmd if arg.startswith("-I")]
            print("\nFirst 10 include flags in command:")
            for flag in include_flags[:10]:
                print(f"  {flag}")

            # Verify short paths are in command
            short_path_count = sum(1 for path in short_trampoline_paths if str(path).replace("\\", "/") in cmd_str)
            long_path_count = sum(1 for path in long_include_paths if str(path).replace("\\", "/") in cmd_str)
            print(f"\nShort trampoline paths in command: {short_path_count}/{len(short_trampoline_paths)}")
            print(f"Long original paths in command: {long_path_count}/{len(long_include_paths)}")

            assert short_path_count > 0, "Command should use short trampoline paths"
            assert long_path_count == 0, "Command should NOT contain long original paths"
        else:
            print("❌ Trampolines were NOT generated!")
            # Check if long paths are in command
            long_path_count = sum(1 for path in long_include_paths if str(path) in cmd_str)
            print(f"Long original paths in command: {long_path_count}/{len(long_include_paths)}")

            # This is the BUG - parallel mode doesn't use trampolines!
            pytest.fail("BUG DETECTED: Parallel compilation does not use header trampolines!")

        # Verify command is under Windows limit
        assert cmd_length < 32767, f"Command length ({cmd_length}) exceeds Windows limit (32,767)"


if __name__ == "__main__":
    pytest.main([__file__, "-v", "-s"])
