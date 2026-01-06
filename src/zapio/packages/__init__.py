"""Package management for Zapio.

This module handles downloading, caching, and managing external packages
including toolchains, platforms, and libraries.
"""

from .arduino_core import ArduinoCore, ArduinoCoreError
from .cache import Cache
from .downloader import ChecksumError, DownloadError, ExtractionError, PackageDownloader
from .toolchain import Toolchain, ToolchainError

__all__ = [
    "Cache",
    "PackageDownloader",
    "DownloadError",
    "ChecksumError",
    "ExtractionError",
    "Toolchain",
    "ToolchainError",
    "ArduinoCore",
    "ArduinoCoreError",
]
