"""Test that framework extraction handles multiple archive formats.

The bug: _download_and_extract_to_temp() in framework_esp32.py hardcoded
`tarfile.open(archive_path, "r:xz")` which fails when the framework or
skeleton library archives are in .tar.gz or .zip format.

CI error: "tarfile.ReadError: not an lzma file"
"""

import io
import tarfile
import zipfile
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from fbuild.packages.cache import Cache
from fbuild.packages.framework_esp32 import FrameworkESP32


def _create_tar_xz_archive(archive_path: Path, content_dir_name: str = "framework") -> None:
    """Create a valid .tar.xz archive with a single top-level directory."""
    import os

    with tarfile.open(archive_path, "w:xz") as tar:
        # Create a directory entry
        dirinfo = tarfile.TarInfo(name=content_dir_name)
        dirinfo.type = tarfile.DIRTYPE
        dirinfo.mode = 0o755
        tar.addfile(dirinfo)

        # Create a file with random data (incompressible) to exceed 1024-byte validation
        fileinfo = tarfile.TarInfo(name=f"{content_dir_name}/package.json")
        data = os.urandom(4096)
        fileinfo.size = len(data)
        tar.addfile(fileinfo, io.BytesIO(data))


def _create_tar_gz_archive(archive_path: Path, content_dir_name: str = "framework") -> None:
    """Create a valid .tar.gz archive with a single top-level directory."""
    import os

    with tarfile.open(archive_path, "w:gz") as tar:
        dirinfo = tarfile.TarInfo(name=content_dir_name)
        dirinfo.type = tarfile.DIRTYPE
        dirinfo.mode = 0o755
        tar.addfile(dirinfo)

        fileinfo = tarfile.TarInfo(name=f"{content_dir_name}/package.json")
        data = os.urandom(4096)
        fileinfo.size = len(data)
        tar.addfile(fileinfo, io.BytesIO(data))


def _create_zip_archive(archive_path: Path, content_dir_name: str = "framework") -> None:
    """Create a valid .zip archive with a single top-level directory."""
    import os

    with zipfile.ZipFile(archive_path, "w") as zf:
        zf.writestr(f"{content_dir_name}/package.json", os.urandom(4096).hex())


class TestArchiveMagicByteValidation:
    """Test magic byte validation for different archive formats."""

    def test_xz_magic_bytes_valid(self, tmp_path: Path) -> None:
        """Valid .tar.xz file passes magic byte check."""
        archive_path = tmp_path / "framework.tar.xz"
        _create_tar_xz_archive(archive_path)

        with open(archive_path, "rb") as f:
            magic = f.read(6)
        assert magic == b"\xfd7zXZ\x00"

    def test_gz_magic_bytes_valid(self, tmp_path: Path) -> None:
        """Valid .tar.gz file has correct gzip magic bytes."""
        archive_path = tmp_path / "framework.tar.gz"
        _create_tar_gz_archive(archive_path)

        with open(archive_path, "rb") as f:
            magic = f.read(2)
        assert magic == b"\x1f\x8b"

    def test_zip_magic_bytes_valid(self, tmp_path: Path) -> None:
        """Valid .zip file has correct PK magic bytes."""
        archive_path = tmp_path / "framework.zip"
        _create_zip_archive(archive_path)

        with open(archive_path, "rb") as f:
            magic = f.read(4)
        assert magic == b"PK\x03\x04"


class TestFrameworkExtractionFormats:
    """Test that _download_and_extract_to_temp handles all supported formats."""

    def _run_extraction(self, tmp_path: Path, archive_filename: str, create_fn) -> Path:
        """Helper: create archive, mock download, run extraction, return extracted dir.

        The _download_framework_components_parallel method uses cache_dir = install_dir.parent,
        so we place the install_dir inside a parent directory and pre-place the archive there.
        """
        parent_dir = tmp_path / "parent"
        parent_dir.mkdir()
        install_dir = parent_dir / "install"
        install_dir.mkdir()

        # Place the archive in parent_dir (where cache_dir points)
        archive_path = parent_dir / archive_filename
        create_fn(archive_path)

        # Build a minimal FrameworkESP32 with a dummy URL ending in the archive filename
        mock_cache = MagicMock(spec=Cache)
        mock_cache.platforms_dir = tmp_path / "platforms"
        mock_cache.platforms_dir.mkdir()
        mock_cache.get_platform_path.return_value = install_dir

        framework = FrameworkESP32(
            cache=mock_cache,
            framework_url=f"https://example.com/releases/download/1.0.0/{archive_filename}",
            libs_url="",
            show_progress=False,
        )

        # Directly call the extraction logic by invoking the parallel download method
        # The archive already exists in cache_dir (install_dir.parent), so download is skipped
        framework._download_framework_components_parallel(install_dir)

        return install_dir

    def test_tar_xz_extraction(self, tmp_path: Path) -> None:
        """Framework in .tar.xz format extracts correctly."""
        install_dir = self._run_extraction(tmp_path, "esp32-core-1.0.0.tar.xz", _create_tar_xz_archive)
        assert (install_dir / "package.json").exists()

    def test_tar_gz_extraction(self, tmp_path: Path) -> None:
        """Framework in .tar.gz format extracts correctly."""
        install_dir = self._run_extraction(tmp_path, "esp32-core-1.0.0.tar.gz", _create_tar_gz_archive)
        assert (install_dir / "package.json").exists()

    def test_zip_extraction(self, tmp_path: Path) -> None:
        """Framework in .zip format extracts correctly."""
        install_dir = self._run_extraction(tmp_path, "esp32-core-1.0.0.zip", _create_zip_archive)
        assert (install_dir / "package.json").exists()

    def test_tar_gz_not_treated_as_xz(self, tmp_path: Path) -> None:
        """A .tar.gz file must not fail with 'not an lzma file' error."""
        # This is the exact bug that was happening: .tar.gz or .zip was opened with "r:xz"
        cache_dir = tmp_path / "cache"
        cache_dir.mkdir()

        archive_path = cache_dir / "framework.tar.gz"
        _create_tar_gz_archive(archive_path)

        temp_dir = cache_dir / "_temp_extract_test"
        temp_dir.mkdir()

        # Opening a .tar.gz with "r:*" (auto-detect) should work
        with tarfile.open(archive_path, "r:*") as tar:
            tar.extractall(temp_dir)

        assert len(list(temp_dir.iterdir())) > 0

        # Opening with "r:xz" should fail - this is what the old code did
        with pytest.raises(Exception):
            with tarfile.open(archive_path, "r:xz") as tar:
                tar.extractall(temp_dir)
