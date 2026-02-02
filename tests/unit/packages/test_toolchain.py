"""Unit tests for toolchain management."""

import os
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

import pytest

from fbuild.packages.cache import Cache
from fbuild.packages.toolchain import ToolchainAVR


class TestToolchain:
    """Test cases for Toolchain class."""

    def test_detect_platform_windows(self):
        """Test platform detection for Windows."""
        with patch("fbuild.packages.platform_utils.platform.system", return_value="Windows"):
            with patch("fbuild.packages.platform_utils.platform.machine", return_value="AMD64"):
                plat, arch = ToolchainAVR.detect_platform()
                assert plat == "windows"
                assert arch == "x86_64"

    def test_detect_platform_linux_x86_64(self):
        """Test platform detection for Linux x86_64."""
        with patch("fbuild.packages.platform_utils.platform.system", return_value="Linux"):
            with patch("fbuild.packages.platform_utils.platform.machine", return_value="x86_64"):
                plat, arch = ToolchainAVR.detect_platform()
                assert plat == "linux"
                assert arch == "x86_64"

    def test_detect_platform_linux_arm(self):
        """Test platform detection for Linux ARM."""
        with patch("fbuild.packages.platform_utils.platform.system", return_value="Linux"):
            with patch("fbuild.packages.platform_utils.platform.machine", return_value="aarch64"):
                plat, arch = ToolchainAVR.detect_platform()
                assert plat == "linux"
                assert arch == "aarch64"

    def test_detect_platform_macos(self):
        """Test platform detection for macOS."""
        with patch("fbuild.packages.platform_utils.platform.system", return_value="Darwin"):
            with patch("fbuild.packages.platform_utils.platform.machine", return_value="x86_64"):
                plat, arch = ToolchainAVR.detect_platform()
                assert plat == "darwin"
                assert arch == "x86_64"

    def test_detect_platform_unsupported(self):
        """Test error on unsupported platform."""
        # Import at function level to get current class after any module reloads
        import importlib

        toolchain_module = importlib.import_module("fbuild.packages.toolchain")
        importlib.reload(toolchain_module)

        with patch("fbuild.packages.platform_utils.platform.system", return_value="UnknownOS"):
            with pytest.raises(toolchain_module.ToolchainError, match="Unsupported platform"):
                toolchain_module.ToolchainAVR.detect_platform()

    def test_get_package_info_windows(self):
        """Test getting package info for Windows."""
        with patch("fbuild.packages.platform_utils.PlatformDetector.detect_avr_platform", return_value=("windows", "x86_64")):
            with tempfile.TemporaryDirectory() as temp_dir:
                cache = Cache(Path(temp_dir))
                toolchain = ToolchainAVR(cache)

                package_name, checksum = toolchain._get_package_details()
                assert package_name.endswith(".zip")
                assert "mingw32" in package_name
                assert checksum is not None

    def test_get_package_info_linux(self):
        """Test getting package info for Linux."""
        with patch("fbuild.packages.platform_utils.PlatformDetector.detect_avr_platform", return_value=("linux", "x86_64")):
            with tempfile.TemporaryDirectory() as temp_dir:
                cache = Cache(Path(temp_dir))
                toolchain = ToolchainAVR(cache)

                package_name, checksum = toolchain._get_package_details()
                assert package_name.endswith(".tar.bz2")
                assert "linux-gnu" in package_name
                assert checksum is not None

    def test_get_package_info_macos(self):
        """Test getting package info for macOS."""
        with patch("fbuild.packages.platform_utils.PlatformDetector.detect_avr_platform", return_value=("darwin", "x86_64")):
            with tempfile.TemporaryDirectory() as temp_dir:
                cache = Cache(Path(temp_dir))
                toolchain = ToolchainAVR(cache)

                package_name, checksum = toolchain._get_package_details()
                assert package_name.endswith(".tar.bz2")
                assert "darwin" in package_name
                assert checksum is not None

    def test_get_package_info_unsupported_platform(self):
        """Test error for unsupported platform."""
        # Import at function level to get current class after any module reloads
        import importlib

        toolchain_module = importlib.import_module("fbuild.packages.toolchain")
        importlib.reload(toolchain_module)

        with patch("fbuild.packages.platform_utils.PlatformDetector.detect_avr_platform", return_value=("freebsd", "x86_64")):
            with tempfile.TemporaryDirectory() as temp_dir:
                cache = Cache(Path(temp_dir))
                toolchain = toolchain_module.ToolchainAVR(cache)

                with pytest.raises(toolchain_module.ToolchainError, match="No toolchain package"):
                    toolchain._get_package_details()

    def test_verify_tools_success(self):
        """Test tool verification succeeds with all tools present."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            toolchain = ToolchainAVR(cache)

            # Create toolchain directory structure with all required components
            toolchain_path = Path(temp_dir) / "toolchain"

            # Create required directories
            bin_dir = toolchain_path / "bin"
            bin_dir.mkdir(parents=True)
            (toolchain_path / "avr" / "include" / "avr").mkdir(parents=True)
            (toolchain_path / "avr" / "lib").mkdir(parents=True)
            (toolchain_path / "lib" / "gcc" / "avr" / "7.3.0").mkdir(parents=True)

            # Create required executables
            exe_suffix = ".exe" if sys.platform == "win32" else ""
            for tool in ToolchainAVR.REQUIRED_TOOLS:
                (bin_dir / f"{tool}{exe_suffix}").touch()

            # Create required headers
            for header in ["io.h", "interrupt.h"]:
                (toolchain_path / "avr" / "include" / "avr" / header).touch()
            for header in ["stdio.h", "stdlib.h", "string.h"]:
                (toolchain_path / "avr" / "include" / header).touch()

            # Create required libraries
            (toolchain_path / "lib" / "gcc" / "avr" / "7.3.0" / "libgcc.a").touch()
            (toolchain_path / "avr" / "lib" / "libc.a").touch()
            (toolchain_path / "avr" / "lib" / "libm.a").touch()

            assert toolchain._verify_toolchain(toolchain_path)

    def test_verify_tools_missing_bin_dir(self):
        """Test tool verification fails when bin directory missing."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            toolchain = ToolchainAVR(cache)

            toolchain_path = Path(temp_dir) / "toolchain"
            toolchain_path.mkdir()

            assert not toolchain._verify_toolchain(toolchain_path)

    def test_verify_tools_missing_tool(self):
        """Test tool verification fails when a tool is missing."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            toolchain = ToolchainAVR(cache)

            toolchain_path = Path(temp_dir) / "toolchain"
            bin_dir = toolchain_path / "bin"
            bin_dir.mkdir(parents=True)

            # Create only some tools
            exe_suffix = ".exe" if sys.platform == "win32" else ""
            (bin_dir / f"avr-gcc{exe_suffix}").touch()

            assert not toolchain._verify_toolchain(toolchain_path)

    def test_get_tool_path_not_initialized(self):
        """Test error when getting tool path before initialization."""
        # Import at function level to get current class after any module reloads
        import importlib

        toolchain_module = importlib.import_module("fbuild.packages.toolchain")
        importlib.reload(toolchain_module)

        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            toolchain = toolchain_module.ToolchainAVR(cache)

            with pytest.raises(toolchain_module.ToolchainError, match="not initialized"):
                toolchain.get_tool_path("avr-gcc")

    def test_get_tool_path_success(self):
        """Test getting tool path after initialization."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            toolchain = ToolchainAVR(cache)

            # Setup a fake toolchain
            toolchain_path = Path(temp_dir) / "toolchain"
            bin_dir = toolchain_path / "bin"
            bin_dir.mkdir(parents=True)

            exe_suffix = ".exe" if sys.platform == "win32" else ""
            gcc_path = bin_dir / f"avr-gcc{exe_suffix}"
            gcc_path.touch()

            toolchain._toolchain_path = toolchain_path

            result = toolchain.get_tool_path("avr-gcc")
            assert result == gcc_path

    def test_get_tool_path_not_found(self):
        """Test error when tool doesn't exist."""
        # Import at function level to get current class after any module reloads
        import importlib

        toolchain_module = importlib.import_module("fbuild.packages.toolchain")
        importlib.reload(toolchain_module)

        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            toolchain = toolchain_module.ToolchainAVR(cache)

            toolchain_path = Path(temp_dir) / "toolchain"
            bin_dir = toolchain_path / "bin"
            bin_dir.mkdir(parents=True)

            toolchain._toolchain_path = toolchain_path

            with pytest.raises(toolchain_module.ToolchainError, match="Tool not found"):
                toolchain.get_tool_path("avr-gcc")

    def test_get_all_tools(self):
        """Test getting all tool paths."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            toolchain = ToolchainAVR(cache)

            # Setup a fake toolchain
            toolchain_path = Path(temp_dir) / "toolchain"
            bin_dir = toolchain_path / "bin"
            bin_dir.mkdir(parents=True)

            exe_suffix = ".exe" if sys.platform == "win32" else ""
            for tool in ToolchainAVR.REQUIRED_TOOLS:
                (bin_dir / f"{tool}{exe_suffix}").touch()

            toolchain._toolchain_path = toolchain_path

            tools = toolchain.get_all_tools()
            assert len(tools) == len(ToolchainAVR.REQUIRED_TOOLS)
            for tool_name in ToolchainAVR.REQUIRED_TOOLS:
                assert tool_name in tools
                assert tools[tool_name].exists()

    def test_ensure_toolchain_cached(self):
        """Test using cached toolchain."""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Set FBUILD_CACHE_DIR to use temp directory for cache
            old_cache_dir = os.environ.get("FBUILD_CACHE_DIR")
            os.environ["FBUILD_CACHE_DIR"] = temp_dir
            try:
                cache = Cache(Path(temp_dir))
                cache.ensure_directories()
                toolchain = ToolchainAVR(cache)

                # Create a fake cached toolchain with new URL-based structure
                toolchain_path = cache.get_toolchain_path(ToolchainAVR.BASE_URL, ToolchainAVR.VERSION)

                # Create required directories
                bin_dir = toolchain_path / "bin"
                bin_dir.mkdir(parents=True)
                (toolchain_path / "avr" / "include" / "avr").mkdir(parents=True)
                (toolchain_path / "avr" / "include").mkdir(parents=True, exist_ok=True)
                (toolchain_path / "avr" / "lib").mkdir(parents=True)
                (toolchain_path / "lib" / "gcc" / "avr" / "7.3.0").mkdir(parents=True)

                # Create required executables
                exe_suffix = ".exe" if sys.platform == "win32" else ""
                for tool in ToolchainAVR.REQUIRED_TOOLS:
                    (bin_dir / f"{tool}{exe_suffix}").touch()

                # Create required headers
                for header in ["io.h", "interrupt.h"]:
                    (toolchain_path / "avr" / "include" / "avr" / header).touch()
                for header in ["stdio.h", "stdlib.h", "string.h"]:
                    (toolchain_path / "avr" / "include" / header).touch()

                # Create required libraries
                (toolchain_path / "lib" / "gcc" / "avr" / "7.3.0" / "libgcc.a").touch()
                (toolchain_path / "avr" / "lib" / "libc.a").touch()
                (toolchain_path / "avr" / "lib" / "libm.a").touch()

                # Ensure toolchain (should use cache)
                result = toolchain.ensure_toolchain()
                assert result == toolchain_path
            finally:
                if old_cache_dir is None:
                    del os.environ["FBUILD_CACHE_DIR"]
                else:
                    os.environ["FBUILD_CACHE_DIR"] = old_cache_dir

    def test_architecture_fallback(self):
        """Test architecture fallback to x86_64."""
        with patch.object(ToolchainAVR, "detect_platform", return_value=("linux", "unknown_arch")):
            with tempfile.TemporaryDirectory() as temp_dir:
                cache = Cache(Path(temp_dir))
                toolchain = ToolchainAVR(cache)

                # Should fall back to x86_64
                package_name, _ = toolchain._get_package_details()
                assert "x86_64" in package_name
