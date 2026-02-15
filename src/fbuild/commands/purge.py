"""Purge command implementation for managing cached packages.

This module handles listing and deleting cached packages from the global
fbuild cache directory.
"""

import json
import os
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
from fbuild.output import log, log_error, log_success, log_warning


def get_cache_root() -> Path:
    """Get cache root directory respecting FBUILD_DEV_MODE.

    Returns:
        Path to cache root directory
    """
    cache_env = os.environ.get("FBUILD_CACHE_DIR")
    dev_mode = os.environ.get("FBUILD_DEV_MODE") == "1"

    if cache_env:
        return Path(cache_env).resolve()
    elif dev_mode:
        return Path.home() / ".fbuild" / "cache_dev"
    else:
        return Path.home() / ".fbuild" / "cache"


@dataclass
class PackageInfo:
    """Cached package information."""

    name: str
    type: str
    version: str
    path: Path
    size_bytes: int
    install_date: str
    metadata: dict[str, str]


class PackageDiscovery:
    """Discovers packages in cache directory."""

    def __init__(self, cache_root: Path):
        """Initialize package discovery.

        Args:
            cache_root: Root cache directory (e.g., ~/.fbuild/cache/)
        """
        self.cache_root = cache_root

    def discover_packages(self) -> list[PackageInfo]:
        """Walk cache and read manifests.

        Returns:
            List of discovered packages sorted by type then name
        """
        packages: list[PackageInfo] = []

        if not self.cache_root.exists():
            return packages

        # Cache structure: {type}/{url_hash}/{version}/
        # e.g., toolchains/4d17bc247ef03305/14.2.0/
        for type_dir in self.cache_root.iterdir():
            if not type_dir.is_dir():
                continue

            package_type = type_dir.name

            # Iterate through URL hash directories
            for url_hash_dir in type_dir.iterdir():
                if not url_hash_dir.is_dir():
                    continue

                # Iterate through version directories
                for version_dir in url_hash_dir.iterdir():
                    if not version_dir.is_dir():
                        continue

                    # Skip common subdirectories that aren't package roots
                    # (these are extracted package contents, not separate packages)
                    skip_dirs = {
                        "bin",
                        "lib",
                        "include",
                        "share",
                        "libexec",
                        "avr",
                        "xtensa-esp-elf",
                        "riscv32-esp-elf",
                        "i686-w64-mingw32",
                        "x86_64-w64-mingw32",
                        "cores",
                        "variants",
                        "boards",
                        "tools",
                        "examples",
                        "picolibc",
                    }
                    if version_dir.name in skip_dirs:
                        continue

                    # Try to read manifest
                    manifest_path = version_dir / "manifest.json"
                    if manifest_path.exists():
                        pkg_info = self._read_manifest(manifest_path, version_dir, package_type)
                    else:
                        # Legacy package without manifest
                        pkg_info = self._create_synthetic_manifest(version_dir, package_type)

                    if pkg_info:
                        packages.append(pkg_info)

        # Sort by type (toolchains, platforms, frameworks, libraries) then by name
        type_order = {"toolchains": 0, "platforms": 1, "frameworks": 2, "libraries": 3}
        packages.sort(key=lambda p: (type_order.get(p.type, 99), p.name.lower()))

        return packages

    def _read_manifest(self, manifest_path: Path, package_path: Path, package_type: str) -> Optional[PackageInfo]:
        """Read manifest.json and create PackageInfo.

        Args:
            manifest_path: Path to manifest.json
            package_path: Package installation directory
            package_type: Type from directory structure (toolchains, platforms, etc.)

        Returns:
            PackageInfo or None if manifest is invalid
        """
        try:
            with open(manifest_path, "r", encoding="utf-8") as f:
                manifest = json.load(f)

            size_bytes = self._calculate_dir_size(package_path)

            return PackageInfo(
                name=manifest.get("name", "Unknown"),
                type=manifest.get("type", package_type),
                version=manifest.get("version", "unknown"),
                path=package_path,
                size_bytes=size_bytes,
                install_date=manifest.get("install_date", "unknown"),
                metadata=manifest.get("metadata", {}),
            )
        except (json.JSONDecodeError, OSError, KeyError):
            # Invalid manifest, fallback to synthetic
            return self._create_synthetic_manifest(package_path, package_type)

    def _create_synthetic_manifest(self, package_path: Path, package_type: str) -> PackageInfo:
        """Create synthetic manifest for legacy packages.

        Args:
            package_path: Package installation directory
            package_type: Type from directory structure

        Returns:
            PackageInfo with synthetic data
        """
        # Use directory name as version
        version = package_path.name

        # Try to infer name from package type and path
        name = self._infer_package_name(package_path, package_type)

        size_bytes = self._calculate_dir_size(package_path)

        return PackageInfo(
            name=name,
            type=package_type,
            version=version,
            path=package_path,
            size_bytes=size_bytes,
            install_date="unknown",
            metadata={},
        )

    def _infer_package_name(self, package_path: Path, package_type: str) -> str:
        """Infer package name from path and type.

        Args:
            package_path: Package installation directory
            package_type: Type from directory structure

        Returns:
            Inferred human-readable name
        """
        # Check for common subdirectories/files to identify package
        if package_type == "toolchains":
            # Check for AVR or Xtensa toolchains
            bin_dir = package_path / "bin"
            if bin_dir.exists():
                # Look for toolchain binaries
                if list(bin_dir.glob("avr-gcc*")):
                    return "AVR-GCC Toolchain"
                elif list(bin_dir.glob("xtensa-*-gcc*")):
                    # Try to determine MCU from binary names
                    esp32_bins = list(bin_dir.glob("xtensa-esp32-*"))
                    if esp32_bins:
                        return "Xtensa ESP32 Toolchain"
                    return "Xtensa Toolchain"
                elif list(bin_dir.glob("riscv*-gcc*")):
                    return "RISC-V Toolchain"

        elif package_type == "platforms":
            # Check platform.json or boards directory
            platform_json = package_path / "platform.json"
            if platform_json.exists():
                try:
                    with open(platform_json, "r", encoding="utf-8") as f:
                        platform_data = json.load(f)
                        title = platform_data.get("title", "")
                        if title:
                            return f"{title} Platform"
                except KeyboardInterrupt as ke:
                    handle_keyboard_interrupt_properly(ke)
                except Exception:
                    pass

            # Check for common platform indicators
            if (package_path / "boards").exists():
                if list(package_path.glob("*esp32*")):
                    return "ESP32 Platform"
                elif list(package_path.glob("*avr*")):
                    return "AVR Platform"

        elif package_type == "frameworks":
            # Check for Arduino framework
            if (package_path / "cores" / "arduino").exists():
                return "Arduino Framework"
            elif (package_path / "tools" / "sdk").exists():
                return "ESP-IDF Framework"

        # Fallback to generic name
        return f"{package_type.rstrip('s').title()} Package"

    def _calculate_dir_size(self, directory: Path) -> int:
        """Calculate total size of directory in bytes.

        Args:
            directory: Directory to measure

        Returns:
            Total size in bytes
        """
        total_size = 0
        try:
            for dirpath, _, filenames in os.walk(directory):
                for filename in filenames:
                    filepath = Path(dirpath) / filename
                    try:
                        total_size += filepath.stat().st_size
                    except (OSError, FileNotFoundError):
                        # File might be deleted/inaccessible
                        continue
        except (OSError, PermissionError):
            # Directory not accessible
            pass

        return total_size

    @staticmethod
    def format_size(size_bytes: int) -> str:
        """Format bytes as KB/MB/GB.

        Args:
            size_bytes: Size in bytes

        Returns:
            Formatted string (e.g., "95.1 MB")
        """
        if size_bytes < 1024:
            return f"{size_bytes} B"
        elif size_bytes < 1024 * 1024:
            return f"{size_bytes / 1024:.1f} KB"
        elif size_bytes < 1024 * 1024 * 1024:
            return f"{size_bytes / (1024 * 1024):.1f} MB"
        else:
            return f"{size_bytes / (1024 * 1024 * 1024):.1f} GB"


class PackagePurger:
    """Handles package deletion."""

    def purge_packages(self, packages: list[PackageInfo], dry_run: bool) -> tuple[int, int, list[str]]:
        """Delete packages with error handling.

        Args:
            packages: List of packages to delete
            dry_run: If True, only show what would be deleted

        Returns:
            Tuple of (deleted_count, failed_count, error_messages)
        """
        deleted_count = 0
        failed_count = 0
        error_messages: list[str] = []

        for pkg in packages:
            try:
                if dry_run:
                    log(f"Would delete: {pkg.name} {pkg.version} ({PackageDiscovery.format_size(pkg.size_bytes)})")
                else:
                    # Delete the package directory
                    if pkg.path.exists():
                        shutil.rmtree(pkg.path)
                        log_success(f"Deleted: {pkg.name} {pkg.version} ({PackageDiscovery.format_size(pkg.size_bytes)})")
                        deleted_count += 1
                    else:
                        log_warning(f"Already deleted: {pkg.name} {pkg.version}")

            except (PermissionError, OSError) as e:
                error_msg = f"Failed to delete {pkg.name} {pkg.version}: {e}"
                log_error(error_msg)
                error_messages.append(error_msg)
                failed_count += 1

        return deleted_count, failed_count, error_messages


class EnvironmentMatcher:
    """Matches packages to environments."""

    def __init__(self, project_dir: Path):
        """Initialize environment matcher.

        Args:
            project_dir: Project directory containing platformio.ini
        """
        self.project_dir = project_dir
        self.platformio_ini = project_dir / "platformio.ini"

    def get_environment_packages(self, env_name: str, all_packages: list[PackageInfo]) -> list[PackageInfo]:
        """Filter packages that belong to environment.

        Args:
            env_name: Environment name from platformio.ini
            all_packages: List of all discovered packages

        Returns:
            Filtered list of packages for the environment
        """
        if not self.platformio_ini.exists():
            log_error(f"platformio.ini not found in {self.project_dir}")
            return []

        # Read platformio.ini to get environment configuration
        env_config = self._read_environment_config(env_name)
        if not env_config:
            log_error(f"Environment '{env_name}' not found in platformio.ini")
            return []

        # Extract platform, board, framework from config
        platform = env_config.get("platform", "")
        board = env_config.get("board", "")
        framework = env_config.get("framework", "")

        # Match packages based on metadata
        matched_packages: list[PackageInfo] = []

        for pkg in all_packages:
            if self._package_matches_environment(pkg, platform, board, framework):
                matched_packages.append(pkg)

        return matched_packages

    def _read_environment_config(self, env_name: str) -> dict[str, str]:
        """Read environment configuration from platformio.ini.

        Args:
            env_name: Environment name

        Returns:
            Dictionary of environment configuration
        """
        import configparser

        config = configparser.ConfigParser()
        try:
            config.read(self.platformio_ini)

            section_name = f"env:{env_name}"
            if section_name not in config:
                return {}

            return dict(config[section_name])

        except KeyboardInterrupt as ke:
            handle_keyboard_interrupt_properly(ke)
        except Exception:
            return {}

    def _package_matches_environment(self, pkg: PackageInfo, platform: str, board: str, framework: str) -> bool:
        """Check if package matches environment configuration.

        Args:
            pkg: Package to check
            platform: Platform from platformio.ini
            board: Board from platformio.ini
            framework: Framework from platformio.ini

        Returns:
            True if package matches environment
        """
        # Match platform packages
        if pkg.type == "platforms":
            # Check if platform name matches
            if platform and platform.lower() in pkg.name.lower():
                return True
            # Check metadata
            if "platform" in pkg.metadata and platform and pkg.metadata["platform"].lower() in platform.lower():
                return True

        # Match toolchain packages
        elif pkg.type == "toolchains":
            # Match by architecture/MCU in metadata
            if "mcu" in pkg.metadata:
                pkg_mcu = pkg.metadata["mcu"].lower()
                # Check if board starts with MCU prefix
                if board and board.lower().startswith(pkg_mcu):
                    return True

            if "architecture" in pkg.metadata:
                arch = pkg.metadata["architecture"].lower()
                # Match platform to architecture
                if platform:
                    if "esp32" in platform.lower() and "xtensa" in arch:
                        return True
                    elif "esp32c" in platform.lower() and "riscv" in arch:
                        return True
                    elif "atmelavr" in platform.lower() and "avr" in arch:
                        return True

        # Match framework packages
        elif pkg.type == "frameworks":
            if framework and framework.lower() in pkg.name.lower():
                return True

        return False


def purge_packages(target: Optional[str], dry_run: bool, project_dir: Path) -> bool:
    """Main entry point for purge command.

    Args:
        target: Target to purge ('all', environment name, or None to list)
        dry_run: If True, show what would be deleted without deleting
        project_dir: Project directory (for environment-specific purge)

    Returns:
        True if successful, False otherwise
    """
    # Get cache directory (respect FBUILD_DEV_MODE)
    cache_root = get_cache_root()

    # Discover all packages
    discovery = PackageDiscovery(cache_root)
    all_packages = discovery.discover_packages()

    # Handle different targets
    if target is None:
        # List mode: show all packages
        return _list_packages(all_packages, cache_root)

    elif target == "all":
        # Purge all packages
        return _purge_all_packages(all_packages, cache_root, dry_run)

    else:
        # Purge environment-specific packages
        return _purge_environment_packages(all_packages, target, project_dir, cache_root, dry_run)


def _list_packages(packages: list[PackageInfo], cache_root: Path) -> bool:
    """List all cached packages.

    Args:
        packages: List of discovered packages
        cache_root: Cache root directory

    Returns:
        False (to exit with code 1 as per requirements)
    """
    if not packages:
        log(f"No packages cached at {cache_root}")
        return False  # Exit with code 1 (list mode should always fail)

    # Calculate total size
    total_size = sum(pkg.size_bytes for pkg in packages)

    log(f"Cached Packages ({PackageDiscovery.format_size(total_size)} total):\n")

    # Group by type
    packages_by_type: dict[str, list[PackageInfo]] = {}
    for pkg in packages:
        pkg_type = pkg.type
        if pkg_type not in packages_by_type:
            packages_by_type[pkg_type] = []
        packages_by_type[pkg_type].append(pkg)

    # Display each type group
    type_labels = {
        "toolchains": "Toolchains",
        "platforms": "Platforms",
        "frameworks": "Frameworks",
        "libraries": "Libraries",
    }

    for pkg_type in ["toolchains", "platforms", "frameworks", "libraries"]:
        if pkg_type not in packages_by_type:
            print(f"{type_labels.get(pkg_type, pkg_type.title())} (0 packages):\n")
            continue

        type_packages = packages_by_type[pkg_type]
        type_size = sum(pkg.size_bytes for pkg in type_packages)

        print(f"{type_labels.get(pkg_type, pkg_type.title())} ({len(type_packages)} packages, {PackageDiscovery.format_size(type_size)}):")

        for pkg in type_packages:
            print(f"  • {pkg.name} {pkg.version} ({PackageDiscovery.format_size(pkg.size_bytes)})")
            if pkg.install_date != "unknown":
                print(f"    Installed: {pkg.install_date}")
            # Show relevant metadata
            if pkg.metadata:
                for key, value in pkg.metadata.items():
                    if key in ["mcu", "architecture", "platform"]:
                        print(f"    {key.title()}: {value}")
            print(f"    Path: {pkg.path}\n")

        print()

    print(f"Total: {len(packages)} packages, {PackageDiscovery.format_size(total_size)}")

    # Exit with code 1 as per requirements (list mode should fail)
    return False


def _purge_all_packages(packages: list[PackageInfo], cache_root: Path, dry_run: bool) -> bool:
    """Purge all global packages.

    Args:
        packages: List of all packages
        cache_root: Cache root directory
        dry_run: If True, only show what would be deleted

    Returns:
        True if successful
    """
    if not packages:
        log(f"No packages to purge at {cache_root}")
        return True

    if dry_run:
        log(f"Dry run: showing what would be deleted from {cache_root}\n")
    else:
        log(f"Purging all global packages at {cache_root}\n")

    # Purge packages
    purger = PackagePurger()
    deleted_count, failed_count, _ = purger.purge_packages(packages, dry_run)

    # Calculate total size
    total_size = sum(pkg.size_bytes for pkg in packages)

    # Print summary
    print()
    if dry_run:
        log(f"Total: {len(packages)} packages, {PackageDiscovery.format_size(total_size)} would be freed")
    else:
        if deleted_count > 0:
            log_success(f"Purged {deleted_count} packages, freed {PackageDiscovery.format_size(total_size)}")
        if failed_count > 0:
            log_error(f"Failed to delete {failed_count} packages")

    return failed_count == 0


def _purge_environment_packages(packages: list[PackageInfo], env_name: str, project_dir: Path, cache_root: Path, dry_run: bool) -> bool:
    """Purge packages for specific environment.

    Args:
        packages: List of all packages
        env_name: Environment name
        project_dir: Project directory
        cache_root: Cache root directory
        dry_run: If True, only show what would be deleted

    Returns:
        True if successful
    """
    # Match packages to environment
    matcher = EnvironmentMatcher(project_dir)
    env_packages = matcher.get_environment_packages(env_name, packages)

    if not env_packages:
        log(f"No packages found for environment '{env_name}'")
        return True

    if dry_run:
        log(f"Dry run: showing what would be deleted for environment '{env_name}'\n")
    else:
        log(f"Purging packages for environment '{env_name}'\n")

    # Purge packages
    purger = PackagePurger()
    deleted_count, failed_count, _ = purger.purge_packages(env_packages, dry_run)

    # Calculate total size
    total_size = sum(pkg.size_bytes for pkg in env_packages)

    # Print summary
    print()
    if dry_run:
        log(f"Total: {len(env_packages)} packages, {PackageDiscovery.format_size(total_size)} would be freed")
    else:
        if deleted_count > 0:
            log_success(f"Purged {deleted_count} packages, freed {PackageDiscovery.format_size(total_size)}")
        if failed_count > 0:
            log_error(f"Failed to delete {failed_count} packages")

    return failed_count == 0
