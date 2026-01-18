"""Package management for fbuild.

This module handles downloading, caching, and managing external packages
including toolchains, platforms, and libraries.
"""

from .archive_utils import ArchiveExtractionError, ArchiveExtractor, URLVersionExtractor
from .arduino_core import ArduinoCore, ArduinoCoreError
from .cache import Cache
from .concurrent_manager import (
    ConcurrentPackageManager,
    PackageLockError,
    PackageResult,
    PackageSpec,
)
from .downloader import ChecksumError, DownloadError, ExtractionError, PackageDownloader
from .fingerprint import FingerprintRegistry, PackageFingerprint
from .github_utils import GitHubURLOptimizer
from .library_compiler import LibraryCompilationError, LibraryCompiler
from .package import IFramework, IPackage
from .package import IToolchain as BaseToolchain
from .package import PackageError
from .platform_esp32 import PlatformErrorESP32, PlatformESP32
from .platform_utils import PlatformDetector, PlatformError
from .sdk_utils import SDKPathResolver
from .toolchain import ToolchainAVR as Toolchain
from .toolchain import ToolchainError
from .toolchain_binaries import BinaryNotFoundError, ToolchainBinaryFinder
from .toolchain_metadata import MetadataParseError, ToolchainMetadataParser

__all__ = [
    "IPackage",
    "BaseToolchain",
    "IFramework",
    "PackageError",
    "Cache",
    "PackageDownloader",
    "DownloadError",
    "ChecksumError",
    "ExtractionError",
    "Toolchain",
    "ToolchainError",
    "ArduinoCore",
    "ArduinoCoreError",
    "PlatformESP32",
    "PlatformErrorESP32",
    "GitHubURLOptimizer",
    "LibraryCompiler",
    "LibraryCompilationError",
    "ArchiveExtractor",
    "ArchiveExtractionError",
    "URLVersionExtractor",
    "SDKPathResolver",
    "PlatformDetector",
    "PlatformError",
    "ToolchainBinaryFinder",
    "BinaryNotFoundError",
    "ToolchainMetadataParser",
    "MetadataParseError",
    "PackageFingerprint",
    "FingerprintRegistry",
    "ConcurrentPackageManager",
    "PackageSpec",
    "PackageResult",
    "PackageLockError",
]
