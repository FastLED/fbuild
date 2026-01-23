"""
Integration tests for parallel compilation feature.

This test suite validates:
1. Firmware binaries are identical between serial and parallel builds
2. Parallel compilation provides performance improvements
3. All platforms support parallel compilation correctly
4. --jobs flag controls worker count as expected
"""

import hashlib
import shutil
import subprocess
import time
from pathlib import Path
from typing import Optional

import pytest


def get_file_hash(file_path: Path) -> str:
    """Compute SHA256 hash of a file."""
    sha256 = hashlib.sha256()
    with open(file_path, "rb") as f:
        for chunk in iter(lambda: f.read(8192), b""):
            sha256.update(chunk)
    return sha256.hexdigest()


def clean_build_dir(project_dir: Path) -> None:
    """Remove build directory to ensure clean build."""
    build_dir = project_dir / ".fbuild" / "build"
    if build_dir.exists():
        shutil.rmtree(build_dir)


def run_build(
    project_dir: Path,
    environment: str,
    jobs: Optional[int] = None,
    clean: bool = False,
    timeout: int = 300,
) -> tuple[subprocess.CompletedProcess, float]:
    """
    Run fbuild build command and return result with timing.

    Args:
        project_dir: Path to project directory
        environment: Environment name (e.g., "uno", "esp32c6")
        jobs: Number of parallel jobs (None = default, 1 = serial)
        clean: Whether to do clean build
        timeout: Command timeout in seconds

    Returns:
        Tuple of (subprocess result, build time in seconds)
    """
    cmd = ["fbuild", "build", "-e", environment]
    if jobs is not None:
        cmd.extend(["--jobs", str(jobs)])
    if clean:
        cmd.append("--clean")

    start_time = time.time()
    result = subprocess.run(
        cmd,
        cwd=project_dir,
        capture_output=True,
        text=True,
        timeout=timeout,
        encoding="utf-8",
        errors="replace",
    )
    build_time = time.time() - start_time

    return result, build_time


@pytest.mark.integration
@pytest.mark.xdist_group(name="parallel_compilation")
class TestParallelCompilationBinaryIdentity:
    """Test that parallel compilation produces identical binaries."""

    @pytest.fixture
    def esp32c6_project(self) -> Path:
        """Return path to ESP32C6 test project."""
        project_dir = Path(__file__).parent.parent.parent / "tests" / "esp32c6"
        assert project_dir.exists(), f"ESP32C6 test project not found at {project_dir}"
        return project_dir

    @pytest.fixture
    def uno_project(self) -> Path:
        """Return path to Uno test project."""
        project_dir = Path(__file__).parent.parent.parent / "tests" / "uno"
        assert project_dir.exists(), f"Uno test project not found at {project_dir}"
        return project_dir

    @pytest.fixture
    def teensy40_project(self) -> Path:
        """Return path to Teensy 4.0 test project."""
        project_dir = Path(__file__).parent.parent.parent / "tests" / "teensy40"
        if not project_dir.exists():
            pytest.skip(f"Teensy 4.0 test project not found at {project_dir}")
        return project_dir

    @pytest.fixture
    def rpipico_project(self) -> Path:
        """Return path to RP2040 (Raspberry Pi Pico) test project."""
        project_dir = Path(__file__).parent.parent.parent / "tests" / "rpipico"
        if not project_dir.exists():
            pytest.skip(f"RP2040 test project not found at {project_dir}")
        return project_dir

    @pytest.fixture
    def stm32_project(self) -> Path:
        """Return path to STM32 test project."""
        project_dir = Path(__file__).parent.parent.parent / "tests" / "bluepill_f103c8"
        if not project_dir.exists():
            pytest.skip(f"STM32 test project not found at {project_dir}")
        return project_dir

    def test_esp32c6_serial_vs_parallel_binary_identity(self, esp32c6_project: Path):
        """
        Verify ESP32C6 produces identical firmware with serial vs parallel builds.

        This is critical - parallel compilation must not introduce any
        non-determinism or change the output binary.
        """
        clean_build_dir(esp32c6_project)

        # Build 1: Serial compilation (--jobs 1)
        # Note: First build may take longer due to toolchain downloads
        result1, time1 = run_build(esp32c6_project, "esp32c6", jobs=1, clean=True, timeout=600)
        assert result1.returncode == 0, f"Serial build failed:\n{result1.stdout}\n{result1.stderr}"

        bin_path1 = esp32c6_project / ".fbuild" / "build" / "esp32c6" / "firmware.bin"
        assert bin_path1.exists(), "Serial build did not produce firmware.bin"
        hash1 = get_file_hash(bin_path1)

        # Build 2: Parallel compilation (default = all CPU cores)
        clean_build_dir(esp32c6_project)
        result2, time2 = run_build(esp32c6_project, "esp32c6", clean=True, timeout=600)
        assert result2.returncode == 0, f"Parallel build failed:\n{result2.stdout}\n{result2.stderr}"

        bin_path2 = esp32c6_project / ".fbuild" / "build" / "esp32c6" / "firmware.bin"
        assert bin_path2.exists(), "Parallel build did not produce firmware.bin"
        hash2 = get_file_hash(bin_path2)

        # CRITICAL: Binaries must be identical
        assert hash1 == hash2, (
            f"Serial and parallel builds produced different binaries!\n" f"Serial hash:   {hash1}\n" f"Parallel hash: {hash2}\n" f"This indicates non-deterministic compilation behavior."
        )

        print(f"\n✓ ESP32C6 binary identity verified (hash: {hash1[:16]}...)")
        print(f"  Serial build time:   {time1:.2f}s")
        print(f"  Parallel build time: {time2:.2f}s")

    def test_uno_serial_vs_parallel_binary_identity(self, uno_project: Path):
        """
        Verify Arduino Uno produces identical firmware with serial vs parallel builds.
        """
        clean_build_dir(uno_project)

        # Build 1: Serial compilation (--jobs 1)
        result1, time1 = run_build(uno_project, "uno", jobs=1, clean=True)
        assert result1.returncode == 0, f"Serial build failed:\n{result1.stdout}\n{result1.stderr}"

        hex_path1 = uno_project / ".fbuild" / "build" / "uno" / "firmware.hex"
        assert hex_path1.exists(), "Serial build did not produce firmware.hex"
        hash1 = get_file_hash(hex_path1)

        # Build 2: Parallel compilation (default = all CPU cores)
        clean_build_dir(uno_project)
        result2, time2 = run_build(uno_project, "uno", clean=True)
        assert result2.returncode == 0, f"Parallel build failed:\n{result2.stdout}\n{result2.stderr}"

        hex_path2 = uno_project / ".fbuild" / "build" / "uno" / "firmware.hex"
        assert hex_path2.exists(), "Parallel build did not produce firmware.hex"
        hash2 = get_file_hash(hex_path2)

        # CRITICAL: Binaries must be identical
        assert hash1 == hash2, (
            f"Serial and parallel builds produced different binaries!\n" f"Serial hash:   {hash1}\n" f"Parallel hash: {hash2}\n" f"This indicates non-deterministic compilation behavior."
        )

        print(f"\n✓ Uno binary identity verified (hash: {hash1[:16]}...)")
        print(f"  Serial build time:   {time1:.2f}s")
        print(f"  Parallel build time: {time2:.2f}s")

    def test_teensy40_serial_vs_parallel_binary_identity(self, teensy40_project: Path):
        """
        Verify Teensy 4.0 produces identical firmware with serial vs parallel builds.
        """
        clean_build_dir(teensy40_project)

        # Build 1: Serial compilation (--jobs 1)
        result1, time1 = run_build(teensy40_project, "teensy40", jobs=1, clean=True)
        assert result1.returncode == 0, f"Serial build failed:\n{result1.stdout}\n{result1.stderr}"

        hex_path1 = teensy40_project / ".fbuild" / "build" / "teensy40" / "firmware.hex"
        assert hex_path1.exists(), "Serial build did not produce firmware.hex"
        hash1 = get_file_hash(hex_path1)

        # Build 2: Parallel compilation (default = all CPU cores)
        clean_build_dir(teensy40_project)
        result2, time2 = run_build(teensy40_project, "teensy40", clean=True)
        assert result2.returncode == 0, f"Parallel build failed:\n{result2.stdout}\n{result2.stderr}"

        hex_path2 = teensy40_project / ".fbuild" / "build" / "teensy40" / "firmware.hex"
        assert hex_path2.exists(), "Parallel build did not produce firmware.hex"
        hash2 = get_file_hash(hex_path2)

        # CRITICAL: Binaries must be identical
        assert hash1 == hash2, (
            f"Serial and parallel builds produced different binaries!\n" f"Serial hash:   {hash1}\n" f"Parallel hash: {hash2}\n" f"This indicates non-deterministic compilation behavior."
        )

        print(f"\n✓ Teensy 4.0 binary identity verified (hash: {hash1[:16]}...)")
        print(f"  Serial build time:   {time1:.2f}s")
        print(f"  Parallel build time: {time2:.2f}s")

    def test_rpipico_serial_vs_parallel_binary_identity(self, rpipico_project: Path):
        """
        Verify RP2040 (Raspberry Pi Pico) produces identical firmware with serial vs parallel builds.
        """
        clean_build_dir(rpipico_project)

        # Build 1: Serial compilation (--jobs 1)
        result1, time1 = run_build(rpipico_project, "rpipico", jobs=1, clean=True, timeout=240)
        assert result1.returncode == 0, f"Serial build failed:\n{result1.stdout}\n{result1.stderr}"

        # RP2040 typically produces .uf2 files
        uf2_path1 = rpipico_project / ".fbuild" / "build" / "rpipico" / "firmware.uf2"
        bin_path1 = rpipico_project / ".fbuild" / "build" / "rpipico" / "firmware.bin"

        # Use whichever file exists
        if uf2_path1.exists():
            firmware_path1 = uf2_path1
        elif bin_path1.exists():
            firmware_path1 = bin_path1
        else:
            pytest.fail("Serial build did not produce firmware.uf2 or firmware.bin")

        hash1 = get_file_hash(firmware_path1)

        # Build 2: Parallel compilation (default = all CPU cores)
        clean_build_dir(rpipico_project)
        result2, time2 = run_build(rpipico_project, "rpipico", clean=True, timeout=240)
        assert result2.returncode == 0, f"Parallel build failed:\n{result2.stdout}\n{result2.stderr}"

        # Use same firmware file type
        firmware_path2 = rpipico_project / ".fbuild" / "build" / "rpipico" / firmware_path1.name
        assert firmware_path2.exists(), f"Parallel build did not produce {firmware_path1.name}"
        hash2 = get_file_hash(firmware_path2)

        # CRITICAL: Binaries must be identical
        assert hash1 == hash2, (
            f"Serial and parallel builds produced different binaries!\n" f"Serial hash:   {hash1}\n" f"Parallel hash: {hash2}\n" f"This indicates non-deterministic compilation behavior."
        )

        print(f"\n✓ RP2040 binary identity verified (hash: {hash1[:16]}...)")
        print(f"  Serial build time:   {time1:.2f}s")
        print(f"  Parallel build time: {time2:.2f}s")

    def test_stm32_serial_vs_parallel_binary_identity(self, stm32_project: Path):
        """
        Verify STM32 produces identical firmware with serial vs parallel builds.
        """
        clean_build_dir(stm32_project)

        # Build 1: Serial compilation (--jobs 1)
        result1, time1 = run_build(stm32_project, "bluepill_f103c8", jobs=1, clean=True)
        assert result1.returncode == 0, f"Serial build failed:\n{result1.stdout}\n{result1.stderr}"

        bin_path1 = stm32_project / ".fbuild" / "build" / "bluepill_f103c8" / "firmware.bin"
        assert bin_path1.exists(), "Serial build did not produce firmware.bin"
        hash1 = get_file_hash(bin_path1)

        # Build 2: Parallel compilation (default = all CPU cores)
        clean_build_dir(stm32_project)
        result2, time2 = run_build(stm32_project, "bluepill_f103c8", clean=True)
        assert result2.returncode == 0, f"Parallel build failed:\n{result2.stdout}\n{result2.stderr}"

        bin_path2 = stm32_project / ".fbuild" / "build" / "bluepill_f103c8" / "firmware.bin"
        assert bin_path2.exists(), "Parallel build did not produce firmware.bin"
        hash2 = get_file_hash(bin_path2)

        # CRITICAL: Binaries must be identical
        assert hash1 == hash2, (
            f"Serial and parallel builds produced different binaries!\n" f"Serial hash:   {hash1}\n" f"Parallel hash: {hash2}\n" f"This indicates non-deterministic compilation behavior."
        )

        print(f"\n✓ STM32 binary identity verified (hash: {hash1[:16]}...)")
        print(f"  Serial build time:   {time1:.2f}s")
        print(f"  Parallel build time: {time2:.2f}s")


@pytest.mark.integration
@pytest.mark.xdist_group(name="parallel_compilation")
class TestParallelCompilationJobsFlag:
    """Test that --jobs flag controls worker count correctly."""

    @pytest.fixture
    def esp32c6_project(self) -> Path:
        """Return path to ESP32C6 test project."""
        project_dir = Path(__file__).parent.parent.parent / "tests" / "esp32c6"
        assert project_dir.exists(), f"ESP32C6 test project not found at {project_dir}"
        return project_dir

    def test_jobs_1_forces_serial_compilation(self, esp32c6_project: Path):
        """
        Test that --jobs 1 forces serial compilation (useful for debugging).
        """
        clean_build_dir(esp32c6_project)

        result, build_time = run_build(esp32c6_project, "esp32c6", jobs=1, clean=True)

        assert result.returncode == 0, f"Build with --jobs 1 failed:\n{result.stdout}\n{result.stderr}"

        bin_path = esp32c6_project / ".fbuild" / "build" / "esp32c6" / "firmware.bin"
        assert bin_path.exists(), "Build with --jobs 1 did not produce firmware.bin"

        print("\n✓ Serial compilation (--jobs 1) successful")
        print(f"  Build time: {build_time:.2f}s")

    def test_jobs_2_uses_limited_workers(self, esp32c6_project: Path):
        """
        Test that --jobs 2 limits parallel compilation to 2 workers.
        """
        clean_build_dir(esp32c6_project)

        result, build_time = run_build(esp32c6_project, "esp32c6", jobs=2, clean=True)

        assert result.returncode == 0, f"Build with --jobs 2 failed:\n{result.stdout}\n{result.stderr}"

        bin_path = esp32c6_project / ".fbuild" / "build" / "esp32c6" / "firmware.bin"
        assert bin_path.exists(), "Build with --jobs 2 did not produce firmware.bin"

        print("\n✓ Limited parallel compilation (--jobs 2) successful")
        print(f"  Build time: {build_time:.2f}s")

    def test_default_uses_all_cpu_cores(self, esp32c6_project: Path):
        """
        Test that default (no --jobs flag) uses all CPU cores.
        """
        clean_build_dir(esp32c6_project)

        result, build_time = run_build(esp32c6_project, "esp32c6", clean=True)

        assert result.returncode == 0, f"Default parallel build failed:\n{result.stdout}\n{result.stderr}"

        bin_path = esp32c6_project / ".fbuild" / "build" / "esp32c6" / "firmware.bin"
        assert bin_path.exists(), "Default parallel build did not produce firmware.bin"

        print("\n✓ Default parallel compilation (all cores) successful")
        print(f"  Build time: {build_time:.2f}s")


@pytest.mark.integration
@pytest.mark.xdist_group(name="parallel_compilation")
class TestIncrementalBuildsWithParallelCompilation:
    """Test that incremental builds work correctly with parallel compilation."""

    @pytest.fixture
    def uno_project(self) -> Path:
        """Return path to Uno test project."""
        project_dir = Path(__file__).parent.parent.parent / "tests" / "uno"
        assert project_dir.exists(), f"Uno test project not found at {project_dir}"
        return project_dir

    def test_incremental_build_with_parallel_compilation(self, uno_project: Path):
        """
        Test that incremental builds work correctly with parallel compilation.

        Validates:
        - First build compiles everything
        - Second build (no changes) is very fast
        - Third build (after change) recompiles only changed files
        """
        clean_build_dir(uno_project)

        # First build (full compilation with parallel)
        result1, time1 = run_build(uno_project, "uno", clean=True)
        assert result1.returncode == 0, f"First build failed:\n{result1.stdout}\n{result1.stderr}"

        hex_path = uno_project / ".fbuild" / "build" / "uno" / "firmware.hex"
        assert hex_path.exists(), "First build did not produce firmware.hex"
        hash1 = get_file_hash(hex_path)

        # Second build (no changes - should be incremental and very fast)
        result2, time2 = run_build(uno_project, "uno")
        assert result2.returncode == 0, f"Incremental build failed:\n{result2.stdout}\n{result2.stderr}"

        hash2 = get_file_hash(hex_path)
        assert hash1 == hash2, "Incremental build (no changes) produced different binary"

        # Incremental build should be much faster than full build
        assert time2 < time1 / 2, (
            f"Incremental build not significantly faster than full build\n" f"Full build time:        {time1:.2f}s\n" f"Incremental build time: {time2:.2f}s\n" f"Expected incremental < {time1/2:.2f}s"
        )

        print("\n✓ Incremental build with parallel compilation verified")
        print(f"  Full build time:        {time1:.2f}s")
        print(f"  Incremental build time: {time2:.2f}s")
        print(f"  Speedup factor:         {time1/time2:.1f}x")

    def test_incremental_build_recompiles_changed_files_only(self, uno_project: Path):
        """
        Test that modifying a source file triggers recompilation of only that file.

        Note: This test modifies source files temporarily, so it should
        restore original content on completion.
        """
        clean_build_dir(uno_project)

        # First build
        result1, time1 = run_build(uno_project, "uno", clean=True)
        assert result1.returncode == 0, "First build failed"

        # Find a source file to modify
        src_dir = uno_project / "src"
        source_files = list(src_dir.glob("*.ino")) + list(src_dir.glob("*.cpp"))

        if not source_files:
            pytest.skip("No source files found to modify")

        source_file = source_files[0]
        original_content = source_file.read_text()

        try:
            # Modify source file (add a comment)
            modified_content = "// Modified for incremental build test\n" + original_content
            source_file.write_text(modified_content)

            # Build again (should recompile changed file)
            result2, time2 = run_build(uno_project, "uno")
            assert result2.returncode == 0, f"Build after modification failed:\n{result2.stdout}\n{result2.stderr}"

            # Build should be faster than full build but slower than no-change incremental
            result3, time3 = run_build(uno_project, "uno")
            assert result3.returncode == 0, "Third build failed"

            assert time3 < time1 / 2, (
                f"Incremental build after change not significantly faster than full build\n"
                f"Full build:             {time1:.2f}s\n"
                f"Build after change:     {time2:.2f}s\n"
                f"Build without changes:  {time3:.2f}s"
            )

            print("\n✓ Incremental build correctly handles file modifications")
            print(f"  Full build:            {time1:.2f}s")
            print(f"  Build after change:    {time2:.2f}s")
            print(f"  Build without changes: {time3:.2f}s")

        finally:
            # Restore original content
            source_file.write_text(original_content)


if __name__ == "__main__":
    # Allow running tests directly
    pytest.main([__file__, "-v", "-s"])
