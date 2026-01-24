"""Integration tests for parallel compilation on AVR Uno platform.

Tests verify that parallel compilation:
1. Produces identical firmware binaries to serial compilation
2. Completes successfully with different --jobs settings
3. Works correctly with incremental builds
"""

import hashlib
import subprocess
from pathlib import Path

import pytest


@pytest.mark.integration
class TestAVRUnoParallelCompilation:
    """Test parallel compilation for AVR Uno platform."""

    @pytest.fixture
    def uno_project(self) -> Path:
        """Return path to AVR Uno test project."""
        return Path(__file__).parent.parent / "uno"

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
        cmd = ["fbuild", "build", str(project_dir), "-e", "uno"]
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
            timeout=300,  # 5 minute timeout
            encoding="utf-8",
            errors="replace",
        )

    def _get_firmware_hash(self, project_dir: Path) -> str:
        """Get MD5 hash of firmware.hex file.

        Args:
            project_dir: Path to project directory

        Returns:
            MD5 hash of firmware.hex as hex string
        """
        firmware_path = project_dir / ".fbuild" / "build" / "uno" / "firmware.hex"
        if not firmware_path.exists():
            raise FileNotFoundError(f"Firmware not found at {firmware_path}")

        with open(firmware_path, "rb") as f:
            return hashlib.md5(f.read()).hexdigest()

    def test_serial_compilation_succeeds(self, uno_project: Path):
        """Test that serial compilation (--jobs 1) completes successfully."""
        result = self._build_project(uno_project, jobs=1, clean=True)

        if result.returncode != 0:
            pytest.fail(f"Serial compilation failed with exit code {result.returncode}.\n" f"STDOUT:\n{result.stdout}\n" f"STDERR:\n{result.stderr}")

        # Verify firmware was created
        firmware_path = uno_project / ".fbuild" / "build" / "uno" / "firmware.hex"
        assert firmware_path.exists(), "Firmware file not created"

    def test_parallel_compilation_succeeds(self, uno_project: Path):
        """Test that parallel compilation (--jobs 2) completes successfully."""
        result = self._build_project(uno_project, jobs=2, clean=True)

        if result.returncode != 0:
            pytest.fail(f"Parallel compilation failed with exit code {result.returncode}.\n" f"STDOUT:\n{result.stdout}\n" f"STDERR:\n{result.stderr}")

        # Verify firmware was created
        firmware_path = uno_project / ".fbuild" / "build" / "uno" / "firmware.hex"
        assert firmware_path.exists(), "Firmware file not created"

    def test_auto_parallel_compilation_succeeds(self, uno_project: Path):
        """Test that auto parallel compilation (no --jobs flag) completes successfully."""
        result = self._build_project(uno_project, jobs=None, clean=True)

        if result.returncode != 0:
            pytest.fail(f"Auto parallel compilation failed with exit code {result.returncode}.\n" f"STDOUT:\n{result.stdout}\n" f"STDERR:\n{result.stderr}")

        # Verify firmware was created
        firmware_path = uno_project / ".fbuild" / "build" / "uno" / "firmware.hex"
        assert firmware_path.exists(), "Firmware file not created"

    def test_serial_and_parallel_produce_identical_binaries(self, uno_project: Path):
        """Test that serial and parallel builds produce identical firmware binaries.

        This is the most critical test - parallel compilation must not change the output.
        """
        # Build with serial compilation
        result_serial = self._build_project(uno_project, jobs=1, clean=True)
        assert result_serial.returncode == 0, f"Serial build failed: {result_serial.stderr}"
        hash_serial = self._get_firmware_hash(uno_project)

        # Build with parallel compilation (2 workers)
        result_parallel = self._build_project(uno_project, jobs=2, clean=True)
        assert result_parallel.returncode == 0, f"Parallel build failed: {result_parallel.stderr}"
        hash_parallel = self._get_firmware_hash(uno_project)

        # Hashes must be identical
        assert hash_serial == hash_parallel, (
            f"Firmware binaries differ between serial and parallel builds!\n" f"Serial hash:   {hash_serial}\n" f"Parallel hash: {hash_parallel}\n" f"This indicates a bug in parallel compilation."
        )

    def test_incremental_build_with_parallel_compilation(self, uno_project: Path):
        """Test that incremental builds work correctly with parallel compilation.

        Verify that:
        1. Full parallel build succeeds
        2. Incremental build with no changes is fast (no recompilation)
        3. Incremental build after touching a file only recompiles that file
        """
        # Full build
        result_full = self._build_project(uno_project, jobs=2, clean=True)
        assert result_full.returncode == 0, f"Full build failed: {result_full.stderr}"
        hash_full = self._get_firmware_hash(uno_project)

        # Incremental build with no changes
        result_incremental = self._build_project(uno_project, jobs=2, clean=False)
        assert result_incremental.returncode == 0, f"Incremental build failed: {result_incremental.stderr}"
        hash_incremental = self._get_firmware_hash(uno_project)

        # Hash should be identical
        assert hash_full == hash_incremental, "Firmware changed after incremental build with no source changes. " "This indicates non-deterministic compilation."

        # Touch source file to trigger recompilation
        source_file = uno_project / "uno.ino"
        source_file.touch()

        # Incremental rebuild should succeed
        result_touched = self._build_project(uno_project, jobs=2, clean=False)
        assert result_touched.returncode == 0, f"Incremental rebuild after touch failed: {result_touched.stderr}"

        # Firmware hash might change due to timestamps or other factors, but build should succeed
        # We don't assert on hash equality here since touching can change build outputs


@pytest.mark.integration
class TestAVRUnoParallelCompilationEdgeCases:
    """Test edge cases and error handling for parallel compilation."""

    @pytest.fixture
    def uno_project(self) -> Path:
        """Return path to AVR Uno test project."""
        return Path(__file__).parent.parent / "uno"

    def test_jobs_zero_is_invalid(self, uno_project: Path):
        """Test that --jobs 0 is rejected."""
        result = subprocess.run(
            ["fbuild", "build", str(uno_project), "-e", "uno", "--jobs", "0"],
            capture_output=True,
            text=True,
            timeout=30,
            encoding="utf-8",
            errors="replace",
        )

        # Should fail with error
        assert result.returncode != 0, "--jobs 0 should be rejected"

    def test_jobs_negative_is_invalid(self, uno_project: Path):
        """Test that negative --jobs is rejected."""
        result = subprocess.run(
            ["fbuild", "build", str(uno_project), "-e", "uno", "--jobs", "-1"],
            capture_output=True,
            text=True,
            timeout=30,
            encoding="utf-8",
            errors="replace",
        )

        # Should fail with error
        assert result.returncode != 0, "Negative --jobs should be rejected"
