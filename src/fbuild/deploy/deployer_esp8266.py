"""ESP8266 Deployer - Firmware upload using esptool.

This module handles uploading firmware to ESP8266 devices via serial port.
Uses esptool.py for flashing.
"""

from pathlib import Path
from typing import Optional

from ..subprocess_utils import get_python_executable, safe_run
from .deployer import DeploymentResult, IDeployer


class ESP8266Deployer(IDeployer):
    """Handles firmware deployment to ESP8266 devices.

    Uses esptool.py to flash firmware binaries to ESP8266 via serial port.
    """

    def __init__(self, verbose: bool = False):
        """Initialize ESP8266 deployer.

        Args:
            verbose: Whether to show verbose output
        """
        self.verbose = verbose

    def deploy(
        self,
        project_dir: Path,
        env_name: str,
        port: Optional[str] = None,
    ) -> DeploymentResult:
        """Deploy firmware to ESP8266 device.

        Args:
            project_dir: Path to project directory containing build artifacts
            env_name: Environment name (from platformio.ini)
            port: Serial port (e.g., "COM3", "/dev/ttyUSB0"), auto-detect if None

        Returns:
            DeploymentResult with success status and message
        """
        # Find firmware binary in build directory
        build_dir = project_dir / ".fbuild" / env_name
        firmware_bin = build_dir / "firmware.bin"

        if not firmware_bin.exists():
            return DeploymentResult(
                success=False,
                message=f"Firmware binary not found: {firmware_bin}",
            )

        if not port:
            return DeploymentResult(
                success=False,
                message="Serial port not specified for ESP8266 deployment",
            )

        firmware_path = firmware_bin
        baud_rate = 115200  # Default baud rate for ESP8266

        if not firmware_path.exists():
            return DeploymentResult(
                success=False,
                message=f"Firmware not found: {firmware_path}",
            )

        if not port:
            return DeploymentResult(
                success=False,
                message="Serial port not specified",
            )

        # Build esptool command
        # ESP8266 typically flashes firmware at 0x0
        cmd = [
            get_python_executable(),
            "-m",
            "esptool",
            "--chip",
            "esp8266",
            "--port",
            port,
            "--baud",
            str(baud_rate),
            "write_flash",
            "--flash_mode",
            "dio",
            "--flash_size",
            "4MB",
            "0x0",
            str(firmware_path),
        ]

        if self.verbose:
            print(f"Running esptool: {' '.join(cmd)}")

        try:
            result = safe_run(
                cmd,
                timeout=120000,  # 2 minutes
                description="Flash firmware to ESP8266",
            )

            if result.returncode == 0:
                return DeploymentResult(
                    success=True,
                    message=f"Successfully deployed firmware to {port}",
                )
            else:
                error_msg = result.stderr if result.stderr else "Unknown error"
                return DeploymentResult(
                    success=False,
                    message=f"Flash failed: {error_msg}",
                )

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            return DeploymentResult(
                success=False,
                message=f"Deployment failed: {str(e)}",
            )
