"""ESP32 Toolchain Metadata Parser.

This module handles downloading and parsing ESP32 toolchain metadata packages
to extract platform-specific toolchain URLs from tools.json.

Metadata Structure:
    The metadata package contains a tools.json file with the following structure:
    {
        "tools": [
            {
                "name": "toolchain-riscv32-esp",
                "versions": [
                    {
                        "win64": {"url": "...", "sha256": "..."},
                        "linux-amd64": {"url": "...", "sha256": "..."},
                        ...
                    }
                ]
            }
        ]
    }
"""

import json
import shutil
from pathlib import Path
from typing import Optional

from .downloader import PackageDownloader


class MetadataParseError(Exception):
    """Raised when metadata parsing fails."""

    pass


class ToolchainMetadataParser:
    """Parses ESP32 toolchain metadata to extract platform-specific URLs."""

    def __init__(self, downloader: Optional[PackageDownloader] = None):
        """Initialize the metadata parser.

        Args:
            downloader: Optional PackageDownloader instance. If not provided, creates a new one.
        """
        self.downloader = downloader or PackageDownloader()

    def download_and_extract_metadata(
        self,
        metadata_url: str,
        metadata_path: Path,
        show_progress: bool = True,
    ) -> Path:
        """Download and extract metadata package.

        Args:
            metadata_url: URL to the metadata package (ZIP file)
            metadata_path: Path where the metadata should be extracted
            show_progress: Whether to show download/extraction progress

        Returns:
            Path to the extracted metadata directory

        Raises:
            MetadataParseError: If download or extraction fails
        """
        if metadata_path.exists():
            return metadata_path

        try:
            if show_progress:
                print("Downloading toolchain metadata...")

            # Download metadata archive
            archive_name = Path(metadata_url).name
            archive_path = metadata_path.parent / archive_name

            if not archive_path.exists():
                archive_path.parent.mkdir(parents=True, exist_ok=True)
                self.downloader.download(
                    metadata_url, archive_path, show_progress=show_progress
                )

            # Extract metadata to temp directory
            temp_extract = metadata_path.parent / "temp_metadata"
            temp_extract.mkdir(parents=True, exist_ok=True)

            self.downloader.extract_archive(
                archive_path, temp_extract, show_progress=False
            )

            # Move to final location
            if metadata_path.exists():
                shutil.rmtree(metadata_path)

            temp_extract.rename(metadata_path)

            return metadata_path

        except KeyboardInterrupt as ke:
            from zapio.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise MetadataParseError(f"Failed to download metadata: {e}")

    def parse_tools_json(
        self,
        tools_json_path: Path,
        toolchain_name: str,
        platform: str,
    ) -> str:
        """Parse tools.json to extract platform-specific toolchain URL.

        Args:
            tools_json_path: Path to tools.json file
            toolchain_name: Name of the toolchain (e.g., "toolchain-riscv32-esp")
            platform: Platform identifier (e.g., "win64", "linux-amd64")

        Returns:
            URL to the platform-specific toolchain archive

        Raises:
            MetadataParseError: If parsing fails or platform/toolchain not found
        """
        if not tools_json_path.exists():
            raise MetadataParseError(f"tools.json not found at {tools_json_path}")

        try:
            with open(tools_json_path, "r") as f:
                tools_data = json.load(f)
        except json.JSONDecodeError as e:
            raise MetadataParseError(f"Invalid JSON in tools.json: {e}")

        # Find the toolchain tool
        tools = tools_data.get("tools", [])
        for tool in tools:
            if tool.get("name") == toolchain_name:
                # Get versions
                versions = tool.get("versions", [])
                if not versions:
                    raise MetadataParseError(f"No versions found for {toolchain_name}")

                # Use the first version (usually the recommended one)
                version_info = versions[0]

                # Get URL for the specified platform
                if platform not in version_info:
                    available_platforms = list(version_info.keys())
                    raise MetadataParseError(
                        f"Platform {platform} not supported for {toolchain_name}. Available platforms: {available_platforms}"
                    )

                platform_info = version_info[platform]
                return platform_info["url"]

        raise MetadataParseError(f"Toolchain {toolchain_name} not found in tools.json")

    def get_platform_url(
        self,
        metadata_url: str,
        metadata_path: Path,
        toolchain_name: str,
        platform: str,
        show_progress: bool = True,
    ) -> str:
        """Download metadata and extract platform-specific toolchain URL.

        This is a convenience method that combines downloading, extracting, and parsing.

        Args:
            metadata_url: URL to the metadata package
            metadata_path: Path where metadata should be extracted
            toolchain_name: Name of the toolchain (e.g., "toolchain-riscv32-esp")
            platform: Platform identifier (e.g., "win64", "linux-amd64")
            show_progress: Whether to show progress messages

        Returns:
            URL to the platform-specific toolchain archive

        Raises:
            MetadataParseError: If any step fails
        """
        # Download and extract metadata
        extracted_path = self.download_and_extract_metadata(
            metadata_url, metadata_path, show_progress
        )

        # Parse tools.json
        tools_json_path = extracted_path / "tools.json"
        return self.parse_tools_json(tools_json_path, toolchain_name, platform)
