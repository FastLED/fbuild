"""Unit tests for toolchain binary discovery functionality."""

from fbuild.packages.toolchain_binaries import ToolchainBinaryFinder


def test_discover_binary_prefix_riscv(tmp_path):
    """Test binary prefix discovery for RISC-V toolchain."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "riscv32-esp-elf-gcc.exe").touch()

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "placeholder")
    discovered = finder.discover_binary_prefix()

    assert discovered == "riscv32-esp-elf"


def test_discover_binary_prefix_xtensa(tmp_path):
    """Test binary prefix discovery for Xtensa toolchain."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "xtensa-esp32-elf-gcc.exe").touch()

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "placeholder")
    discovered = finder.discover_binary_prefix()

    assert discovered == "xtensa-esp32-elf"


def test_discover_binary_prefix_xtensa_no_extension(tmp_path):
    """Test binary prefix discovery for Xtensa toolchain without .exe extension (Linux/macOS)."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "xtensa-esp32-elf-gcc").touch()

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "placeholder")
    discovered = finder.discover_binary_prefix()

    assert discovered == "xtensa-esp32-elf"


def test_discover_binary_prefix_not_found(tmp_path):
    """Test binary discovery when bin directory doesn't exist."""
    finder = ToolchainBinaryFinder(tmp_path / "nonexistent", "fallback")
    discovered = finder.discover_binary_prefix()

    assert discovered is None


def test_discover_binary_prefix_no_gcc(tmp_path):
    """Test binary discovery when gcc binary is missing."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "some-other-tool.exe").touch()

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "fallback")
    discovered = finder.discover_binary_prefix()

    assert discovered is None


def test_discover_binary_prefix_verbose(tmp_path, capsys):
    """Test binary discovery with verbose output."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "riscv32-esp-elf-gcc.exe").touch()

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "placeholder")
    discovered = finder.discover_binary_prefix(verbose=True)

    assert discovered == "riscv32-esp-elf"
    captured = capsys.readouterr()
    assert "Discovered binary prefix: riscv32-esp-elf" in captured.out


def test_discover_binary_prefix_multiple_binaries(tmp_path):
    """Test binary discovery when multiple gcc variants exist."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "riscv32-esp-elf-gcc.exe").touch()
    (bin_dir / "xtensa-esp32-elf-gcc.exe").touch()

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "placeholder")
    discovered = finder.discover_binary_prefix()

    # Should return first match found
    assert discovered in ["riscv32-esp-elf", "xtensa-esp32-elf"]


def test_discover_binary_prefix_with_expected_name(tmp_path):
    """Test binary discovery with expected binary name from tools.json."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    # Create multiple binaries
    (bin_dir / "xtensa-esp-elf-gcc.exe").touch()  # Generic (from tools.json)
    (bin_dir / "xtensa-esp32-elf-gcc.exe").touch()  # Chip-specific
    (bin_dir / "xtensa-esp32s2-elf-gcc.exe").touch()  # Chip-specific
    (bin_dir / "xtensa-esp32s3-elf-gcc.exe").touch()  # Chip-specific

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "placeholder")

    # Should prefer the expected binary name
    discovered = finder.discover_binary_prefix(expected_binary_name="xtensa-esp-elf-gcc")

    assert discovered == "xtensa-esp-elf"


def test_discover_binary_prefix_expected_not_found_fallback(tmp_path):
    """Test binary discovery falls back to scanning when expected binary not found."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "xtensa-esp32-elf-gcc.exe").touch()

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "placeholder")

    # Expected binary doesn't exist, should fall back to scanning
    discovered = finder.discover_binary_prefix(expected_binary_name="xtensa-esp-elf-gcc")

    assert discovered == "xtensa-esp32-elf"


def test_discover_binary_prefix_expected_riscv(tmp_path):
    """Test binary discovery with expected binary name for RISC-V."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "riscv32-esp-elf-gcc.exe").touch()

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "placeholder")
    discovered = finder.discover_binary_prefix(expected_binary_name="riscv32-esp-elf-gcc")

    assert discovered == "riscv32-esp-elf"


def test_discover_binary_prefix_expected_no_extension(tmp_path):
    """Test binary discovery with expected name without .exe (Linux/macOS)."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "xtensa-esp-elf-gcc").touch()  # No .exe extension

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "placeholder")
    discovered = finder.discover_binary_prefix(expected_binary_name="xtensa-esp-elf-gcc")

    assert discovered == "xtensa-esp-elf"


def test_discover_binary_prefix_doesnt_match_gcc_suffix_binaries(tmp_path):
    """Test that discovery doesn't incorrectly match binaries like gcc-ar or gcc-ranlib."""
    bin_dir = tmp_path / "bin" / "bin"
    bin_dir.mkdir(parents=True)
    (bin_dir / "xtensa-esp32-elf-gcc-ar.exe").touch()
    (bin_dir / "xtensa-esp32-elf-gcc-ranlib.exe").touch()
    (bin_dir / "xtensa-esp32-elf-gcc.exe").touch()  # This should be matched

    finder = ToolchainBinaryFinder(tmp_path / "metadata", "placeholder")
    discovered = finder.discover_binary_prefix()

    # Should match only the exact gcc binary, not gcc-ar or gcc-ranlib
    assert discovered == "xtensa-esp32-elf"
