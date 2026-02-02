"""Cache management for fbuild packages.

This module provides a unified cache structure for storing downloaded
packages, toolchains, platforms, and build artifacts.

Cache Structure:
    .fbuild/
    ├── cache/
    │   ├── packages/
    │   │   └── {url_hash}/         # SHA256 hash of base URL
    │   │       └── {version}/      # Version string
    │   │           └── archive     # Downloaded archive
    │   ├── toolchains/
    │   │   └── {url_hash}/         # SHA256 hash of base URL
    │   │       └── {version}/      # Version string
    │   │           └── bin/        # Extracted toolchain binaries
    │   ├── platforms/
    │   │   └── {url_hash}/         # SHA256 hash of base URL
    │   │       └── {version}/      # Version string
    │   │           ├── cores/
    │   │           ├── variants/
    │   │           ├── boards.txt
    │   │           └── platform.txt
    │   ├── libraries/
    │   │   └── {url_hash}/         # SHA256 hash of base URL
    │   │       └── {version}/      # Version string
    │   └── trampolines/
    │       └── {mcu}/              # MCU variant (esp32c6, esp32c3, etc.)
    │           └── {hash}/         # SHA256 hash of paths+version+mcu
    │               ├── 000/
    │               ├── 001/
    │               └── .metadata.json
    └── build/
        └── {env_name}/             # Build output per environment
            ├── core/               # Compiled core objects
            ├── src/                # Compiled sketch objects
            └── firmware.*          # Final firmware files

This structure ensures that different versions from the same URL don't
stomp on each other, and allows multiple sources to coexist.
"""

import hashlib
import os
from pathlib import Path
from typing import Optional


class Cache:
    """Manages the fbuild cache directory structure.

    The cache can be located in the project directory (.fbuild/) or in a
    global location specified by the FBUILD_CACHE_DIR environment variable.

    Uses URL hashing to organize cached items, preventing version conflicts.
    """

    def __init__(self, project_dir: Optional[Path] = None):
        """Initialize cache manager.

        Args:
            project_dir: Project directory. If None, uses current directory.

        Cache location priority:
        1. FBUILD_CACHE_DIR environment variable (explicit override)
        2. Dev mode (FBUILD_DEV_MODE=1): ~/.fbuild/cache_dev/ (isolated from prod)
        3. Production: ~/.fbuild/cache/ (global)
        """
        if project_dir is None:
            project_dir = Path.cwd()

        self.project_dir = Path(project_dir).resolve()

        # Check for environment variable override
        cache_env = os.environ.get("FBUILD_CACHE_DIR")
        build_env = os.environ.get("FBUILD_BUILD_DIR")
        dev_mode = os.environ.get("FBUILD_DEV_MODE") == "1"

        if cache_env:
            # Explicit cache directory override
            self.cache_root = Path(cache_env).resolve()
        elif dev_mode:
            # Development mode: use global ~/.fbuild/cache_dev/ (isolated from prod cache)
            self.cache_root = Path.home() / ".fbuild" / "cache_dev"
        else:
            # Production: use global ~/.fbuild/cache/
            self.cache_root = Path.home() / ".fbuild" / "cache"

        if build_env:
            # Explicit build directory override (useful for Windows path length limits)
            self.build_root = Path(build_env).resolve()
        else:
            # Default: project-local build directory
            self.build_root = self.project_dir / ".fbuild" / "build"

    @staticmethod
    def hash_url(url: str) -> str:
        """Generate a SHA256 hash of a URL for cache directory naming.

        Args:
            url: The base URL to hash

        Returns:
            First 16 characters of SHA256 hash (sufficient for uniqueness)
        """
        return hashlib.sha256(url.encode("utf-8")).hexdigest()[:16]

    @property
    def packages_dir(self) -> Path:
        """Directory for downloaded package archives."""
        return self.cache_root / "packages"

    @property
    def toolchains_dir(self) -> Path:
        """Directory for extracted toolchain binaries."""
        return self.cache_root / "toolchains"

    @property
    def platforms_dir(self) -> Path:
        """Directory for extracted platform cores."""
        return self.cache_root / "platforms"

    @property
    def libraries_dir(self) -> Path:
        """Directory for downloaded libraries."""
        return self.cache_root / "libraries"

    @property
    def trampolines_dir(self) -> Path:
        """Directory for header trampoline caches."""
        return self.cache_root / "trampolines"

    def get_build_dir(self, env_name: str) -> Path:
        """Get build directory for a specific environment.

        Args:
            env_name: Environment name (e.g., 'uno', 'mega')

        Returns:
            Path to the environment's build directory
        """
        return self.build_root / env_name

    def get_core_build_dir(self, env_name: str) -> Path:
        """Get directory for compiled core objects.

        Args:
            env_name: Environment name

        Returns:
            Path to core build directory
        """
        return self.get_build_dir(env_name) / "core"

    def get_src_build_dir(self, env_name: str) -> Path:
        """Get directory for compiled sketch objects.

        Args:
            env_name: Environment name

        Returns:
            Path to sketch build directory
        """
        return self.get_build_dir(env_name) / "src"

    def ensure_directories(self) -> None:
        """Create all cache directories if they don't exist."""
        for directory in [
            self.packages_dir,
            self.toolchains_dir,
            self.platforms_dir,
            self.libraries_dir,
            self.trampolines_dir,
        ]:
            directory.mkdir(parents=True, exist_ok=True)

    def ensure_build_directories(self, env_name: str) -> None:
        """Create build directories for a specific environment.

        Args:
            env_name: Environment name
        """
        for directory in [
            self.get_build_dir(env_name),
            self.get_core_build_dir(env_name),
            self.get_src_build_dir(env_name),
        ]:
            directory.mkdir(parents=True, exist_ok=True)

    def clean_build(self, env_name: str) -> None:
        """Remove all build artifacts for an environment.

        Args:
            env_name: Environment name
        """
        import shutil

        build_dir = self.get_build_dir(env_name)
        if build_dir.exists():
            shutil.rmtree(build_dir)

    def get_package_path(self, url: str, version: str, filename: str) -> Path:
        """Get path where a package archive would be stored.

        Args:
            url: Base URL for the package source
            version: Version string (e.g., '7.3.0-atmel3.6.1-arduino7')
            filename: Archive filename (e.g., 'avr-gcc-7.3.0.tar.bz2')

        Returns:
            Path to the package archive
        """
        url_hash = self.hash_url(url)
        return self.packages_dir / url_hash / version / filename

    def get_toolchain_path(self, url: str, version: str) -> Path:
        """Get path where a toolchain would be extracted.

        Args:
            url: Base URL for the toolchain source
            version: Version string (e.g., '7.3.0-atmel3.6.1-arduino7')

        Returns:
            Path to the extracted toolchain directory
        """
        url_hash = self.hash_url(url)
        return self.toolchains_dir / url_hash / version

    def get_platform_path(self, url: str, version: str) -> Path:
        """Get path where a platform would be extracted.

        Args:
            url: Base URL for the platform source
            version: Version string (e.g., '1.8.6')

        Returns:
            Path to the extracted platform directory
        """
        url_hash = self.hash_url(url)
        return self.platforms_dir / url_hash / version

    def is_package_cached(self, url: str, version: str, filename: str) -> bool:
        """Check if a package is already downloaded.

        Args:
            url: Base URL for the package source
            version: Version string
            filename: Archive filename

        Returns:
            True if package exists in cache
        """
        return self.get_package_path(url, version, filename).exists()

    def is_toolchain_cached(self, url: str, version: str) -> bool:
        """Check if a toolchain is already extracted.

        Args:
            url: Base URL for the toolchain source
            version: Version string

        Returns:
            True if toolchain exists in cache
        """
        toolchain_path = self.get_toolchain_path(url, version)
        return toolchain_path.exists() and toolchain_path.is_dir()

    def is_platform_cached(self, url: str, version: str) -> bool:
        """Check if a platform is already extracted.

        Args:
            url: Base URL for the platform source
            version: Version string

        Returns:
            True if platform exists in cache
        """
        platform_path = self.get_platform_path(url, version)
        return platform_path.exists() and platform_path.is_dir()
