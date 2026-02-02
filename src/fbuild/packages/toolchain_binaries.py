"""Toolchain Binary Finder Utilities.

This module provides utilities for locating and verifying toolchain binaries
in the toolchain installation directory.

Binary Naming Conventions:
    - AVR: avr-gcc, avr-g++, avr-ar, avr-objcopy, etc.
    - RISC-V ESP32: riscv32-esp-elf-gcc, riscv32-esp-elf-g++, etc.
    - Xtensa ESP32: xtensa-esp32-elf-gcc, xtensa-esp32-elf-g++, etc.

Directory Structure:
    The toolchain binaries are typically located in:
    - toolchain_path/bin/bin/ (nested bin directory after extraction)
    - toolchain_path/bin/{toolchain_name}/bin/ (e.g., bin/riscv32-esp-elf/bin/)
"""

from pathlib import Path
from typing import Dict, List, Optional


class BinaryNotFoundError(Exception):
    """Raised when a required toolchain binary is not found."""

    pass


class ToolchainBinaryFinder:
    """Finds and verifies toolchain binaries in the installation directory."""

    def __init__(self, toolchain_path: Path, binary_prefix: str):
        """Initialize the binary finder.

        Args:
            toolchain_path: Base path to the toolchain installation
            binary_prefix: Binary name prefix (e.g., "avr", "riscv32-esp-elf", "xtensa-esp32-elf")
        """
        self.toolchain_path = toolchain_path
        self.binary_prefix = binary_prefix

    def find_bin_dir(self) -> Optional[Path]:
        """Find the bin directory containing toolchain binaries.

        Searches for binaries in common locations:
        1. toolchain_path/bin/bin/ (nested bin, common after extraction)
        2. toolchain_path/bin/{toolchain_name}/bin/ (nested toolchain directory)
        3. toolchain_path/bin/ (direct bin directory)

        Returns:
            Path to bin directory, or None if not found
        """
        # The toolchain structure is: toolchain_path/bin/bin/
        # (after extraction, the toolchain extracts to a subdirectory,
        # and we copy it to toolchain_path/bin/)
        bin_parent = self.toolchain_path.parent / "bin"

        if not bin_parent.exists():
            return None

        # Check for bin/bin/ (most common after extraction)
        bin_dir = bin_parent / "bin"
        if bin_dir.exists() and bin_dir.is_dir():
            # Verify it has binaries
            binaries = list(bin_dir.glob("*.exe")) or list(bin_dir.glob("*-gcc"))
            if binaries:
                return bin_dir

        # Look for nested toolchain directory (e.g., bin/riscv32-esp-elf/bin/)
        for item in bin_parent.iterdir():
            if item.is_dir() and "esp" in item.name.lower():
                nested_bin = item / "bin"
                if nested_bin.exists():
                    return nested_bin

        # Check if bin_parent itself has binaries
        binaries = list(bin_parent.glob("*.exe")) or list(bin_parent.glob("*-gcc"))
        if binaries:
            return bin_parent

        return None

    def find_binary(self, binary_name: str) -> Optional[Path]:
        """Find a specific binary in the toolchain bin directory.

        Args:
            binary_name: Name of the binary without prefix (e.g., "gcc", "g++", "ar")

        Returns:
            Path to the binary, or None if not found
        """
        bin_dir = self.find_bin_dir()
        if bin_dir is None or not bin_dir.exists():
            return None

        # Construct full binary name with prefix
        binary_with_prefix = f"{self.binary_prefix}-{binary_name}"

        # Check both with and without .exe extension (Windows compatibility)
        for ext in [".exe", ""]:
            binary_path = bin_dir / f"{binary_with_prefix}{ext}"
            if binary_path.exists():
                return binary_path

        return None

    def find_all_binaries(self, binary_names: List[str]) -> Dict[str, Optional[Path]]:
        """Find multiple binaries at once.

        Args:
            binary_names: List of binary names without prefix (e.g., ["gcc", "g++", "ar"])

        Returns:
            Dictionary mapping binary names to their paths (None if not found)
        """
        return {name: self.find_binary(name) for name in binary_names}

    def get_common_tool_paths(self) -> Dict[str, Optional[Path]]:
        """Get paths to common toolchain binaries.

        Returns:
            Dictionary mapping tool names to their paths
        """
        common_tools = ["gcc", "g++", "ar", "gcc-ar", "gcc-ranlib", "objcopy", "size", "objdump"]
        return self.find_all_binaries(common_tools)

    def verify_binary_exists(self, binary_name: str) -> bool:
        """Verify that a specific binary exists.

        Args:
            binary_name: Name of the binary without prefix

        Returns:
            True if binary exists and is a file
        """
        binary_path = self.find_binary(binary_name)
        return binary_path is not None and binary_path.exists() and binary_path.is_file()

    def verify_required_binaries(self, required_binaries: List[str]) -> tuple[bool, List[str]]:
        """Verify that all required binaries exist.

        Args:
            required_binaries: List of required binary names without prefix

        Returns:
            Tuple of (all_found, missing_binaries)
            - all_found: True if all binaries were found
            - missing_binaries: List of binary names that were not found
        """
        missing = []
        for binary_name in required_binaries:
            if not self.verify_binary_exists(binary_name):
                missing.append(binary_name)

        return len(missing) == 0, missing

    def verify_installation(self) -> bool:
        """Verify that the toolchain is properly installed.

        Checks for essential binaries: gcc, g++, ar, objcopy

        Returns:
            True if all essential binaries are present

        Raises:
            BinaryNotFoundError: If essential binaries are missing
        """
        required_tools = ["gcc", "g++", "ar", "objcopy"]
        all_found, missing = self.verify_required_binaries(required_tools)

        if not all_found:
            raise BinaryNotFoundError(f"Toolchain installation incomplete. Missing binaries: {', '.join(missing)}")

        return True

    def get_gcc_path(self) -> Optional[Path]:
        """Get path to GCC compiler."""
        return self.find_binary("gcc")

    def get_gxx_path(self) -> Optional[Path]:
        """Get path to G++ compiler."""
        return self.find_binary("g++")

    def get_ar_path(self) -> Optional[Path]:
        """Get path to archiver (ar)."""
        return self.find_binary("ar")

    def get_objcopy_path(self) -> Optional[Path]:
        """Get path to objcopy utility."""
        return self.find_binary("objcopy")

    def get_size_path(self) -> Optional[Path]:
        """Get path to size utility."""
        return self.find_binary("size")

    def get_objdump_path(self) -> Optional[Path]:
        """Get path to objdump utility."""
        return self.find_binary("objdump")

    def get_gcc_ar_path(self) -> Optional[Path]:
        """Get path to gcc-ar (LTO-aware archiver).

        gcc-ar is a wrapper around ar that works with LTO bytecode objects.
        It ensures proper symbol table generation for archives containing
        objects compiled with -flto -fno-fat-lto-objects.
        """
        return self.find_binary("gcc-ar")

    def get_gcc_ranlib_path(self) -> Optional[Path]:
        """Get path to gcc-ranlib (LTO-aware ranlib).

        gcc-ranlib is a wrapper around ranlib that works with LTO bytecode objects.
        It updates the symbol table of archives containing LTO objects.
        """
        return self.find_binary("gcc-ranlib")
