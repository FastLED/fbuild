"""ESP8266 Toolchain Management.

This module handles downloading, extracting, and managing the ESP8266 toolchain
(Xtensa LX106 GCC compiler) needed for ESP8266 builds.

Toolchain Structure (after extraction):
    toolchain-xtensa/
    ├── xtensa-lx106-elf/
    │   ├── bin/
    │   │   ├── xtensa-lx106-elf-gcc.exe
    │   │   ├── xtensa-lx106-elf-g++.exe
    │   │   ├── xtensa-lx106-elf-ar.exe
    │   │   ├── xtensa-lx106-elf-objcopy.exe
    │   │   └── ...
    │   ├── lib/
    │   └── include/
"""

from pathlib import Path
from typing import Any, Dict

from .cache import Cache
from .downloader import DownloadError, ExtractionError, PackageDownloader
from .package import IToolchain, PackageError
from .platform_utils import PlatformDetector
from .toolchain_binaries import ToolchainBinaryFinder
from .toolchain_metadata import ToolchainMetadataParser


class ToolchainErrorESP8266(PackageError):
    """Raised when ESP8266 toolchain operations fail."""

    pass


class ToolchainESP8266(IToolchain):
    """Manages ESP8266 toolchain download, extraction, and access.

    This class handles downloading and managing the Xtensa LX106 GCC toolchain
    for ESP8266 builds.
    """

    # Toolchain binary prefix
    TOOLCHAIN_PREFIX = "xtensa-lx106-elf"

    def __init__(
        self,
        cache: Cache,
        toolchain_url: str,
        show_progress: bool = True,
    ):
        """Initialize ESP8266 toolchain manager.

        Args:
            cache: Cache manager instance
            toolchain_url: URL to toolchain package (e.g., GitHub release ZIP)
            show_progress: Whether to show download/extraction progress
        """
        self.cache = cache
        self.toolchain_url = toolchain_url
        self.show_progress = show_progress
        self.downloader = PackageDownloader()
        self.metadata_parser = ToolchainMetadataParser()
        self.platform_detector = PlatformDetector()

        # Extract version from URL
        self.version = self._extract_version_from_url(toolchain_url)

        # Toolchain path will be determined after download
        self._toolchain_path: Path | None = None

    @staticmethod
    def _extract_version_from_url(url: str) -> str:
        """Extract version string from toolchain URL.

        Handles both GitHub release URLs and PlatformIO registry URLs:
        - GitHub: .../releases/download/{version}/filename
        - PlatformIO: .../download/{owner}/{type}/{name}/{version}/filename

        Args:
            url: Toolchain URL

        Returns:
            Version string
        """
        parts = url.split("/")

        # PlatformIO registry format:
        # .../download/platformio/tool/toolchain-xtensa/{version}/{filename}
        # The version is the second-to-last path segment (last is the filename)
        if "registry.platformio.org" in url and len(parts) >= 2:
            return parts[-2]

        # GitHub release format: .../releases/download/{version}/{filename}
        for i, part in enumerate(parts):
            if part == "download" and i + 1 < len(parts):
                return parts[i + 1].lstrip("v")

        # Fallback: use URL hash
        from .cache import Cache

        return Cache.hash_url(url)[:8]

    def ensure_package(self) -> Path:
        """Ensure toolchain is downloaded and extracted.

        Returns:
            Path to the extracted toolchain directory

        Raises:
            ToolchainErrorESP8266: If download or extraction fails
        """
        return self.ensure_toolchain()

    def ensure_toolchain(self) -> Path:
        """Ensure ESP8266 toolchain is downloaded and extracted.

        The toolchain is extracted so that binaries end up at
        ``toolchain_cache_path.parent / "bin" / bin / ...`` which is where
        ``ToolchainBinaryFinder.find_bin_dir()`` looks (``self.toolchain_path.parent / "bin"``).

        Returns:
            Path to the extracted toolchain directory

        Raises:
            ToolchainErrorESP8266: If download or extraction fails
        """
        if self._toolchain_path and self._toolchain_path.exists():
            # Verify bin dir actually exists before returning cached path
            bin_finder = ToolchainBinaryFinder(self._toolchain_path, self.TOOLCHAIN_PREFIX)
            if bin_finder.find_bin_dir() is not None:
                return self._toolchain_path

        try:
            import shutil
            from urllib.parse import urlparse

            # Get cache path for this toolchain
            toolchain_cache_path = self.cache.get_toolchain_path(self.toolchain_url, self.version)

            # The bin directory where find_bin_dir() looks: toolchain_path.parent / "bin"
            bin_dir = toolchain_cache_path.parent / "bin"

            # Check if already extracted correctly
            if bin_dir.exists():
                bin_finder = ToolchainBinaryFinder(toolchain_cache_path, self.TOOLCHAIN_PREFIX)
                if bin_finder.find_bin_dir() is not None:
                    self._toolchain_path = toolchain_cache_path
                    return toolchain_cache_path

            # Download archive
            bin_dir.mkdir(parents=True, exist_ok=True)
            toolchain_cache_path.mkdir(parents=True, exist_ok=True)

            archive_name = Path(urlparse(self.toolchain_url).path).name
            archive_path = bin_dir / archive_name

            if not archive_path.exists():
                self.downloader.download(self.toolchain_url, archive_path, show_progress=self.show_progress)

            # Extract to temp dir
            temp_extract = bin_dir / "temp_extract"
            temp_extract.mkdir(parents=True, exist_ok=True)

            self.downloader.extract_archive(archive_path, temp_extract, show_progress=self.show_progress)

            # extract_archive strips one top-level dir (e.g., toolchain-xtensa/).
            # If a second wrapper dir remains (e.g., xtensa-lx106-elf/), strip it
            # too so bin/, lib/ end up directly in temp_extract.
            contents = list(temp_extract.iterdir())
            if len(contents) == 1 and contents[0].is_dir():
                single_dir = contents[0]
                for child in list(single_dir.iterdir()):
                    shutil.move(str(child), str(temp_extract / child.name))
                single_dir.rmdir()

            # Now temp_extract contains bin/, lib/, include/, etc. directly.
            # Copy to bin_dir (the location that find_bin_dir() searches).
            for item in temp_extract.iterdir():
                dest = bin_dir / item.name
                if item.is_dir():
                    if dest.exists():
                        shutil.rmtree(dest)
                    shutil.copytree(item, dest)
                else:
                    if dest.exists():
                        dest.unlink()
                    shutil.copy2(item, dest)

            # Clean up temp directory
            if temp_extract.exists():
                shutil.rmtree(temp_extract, ignore_errors=True)

            self._toolchain_path = toolchain_cache_path
            return toolchain_cache_path

        except (DownloadError, ExtractionError) as e:
            raise ToolchainErrorESP8266(f"Failed to setup ESP8266 toolchain: {e}") from e

    def get_toolchain_path(self) -> Path:
        """Get the path to the extracted toolchain directory.

        Returns:
            Path to toolchain directory

        Raises:
            ToolchainErrorESP8266: If toolchain not yet downloaded
        """
        if not self._toolchain_path:
            raise ToolchainErrorESP8266("Toolchain not initialized - call ensure_toolchain() first")

        return self._toolchain_path

    def get_bin_dir(self) -> Path:
        """Get the path to the toolchain's bin/ directory.

        Returns:
            Path to bin directory containing compiler executables

        Raises:
            ToolchainErrorESP8266: If toolchain not downloaded or bin directory not found
        """
        toolchain_path = self.get_toolchain_path()

        # Try to find bin directory
        bin_finder = ToolchainBinaryFinder(toolchain_path, self.TOOLCHAIN_PREFIX)
        bin_dir = bin_finder.find_bin_dir()

        if not bin_dir:
            raise ToolchainErrorESP8266(f"Could not find toolchain bin directory in {toolchain_path}")

        return bin_dir

    def get_gcc_path(self) -> Path:
        """Get path to xtensa-lx106-elf-gcc executable.

        Returns:
            Path to gcc executable

        Raises:
            ToolchainErrorESP8266: If gcc executable not found
        """
        bin_dir = self.get_bin_dir()
        gcc_name = f"{self.TOOLCHAIN_PREFIX}-gcc"

        # Try with and without .exe extension
        gcc_path = bin_dir / gcc_name
        if gcc_path.exists():
            return gcc_path

        gcc_path = bin_dir / f"{gcc_name}.exe"
        if gcc_path.exists():
            return gcc_path

        raise ToolchainErrorESP8266(f"GCC executable not found: {gcc_name}")

    def get_gxx_path(self) -> Path:
        """Get path to xtensa-lx106-elf-g++ executable.

        Returns:
            Path to g++ executable

        Raises:
            ToolchainErrorESP8266: If g++ executable not found
        """
        bin_dir = self.get_bin_dir()
        gxx_name = f"{self.TOOLCHAIN_PREFIX}-g++"

        # Try with and without .exe extension
        gxx_path = bin_dir / gxx_name
        if gxx_path.exists():
            return gxx_path

        gxx_path = bin_dir / f"{gxx_name}.exe"
        if gxx_path.exists():
            return gxx_path

        raise ToolchainErrorESP8266(f"G++ executable not found: {gxx_name}")

    def get_ar_path(self) -> Path:
        """Get path to xtensa-lx106-elf-ar executable.

        Returns:
            Path to ar executable

        Raises:
            ToolchainErrorESP8266: If ar executable not found
        """
        bin_dir = self.get_bin_dir()
        ar_name = f"{self.TOOLCHAIN_PREFIX}-ar"

        # Try with and without .exe extension
        ar_path = bin_dir / ar_name
        if ar_path.exists():
            return ar_path

        ar_path = bin_dir / f"{ar_name}.exe"
        if ar_path.exists():
            return ar_path

        raise ToolchainErrorESP8266(f"AR executable not found: {ar_name}")

    def get_objcopy_path(self) -> Path:
        """Get path to xtensa-lx106-elf-objcopy executable.

        Returns:
            Path to objcopy executable

        Raises:
            ToolchainErrorESP8266: If objcopy executable not found
        """
        bin_dir = self.get_bin_dir()
        objcopy_name = f"{self.TOOLCHAIN_PREFIX}-objcopy"

        # Try with and without .exe extension
        objcopy_path = bin_dir / objcopy_name
        if objcopy_path.exists():
            return objcopy_path

        objcopy_path = bin_dir / f"{objcopy_name}.exe"
        if objcopy_path.exists():
            return objcopy_path

        raise ToolchainErrorESP8266(f"Objcopy executable not found: {objcopy_name}")

    def get_size_path(self) -> Path:
        """Get path to xtensa-lx106-elf-size executable.

        Returns:
            Path to size executable

        Raises:
            ToolchainErrorESP8266: If size executable not found
        """
        bin_dir = self.get_bin_dir()
        size_name = f"{self.TOOLCHAIN_PREFIX}-size"

        # Try with and without .exe extension
        size_path = bin_dir / size_name
        if size_path.exists():
            return size_path

        size_path = bin_dir / f"{size_name}.exe"
        if size_path.exists():
            return size_path

        raise ToolchainErrorESP8266(f"Size executable not found: {size_name}")

    def get_all_tools(self) -> Dict[str, Path]:
        """Get paths to all required tools.

        Returns:
            Dictionary mapping tool names to their paths
        """
        return {
            "gcc": self.get_gcc_path(),
            "gxx": self.get_gxx_path(),
            "ar": self.get_ar_path(),
            "objcopy": self.get_objcopy_path(),
            "size": self.get_size_path(),
        }

    def is_installed(self) -> bool:
        """Check if toolchain is already installed.

        Returns:
            True if toolchain directory exists with key files
        """
        if not self._toolchain_path or not self._toolchain_path.exists():
            return False

        # Verify bin directory and essential binaries exist
        try:
            bin_dir = self.get_bin_dir()
            required_binaries = [
                bin_dir / f"{self.TOOLCHAIN_PREFIX}-gcc",
                bin_dir / f"{self.TOOLCHAIN_PREFIX}-g++",
                bin_dir / f"{self.TOOLCHAIN_PREFIX}-ar",
            ]

            # Check with and without .exe extension
            for binary in required_binaries:
                if not (binary.exists() or binary.with_suffix(".exe").exists()):
                    return False

            return True

        except ToolchainErrorESP8266:
            return False

    def get_package_info(self) -> Dict[str, Any]:
        """Get information about the installed toolchain.

        Returns:
            Dictionary with toolchain information
        """
        return {
            "type": "toolchain",
            "name": "xtensa-lx106",
            "version": self.version,
            "path": str(self._toolchain_path) if self._toolchain_path else None,
            "url": self.toolchain_url,
            "prefix": self.TOOLCHAIN_PREFIX,
        }
