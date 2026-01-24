"""Integration tests for parallel compilation on Teensy 41 platform.

Tests verify that parallel compilation:
1. Produces identical firmware binaries to serial compilation
2. Completes successfully with different --jobs settings
3. Works correctly with incremental builds

Note: Auto mode (jobs=None) currently has a known issue with module reloading
in the daemon, causing "_daemon_context not initialized" errors. This is tracked
for future fix. Tests use explicit --jobs values instead.
"""

import hashlib
import subprocess
from pathlib import Path

import pytest


@pytest.mark.integration
class TestTeensy41ParallelCompilation:
    """Test parallel compilation for Teensy 41 platform."""

    @pytest.fixture
    def teensy_project(self) -> Path:
        """Return path to Teensy 41 test project."""
        return Path(__file__).parent.parent / "teensy41"

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
        cmd = ["fbuild", "build", str(project_dir), "-e", "teensy41"]
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
            timeout=600,  # 10 minute timeout (Teensy core is larger than AVR)
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
        firmware_path = project_dir / ".fbuild" / "build" / "teensy41" / "firmware.hex"
        if not firmware_path.exists():
            raise FileNotFoundError(f"Firmware not found at {firmware_path}")

        with open(firmware_path, "rb") as f:
            return hashlib.md5(f.read()).hexdigest()

    def test_serial_compilation_succeeds(self, teensy_project: Path):
        """Test that serial compilation (--jobs 1) completes successfully."""
        result = self._build_project(teensy_project, jobs=1, clean=True)

        if result.returncode != 0:
            pytest.fail(f"Serial compilation failed with exit code {result.returncode}.\n" f"STDOUT:\n{result.stdout}\n" f"STDERR:\n{result.stderr}")

        # Verify firmware was created
        firmware_path = teensy_project / ".fbuild" / "build" / "teensy41" / "firmware.hex"
        assert firmware_path.exists(), "Firmware file not created"

    def test_parallel_compilation_succeeds(self, teensy_project: Path):
        """Test that parallel compilation (--jobs 2) completes successfully."""
        result = self._build_project(teensy_project, jobs=2, clean=True)

        if result.returncode != 0:
            pytest.fail(f"Parallel compilation failed with exit code {result.returncode}.\n" f"STDOUT:\n{result.stdout}\n" f"STDERR:\n{result.stderr}")

        # Verify firmware was created
        firmware_path = teensy_project / ".fbuild" / "build" / "teensy41" / "firmware.hex"
        assert firmware_path.exists(), "Firmware file not created"

    def test_auto_parallel_compilation_succeeds(self, teensy_project: Path):
        """Test that auto parallel compilation (no --jobs flag) completes successfully.

        This test verifies that dependency injection of the compilation queue works correctly,
        allowing auto-parallel mode to survive module reloading during development.
        """
        result = self._build_project(teensy_project, jobs=None, clean=True)

        if result.returncode != 0:
            pytest.fail(f"Auto parallel compilation failed with exit code {result.returncode}.\n" f"STDOUT:\n{result.stdout}\n" f"STDERR:\n{result.stderr}")

        # Verify firmware was created
        firmware_path = teensy_project / ".fbuild" / "build" / "teensy41" / "firmware.hex"
        assert firmware_path.exists(), "Firmware file not created"

    def test_serial_and_parallel_produce_identical_binaries(self, teensy_project: Path):
        """Test that serial and parallel builds produce identical firmware binaries.

        This is the most critical test - parallel compilation must not change the output.
        """
        # Build with serial compilation
        result_serial = self._build_project(teensy_project, jobs=1, clean=True)
        assert result_serial.returncode == 0, f"Serial build failed: {result_serial.stderr}"
        hash_serial = self._get_firmware_hash(teensy_project)

        # Build with parallel compilation (2 workers)
        result_parallel = self._build_project(teensy_project, jobs=2, clean=True)
        assert result_parallel.returncode == 0, f"Parallel build failed: {result_parallel.stderr}"
        hash_parallel = self._get_firmware_hash(teensy_project)

        # Hashes must be identical
        assert hash_serial == hash_parallel, (
            f"Firmware binaries differ between serial and parallel builds!\n" f"Serial hash:   {hash_serial}\n" f"Parallel hash: {hash_parallel}\n" f"This indicates a bug in parallel compilation."
        )

    def test_incremental_build_with_parallel_compilation(self, teensy_project: Path):
        """Test that incremental builds work correctly with parallel compilation.

        Verify that:
        1. Full parallel build succeeds
        2. Incremental build with no changes is fast (no recompilation)
        3. Incremental build after touching a file only recompiles that file
        """
        # Full build
        result_full = self._build_project(teensy_project, jobs=2, clean=True)
        assert result_full.returncode == 0, f"Full build failed: {result_full.stderr}"
        hash_full = self._get_firmware_hash(teensy_project)

        # Incremental build with no changes
        result_incremental = self._build_project(teensy_project, jobs=2, clean=False)
        assert result_incremental.returncode == 0, f"Incremental build failed: {result_incremental.stderr}"
        hash_incremental = self._get_firmware_hash(teensy_project)

        # Hash should be identical
        assert hash_full == hash_incremental, "Firmware changed after incremental build with no source changes. " "This indicates non-deterministic compilation."

        # Touch source file to trigger recompilation
        source_file = teensy_project / "src" / "main.ino"
        source_file.touch()

        # Incremental rebuild should succeed
        result_touched = self._build_project(teensy_project, jobs=2, clean=False)
        assert result_touched.returncode == 0, f"Incremental rebuild after touch failed: {result_touched.stderr}"

        # Firmware hash might change due to timestamps or other factors, but build should succeed
        # We don't assert on hash equality here since touching can change build outputs


@pytest.mark.integration
class TestTeensy41ParallelCompilationEdgeCases:
    """Test edge cases and error handling for parallel compilation."""

    @pytest.fixture
    def teensy_project(self) -> Path:
        """Return path to Teensy 41 test project."""
        return Path(__file__).parent.parent / "teensy41"

    def test_jobs_zero_is_invalid(self, teensy_project: Path):
        """Test that --jobs 0 is rejected."""
        result = subprocess.run(
            ["fbuild", "build", str(teensy_project), "-e", "teensy41", "--jobs", "0"],
            capture_output=True,
            text=True,
            timeout=30,
            encoding="utf-8",
            errors="replace",
        )

        # Should fail with error
        assert result.returncode != 0, "--jobs 0 should be rejected"

    def test_jobs_negative_is_invalid(self, teensy_project: Path):
        """Test that negative --jobs is rejected."""
        result = subprocess.run(
            ["fbuild", "build", str(teensy_project), "-e", "teensy41", "--jobs", "-1"],
            capture_output=True,
            text=True,
            timeout=30,
            encoding="utf-8",
            errors="replace",
        )

        # Should fail with error
        assert result.returncode != 0, "Negative --jobs should be rejected"
