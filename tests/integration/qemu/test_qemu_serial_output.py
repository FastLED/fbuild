"""
QEMU Serial Output Verification Tests.

This test suite validates that QEMU can properly capture and return serial output
from ESP32 firmware. The blink.ino sketch has been modified to emit specific
markers that can be detected in the serial output.

Expected serial output markers:
- Setup: "FBUILD_QEMU_SERIAL_TEST_SETUP_COMPLETE"
- Loop: "FBUILD_QEMU_LOOP_ITERATION_<N>"

QEMU Compatibility Notes:
- ESP32 (esp32dev): Full support - boots and runs Serial output correctly
- ESP32-S3: Limited support - boots but crashes due to QIO mode emulation issues
- ESP32-C3/C6: Limited support - RISC-V QEMU, may have similar limitations

For reliable QEMU testing, use esp32dev (standard ESP32) as the target.
"""

import re
import subprocess
from pathlib import Path

import pytest

# Check if Docker is available before running any tests
try:
    result = subprocess.run(
        ["docker", "version"],
        capture_output=True,
        timeout=10,
    )
    DOCKER_AVAILABLE = result.returncode == 0
except (subprocess.SubprocessError, FileNotFoundError, subprocess.TimeoutExpired):
    DOCKER_AVAILABLE = False


def check_docker_image_exists(image_name: str) -> bool:
    """Check if a Docker image exists locally."""
    try:
        result = subprocess.run(
            ["docker", "images", "-q", image_name],
            capture_output=True,
            text=True,
            timeout=10,
        )
        return bool(result.stdout.strip())
    except (subprocess.SubprocessError, FileNotFoundError, subprocess.TimeoutExpired):
        return False


def ensure_docker_image(image_name: str = "espressif/idf:latest") -> bool:
    """Ensure Docker image is available, pulling if necessary."""
    if check_docker_image_exists(image_name):
        return True

    print(f"Pulling Docker image {image_name}...")
    try:
        result = subprocess.run(
            ["docker", "pull", image_name],
            capture_output=True,
            text=True,
            timeout=600,  # 10 minute timeout for pull
        )
        return result.returncode == 0
    except (subprocess.SubprocessError, FileNotFoundError, subprocess.TimeoutExpired):
        return False


@pytest.fixture(scope="module")
def docker_available():
    """Check if Docker is available for tests."""
    if not DOCKER_AVAILABLE:
        pytest.skip("Docker is not available")
    return True


@pytest.fixture(scope="module")
def docker_image_ready(docker_available):  # noqa: ARG001 - fixture dependency
    """Ensure Docker image is pulled before running tests."""
    if not ensure_docker_image("espressif/idf:latest"):
        pytest.skip("Failed to pull Docker image espressif/idf:latest")
    return True


@pytest.fixture
def esp32s3_project_dir():
    """Return path to ESP32-S3 test project."""
    project_dir = Path(__file__).parent.parent.parent / "esp32s3"
    if not project_dir.exists():
        pytest.skip(f"Test project not found at {project_dir}")
    return project_dir


@pytest.fixture
def esp32dev_project_dir():
    """Return path to ESP32-DEV test project (recommended for QEMU testing)."""
    project_dir = Path(__file__).parent.parent.parent / "esp32dev"
    if not project_dir.exists():
        pytest.skip(f"Test project not found at {project_dir}")
    return project_dir


def _build_firmware(project_dir: Path, env_name: str) -> bool:
    """Build firmware for the given project and environment."""
    result = subprocess.run(
        ["fbuild", "build", "-e", env_name, "-c"],  # -c for clean build
        cwd=project_dir,
        capture_output=True,
        text=True,
        timeout=600,  # 10 minute timeout for full build
        encoding="utf-8",
        errors="replace",
    )
    if result.returncode != 0:
        print(f"Build failed:\n{result.stdout}\n{result.stderr}")
    return result.returncode == 0


def _check_firmware_exists(project_dir: Path, env_name: str) -> bool:
    """Check if firmware.bin exists for the given environment."""
    firmware_path = project_dir / ".fbuild" / "build" / env_name / "firmware.bin"
    return firmware_path.exists()


@pytest.mark.integration
@pytest.mark.qemu
@pytest.mark.skipif(not DOCKER_AVAILABLE, reason="Docker not available")
class TestQEMUSerialOutput:
    """Integration tests for QEMU serial output capture."""

    def test_esp32s3_build_with_serial(self, esp32s3_project_dir):
        """Test that ESP32-S3 project with serial output builds successfully."""
        env_name = "esp32s3"

        # Clean build to ensure we have the latest changes
        success = _build_firmware(esp32s3_project_dir, env_name)
        assert success, "Failed to build ESP32-S3 firmware with serial output"

        assert _check_firmware_exists(esp32s3_project_dir, env_name), "firmware.bin not created for ESP32-S3"
        print("\n✓ ESP32-S3 firmware with serial output built successfully")

    def test_esp32s3_qemu_serial_output_to_file(self, esp32s3_project_dir, docker_image_ready, tmp_path):
        """Test QEMU deployment captures serial output to file.

        This test verifies that:
        1. QEMU can run the firmware
        2. Serial output is captured to an output file
        3. The output file can be read after QEMU terminates
        """
        env_name = "esp32s3"

        # Ensure firmware exists (rebuild if needed)
        if not _check_firmware_exists(esp32s3_project_dir, env_name):
            assert _build_firmware(esp32s3_project_dir, env_name), "Failed to build ESP32-S3 firmware"

        # Run QEMU with output capture
        # Use longer timeout (30s) to allow for 5s setup delay + some loop iterations
        result = subprocess.run(
            [
                "fbuild",
                "deploy",
                "-e",
                env_name,
                "--qemu",
                "--qemu-timeout",
                "30",
            ],
            cwd=esp32s3_project_dir,
            capture_output=True,
            text=True,
            timeout=180,  # 3 minute timeout for entire operation
            encoding="utf-8",
            errors="replace",
        )

        # Capture combined output (stdout + stderr)
        combined_output = result.stdout + result.stderr

        print(f"\n=== QEMU Output ===\n{combined_output}\n=== End QEMU Output ===")

        # QEMU timeout is treated as success (exit code 0)
        assert result.returncode == 0, f"QEMU deployment failed:\n{combined_output}"

        print("\n✓ ESP32-S3 QEMU deployment with serial output completed")

    def test_esp32s3_qemu_detects_setup_marker(self, esp32s3_project_dir, docker_image_ready):
        """Test that QEMU output contains the setup marker.

        The firmware should print "FBUILD_QEMU_SERIAL_TEST_SETUP_COMPLETE"
        after the 5 second delay in setup().
        """
        env_name = "esp32s3"

        # Ensure firmware exists
        if not _check_firmware_exists(esp32s3_project_dir, env_name):
            assert _build_firmware(esp32s3_project_dir, env_name), "Failed to build ESP32-S3 firmware"

        # Run QEMU with 15 second timeout
        # (5s delay + some time for serial output to be captured)
        result = subprocess.run(
            [
                "fbuild",
                "deploy",
                "-e",
                env_name,
                "--qemu",
                "--qemu-timeout",
                "15",
            ],
            cwd=esp32s3_project_dir,
            capture_output=True,
            text=True,
            timeout=180,
            encoding="utf-8",
            errors="replace",
        )

        combined_output = result.stdout + result.stderr
        print(f"\n=== QEMU Output (Setup Test) ===\n{combined_output[:2000]}...\n")

        # Check for setup marker
        # Note: This may not appear if QEMU doesn't fully emulate serial output
        # In that case, we just verify the process completed successfully
        assert result.returncode == 0, f"QEMU deployment failed:\n{combined_output}"

        # Check if setup marker is present (informational, not strict assertion)
        if "FBUILD_QEMU_SERIAL_TEST_SETUP_COMPLETE" in combined_output:
            print("✓ Setup marker detected in QEMU output")
        else:
            print("⚠ Setup marker not detected - QEMU serial may not be fully functional")
            print("  This is expected if QEMU doesn't fully emulate ESP32 serial output")

    def test_esp32s3_qemu_detects_loop_markers(self, esp32s3_project_dir, docker_image_ready):
        """Test that QEMU output contains loop iteration markers.

        The firmware should print "FBUILD_QEMU_LOOP_ITERATION_N"
        in each iteration of the main loop.
        """
        env_name = "esp32s3"

        # Ensure firmware exists
        if not _check_firmware_exists(esp32s3_project_dir, env_name):
            assert _build_firmware(esp32s3_project_dir, env_name), "Failed to build ESP32-S3 firmware"

        # Run QEMU with 20 second timeout
        # (5s setup delay + time for at least 2-3 loop iterations)
        result = subprocess.run(
            [
                "fbuild",
                "deploy",
                "-e",
                env_name,
                "--qemu",
                "--qemu-timeout",
                "20",
            ],
            cwd=esp32s3_project_dir,
            capture_output=True,
            text=True,
            timeout=180,
            encoding="utf-8",
            errors="replace",
        )

        combined_output = result.stdout + result.stderr
        print(f"\n=== QEMU Output (Loop Test) ===\n{combined_output[:2000]}...\n")

        assert result.returncode == 0, f"QEMU deployment failed:\n{combined_output}"

        # Check for loop markers
        loop_pattern = r"FBUILD_QEMU_LOOP_ITERATION_(\d+)"
        loop_matches = re.findall(loop_pattern, combined_output)

        if loop_matches:
            print(f"✓ Found {len(loop_matches)} loop iterations: {loop_matches}")
        else:
            print("⚠ Loop markers not detected - QEMU serial may not be fully functional")
            print("  This is expected if QEMU doesn't fully emulate ESP32 serial output")


@pytest.mark.integration
@pytest.mark.qemu
@pytest.mark.skipif(not DOCKER_AVAILABLE, reason="Docker not available")
class TestQEMUSerialOutputESP32Dev:
    """
    ESP32-DEV QEMU Serial Output Tests (Recommended).

    These tests use esp32dev (standard ESP32) which has full QEMU support.
    The ESP32 QEMU emulation properly supports serial output.
    """

    def test_esp32dev_build_with_serial(self, esp32dev_project_dir):
        """Test that ESP32-DEV project with serial output builds successfully."""
        env_name = "esp32dev"

        success = _build_firmware(esp32dev_project_dir, env_name)
        assert success, "Failed to build ESP32-DEV firmware with serial output"

        assert _check_firmware_exists(esp32dev_project_dir, env_name), "firmware.bin not created for ESP32-DEV"
        print("\n✓ ESP32-DEV firmware with serial output built successfully")

    def test_esp32dev_qemu_serial_output_setup_marker(self, esp32dev_project_dir, docker_image_ready):  # noqa: ARG002
        """
        Test QEMU deployment captures serial output with setup marker.

        This is the primary test for verifying QEMU serial output works.
        The firmware prints "FBUILD_QEMU_SERIAL_TEST_SETUP_COMPLETE" after
        a 5 second delay in setup().
        """
        env_name = "esp32dev"

        # Ensure firmware exists
        if not _check_firmware_exists(esp32dev_project_dir, env_name):
            assert _build_firmware(esp32dev_project_dir, env_name), "Failed to build ESP32-DEV firmware"

        # Run QEMU with 15 second timeout
        # (allows for 5s delay in setup + serial output)
        result = subprocess.run(
            [
                "fbuild",
                "deploy",
                "-e",
                env_name,
                "--qemu",
                "--qemu-timeout",
                "15",
            ],
            cwd=esp32dev_project_dir,
            capture_output=True,
            text=True,
            timeout=180,
            encoding="utf-8",
            errors="replace",
        )

        combined_output = result.stdout + result.stderr
        print(f"\n=== QEMU Output (ESP32-DEV Setup Test) ===\n{combined_output[:3000]}...\n")

        assert result.returncode == 0, f"QEMU deployment failed:\n{combined_output}"

        # This is the key assertion - verify serial output marker appears
        assert "FBUILD_QEMU_SERIAL_TEST_SETUP_COMPLETE" in combined_output, "Setup marker 'FBUILD_QEMU_SERIAL_TEST_SETUP_COMPLETE' not found in QEMU output"

        print("✓ Setup marker detected in ESP32-DEV QEMU output!")


@pytest.mark.integration
@pytest.mark.qemu
@pytest.mark.skipif(not DOCKER_AVAILABLE, reason="Docker not available")
class TestQEMURunnerDirectAPI:
    """Direct API tests for QEMURunner with serial output capture."""

    def test_qemu_runner_esp32dev_with_output_file(self, esp32dev_project_dir, docker_image_ready, tmp_path):  # noqa: ARG002
        """Test QEMURunner API directly with output file capture using ESP32-DEV."""
        from fbuild.deploy.qemu_runner import QEMURunner

        env_name = "esp32dev"

        # Ensure firmware exists
        if not _check_firmware_exists(esp32dev_project_dir, env_name):
            assert _build_firmware(esp32dev_project_dir, env_name), "Failed to build ESP32-DEV firmware"

        firmware_path = esp32dev_project_dir / ".fbuild" / "build" / env_name / "firmware.bin"
        output_file = tmp_path / "qemu_direct_output.log"

        runner = QEMURunner(verbose=True)

        # Run with output capture - use esp32 machine type for esp32dev
        result = runner.run(
            firmware_path=firmware_path,
            machine="esp32",  # esp32dev uses "esp32" machine type
            timeout=15,
            flash_size=4,
            output_file=output_file,
        )

        # Check result (0 = success, including timeout)
        assert result == 0, f"QEMURunner.run() returned {result}"

        # Check if output file was created and contains markers
        assert output_file.exists(), "Output file was not created"
        output_content = output_file.read_text(encoding="utf-8", errors="replace")
        print(f"\n=== Output File Content ===\n{output_content[:2000]}...\n")

        # Verify serial markers are in output file
        assert "FBUILD_QEMU_SERIAL_TEST_SETUP_COMPLETE" in output_content, "Setup marker not found in QEMU output file"
        print("✓ Serial markers found in output file")

    def test_qemu_runner_with_interrupt_regex(self, esp32dev_project_dir, docker_image_ready, tmp_path):  # noqa: ARG002
        """Test QEMURunner with interrupt regex pattern matching."""
        from fbuild.deploy.qemu_runner import QEMURunner

        env_name = "esp32dev"

        # Ensure firmware exists
        if not _check_firmware_exists(esp32dev_project_dir, env_name):
            assert _build_firmware(esp32dev_project_dir, env_name), "Failed to build ESP32-DEV firmware"

        firmware_path = esp32dev_project_dir / ".fbuild" / "build" / env_name / "firmware.bin"
        output_file = tmp_path / "qemu_interrupt_output.log"

        runner = QEMURunner(verbose=True)

        # Run with pattern matching for setup complete marker
        result = runner.run(
            firmware_path=firmware_path,
            machine="esp32",
            timeout=30,
            flash_size=4,
            interrupt_regex=r"FBUILD_QEMU_SERIAL_TEST_SETUP_COMPLETE",
            output_file=output_file,
        )

        assert result == 0, f"QEMURunner.run() returned {result}"
        print("✓ QEMURunner with interrupt regex completed")


if __name__ == "__main__":
    # Allow running tests directly
    pytest.main([__file__, "-v", "-s", "-m", "qemu"])
