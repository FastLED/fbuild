"""PlatformIO Registry client for downloading libraries.

This module provides access to the PlatformIO registry API for resolving
and downloading library dependencies.
"""

import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

import requests

from fbuild.packages.downloader import PackageDownloader


class RegistryError(Exception):
    """Exception raised for registry-related errors."""

    pass


@dataclass
class LibrarySpec:
    """Parsed library specification from platformio.ini lib_deps."""

    owner: str
    name: str
    version: Optional[str] = None
    is_local: bool = False
    local_path: Optional[Path] = None

    @classmethod
    def parse(cls, spec: str) -> "LibrarySpec":
        """Parse a library specification string.

        Supports formats:
        - owner/name@version (e.g., fastled/FastLED@^3.7.8)
        - owner/name (e.g., fastled/FastLED)
        - name@version (e.g., FastLED@^3.7.8)
        - name (e.g., FastLED)
        - URL (e.g., https://github.com/FastLED/FastLED)
        - file:// path (e.g., file://../../fastled9)
        - Relative path (e.g., ../../fastled9)

        Args:
            spec: Library specification string

        Returns:
            LibrarySpec instance

        Raises:
            RegistryError: If spec format is invalid
        """
        # Handle file:// URLs - local library paths
        if spec.startswith("file://"):
            path_str = spec.replace("file://", "", 1)
            local_path = Path(path_str)
            # Extract library name from path
            name = local_path.name
            return cls(owner="", name=name, version=None, is_local=True, local_path=local_path)

        # Handle URLs - convert to owner/name format
        if spec.startswith("http://") or spec.startswith("https://"):
            # Extract owner/name from GitHub URL
            match = re.search(r"github\.com/([^/]+)/([^/]+?)(?:\.git)?/?$", spec)
            if match:
                owner, name = match.groups()
                return cls(owner=owner, name=name, version=None)
            raise RegistryError(f"Cannot parse URL as library spec: {spec}")

        # Handle relative/absolute paths (../foo, ./foo, /abs/path, C:/path)
        # IMPORTANT: Check this AFTER URLs and BEFORE registry specs
        # to avoid confusing "owner/name" registry specs with paths
        is_path = (
            spec.startswith(".")  # Relative paths like ./foo, ../bar
            or spec.startswith("/")  # Absolute Unix paths
            or spec.startswith("\\")  # Absolute Windows paths (UNC)
            or (len(spec) > 2 and spec[1] == ":")  # Windows drive letter (C:/, D:/)
        )
        if is_path:
            local_path = Path(spec)
            name = local_path.name
            return cls(owner="", name=name, version=None, is_local=True, local_path=local_path)

        # Parse owner/name@version format (registry specs)
        # This must come AFTER path checking to avoid treating "owner/name" as a path
        if "@" in spec:
            lib_part, version = spec.rsplit("@", 1)
        else:
            lib_part = spec
            version = None

        # Split owner/name
        if "/" in lib_part:
            owner, name = lib_part.split("/", 1)
        else:
            # If no owner specified, we'll need to search registry
            owner = ""
            name = lib_part

        return cls(owner=owner, name=name, version=version)

    def __str__(self) -> str:
        """String representation."""
        result = f"{self.owner}/{self.name}" if self.owner else self.name
        if self.version:
            result += f"@{self.version}"
        return result


@dataclass
class LibraryVersion:
    """Information about a specific library version."""

    version: str
    download_url: str
    homepage: Optional[str] = None
    repository: Optional[str] = None


class PlatformIORegistry:
    """Client for PlatformIO registry API."""

    API_URL = "https://api.registry.platformio.org/v3"

    def __init__(self, downloader: Optional[PackageDownloader] = None):
        """Initialize registry client.

        Args:
            downloader: Optional package downloader instance
        """
        self.downloader = downloader or PackageDownloader()

    def search_library(self, name: str) -> Optional[str]:
        """Search for a library by name to find its owner.

        Args:
            name: Library name

        Returns:
            Owner name if found, None otherwise
        """
        try:
            # Use search API
            search_url = f"{self.API_URL}/search"
            response = requests.get(search_url, params={"query": name}, timeout=10)
            response.raise_for_status()

            result = response.json()
            items = result.get("items", [])

            if items:
                # Return first match owner
                first = items[0]
                owner_info = first.get("owner", {})
                if isinstance(owner_info, dict):
                    return owner_info.get("username")
                return owner_info

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            print(f"Warning: Could not search for library {name}: {e}")

        return None

    def get_library_info(self, owner: str, name: str) -> dict:
        """Get library information from registry using search API.

        Args:
            owner: Library owner (user/org name)
            name: Library name

        Returns:
            Library information dictionary

        Raises:
            RegistryError: If library not found or API error
        """
        try:
            # Build query
            query = f"{owner}/{name}".lower() if owner else name.lower()

            search_url = f"{self.API_URL}/search"
            response = requests.get(search_url, params={"query": query}, timeout=10)
            response.raise_for_status()

            result = response.json()
            items = result.get("items", [])

            # Find exact match
            for item in items:
                item_owner = item.get("owner", {}).get("username", "").lower()
                item_name = item.get("name", "").lower()

                # Exact match
                if item_name == name.lower():
                    if not owner or item_owner == owner.lower():
                        return item

            # No match found
            raise RegistryError(f"Library '{owner}/{name}' not found in registry")

        except requests.RequestException as e:
            raise RegistryError(f"Registry API error: {e}") from e

    def resolve_version(self, owner: str, name: str, version_spec: Optional[str] = None) -> LibraryVersion:
        """Resolve a version specification to a specific version.

        Args:
            owner: Library owner
            name: Library name
            version_spec: Version specification (e.g., "^3.7.8", "3.7.8", "latest")
                         If None, uses latest version

        Returns:
            LibraryVersion with download URL

        Raises:
            RegistryError: If version cannot be resolved
        """
        info = self.get_library_info(owner, name)

        # Get version info from search result (latest version)
        version_info = info.get("version")
        if not version_info:
            raise RegistryError(f"No version information available for {owner}/{name}")

        # For now, use the latest version from search results
        # TODO: Implement proper version constraint matching by querying version history
        version_str = version_info.get("name")
        if not version_str:
            raise RegistryError(f"No version name for {owner}/{name}")

        files = version_info.get("files", [])
        if not files:
            raise RegistryError(f"No download files available for {owner}/{name}@{version_str}")

        # Use first file (should be .tar.gz)
        download_url = files[0].get("download_url")
        if not download_url:
            raise RegistryError(f"No download URL for {owner}/{name}@{version_str}")

        return LibraryVersion(
            version=version_str,
            download_url=download_url,
            homepage=None,
            repository=None,
        )

    def download_library(self, spec: LibrarySpec, dest_dir: Path, show_progress: bool = True) -> Path:
        """Download a library from the registry.

        Args:
            spec: Library specification
            dest_dir: Destination directory for extraction
            show_progress: Whether to show download progress

        Returns:
            Path to extracted library directory

        Raises:
            RegistryError: If download fails
        """
        # Resolve owner if not specified
        owner = spec.owner
        if not owner:
            owner = self.search_library(spec.name)
            if not owner:
                raise RegistryError(f"Could not find owner for library '{spec.name}'")

        # Resolve version
        lib_version = self.resolve_version(owner, spec.name, spec.version)

        if show_progress:
            print(f"Downloading {owner}/{spec.name}@{lib_version.version}")

        # Download archive
        dest_dir.mkdir(parents=True, exist_ok=True)
        archive_path = dest_dir / "library.tar.gz"

        try:
            self.downloader.download(lib_version.download_url, archive_path, show_progress=show_progress)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise RegistryError(f"Failed to download library: {e}") from e

        # Extract archive
        if show_progress:
            print(f"Extracting {spec.name}...")

        extract_dir = dest_dir / "_extract"
        extract_dir.mkdir(exist_ok=True)

        try:
            self.downloader.extract_archive(archive_path, extract_dir, show_progress=show_progress)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise RegistryError(f"Failed to extract library: {e}") from e

        # Find the actual library directory
        # Archives often have a top-level directory
        extracted_items = list(extract_dir.iterdir())

        if len(extracted_items) == 1 and extracted_items[0].is_dir():
            src_dir = extracted_items[0]
        else:
            src_dir = extract_dir

        # Move to final location
        final_dir = dest_dir / "src"
        if final_dir.exists():
            import shutil

            shutil.rmtree(final_dir)

        src_dir.rename(final_dir)

        # Clean up
        if extract_dir.exists():
            import shutil

            shutil.rmtree(extract_dir)
        archive_path.unlink()

        # Save library info
        info_file = dest_dir / "library.json"
        with open(info_file, "w", encoding="utf-8") as f:
            json.dump(
                {
                    "name": spec.name,
                    "owner": owner,
                    "version": lib_version.version,
                    "download_url": lib_version.download_url,
                    "repository": lib_version.repository,
                    "homepage": lib_version.homepage,
                },
                f,
                indent=2,
            )

        return final_dir
