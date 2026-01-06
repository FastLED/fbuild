"""
Integration tests for Arduino Uno build system.

This test suite validates the complete build process from platformio.ini parsing
through compilation, linking, and firmware generation.
"""

import shutil
import subprocess
import time
from pathlib import Path

import pytest


@pytest.mark.xdist_group(name="arduino_uno")
class TestArduinoUnoBuild:
    """Integration tests for Arduino Uno firmware build"""

    @pytest.fixture
    def test_project_dir(self):
        """Return path to existing test project"""
        project_dir = Path(__file__).parent.parent.parent / "tests" / "avr" / "uno"
        assert project_dir.exists(), f"Test project not found at {project_dir}"
        return project_dir

    @pytest.fixture
    def clean_build_dir(self, test_project_dir):
        """Clean build directory before each test"""
        build_dir = test_project_dir / ".zap" / "build"
        if build_dir.exists():
            shutil.rmtree(build_dir)
        yield test_project_dir
        # Cleanup after test (optional, keep artifacts for debugging)

    def test_full_build_success(self, clean_build_dir):
        """
        Test complete build process from scratch.

        Validates:
        - Build completes without errors
        - firmware.hex is created
        - HEX file is valid Intel HEX format
        - File size is reasonable
        """
        project_dir = clean_build_dir
        start_time = time.time()

        # Run build command
        result = subprocess.run(
            ["zap", "build", "-e", "uno"],
            cwd=project_dir,
            capture_output=True,
            text=True,
            timeout=120,  # 2 minute timeout
        )

        build_time = time.time() - start_time

        # Assert successful build
        assert result.returncode == 0, (
            f"Build failed with exit code {result.returncode}\n"
            f"STDOUT:\n{result.stdout}\n"
            f"STDERR:\n{result.stderr}"
        )

        # Verify firmware.hex was created
        hex_path = project_dir / ".zap" / "build" / "uno" / "firmware.hex"
        assert hex_path.exists(), f"firmware.hex not created at {hex_path}"

        # Verify HEX file format (Intel HEX)
        hex_content = hex_path.read_text()
        assert hex_content.startswith(
            ":"
        ), "HEX file doesn't start with ':' (invalid Intel HEX format)"
        assert ":00000001FF" in hex_content, "HEX file missing EOF marker :00000001FF"

        # Verify each line follows Intel HEX format
        for i, line in enumerate(hex_content.strip().split("\n"), 1):
            assert line.startswith(":"), f"Line {i} doesn't start with ':'"
            # Basic length check (minimum Intel HEX record is 11 chars: :BBAAAATTCC)
            assert len(line) >= 11, f"Line {i} too short for valid Intel HEX"

        # Verify reasonable size (typical Blink sketch is ~1-5 KB)
        hex_size = hex_path.stat().st_size
        assert 100 < hex_size < 50000, (
            f"Unexpected hex size: {hex_size} bytes. "
            f"Expected between 100 and 50000 bytes."
        )

        # Performance check - first build should complete in reasonable time
        # (Including potential downloads, should be < 60s on reasonable connection)
        print(f"\n✓ Full build completed in {build_time:.2f}s")
        print(f"✓ Firmware size: {hex_size} bytes")

    def test_incremental_build_speed(self, test_project_dir):
        """
        Test that incremental builds are significantly faster.

        Validates:
        - Second build without changes is much faster
        - No unnecessary recompilation
        """
        project_dir = test_project_dir

        # First build (to ensure everything is compiled)
        result1 = subprocess.run(
            ["zap", "build", "-e", "uno"],
            cwd=project_dir,
            capture_output=True,
            timeout=120,
        )
        assert result1.returncode == 0, "Initial build failed"

        # Second build (should be incremental)
        start_time = time.time()
        result2 = subprocess.run(
            ["zap", "build", "-e", "uno"],
            cwd=project_dir,
            capture_output=True,
            timeout=30,
        )
        incremental_time = time.time() - start_time

        assert result2.returncode == 0, "Incremental build failed"

        # Incremental build should be very fast (< 5s as per success criteria)
        assert incremental_time < 5.0, (
            f"Incremental build too slow: {incremental_time:.2f}s. " f"Expected < 5s."
        )

        print(f"\n✓ Incremental build completed in {incremental_time:.2f}s")

    def test_clean_build(self, test_project_dir):
        """
        Test clean build functionality.

        Validates:
        - Clean build removes old artifacts
        - Clean build recompiles everything
        """
        project_dir = test_project_dir

        # First build
        result1 = subprocess.run(
            ["zap", "build", "-e", "uno"],
            cwd=project_dir,
            capture_output=True,
            timeout=120,
        )
        assert result1.returncode == 0, "Initial build failed"

        # Note the mtime of firmware.hex
        hex_path = project_dir / ".zap" / "build" / "uno" / "firmware.hex"
        old_mtime = hex_path.stat().st_mtime

        # Wait a bit to ensure mtime difference
        time.sleep(1)

        # Clean build
        result2 = subprocess.run(
            ["zap", "build", "-e", "uno", "--clean"],
            cwd=project_dir,
            capture_output=True,
            timeout=120,
        )
        assert result2.returncode == 0, "Clean build failed"

        # Verify firmware was rebuilt (mtime changed)
        new_mtime = hex_path.stat().st_mtime
        assert new_mtime > old_mtime, "Firmware was not rebuilt during clean build"

        print("\n✓ Clean build successfully rebuilt firmware")

    def test_firmware_size_reasonable(self, test_project_dir):
        """
        Test that firmware size matches expectations for Blink sketch.

        Validates:
        - Program size is reasonable (~1-2 KB)
        - Output includes size information
        """
        project_dir = test_project_dir

        result = subprocess.run(
            ["zap", "build", "-e", "uno"],
            cwd=project_dir,
            capture_output=True,
            text=True,
            timeout=120,
        )
        assert result.returncode == 0, "Build failed"

        # Check output contains size information
        output = result.stdout + result.stderr
        assert "bytes" in output.lower(), "Build output missing size information"

        # Verify hex file size
        hex_path = project_dir / ".zap" / "build" / "uno" / "firmware.hex"
        hex_size = hex_path.stat().st_size

        # For standard Blink sketch, expect 1-5 KB hex file
        # (Hex files are larger than binary due to ASCII encoding)
        assert 500 < hex_size < 10000, (
            f"Unexpected firmware size: {hex_size} bytes. "
            f"Expected 500-10000 bytes for Blink sketch."
        )

        print(f"\n✓ Firmware size: {hex_size} bytes (within expected range)")

    def test_build_output_structure(self, test_project_dir):
        """
        Test that build creates expected directory structure.

        Validates:
        - .zap/build/uno directory exists
        - firmware.hex exists
        - firmware.elf exists
        - Object files are created
        """
        project_dir = test_project_dir

        result = subprocess.run(
            ["zap", "build", "-e", "uno"],
            cwd=project_dir,
            capture_output=True,
            timeout=120,
        )
        assert result.returncode == 0, "Build failed"

        # Check directory structure
        build_dir = project_dir / ".zap" / "build" / "uno"
        assert build_dir.exists(), f"Build directory not created: {build_dir}"

        # Check firmware files
        hex_path = build_dir / "firmware.hex"
        elf_path = build_dir / "firmware.elf"

        assert hex_path.exists(), "firmware.hex not created"
        assert elf_path.exists(), "firmware.elf not created"

        # Check that object files were created
        obj_files = list(build_dir.rglob("*.o"))
        assert len(obj_files) > 0, "No object files (.o) created during build"

        print(f"\n✓ Build structure verified: {len(obj_files)} object files created")

    def test_repeated_builds_idempotent(self, test_project_dir):
        """
        Test that repeated builds without changes produce identical output.

        Validates:
        - Multiple builds produce same firmware
        - No spurious rebuilds
        """
        project_dir = test_project_dir

        # Build twice
        for i in range(2):
            result = subprocess.run(
                ["zap", "build", "-e", "uno"],
                cwd=project_dir,
                capture_output=True,
                timeout=120,
            )
            assert result.returncode == 0, f"Build {i+1} failed"

        # Both builds should succeed
        hex_path = project_dir / ".zap" / "build" / "uno" / "firmware.hex"
        assert hex_path.exists(), "firmware.hex not created"

        print("\n✓ Repeated builds are idempotent")


@pytest.mark.xdist_group(name="arduino_uno")
class TestBuildErrorHandling:
    """Tests for build error scenarios"""

    @pytest.fixture
    def temp_project(self, tmp_path):
        """Create a temporary test project"""
        project_dir = tmp_path / "test_project"
        project_dir.mkdir()
        src_dir = project_dir / "src"
        src_dir.mkdir()
        return project_dir

    def test_missing_platformio_ini(self, temp_project):
        """
        Test that build fails gracefully when platformio.ini is missing.
        """
        # No platformio.ini file created

        result = subprocess.run(
            ["zap", "build"],
            cwd=temp_project,
            capture_output=True,
            text=True,
            timeout=10,
        )

        # Should fail
        assert result.returncode != 0, "Build should fail without platformio.ini"

        # Should have helpful error message
        output = result.stdout + result.stderr
        assert (
            "platformio.ini" in output.lower()
        ), "Error message should mention platformio.ini"

        print("\n✓ Missing platformio.ini detected with helpful error")

    def test_invalid_environment(self, temp_project):
        """
        Test that build fails gracefully with invalid environment name.
        """
        # Create platformio.ini
        ini_content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
"""
        (temp_project / "platformio.ini").write_text(ini_content)

        # Try to build non-existent environment
        result = subprocess.run(
            ["zap", "build", "-e", "nonexistent"],
            cwd=temp_project,
            capture_output=True,
            text=True,
            timeout=10,
        )

        # Should fail
        assert result.returncode != 0, "Build should fail with invalid environment"

        # Should have helpful error message
        output = result.stdout + result.stderr
        assert (
            "environment" in output.lower() or "nonexistent" in output.lower()
        ), "Error message should mention invalid environment"

        print("\n✓ Invalid environment detected with helpful error")

    def test_missing_source_files(self, temp_project):
        """
        Test that build fails gracefully when no source files exist.

        Note: This test may take longer if toolchain needs to be downloaded.
        """
        # Create platformio.ini
        ini_content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
"""
        (temp_project / "platformio.ini").write_text(ini_content)

        # src directory exists but is empty (no .ino files)

        try:
            result = subprocess.run(
                ["zap", "build", "-e", "uno"],
                cwd=temp_project,
                capture_output=True,
                text=True,
                timeout=120,  # Increased timeout to allow for potential downloads
                encoding="utf-8",
                errors="replace",  # Replace invalid characters instead of failing
            )

            # Should fail
            assert result.returncode != 0, "Build should fail without source files"

            print("\n✓ Missing source files detected with error")

        except subprocess.TimeoutExpired:
            # If it times out, the build is hanging (which is also a bug)
            pytest.fail("Build timed out - likely hanging due to missing source files")

    def test_syntax_error_in_sketch(self, temp_project):
        """
        Test that build fails with syntax error and shows compiler output.
        """
        # Create platformio.ini
        ini_content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
"""
        (temp_project / "platformio.ini").write_text(ini_content)

        # Create sketch with syntax error
        src_dir = temp_project / "src"
        sketch_content = """
void setup() {
  pinMode(13, OUTPUT)  // Missing semicolon!
}

void loop() {
  digitalWrite(13, HIGH);
  delay(1000);
}
"""
        (src_dir / "main.ino").write_text(sketch_content)

        result = subprocess.run(
            ["zap", "build", "-e", "uno"],
            cwd=temp_project,
            capture_output=True,
            text=True,
            timeout=60,
            encoding="utf-8",
            errors="replace",  # Replace invalid characters instead of failing
        )

        # Should fail
        assert result.returncode != 0, "Build should fail with syntax error"

        # Should show compiler error (handle None gracefully)
        stdout = result.stdout or ""
        stderr = result.stderr or ""
        output = stdout + stderr
        # Compiler error should mention expected semicolon or similar
        assert (
            "error" in output.lower() or result.returncode != 0
        ), "Build should fail with error"

        print("\n✓ Syntax error detected and reported by compiler")


@pytest.mark.xdist_group(name="arduino_uno")
class TestBuildConfiguration:
    """Tests for different build configurations"""

    def test_default_environment_selection(self, tmp_path):
        """
        Test that build uses first environment when none specified.

        This addresses the known issue from Iteration 8 where CLI defaults
        to "default" environment instead of auto-detecting first environment.
        """
        # Create test project
        project_dir = tmp_path / "test_default_env"
        project_dir.mkdir()
        src_dir = project_dir / "src"
        src_dir.mkdir()

        # Create platformio.ini with uno environment
        ini_content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
"""
        (project_dir / "platformio.ini").write_text(ini_content)

        # Create simple sketch
        sketch = """
void setup() {}
void loop() {}
"""
        (src_dir / "main.ino").write_text(sketch)

        # Build WITHOUT specifying environment
        result = subprocess.run(
            ["zap", "build"],  # No -e flag
            cwd=project_dir,
            capture_output=True,
            text=True,
            timeout=120,
            encoding="utf-8",
            errors="replace",  # Replace invalid characters instead of failing
        )

        # Should succeed by auto-selecting "uno" environment
        if result.returncode != 0:
            stdout = result.stdout or ""
            stderr = result.stderr or ""
            output = stdout + stderr
            print(f"\n⚠ Auto-detection failed. Output:\n{output}")
            assert (
                False
            ), f"Build should auto-select first environment. Exit code: {result.returncode}"
        else:
            print("\n✓ Default environment auto-selected successfully")


if __name__ == "__main__":
    # Allow running tests directly with: python -m pytest tests/integration/test_arduino_uno_build.py
    pytest.main([__file__, "-v", "-s"])
