"""Unit tests for toolchain metadata parser."""

import json

from fbuild.packages.toolchain_metadata import ToolchainMetadataParser


def test_get_expected_binary_name_xtensa(tmp_path):
    """Test extracting expected binary name for Xtensa toolchain."""
    tools_json = tmp_path / "tools.json"
    tools_json.write_text(
        json.dumps(
            {
                "tools": [
                    {
                        "name": "toolchain-xtensa-esp-elf",
                        "version_cmd": ["xtensa-esp-elf-gcc", "--version"],
                    }
                ]
            }
        )
    )

    parser = ToolchainMetadataParser()
    binary_name = parser.get_expected_binary_name(tools_json, "toolchain-xtensa-esp-elf")

    assert binary_name == "xtensa-esp-elf-gcc"


def test_get_expected_binary_name_riscv(tmp_path):
    """Test extracting expected binary name for RISC-V toolchain."""
    tools_json = tmp_path / "tools.json"
    tools_json.write_text(
        json.dumps(
            {
                "tools": [
                    {
                        "name": "toolchain-riscv32-esp",
                        "version_cmd": ["riscv32-esp-elf-gcc", "--version"],
                    }
                ]
            }
        )
    )

    parser = ToolchainMetadataParser()
    binary_name = parser.get_expected_binary_name(tools_json, "toolchain-riscv32-esp")

    assert binary_name == "riscv32-esp-elf-gcc"


def test_get_expected_binary_name_file_not_found(tmp_path):
    """Test that None is returned when tools.json doesn't exist."""
    parser = ToolchainMetadataParser()
    binary_name = parser.get_expected_binary_name(tmp_path / "nonexistent.json", "toolchain-xtensa-esp-elf")

    assert binary_name is None


def test_get_expected_binary_name_toolchain_not_found(tmp_path):
    """Test that None is returned when toolchain not found in tools.json."""
    tools_json = tmp_path / "tools.json"
    tools_json.write_text(
        json.dumps(
            {
                "tools": [
                    {
                        "name": "toolchain-other",
                        "version_cmd": ["other-gcc", "--version"],
                    }
                ]
            }
        )
    )

    parser = ToolchainMetadataParser()
    binary_name = parser.get_expected_binary_name(tools_json, "toolchain-xtensa-esp-elf")

    assert binary_name is None


def test_get_expected_binary_name_no_version_cmd(tmp_path):
    """Test that None is returned when version_cmd is missing."""
    tools_json = tmp_path / "tools.json"
    tools_json.write_text(
        json.dumps(
            {
                "tools": [
                    {
                        "name": "toolchain-xtensa-esp-elf",
                        # version_cmd is missing
                    }
                ]
            }
        )
    )

    parser = ToolchainMetadataParser()
    binary_name = parser.get_expected_binary_name(tools_json, "toolchain-xtensa-esp-elf")

    assert binary_name is None


def test_get_expected_binary_name_empty_version_cmd(tmp_path):
    """Test that None is returned when version_cmd is empty."""
    tools_json = tmp_path / "tools.json"
    tools_json.write_text(
        json.dumps(
            {
                "tools": [
                    {
                        "name": "toolchain-xtensa-esp-elf",
                        "version_cmd": [],  # Empty array
                    }
                ]
            }
        )
    )

    parser = ToolchainMetadataParser()
    binary_name = parser.get_expected_binary_name(tools_json, "toolchain-xtensa-esp-elf")

    assert binary_name is None


def test_get_expected_binary_name_invalid_json(tmp_path):
    """Test that None is returned when tools.json has invalid JSON."""
    tools_json = tmp_path / "tools.json"
    tools_json.write_text("{ invalid json")

    parser = ToolchainMetadataParser()
    binary_name = parser.get_expected_binary_name(tools_json, "toolchain-xtensa-esp-elf")

    assert binary_name is None


def test_get_expected_binary_name_multiple_tools(tmp_path):
    """Test extracting binary name when multiple tools are present."""
    tools_json = tmp_path / "tools.json"
    tools_json.write_text(
        json.dumps(
            {
                "tools": [
                    {
                        "name": "toolchain-riscv32-esp",
                        "version_cmd": ["riscv32-esp-elf-gcc", "--version"],
                    },
                    {
                        "name": "toolchain-xtensa-esp-elf",
                        "version_cmd": ["xtensa-esp-elf-gcc", "--version"],
                    },
                ]
            }
        )
    )

    parser = ToolchainMetadataParser()

    # Should find the correct toolchain
    binary_name_xtensa = parser.get_expected_binary_name(tools_json, "toolchain-xtensa-esp-elf")
    binary_name_riscv = parser.get_expected_binary_name(tools_json, "toolchain-riscv32-esp")

    assert binary_name_xtensa == "xtensa-esp-elf-gcc"
    assert binary_name_riscv == "riscv32-esp-elf-gcc"
