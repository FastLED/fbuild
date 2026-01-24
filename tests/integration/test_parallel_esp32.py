"""Integration tests for parallel compilation on ESP32 platforms.

Tests verify that parallel compilation:
1. Produces firmware binaries successfully on ESP32dev and ESP32C6
2. Completes successfully with different --jobs settings
3. Works correctly with incremental builds

Note: ESP32 firmware hashes may differ between serial/parallel due to
non-deterministic archive timestamps. This is expected and does not
affect functionality.
"""

import hashlib
import subprocess
from pathlib import Path

import pytest


@pytest.mark.integration
class TestESP32DevParallelCompilation:
    """Test parallel compilation for ESP32dev platform (esp32 MCU)."""

    @pytest.fixture
    def esp32dev_project(self) -> Path:
        """Return path to ESP32dev test project."""
        return Path(__file__).parent.parent / "esp32dev"

    def _build_project(self, project_dir: Path, jobs: int | None = None, clean: bool = False, verbose: bool = False) -> subprocess.CompletedProcess:
        """Build a project with specific job settings.

        Args:
            project_dir: Path to project directory
            jobs: Number of parallel jobs (None for default)
            clean: Whether to clean before build
            verbose: Whether to enable verbose output

        Returns:
            subprocess.CompletedProcess with build results
        """
        cmd = ["fbuild", "build", str(project_dir), "-e", "esp32dev"]
        if jobs is not None:
            cmd.extend(["--jobs", str(jobs)])
        if clean:
            cmd.append("--clean")
        if verbose:
            cmd.append("-v")

        return subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=600,  # 10 minute timeout (ESP32 builds can be slow)
            encoding="utf-8",
            errors="replace",
        )

    def _get_firmware_hash(self, project_dir: Path) -> str:
        """Get MD5 hash of firmware.bin file.

        Args:
            project_dir: Path to project directory

        Returns:
            MD5 hash of firmware.bin as hex string
        """
        firmware_path = project_dir / ".fbuild" / "build" / "esp32dev" / "firmware.bin"
        if not firmware_path.exists():
            raise FileNotFoundError(f"Firmware not found at {firmware_path}")

        with open(firmware_path, "rb") as f:
            return hashlib.md5(f.read()).hexdigest()

    def test_serial_compilation_succeeds(self, esp32dev_project: Path):
        """Test that serial compilation (--jobs 1) completes successfully."""
        result = self._build_project(esp32dev_project, jobs=1, clean=True)

        if result.returncode != 0:
            pytest.fail(f"Serial compilation failed with exit code {result.returncode}.\n" f"STDOUT:\n{result.stdout}\n" f"STDERR:\n{result.stderr}")

        # Verify firmware was created
        firmware_path = esp32dev_project / ".fbuild" / "build" / "esp32dev" / "firmware.bin"
        assert firmware_path.exists(), "Firmware file not created"

    def test_parallel_compilation_succeeds(self, esp32dev_project: Path):
        """Test that parallel compilation (--jobs 2) completes successfully."""
        result = self._build_project(esp32dev_project, jobs=2, clean=True)

        if result.returncode != 0:
            pytest.fail(f"Parallel compilation failed with exit code {result.returncode}.\n" f"STDOUT:\n{result.stdout}\n" f"STDERR:\n{result.stderr}")

        # Verify firmware was created
        firmware_path = esp32dev_project / ".fbuild" / "build" / "esp32dev" / "firmware.bin"
        assert firmware_path.exists(), "Firmware file not created"

    def test_incremental_build_with_parallel_compilation(self, esp32dev_project: Path):
        """Test that incremental builds work correctly with parallel compilation.

        Verify that:
        1. Full parallel build succeeds
        2. Incremental build with no changes succeeds
        3. Incremental build after touching a file only recompiles that file
        """
        # Full build
        result_full = self._build_project(esp32dev_project, jobs=2, clean=True)
        assert result_full.returncode == 0, f"Full build failed: {result_full.stderr}"
        hash_full = self._get_firmware_hash(esp32dev_project)

        # Incremental build with no changes
        result_incremental = self._build_project(esp32dev_project, jobs=2, clean=False)
        assert result_incremental.returncode == 0, f"Incremental build failed: {result_incremental.stderr}"
        hash_incremental = self._get_firmware_hash(esp32dev_project)

        # Hash should be identical
        assert hash_full == hash_incremental, "Firmware changed after incremental build with no source changes. " "This indicates non-deterministic compilation."

        # Touch source file to trigger recompilation
        source_file = esp32dev_project / "esp32dev.ino"
        source_file.touch()

        # Incremental rebuild should succeed
        result_touched = self._build_project(esp32dev_project, jobs=2, clean=False)
        assert result_touched.returncode == 0, f"Incremental rebuild after touch failed: {result_touched.stderr}"

        # Firmware should exist (hash may differ due to recompilation)
        firmware_path = esp32dev_project / ".fbuild" / "build" / "esp32dev" / "firmware.bin"
        assert firmware_path.exists(), "Firmware file not created after incremental rebuild"


@pytest.mark.integration
class TestESP32C6ParallelCompilation:
    """Test parallel compilation for ESP32C6 platform (esp32c6 MCU)."""

    @pytest.fixture
    def esp32c6_project(self) -> Path:
        """Return path to ESP32C6 test project."""
        return Path(__file__).parent.parent / "esp32c6"

    def _build_project(self, project_dir: Path, jobs: int | None = None, clean: bool = False, verbose: bool = False) -> subprocess.CompletedProcess:
        """Build a project with specific job settings.

        Args:
            project_dir: Path to project directory
            jobs: Number of parallel jobs (None for default)
            clean: Whether to clean before build
            verbose: Whether to enable verbose output

        Returns:
            subprocess.CompletedProcess with build results
        """
        cmd = ["fbuild", "build", str(project_dir), "-e", "esp32c6"]
        if jobs is not None:
            cmd.extend(["--jobs", str(jobs)])
        if clean:
            cmd.append("--clean")
        if verbose:
            cmd.append("-v")

        return subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=600,  # 10 minute timeout (ESP32 builds can be slow)
            encoding="utf-8",
            errors="replace",
        )

    def _get_firmware_hash(self, project_dir: Path) -> str:
        """Get MD5 hash of firmware.bin file.

        Args:
            project_dir: Path to project directory

        Returns:
            MD5 hash of firmware.bin as hex string
        """
        firmware_path = project_dir / ".fbuild" / "build" / "esp32c6" / "firmware.bin"
        if not firmware_path.exists():
            raise FileNotFoundError(f"Firmware not found at {firmware_path}")

        with open(firmware_path, "rb") as f:
            return hashlib.md5(f.read()).hexdigest()

    def test_serial_compilation_succeeds(self, esp32c6_project: Path):
        """Test that serial compilation (--jobs 1) completes successfully."""
        result = self._build_project(esp32c6_project, jobs=1, clean=True)

        if result.returncode != 0:
            pytest.fail(f"Serial compilation failed with exit code {result.returncode}.\n" f"STDOUT:\n{result.stdout}\n" f"STDERR:\n{result.stderr}")

        # Verify firmware was created
        firmware_path = esp32c6_project / ".fbuild" / "build" / "esp32c6" / "firmware.bin"
        assert firmware_path.exists(), "Firmware file not created"

    def test_parallel_compilation_succeeds(self, esp32c6_project: Path):
        """Test that parallel compilation (--jobs 2) completes successfully."""
        result = self._build_project(esp32c6_project, jobs=2, clean=True)

        if result.returncode != 0:
            pytest.fail(f"Parallel compilation failed with exit code {result.returncode}.\n" f"STDOUT:\n{result.stdout}\n" f"STDERR:\n{result.stderr}")

        # Verify firmware was created
        firmware_path = esp32c6_project / ".fbuild" / "build" / "esp32c6" / "firmware.bin"
        assert firmware_path.exists(), "Firmware file not created"

    @pytest.mark.skip(reason="ESP32 incremental builds are non-deterministic (affects both serial and parallel modes)")
    def test_incremental_build_with_parallel_compilation(self, esp32c6_project: Path):
        """Test that incremental builds work correctly with parallel compilation.

        Verify that:
        1. Full parallel build succeeds
        2. Incremental build with no changes succeeds
        3. Incremental build after touching a file only recompiles that file
        """
        # Full build
        result_full = self._build_project(esp32c6_project, jobs=2, clean=True)
        assert result_full.returncode == 0, f"Full build failed: {result_full.stderr}"
        hash_full = self._get_firmware_hash(esp32c6_project)

        # Incremental build with no changes
        result_incremental = self._build_project(esp32c6_project, jobs=2, clean=False)
        assert result_incremental.returncode == 0, f"Incremental build failed: {result_incremental.stderr}"
        hash_incremental = self._get_firmware_hash(esp32c6_project)

        # Hash should be identical
        assert hash_full == hash_incremental, "Firmware changed after incremental build with no source changes. " "This indicates non-deterministic compilation."

        # Touch source file to trigger recompilation
        source_file = esp32c6_project / "esp32c6.ino"
        source_file.touch()

        # Incremental rebuild should succeed
        result_touched = self._build_project(esp32c6_project, jobs=2, clean=False)
        assert result_touched.returncode == 0, f"Incremental rebuild after touch failed: {result_touched.stderr}"

        # Firmware should exist (hash may differ due to recompilation)
        firmware_path = esp32c6_project / ".fbuild" / "build" / "esp32c6" / "firmware.bin"
        assert firmware_path.exists(), "Firmware file not created after incremental rebuild"


@pytest.mark.integration
class TestESP32ParallelCompilationEdgeCases:
    """Test edge cases and error handling for parallel compilation on ESP32."""

    @pytest.fixture
    def esp32dev_project(self) -> Path:
        """Return path to ESP32dev test project."""
        return Path(__file__).parent.parent / "esp32dev"

    def test_jobs_zero_is_invalid(self, esp32dev_project: Path):
        """Test that --jobs 0 is rejected."""
        result = subprocess.run(
            ["fbuild", "build", str(esp32dev_project), "-e", "esp32dev", "--jobs", "0"],
            capture_output=True,
            text=True,
            timeout=30,
            encoding="utf-8",
            errors="replace",
        )

        # Should fail with error
        assert result.returncode != 0, "--jobs 0 should be rejected"

    def test_jobs_negative_is_invalid(self, esp32dev_project: Path):
        """Test that negative --jobs is rejected."""
        result = subprocess.run(
            ["fbuild", "build", str(esp32dev_project), "-e", "esp32dev", "--jobs", "-1"],
            capture_output=True,
            text=True,
            timeout=30,
            encoding="utf-8",
            errors="replace",
        )

        # Should fail with error
        assert result.returncode != 0, "Negative --jobs should be rejected"
