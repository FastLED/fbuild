"""Integration tests for NightDriverStrip build.

Tests verify that fbuild can compile the NightDriverStrip demo environments,
which are real-world ESP32 Arduino projects with multiple library dependencies.

Environments tested:
- demo: ESP32dev (Xtensa), minimal demo with WiFi/audio/OTA disabled
- demo_c6: ESP32-C6 (RISC-V), same minimal demo config
"""

import subprocess
from pathlib import Path

import pytest

NIGHTDRIVER_DIR = Path(__file__).parent.parent / "NightDriverStrip"


def _skip_if_missing():
    """Skip tests if the NightDriverStrip submodule is not checked out."""
    if not (NIGHTDRIVER_DIR / "platformio.ini").exists():
        pytest.skip("NightDriverStrip not found at tests/NightDriverStrip")


def _build(env: str, jobs: int | None = None, clean: bool = False, verbose: bool = False) -> subprocess.CompletedProcess:
    """Build NightDriverStrip with the given environment.

    Args:
        env: PlatformIO environment name (e.g. "demo", "demo_c6")
        jobs: Number of parallel jobs (None for default)
        clean: Whether to clean before build
        verbose: Whether to enable verbose output

    Returns:
        subprocess.CompletedProcess with build results
    """
    cmd = ["uv", "run", "fbuild", "build", str(NIGHTDRIVER_DIR), "-e", env]
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
        timeout=900,  # 15 minute timeout (large project with many libraries)
        encoding="utf-8",
        errors="replace",
    )


@pytest.mark.integration
class TestNightDriverStripDemo:
    """Test building the NightDriverStrip 'demo' environment (ESP32dev, Xtensa)."""

    def test_demo_build_succeeds(self):
        """Test that the demo environment builds successfully."""
        _skip_if_missing()

        result = _build("demo", clean=True)

        if result.returncode != 0:
            pytest.fail(f"demo build failed (exit {result.returncode}).\nSTDOUT:\n{result.stdout[-2000:]}\nSTDERR:\n{result.stderr[-2000:]}")

        firmware_path = NIGHTDRIVER_DIR / ".fbuild" / "build" / "demo" / "firmware.bin"
        assert firmware_path.exists(), "firmware.bin not created"

        # NightDriverStrip demo firmware is typically ~1-2 MB
        size = firmware_path.stat().st_size
        assert size > 100_000, f"Firmware too small ({size} bytes), likely incomplete"
        print(f"\nDemo firmware: {size:,} bytes")

    def test_demo_incremental_build(self):
        """Test that incremental build with no changes is fast and succeeds."""
        _skip_if_missing()

        # Ensure a full build exists first
        firmware_path = NIGHTDRIVER_DIR / ".fbuild" / "build" / "demo" / "firmware.bin"
        if not firmware_path.exists():
            result = _build("demo")
            assert result.returncode == 0, f"Initial build failed: {result.stderr[-1000:]}"

        old_mtime = firmware_path.stat().st_mtime

        # Incremental build - no changes
        result = _build("demo")
        assert result.returncode == 0, f"Incremental build failed: {result.stderr[-1000:]}"

        # Firmware should still exist with same mtime (nothing recompiled)
        assert firmware_path.exists(), "firmware.bin missing after incremental build"
        new_mtime = firmware_path.stat().st_mtime
        assert new_mtime == old_mtime, "Firmware was rebuilt despite no source changes"


@pytest.mark.integration
class TestNightDriverStripDemoC6:
    """Test building the NightDriverStrip 'demo_c6' environment (ESP32-C6, RISC-V)."""

    def test_demo_c6_build_succeeds(self):
        """Test that the demo_c6 environment builds successfully."""
        _skip_if_missing()

        result = _build("demo_c6", clean=True)

        if result.returncode != 0:
            pytest.fail(f"demo_c6 build failed (exit {result.returncode}).\nSTDOUT:\n{result.stdout[-2000:]}\nSTDERR:\n{result.stderr[-2000:]}")

        firmware_path = NIGHTDRIVER_DIR / ".fbuild" / "build" / "demo_c6" / "firmware.bin"
        assert firmware_path.exists(), "firmware.bin not created"

        size = firmware_path.stat().st_size
        assert size > 100_000, f"Firmware too small ({size} bytes), likely incomplete"
        print(f"\nDemo C6 firmware: {size:,} bytes")

    def test_demo_c6_incremental_build(self):
        """Test that incremental build with no changes is fast and succeeds."""
        _skip_if_missing()

        firmware_path = NIGHTDRIVER_DIR / ".fbuild" / "build" / "demo_c6" / "firmware.bin"
        if not firmware_path.exists():
            result = _build("demo_c6")
            assert result.returncode == 0, f"Initial build failed: {result.stderr[-1000:]}"

        old_mtime = firmware_path.stat().st_mtime

        result = _build("demo_c6")
        assert result.returncode == 0, f"Incremental build failed: {result.stderr[-1000:]}"

        assert firmware_path.exists(), "firmware.bin missing after incremental build"
        new_mtime = firmware_path.stat().st_mtime
        assert new_mtime == old_mtime, "Firmware was rebuilt despite no source changes"


@pytest.mark.integration
class TestNightDriverStripBuildArtifacts:
    """Test build output structure for NightDriverStrip."""

    def test_demo_build_output_structure(self):
        """Verify build produces expected directory structure and artifacts."""
        _skip_if_missing()

        firmware_path = NIGHTDRIVER_DIR / ".fbuild" / "build" / "demo" / "firmware.bin"
        if not firmware_path.exists():
            result = _build("demo")
            assert result.returncode == 0, f"Build failed: {result.stderr[-1000:]}"

        build_dir = NIGHTDRIVER_DIR / ".fbuild" / "build" / "demo"
        assert build_dir.exists(), "Build directory not created"

        # Check firmware files
        assert (build_dir / "firmware.bin").exists(), "firmware.bin not created"
        assert (build_dir / "firmware.elf").exists(), "firmware.elf not created"

        # Check that object files were created
        obj_files = list(build_dir.rglob("*.o"))
        assert len(obj_files) > 0, "No object files (.o) created"

        # NightDriverStrip has many source files; expect a significant number of objects
        assert len(obj_files) > 20, f"Too few object files ({len(obj_files)}), expected >20 for NightDriverStrip"

        # Check that library archives were created
        lib_archives = list(build_dir.rglob("*.a"))
        assert len(lib_archives) > 0, "No library archives (.a) created"

        print(f"\nBuild artifacts: {len(obj_files)} .o files, {len(lib_archives)} .a archives")
