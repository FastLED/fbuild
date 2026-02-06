"""Unit tests for ESP32 toolchain binary prefix discovery integration."""

from unittest.mock import patch

from fbuild.packages.cache import Cache
from fbuild.packages.toolchain_esp32 import ToolchainESP32


def test_binary_prefix_discovery_after_install(tmp_path, monkeypatch):
    """Test that binary prefix is updated after toolchain installation."""
    # Use isolated cache in tmp_path
    monkeypatch.setenv("FBUILD_CACHE_DIR", str(tmp_path / "cache"))
    cache = Cache(tmp_path)

    toolchain_url = "https://example.com/xtensa-esp-elf-1.0.0.zip"

    # Create fake installed toolchain
    toolchain_path = cache.get_toolchain_path(toolchain_url, "1.0.0")
    toolchain_path.mkdir(parents=True, exist_ok=True)  # Create toolchain_path
    bin_dir = toolchain_path.parent / "bin" / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    (bin_dir / "xtensa-esp32-elf-gcc.exe").touch()
    (bin_dir / "xtensa-esp32-elf-g++.exe").touch()
    (bin_dir / "xtensa-esp32-elf-ar.exe").touch()
    (bin_dir / "xtensa-esp32-elf-objcopy.exe").touch()

    # Mock the downloader to avoid actual downloads
    with patch("fbuild.packages.toolchain_esp32.PackageDownloader"):
        toolchain = ToolchainESP32(cache, toolchain_url, "xtensa-esp-elf", show_progress=False)

        # Should discover "xtensa-esp32-elf" from actual binary
        assert toolchain.binary_prefix == "xtensa-esp32-elf"
        assert toolchain._discovered_prefix == "xtensa-esp32-elf"


def test_binary_prefix_fallback_when_not_installed(tmp_path, monkeypatch):
    """Test that hardcoded mapping is used when toolchain is not installed."""
    # Use isolated cache in tmp_path
    monkeypatch.setenv("FBUILD_CACHE_DIR", str(tmp_path / "cache"))
    cache = Cache(tmp_path)
    toolchain_url = "https://example.com/xtensa-esp-elf-1.0.0.zip"

    with patch("fbuild.packages.toolchain_esp32.PackageDownloader"):
        toolchain = ToolchainESP32(cache, toolchain_url, "xtensa-esp-elf", show_progress=False)

        # Should use fallback from TOOLCHAIN_NAMES
        assert toolchain.binary_prefix == "xtensa-esp32-elf"
        assert toolchain._discovered_prefix is None


def test_binary_prefix_discovery_for_riscv(tmp_path, monkeypatch):
    """Test binary prefix discovery for RISC-V toolchain."""
    # Use isolated cache in tmp_path
    monkeypatch.setenv("FBUILD_CACHE_DIR", str(tmp_path / "cache"))
    cache = Cache(tmp_path)
    toolchain_url = "https://example.com/riscv32-esp-elf-1.0.0.zip"

    # Create fake installed toolchain
    toolchain_path = cache.get_toolchain_path(toolchain_url, "1.0.0")
    toolchain_path.mkdir(parents=True, exist_ok=True)  # Create toolchain_path
    bin_dir = toolchain_path.parent / "bin" / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    (bin_dir / "riscv32-esp-elf-gcc.exe").touch()
    (bin_dir / "riscv32-esp-elf-g++.exe").touch()
    (bin_dir / "riscv32-esp-elf-ar.exe").touch()
    (bin_dir / "riscv32-esp-elf-objcopy.exe").touch()

    with patch("fbuild.packages.toolchain_esp32.PackageDownloader"):
        toolchain = ToolchainESP32(cache, toolchain_url, "riscv32-esp", show_progress=False)

        # Should discover "riscv32-esp-elf" from actual binary
        assert toolchain.binary_prefix == "riscv32-esp-elf"
        assert toolchain._discovered_prefix == "riscv32-esp-elf"


def test_binary_prefix_updated_on_ensure_toolchain(tmp_path, monkeypatch):
    """Test that binary prefix is updated when calling ensure_toolchain on cached installation."""
    # Use isolated cache in tmp_path
    monkeypatch.setenv("FBUILD_CACHE_DIR", str(tmp_path / "cache"))
    cache = Cache(tmp_path)
    toolchain_url = "https://example.com/xtensa-esp-elf-1.0.0.zip"

    # Create fake installed toolchain
    toolchain_path = cache.get_toolchain_path(toolchain_url, "1.0.0")
    toolchain_path.mkdir(parents=True, exist_ok=True)  # Create toolchain_path
    bin_dir = toolchain_path.parent / "bin" / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    (bin_dir / "xtensa-esp32-elf-gcc.exe").touch()
    (bin_dir / "xtensa-esp32-elf-g++.exe").touch()
    (bin_dir / "xtensa-esp32-elf-ar.exe").touch()
    (bin_dir / "xtensa-esp32-elf-objcopy.exe").touch()

    with patch("fbuild.packages.toolchain_esp32.PackageDownloader"):
        toolchain = ToolchainESP32(cache, toolchain_url, "xtensa-esp-elf", show_progress=False)

        # Reset discovered prefix to simulate fresh instance
        toolchain._discovered_prefix = None
        toolchain.binary_prefix = toolchain.TOOLCHAIN_NAMES["xtensa-esp-elf"]
        toolchain.binary_finder.binary_prefix = toolchain.TOOLCHAIN_NAMES["xtensa-esp-elf"]

        # Call ensure_toolchain (should find cached installation and update prefix)
        result = toolchain.ensure_toolchain()

        # Should have discovered the prefix
        assert toolchain.binary_prefix == "xtensa-esp32-elf"
        assert toolchain._discovered_prefix == "xtensa-esp32-elf"
        assert result == toolchain_path


def test_binary_prefix_discovery_failure_uses_fallback(tmp_path, monkeypatch):
    """Test that fallback is used when binary discovery fails."""
    # Use isolated cache in tmp_path
    monkeypatch.setenv("FBUILD_CACHE_DIR", str(tmp_path / "cache"))
    cache = Cache(tmp_path)
    toolchain_url = "https://example.com/xtensa-esp-elf-1.0.0.zip"

    # Create fake installed toolchain but WITHOUT gcc binary
    toolchain_path = cache.get_toolchain_path(toolchain_url, "1.0.0")
    toolchain_path.mkdir(parents=True, exist_ok=True)  # Create toolchain_path
    bin_dir = toolchain_path.parent / "bin" / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    # Create only a non-gcc binary (discovery should fail - it specifically looks for gcc)
    (bin_dir / "xtensa-esp32-elf-ar.exe").touch()

    with patch("fbuild.packages.toolchain_esp32.PackageDownloader"):
        toolchain = ToolchainESP32(cache, toolchain_url, "xtensa-esp-elf", show_progress=False)

        # Should fall back to hardcoded mapping since discovery fails
        assert toolchain.binary_prefix == "xtensa-esp32-elf"
        # But discovered prefix should be None since discovery failed
        assert toolchain._discovered_prefix is None
