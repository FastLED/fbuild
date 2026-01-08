"""Abstract base classes for package management.

This module defines the interface for platform-specific package managers
(toolchains, frameworks, cores, etc.) to ensure consistent behavior across
different platforms.
"""

from abc import ABC, abstractmethod
from pathlib import Path
from typing import Any, Dict, Optional


class PackageError(Exception):
    """Base exception for package management errors."""

    pass


class IPackage(ABC):
    """Interface for downloadable packages.

    This interface defines the common contract for all package types:
    - Toolchains (AVR, RISC-V, Xtensa)
    - Frameworks (Arduino-ESP32)
    - Cores (Arduino AVR Core)
    - Platforms (ESP32 Platform)
    """

    @abstractmethod
    def ensure_package(self) -> Path:
        """Ensure package is downloaded and extracted.

        Returns:
            Path to the extracted package directory

        Raises:
            PackageError: If download or extraction fails
        """
        pass

    @abstractmethod
    def is_installed(self) -> bool:
        """Check if package is already installed.

        Returns:
            True if package directory exists and is valid
        """
        pass

    @abstractmethod
    def get_package_info(self) -> Dict[str, Any]:
        """Get information about the package.

        Returns:
            Dictionary with package metadata (version, path, etc.)
        """
        pass


class IToolchain(IPackage):
    """Interface for toolchain packages.

    Toolchains provide compiler, linker, and binary utilities for
    specific architectures (AVR, RISC-V, Xtensa).
    """

    @abstractmethod
    def get_gcc_path(self) -> Optional[Path]:
        """Get path to GCC compiler.

        Returns:
            Path to gcc binary or None if not found
        """
        pass

    @abstractmethod
    def get_gxx_path(self) -> Optional[Path]:
        """Get path to G++ compiler.

        Returns:
            Path to g++ binary or None if not found
        """
        pass

    @abstractmethod
    def get_ar_path(self) -> Optional[Path]:
        """Get path to archiver (ar).

        Returns:
            Path to ar binary or None if not found
        """
        pass

    @abstractmethod
    def get_objcopy_path(self) -> Optional[Path]:
        """Get path to objcopy utility.

        Returns:
            Path to objcopy binary or None if not found
        """
        pass

    @abstractmethod
    def get_size_path(self) -> Optional[Path]:
        """Get path to size utility.

        Returns:
            Path to size binary or None if not found
        """
        pass

    @abstractmethod
    def get_bin_dir(self) -> Optional[Path]:
        """Get path to toolchain bin directory.

        Returns:
            Path to bin directory containing compiler binaries
        """
        pass

    @abstractmethod
    def get_all_tools(self) -> Dict[str, Path]:
        """Get paths to all required tools.

        Returns:
            Dictionary mapping tool names to their paths
        """
        pass


class IFramework(IPackage):
    """Interface for framework packages.

    Frameworks provide core Arduino implementation, variants,
    and built-in libraries for specific platforms.
    """

    @abstractmethod
    def get_cores_dir(self) -> Path:
        """Get path to cores directory.

        Returns:
            Path to cores directory containing Arduino core implementation
        """
        pass

    @abstractmethod
    def get_variants_dir(self) -> Path:
        """Get path to variants directory.

        Returns:
            Path to variants directory containing board-specific configurations
        """
        pass

    @abstractmethod
    def get_libraries_dir(self) -> Path:
        """Get path to built-in libraries directory.

        Returns:
            Path to libraries directory
        """
        pass
