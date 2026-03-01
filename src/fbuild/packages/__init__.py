"""Package management for fbuild.

This module handles downloading, caching, and managing external packages
including toolchains, platforms, and libraries.
"""

from fbuild.packages.archive_extractors import ArchiveExtractorFactory, BaseArchiveExtractor, IArchiveExtractor, TarGzExtractor, TarXzExtractor, ZipExtractor
from fbuild.packages.archive_strategies import DirectoryMover, FileOperations, IRetryStrategy, UnixRetryStrategy, WindowsRetryStrategy
from fbuild.packages.archive_utils import ArchiveExtractionError, ArchiveExtractor, URLVersionExtractor
from fbuild.packages.arduino_core import ArduinoCore, ArduinoCoreError
from fbuild.packages.cache import Cache
from fbuild.packages.concurrent_manager import ConcurrentPackageManager, PackageLockError, PackageResult, PackageSpec
from fbuild.packages.downloader import ChecksumError, DownloadError, ExtractionError, PackageDownloader
from fbuild.packages.fingerprint import FingerprintRegistry, PackageFingerprint
from fbuild.packages.github_utils import GitHubURLOptimizer
from fbuild.packages.library_compiler import LibraryCompilationError, LibraryCompiler
from fbuild.packages.package import IFramework, IPackage, PackageError
from fbuild.packages.package import IToolchain as BaseToolchain
from fbuild.packages.platform_esp32 import PlatformErrorESP32, PlatformESP32
from fbuild.packages.platform_utils import PlatformDetector, PlatformError
from fbuild.packages.sdk_utils import SDKPathResolver
from fbuild.packages.toolchain import ToolchainAVR as Toolchain
from fbuild.packages.toolchain import ToolchainError
from fbuild.packages.toolchain_binaries import BinaryNotFoundError, ToolchainBinaryFinder
from fbuild.packages.toolchain_metadata import MetadataParseError, ToolchainMetadataParser

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
    # Archive extraction strategies (advanced usage)
    "IRetryStrategy",
    "WindowsRetryStrategy",
    "UnixRetryStrategy",
    "FileOperations",
    "DirectoryMover",
    "IArchiveExtractor",
    "BaseArchiveExtractor",
    "TarXzExtractor",
    "TarGzExtractor",
    "ZipExtractor",
    "ArchiveExtractorFactory",
]
