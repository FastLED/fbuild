"""
QEMU integration tests for ESP32 platforms.

This test suite validates QEMU deployment for ESP32-S3, ESP32-C6, and ESP32-DEV
boards using Docker containers. Tests require Docker to be installed and running.
"""

import shutil
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
def docker_image_ready(docker_available):
    """Ensure Docker image is pulled before running tests."""
    if not ensure_docker_image("espressif/idf:latest"):
        pytest.skip("Failed to pull Docker image espressif/idf:latest")
    return True


@pytest.mark.integration
@pytest.mark.qemu
@pytest.mark.skipif(not DOCKER_AVAILABLE, reason="Docker not available")
class TestQEMUDeployment:
    """Integration tests for QEMU deployment on ESP32 platforms."""

    @pytest.fixture
    def esp32s3_project_dir(self):
        """Return path to ESP32-S3 test project."""
        # Path is: tests/integration/qemu/test_qemu_esp32.py
        # Test project is: tests/esp32s3/
        project_dir = Path(__file__).parent.parent.parent / "esp32s3"
        if not project_dir.exists():
            pytest.skip(f"Test project not found at {project_dir}")
        return project_dir

    @pytest.fixture
    def esp32dev_project_dir(self):
        """Return path to ESP32-DEV test project."""
        project_dir = Path(__file__).parent.parent.parent / "esp32dev"
        if not project_dir.exists():
            pytest.skip(f"Test project not found at {project_dir}")
        return project_dir

    @pytest.fixture
    def esp32c6_project_dir(self):
        """Return path to ESP32-C6 test project."""
        project_dir = Path(__file__).parent.parent.parent / "esp32c6"
        if not project_dir.exists():
            pytest.skip(f"Test project not found at {project_dir}")
        return project_dir

    def _build_firmware(self, project_dir: Path, env_name: str) -> bool:
        """Build firmware for the given project and environment."""
        result = subprocess.run(
            ["fbuild", "build", "-e", env_name],
            cwd=project_dir,
            capture_output=True,
            text=True,
            timeout=300,  # 5 minute timeout for build
            encoding="utf-8",
            errors="replace",
        )
        if result.returncode != 0:
            print(f"Build failed:\n{result.stdout}\n{result.stderr}")
        return result.returncode == 0

    def _check_firmware_exists(self, project_dir: Path, env_name: str) -> bool:
        """Check if firmware.bin exists for the given environment."""
        firmware_path = project_dir / ".fbuild" / "build" / env_name / "firmware.bin"
        return firmware_path.exists()

    def test_docker_available(self, docker_available):
        """Test that Docker is available and running."""
        result = subprocess.run(
            ["docker", "version"],
            capture_output=True,
            timeout=10,
        )
        assert result.returncode == 0, "Docker is not available"
        print("\n✓ Docker is available and running")

    def test_docker_image_pull(self, docker_image_ready):
        """Test that Docker image can be pulled."""
        assert check_docker_image_exists("espressif/idf:latest"), "Docker image espressif/idf:latest not available"
        print("\n✓ Docker image espressif/idf:latest is available")

    def test_esp32s3_build(self, esp32s3_project_dir):
        """Test that ESP32-S3 project builds successfully."""
        env_name = "esp32s3"
        success = self._build_firmware(esp32s3_project_dir, env_name)
        assert success, "Failed to build ESP32-S3 firmware"

        assert self._check_firmware_exists(esp32s3_project_dir, env_name), "firmware.bin not created for ESP32-S3"
        print("\n✓ ESP32-S3 firmware built successfully")

    def test_esp32s3_qemu_deploy(self, esp32s3_project_dir, docker_image_ready):
        """Test QEMU deployment for ESP32-S3."""
        env_name = "esp32s3"

        # Ensure firmware exists
        if not self._check_firmware_exists(esp32s3_project_dir, env_name):
            assert self._build_firmware(esp32s3_project_dir, env_name), "Failed to build ESP32-S3 firmware"

        # Run QEMU deployment
        result = subprocess.run(
            ["fbuild", "deploy", "-e", env_name, "--qemu", "--qemu-timeout", "15"],
            cwd=esp32s3_project_dir,
            capture_output=True,
            text=True,
            timeout=120,  # 2 minute timeout for QEMU
            encoding="utf-8",
            errors="replace",
        )

        # QEMU timeout is treated as success (exit code 0)
        # Non-zero means actual error
        assert result.returncode == 0, f"QEMU deployment failed for ESP32-S3:\n{result.stdout}\n{result.stderr}"
        print("\n✓ ESP32-S3 QEMU deployment completed successfully")

    def test_esp32dev_build(self, esp32dev_project_dir):
        """Test that ESP32-DEV project builds successfully."""
        env_name = "esp32dev"
        success = self._build_firmware(esp32dev_project_dir, env_name)
        assert success, "Failed to build ESP32-DEV firmware"

        assert self._check_firmware_exists(esp32dev_project_dir, env_name), "firmware.bin not created for ESP32-DEV"
        print("\n✓ ESP32-DEV firmware built successfully")

    def test_esp32dev_qemu_deploy(self, esp32dev_project_dir, docker_image_ready):
        """Test QEMU deployment for ESP32-DEV."""
        env_name = "esp32dev"

        # Ensure firmware exists
        if not self._check_firmware_exists(esp32dev_project_dir, env_name):
            assert self._build_firmware(esp32dev_project_dir, env_name), "Failed to build ESP32-DEV firmware"

        # Run QEMU deployment
        result = subprocess.run(
            ["fbuild", "deploy", "-e", env_name, "--qemu", "--qemu-timeout", "15"],
            cwd=esp32dev_project_dir,
            capture_output=True,
            text=True,
            timeout=120,
            encoding="utf-8",
            errors="replace",
        )

        # Check output for expected patterns
        output = result.stdout + result.stderr
        print(f"QEMU output:\n{output}")

        assert result.returncode == 0, f"QEMU deployment failed for ESP32-DEV:\n{output}"
        print("\n✓ ESP32-DEV QEMU deployment completed successfully")

    def test_esp32c6_build(self, esp32c6_project_dir):
        """Test that ESP32-C6 project builds successfully."""
        env_name = "esp32c6"
        success = self._build_firmware(esp32c6_project_dir, env_name)
        assert success, "Failed to build ESP32-C6 firmware"

        assert self._check_firmware_exists(esp32c6_project_dir, env_name), "firmware.bin not created for ESP32-C6"
        print("\n✓ ESP32-C6 firmware built successfully")

    def test_esp32c6_qemu_deploy(self, esp32c6_project_dir, docker_image_ready):
        """Test QEMU deployment for ESP32-C6.

        Note: ESP32-C6 uses RISC-V architecture and may have limited QEMU support.
        The test falls back to esp32c3 emulation.
        """
        env_name = "esp32c6"

        # Ensure firmware exists
        if not self._check_firmware_exists(esp32c6_project_dir, env_name):
            assert self._build_firmware(esp32c6_project_dir, env_name), "Failed to build ESP32-C6 firmware"

        # Run QEMU deployment
        result = subprocess.run(
            ["fbuild", "deploy", "-e", env_name, "--qemu", "--qemu-timeout", "15"],
            cwd=esp32c6_project_dir,
            capture_output=True,
            text=True,
            timeout=120,
            encoding="utf-8",
            errors="replace",
        )

        output = result.stdout + result.stderr
        print(f"QEMU output:\n{output}")

        assert result.returncode == 0, f"QEMU deployment failed for ESP32-C6:\n{output}"
        print("\n✓ ESP32-C6 QEMU deployment completed successfully")


@pytest.mark.integration
@pytest.mark.qemu
@pytest.mark.skipif(not DOCKER_AVAILABLE, reason="Docker not available")
class TestQEMUDockerAutoStart:
    """Test automatic Docker environment startup."""

    def test_docker_daemon_detection(self):
        """Test that Docker daemon running status is detected correctly."""
        from fbuild.deploy.qemu_runner import check_docker_available

        # This should return True if Docker is running
        available = check_docker_available()
        assert available is True, "Docker should be detected as available"
        print("\n✓ Docker daemon detection works correctly")

    def test_qemu_runner_auto_pull(self):
        """Test that QEMURunner automatically pulls image if needed."""
        from fbuild.deploy.qemu_runner import QEMURunner

        runner = QEMURunner(verbose=True)

        # pull_image should succeed without errors
        result = runner.pull_image()
        assert result is True, "QEMURunner should successfully pull/verify image"
        print("\n✓ QEMURunner auto-pull works correctly")


@pytest.mark.integration
@pytest.mark.qemu
@pytest.mark.skipif(not DOCKER_AVAILABLE, reason="Docker not available")
class TestQEMUBoardMapping:
    """Test board to QEMU machine type mapping."""

    def test_esp32s3_mapping(self):
        """Test that esp32s3 board maps correctly."""
        from fbuild.deploy.qemu_runner import map_board_to_machine

        machine = map_board_to_machine("esp32-s3-devkitc-1")
        assert machine == "esp32s3", f"Expected esp32s3, got {machine}"

        machine = map_board_to_machine("esp32s3")
        assert machine == "esp32s3", f"Expected esp32s3, got {machine}"
        print("\n✓ ESP32-S3 board mapping correct")

    def test_esp32c6_mapping(self):
        """Test that esp32c6 board maps correctly (falls back to esp32c3)."""
        from fbuild.deploy.qemu_runner import map_board_to_machine

        machine = map_board_to_machine("esp32-c6-devkitm-1")
        # ESP32-C6 falls back to esp32c3 due to limited QEMU support
        assert machine == "esp32c3", f"Expected esp32c3, got {machine}"

        machine = map_board_to_machine("esp32c6")
        assert machine == "esp32c3", f"Expected esp32c3, got {machine}"
        print("\n✓ ESP32-C6 board mapping correct (falls back to esp32c3)")

    def test_esp32dev_mapping(self):
        """Test that esp32dev board maps correctly."""
        from fbuild.deploy.qemu_runner import map_board_to_machine

        machine = map_board_to_machine("esp32dev")
        assert machine == "esp32", f"Expected esp32, got {machine}"
        print("\n✓ ESP32-DEV board mapping correct")

    def test_esp32c3_mapping(self):
        """Test that esp32c3 board maps correctly."""
        from fbuild.deploy.qemu_runner import map_board_to_machine

        machine = map_board_to_machine("esp32c3")
        assert machine == "esp32c3", f"Expected esp32c3, got {machine}"

        machine = map_board_to_machine("esp32-c3-devkitm-1")
        assert machine == "esp32c3", f"Expected esp32c3, got {machine}"
        print("\n✓ ESP32-C3 board mapping correct")


@pytest.mark.integration
@pytest.mark.qemu
class TestQEMUErrorHandling:
    """Test error handling scenarios for QEMU deployment."""

    def test_missing_firmware_error(self, tmp_path):
        """Test that missing firmware file returns proper error."""
        from fbuild.deploy.qemu_runner import QEMURunner

        runner = QEMURunner(verbose=True)

        # Try to run with non-existent firmware
        non_existent = tmp_path / "firmware.bin"
        result = runner.run(
            firmware_path=non_existent,
            machine="esp32s3",
            timeout=5,
            skip_pull=True,  # Skip pull to speed up test
        )

        assert result != 0, "Should return non-zero for missing firmware"
        print("\n✓ Missing firmware error handled correctly")

    def test_invalid_machine_type(self):
        """Test that invalid machine type maps to default esp32."""
        from fbuild.deploy.qemu_runner import map_board_to_machine

        machine = map_board_to_machine("unknown_board")
        assert machine == "esp32", f"Unknown board should map to esp32, got {machine}"
        print("\n✓ Unknown board mapping handled correctly")

    def test_qemu_config_esp32c3(self):
        """Test QEMU configuration for ESP32-C3 (RISC-V)."""
        from fbuild.deploy.qemu_runner import QEMU_RISCV32_PATH, QEMURunner

        runner = QEMURunner()
        qemu_system, qemu_machine, echo_target = runner._get_qemu_config("esp32c3")

        assert qemu_system == QEMU_RISCV32_PATH, f"Expected RISC-V QEMU, got {qemu_system}"
        assert qemu_machine == "esp32c3", f"Expected esp32c3 machine, got {qemu_machine}"
        assert echo_target == "ESP32C3", f"Expected ESP32C3 target, got {echo_target}"
        print("\n✓ ESP32-C3 QEMU config correct")

    def test_qemu_config_esp32s3(self):
        """Test QEMU configuration for ESP32-S3 (Xtensa)."""
        from fbuild.deploy.qemu_runner import QEMU_XTENSA_PATH, QEMURunner

        runner = QEMURunner()
        qemu_system, qemu_machine, echo_target = runner._get_qemu_config("esp32s3")

        assert qemu_system == QEMU_XTENSA_PATH, f"Expected Xtensa QEMU, got {qemu_system}"
        assert qemu_machine == "esp32s3", f"Expected esp32s3 machine, got {qemu_machine}"
        assert echo_target == "ESP32S3", f"Expected ESP32S3 target, got {echo_target}"
        print("\n✓ ESP32-S3 QEMU config correct")

    def test_qemu_config_esp32_default(self):
        """Test QEMU configuration for default ESP32 (Xtensa)."""
        from fbuild.deploy.qemu_runner import QEMU_XTENSA_PATH, QEMURunner

        runner = QEMURunner()
        qemu_system, qemu_machine, echo_target = runner._get_qemu_config("esp32")

        assert qemu_system == QEMU_XTENSA_PATH, f"Expected Xtensa QEMU, got {qemu_system}"
        assert qemu_machine == "esp32", f"Expected esp32 machine, got {qemu_machine}"
        assert echo_target == "ESP32", f"Expected ESP32 target, got {echo_target}"
        print("\n✓ ESP32 default QEMU config correct")

    def test_firmware_preparation_invalid_flash_size(self, tmp_path):
        """Test that invalid flash size raises ValueError."""
        from fbuild.deploy.qemu_runner import QEMURunner

        runner = QEMURunner()

        # Create a dummy firmware file
        firmware = tmp_path / "firmware.bin"
        firmware.write_bytes(b"\x00" * 1024)

        # Try with invalid flash size
        with pytest.raises(ValueError) as exc_info:
            runner._prepare_firmware(firmware, flash_size_mb=3)  # Invalid size

        assert "must be 2, 4, 8, or 16" in str(exc_info.value)
        print("\n✓ Invalid flash size error handled correctly")

    def test_firmware_preparation_valid(self, tmp_path):
        """Test firmware preparation with valid parameters."""
        from fbuild.deploy.qemu_runner import QEMURunner

        runner = QEMURunner()

        # Create a dummy firmware file
        firmware = tmp_path / "firmware.bin"
        firmware_content = b"\x00" * 1024
        firmware.write_bytes(firmware_content)

        try:
            temp_dir = runner._prepare_firmware(firmware, flash_size_mb=4)

            # Check that files were created
            assert temp_dir.exists()
            assert (temp_dir / "firmware.bin").exists()
            assert (temp_dir / "flash.bin").exists()

            # Check flash.bin size (4MB = 4 * 1024 * 1024)
            flash_data = (temp_dir / "flash.bin").read_bytes()
            assert len(flash_data) == 4 * 1024 * 1024

            print("\n✓ Firmware preparation works correctly")
        finally:
            # Cleanup
            if temp_dir.exists():
                shutil.rmtree(temp_dir, ignore_errors=True)


@pytest.mark.integration
@pytest.mark.qemu
@pytest.mark.skipif(not DOCKER_AVAILABLE, reason="Docker not available")
class TestDockerUtilities:
    """Test Docker utility functions."""

    def test_check_docker_installed(self):
        """Test Docker installation check."""
        from fbuild.deploy.docker_utils import check_docker_installed

        result = check_docker_installed()
        assert result is True, "Docker should be detected as installed"
        print("\n✓ Docker installation check works")

    def test_check_docker_daemon_running(self):
        """Test Docker daemon status check."""
        from fbuild.deploy.docker_utils import check_docker_daemon_running

        result = check_docker_daemon_running()
        assert result is True, "Docker daemon should be running"
        print("\n✓ Docker daemon check works")

    def test_ensure_docker_available(self):
        """Test ensure Docker available function."""
        from fbuild.deploy.docker_utils import ensure_docker_available

        result = ensure_docker_available()
        assert result is True, "Docker should be available"
        print("\n✓ ensure_docker_available works")

    def test_check_docker_image_exists(self):
        """Test Docker image existence check."""
        from fbuild.deploy.docker_utils import check_docker_image_exists

        # Test with an image that definitely doesn't exist
        result = check_docker_image_exists("nonexistent/image:definitely-not-here")
        assert result is False, "Non-existent image should return False"
        print("\n✓ Docker image existence check works")

    def test_get_docker_env(self):
        """Test Docker environment variable setup."""
        from fbuild.deploy.docker_utils import get_docker_env

        env = get_docker_env()

        assert "PYTHONIOENCODING" in env, "PYTHONIOENCODING should be set"
        assert env["PYTHONIOENCODING"] == "utf-8", "PYTHONIOENCODING should be utf-8"
        assert "PYTHONUTF8" in env, "PYTHONUTF8 should be set"
        assert env["PYTHONUTF8"] == "1", "PYTHONUTF8 should be 1"
        print("\n✓ Docker environment setup works")


@pytest.mark.integration
@pytest.mark.qemu
class TestWindowsPathConversion:
    """Test Windows path conversion for Docker volumes."""

    def test_windows_to_docker_path_regular(self):
        """Test path conversion for regular Windows paths."""
        from fbuild.deploy.qemu_runner import QEMURunner

        runner = QEMURunner()

        # Test with a regular path (behavior depends on platform)
        test_path = Path("C:/Users/test/firmware")
        result = runner._windows_to_docker_path(test_path)

        # On Windows with Git Bash, should convert C:/ to /c/
        # On other platforms, should leave path as-is
        assert isinstance(result, str), "Should return a string"
        print(f"\n✓ Path conversion: {test_path} -> {result}")


if __name__ == "__main__":
    # Allow running tests directly
    pytest.main([__file__, "-v", "-s", "-m", "qemu"])
