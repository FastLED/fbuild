"""
QEMU runner module for ESP32 emulation using Docker.

This module handles running ESP32 firmware in QEMU using Docker containers
for ESP32-S3 and other ESP32 variants. It provides an alternative deployment
target for testing without physical hardware.
"""

import _thread
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Optional

from fbuild.subprocess_utils import safe_popen, safe_run

# Docker image constants
DEFAULT_DOCKER_IMAGE = "espressif/idf:latest"
ALTERNATIVE_DOCKER_IMAGE = "espressif/idf:latest"
FALLBACK_DOCKER_IMAGE = "espressif/idf:release-v5.2"

# QEMU binary paths inside espressif/idf Docker container
QEMU_RISCV32_PATH = "/opt/esp/tools/qemu-riscv32/esp_develop_9.2.2_20250817/qemu/bin/qemu-system-riscv32"
QEMU_XTENSA_PATH = "/opt/esp/tools/qemu-xtensa/esp_develop_9.2.2_20250817/qemu/bin/qemu-system-xtensa"

# QEMU wrapper script template (formatted at runtime)
QEMU_WRAPPER_SCRIPT_TEMPLATE = """#!/bin/bash
set -e
echo "Starting {echo_target} QEMU emulation..."
echo "Firmware: {firmware_path}"
echo "Machine: {qemu_machine}"
echo "QEMU system: {qemu_system}"
echo "Container: $(cat /etc/os-release | head -1)"

# Check if firmware file exists
if [ ! -f "{firmware_path}" ]; then
    echo "ERROR: Firmware file not found: {firmware_path}"
    exit 1
fi

# Check firmware size
FIRMWARE_SIZE=$(stat -c%s "{firmware_path}")
echo "Firmware size: $FIRMWARE_SIZE bytes"

# Copy firmware to writable location since QEMU needs write access
cp "{firmware_path}" /tmp/flash.bin
echo "Copied firmware to writable location: /tmp/flash.bin"

# Try different QEMU configurations depending on machine type
if [ "{qemu_machine}" = "esp32c3" ]; then
    # ESP32C3 uses RISC-V architecture
    echo "Running {qemu_system} for {qemu_machine}"
    {qemu_system} \\
        -nographic \\
        -machine {qemu_machine} \\
        -drive file="/tmp/flash.bin",if=mtd,format=raw \\
        -monitor none \\
        -serial mon:stdio
else
    # ESP32 uses Xtensa architecture
    echo "Running {qemu_system} for {qemu_machine}"
    {qemu_system} \\
        -nographic \\
        -machine {qemu_machine} \\
        -drive file="/tmp/flash.bin",if=mtd,format=raw \\
        -global driver=timer.esp32.timg,property=wdt_disable,value=true \\
        -monitor none \\
        -serial mon:stdio
fi

echo "QEMU execution completed"
exit 0
"""


def get_docker_env() -> dict[str, str]:
    """Get environment for Docker commands, handling Git Bash/MSYS2 path conversion."""
    env = os.environ.copy()
    # Set UTF-8 encoding environment variables for Windows
    env["PYTHONIOENCODING"] = "utf-8"
    env["PYTHONUTF8"] = "1"
    # Only set MSYS_NO_PATHCONV if we're in a Git Bash/MSYS2 environment
    if "MSYSTEM" in os.environ or os.environ.get("TERM") == "xterm" or "bash.exe" in os.environ.get("SHELL", ""):
        env["MSYS_NO_PATHCONV"] = "1"
    return env


def check_docker_available() -> bool:
    """Check if Docker is available and running.

    Returns:
        True if Docker is available, False otherwise
    """
    try:
        result = safe_run(
            ["docker", "version"],
            capture_output=True,
            timeout=10,
            env=get_docker_env(),
        )
        return result.returncode == 0
    except (subprocess.SubprocessError, FileNotFoundError, subprocess.TimeoutExpired):
        return False


def ensure_docker_available() -> bool:
    """Ensure Docker is available, attempting to start daemon if necessary.

    This function checks if Docker is installed and running, and attempts
    to start Docker Desktop automatically if it's not running.

    Returns:
        True if Docker is now available, False otherwise
    """
    from fbuild.deploy.docker_utils import ensure_docker_available as _ensure

    return _ensure()


class QEMURunner:
    """Runner for ESP32 QEMU emulation using Docker containers.

    This class handles running ESP32 firmware in a QEMU emulator inside
    Docker containers. It supports esp32, esp32c3, and esp32s3 targets.
    """

    def __init__(self, docker_image: Optional[str] = None, verbose: bool = False):
        """Initialize QEMU runner.

        Args:
            docker_image: Docker image to use, defaults to espressif/idf:latest
            verbose: Whether to show verbose output
        """
        self.docker_image = docker_image or DEFAULT_DOCKER_IMAGE
        self.verbose = verbose
        self.container_name: Optional[str] = None
        # Use Linux-style paths for all containers since we're using Ubuntu/Alpine
        self.firmware_mount_path = "/workspace/firmware"

    def pull_image(self) -> bool:
        """Pull the Docker image if not already available.

        Returns:
            True if image is available, False otherwise
        """
        print(f"Ensuring Docker image {self.docker_image} is available...")
        try:
            # Check if image exists locally
            result = safe_run(
                ["docker", "images", "-q", self.docker_image],
                capture_output=True,
                text=True,
                env=get_docker_env(),
            )
            if result.stdout.strip():
                print(f"Image {self.docker_image} already available locally")
                return True

            # Image doesn't exist, pull it
            print(f"Pulling Docker image: {self.docker_image}")
            print("This may take a few minutes on first run...")
            result = safe_run(
                ["docker", "pull", self.docker_image],
                env=get_docker_env(),
            )
            if result.returncode == 0:
                print(f"Successfully pulled {self.docker_image}")
                return True
            else:
                print(f"Failed to pull {self.docker_image}")
                # Try alternative image
                if self.docker_image == DEFAULT_DOCKER_IMAGE:
                    print(f"Trying fallback image: {FALLBACK_DOCKER_IMAGE}")
                    self.docker_image = FALLBACK_DOCKER_IMAGE
                    result = safe_run(
                        ["docker", "pull", self.docker_image],
                        env=get_docker_env(),
                    )
                    return result.returncode == 0
                return False

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            print(f"Error pulling Docker image: {e}")
            return False

    def _prepare_firmware(self, firmware_path: Path, flash_size_mb: int = 4, machine: str = "esp32") -> Path:
        """Prepare firmware files for mounting into Docker container.

        Creates a complete flash image with bootloader, partition table,
        and application at their correct offsets for QEMU.

        ESP32/ESP32-S2 Flash Layout:
        - 0x1000: Bootloader (second stage)
        - 0x8000: Partition table
        - 0x10000: Application (firmware.bin)

        ESP32-S3/ESP32-C3/ESP32-C6 Flash Layout:
        - 0x0000: Bootloader (second stage)
        - 0x8000: Partition table
        - 0x10000: Application (firmware.bin)

        Args:
            firmware_path: Path to firmware.bin file
            flash_size_mb: Flash size in MB (must be 2, 4, 8, or 16)
            machine: QEMU machine type (esp32, esp32s3, esp32c3)

        Returns:
            Path to the prepared firmware directory
        """
        if flash_size_mb not in [2, 4, 8, 16]:
            raise ValueError(f"Flash size must be 2, 4, 8, or 16 MB, got {flash_size_mb}")

        # Create temporary directory for firmware files
        temp_dir = Path(tempfile.mkdtemp(prefix="qemu_firmware_"))

        try:
            # Copy firmware file
            shutil.copy2(firmware_path, temp_dir / "firmware.bin")

            # Create proper flash image for QEMU
            flash_size = flash_size_mb * 1024 * 1024

            # ESP32 flash layout offsets - different MCUs have different bootloader offsets
            # ESP32/ESP32-S2: 0x1000, ESP32-S3/C3/C6: 0x0
            if machine in ["esp32", "esp32s2"]:
                BOOTLOADER_OFFSET = 0x1000
            else:
                BOOTLOADER_OFFSET = 0x0
            PARTITION_OFFSET = 0x8000
            APP_OFFSET = 0x10000

            # Start with erased flash (0xFF)
            flash_data = bytearray(b"\xff" * flash_size)

            # Try to find bootloader.bin and partitions.bin in the same directory
            build_dir = firmware_path.parent
            bootloader_path = build_dir / "bootloader.bin"
            partitions_path = build_dir / "partitions.bin"

            # Place bootloader at 0x1000 if available
            if bootloader_path.exists():
                bootloader_data = bootloader_path.read_bytes()
                bootloader_end = BOOTLOADER_OFFSET + len(bootloader_data)
                if bootloader_end <= flash_size:
                    flash_data[BOOTLOADER_OFFSET:bootloader_end] = bootloader_data
                    if self.verbose:
                        print(f"  Bootloader: {len(bootloader_data)} bytes at 0x{BOOTLOADER_OFFSET:X}")
                else:
                    print("Warning: Bootloader too large to fit in flash")
            else:
                if self.verbose:
                    print(f"  Warning: bootloader.bin not found at {bootloader_path}")

            # Place partition table at 0x8000 if available
            if partitions_path.exists():
                partitions_data = partitions_path.read_bytes()
                partitions_end = PARTITION_OFFSET + len(partitions_data)
                if partitions_end <= flash_size:
                    flash_data[PARTITION_OFFSET:partitions_end] = partitions_data
                    if self.verbose:
                        print(f"  Partitions: {len(partitions_data)} bytes at 0x{PARTITION_OFFSET:X}")
                else:
                    print("Warning: Partition table too large to fit in flash")
            else:
                if self.verbose:
                    print(f"  Warning: partitions.bin not found at {partitions_path}")

            # Place firmware at 0x10000
            firmware_data = firmware_path.read_bytes()
            firmware_end = APP_OFFSET + len(firmware_data)
            if firmware_end > flash_size:
                raise ValueError(f"Firmware size ({len(firmware_data)} bytes) exceeds available space (flash_size={flash_size} - app_offset={APP_OFFSET})")
            flash_data[APP_OFFSET:firmware_end] = firmware_data
            if self.verbose:
                print(f"  Application: {len(firmware_data)} bytes at 0x{APP_OFFSET:X}")

            # Write the complete flash image
            (temp_dir / "flash.bin").write_bytes(bytes(flash_data))
            print(f"Created flash image: {flash_size_mb}MB ({flash_size:,} bytes)")

            return temp_dir

        except KeyboardInterrupt:
            shutil.rmtree(temp_dir, ignore_errors=True)
            _thread.interrupt_main()
            raise
        except Exception as e:
            # Clean up on error
            shutil.rmtree(temp_dir, ignore_errors=True)
            raise e

    def _get_qemu_config(self, machine: str) -> tuple[str, str, str]:
        """Get QEMU configuration for the given machine type.

        Args:
            machine: QEMU machine type (esp32, esp32c3, esp32s3)

        Returns:
            Tuple of (qemu_system_path, qemu_machine, echo_target)
        """
        if machine == "esp32c3":
            return QEMU_RISCV32_PATH, "esp32c3", "ESP32C3"
        elif machine == "esp32s3":
            return QEMU_XTENSA_PATH, "esp32s3", "ESP32S3"
        else:
            # Default to ESP32 (Xtensa)
            return QEMU_XTENSA_PATH, "esp32", "ESP32"

    def _build_qemu_command(
        self,
        machine: str = "esp32",
    ) -> list[str]:
        """Build QEMU command to run inside Docker container.

        Args:
            machine: QEMU machine type (esp32, esp32c3, esp32s3)

        Returns:
            List of command arguments for QEMU
        """
        firmware_path = f"{self.firmware_mount_path}/flash.bin"
        qemu_system, qemu_machine, echo_target = self._get_qemu_config(machine)

        # Format the wrapper script template with runtime values
        wrapper_script = QEMU_WRAPPER_SCRIPT_TEMPLATE.format(
            echo_target=echo_target,
            firmware_path=firmware_path,
            qemu_machine=qemu_machine,
            qemu_system=qemu_system,
        )

        return ["bash", "-c", wrapper_script]

    def _windows_to_docker_path(self, path: Path) -> str:
        """Convert Windows path to Docker volume mount format.

        Args:
            path: Path to convert

        Returns:
            Docker-compatible path string
        """
        # Check if we're in Git Bash/MSYS2 environment
        is_git_bash = "MSYSTEM" in os.environ or os.environ.get("TERM") == "xterm" or "bash.exe" in os.environ.get("SHELL", "")

        path_str = str(path)

        if os.name == "nt" and is_git_bash:
            # Convert C:\path\to\dir to /c/path/to/dir for Git Bash
            path_str = path_str.replace("\\", "/")
            if len(path_str) > 2 and path_str[1:3] == ":/":  # Drive letter
                path_str = "/" + path_str[0].lower() + path_str[2:]

        return path_str

    def run(
        self,
        firmware_path: Path,
        machine: str = "esp32s3",
        timeout: int = 30,
        flash_size: int = 4,
        interrupt_regex: Optional[str] = None,
        output_file: Optional[Path] = None,
        skip_pull: bool = False,
    ) -> int:
        """Run ESP32 firmware in QEMU using Docker.

        Args:
            firmware_path: Path to firmware.bin file
            machine: QEMU machine type (esp32, esp32c3, esp32s3)
            timeout: Timeout in seconds (timeout is treated as success)
            flash_size: Flash size in MB
            interrupt_regex: Regex pattern to detect in output (informational)
            output_file: Optional file path to write QEMU output to
            skip_pull: Skip pulling Docker image (assumes image already exists)

        Returns:
            Exit code: 0 for success (including timeout), non-zero for error
        """
        if not check_docker_available():
            print("ERROR: Docker is not available or not running", file=sys.stderr)
            print("Please install Docker and ensure it's running", file=sys.stderr)
            print()
            print("Install Docker:")
            print("  - Windows/Mac: https://www.docker.com/products/docker-desktop")
            print("  - Linux: https://docs.docker.com/engine/install/")
            return 1

        # Pull image if needed
        if not skip_pull:
            if not self.pull_image():
                print("ERROR: Failed to pull Docker image", file=sys.stderr)
                return 1
        else:
            print(f"Skipping image pull (using existing {self.docker_image})")

        # Validate firmware path
        if not firmware_path.exists():
            print(f"ERROR: Firmware not found at {firmware_path}", file=sys.stderr)
            return 1

        # Prepare firmware files
        print(f"Preparing firmware from: {firmware_path}")
        temp_firmware_dir: Optional[Path] = None

        try:
            temp_firmware_dir = self._prepare_firmware(firmware_path, flash_size, machine)

            # Generate unique container name
            self.container_name = f"fbuild-qemu-{machine}-{int(time.time())}"

            # Convert path for Docker volume mount
            docker_firmware_path = self._windows_to_docker_path(temp_firmware_dir)

            # Build Docker run command
            docker_cmd = [
                "docker",
                "run",
                "--rm",
                "--name",
                self.container_name,
                "-v",
                f"{docker_firmware_path}:{self.firmware_mount_path}:ro",
            ]

            # Add image and QEMU command
            docker_cmd.append(self.docker_image)
            docker_cmd.extend(self._build_qemu_command(machine))

            print(f"Running QEMU in Docker container: {self.container_name}")
            if self.verbose:
                print(f"Docker command: {' '.join(docker_cmd)}")

            # Run Docker container with streaming output
            return self._run_container_streaming(
                docker_cmd,
                timeout=timeout,
                interrupt_regex=interrupt_regex,
                output_file=output_file,
            )

        except KeyboardInterrupt:
            _thread.interrupt_main()
            raise
        except Exception as e:
            print(f"ERROR: {e}", file=sys.stderr)
            return 1

        finally:
            # Cleanup temp directory
            if temp_firmware_dir and temp_firmware_dir.exists():
                shutil.rmtree(temp_firmware_dir, ignore_errors=True)

    def _run_container_streaming(
        self,
        cmd: list[str],
        timeout: int = 30,
        interrupt_regex: Optional[str] = None,
        output_file: Optional[Path] = None,
    ) -> int:
        """Run Docker container with streaming output.

        Args:
            cmd: Docker command to run
            timeout: Timeout in seconds
            interrupt_regex: Regex pattern to detect in output
            output_file: Optional file to write output to

        Returns:
            Exit code (0 for success/timeout, non-zero for error)
        """
        env = get_docker_env()
        output_handle = None
        timeout_occurred = False
        start_time = time.time()

        if output_file:
            try:
                output_file.parent.mkdir(parents=True, exist_ok=True)
                output_handle = open(output_file, "w", encoding="utf-8")
            except KeyboardInterrupt:
                _thread.interrupt_main()
                raise
            except Exception as e:
                print(f"Warning: Could not open output file {output_file}: {e}")

        try:
            # Start the Docker process
            proc = safe_popen(
                cmd,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                env=env,
                text=True,
                errors="replace",
            )

            # Stream output
            while True:
                # Check timeout
                if (time.time() - start_time) > timeout:
                    print(f"Timeout reached ({timeout}s), terminating container...")
                    timeout_occurred = True
                    break

                # Read line (non-blocking would be better, but this works)
                if proc.stdout:
                    line = proc.stdout.readline()
                    if not line:
                        # Process ended
                        if proc.poll() is not None:
                            break
                        continue

                    # Print the line
                    print(line.rstrip())

                    # Write to output file
                    if output_handle:
                        output_handle.write(line)
                        output_handle.flush()

                    # Check for interrupt pattern
                    if interrupt_regex and re.search(interrupt_regex, line):
                        print(f"Pattern detected: {interrupt_regex}")

                # Check if process ended
                if proc.poll() is not None:
                    # Read any remaining output
                    if proc.stdout:
                        for remaining_line in proc.stdout:
                            print(remaining_line.rstrip())
                            if output_handle:
                                output_handle.write(remaining_line)
                    break

            # Handle timeout case
            if timeout_occurred:
                # Stop the container
                if self.container_name:
                    try:
                        safe_run(
                            ["docker", "stop", "--time=1", self.container_name],
                            capture_output=True,
                            timeout=10,
                            env=env,
                        )
                    except KeyboardInterrupt:
                        _thread.interrupt_main()
                        raise
                    except Exception as e:
                        print(f"Warning: Failed to stop container: {e}")

                # Wait for process to complete
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()

                print("Process terminated due to timeout - treating as success")
                return 0

            # Return actual exit code
            return proc.returncode if proc.returncode is not None else 1

        except KeyboardInterrupt:
            print("\nInterrupted by user")
            if self.container_name:
                try:
                    safe_run(
                        ["docker", "stop", "--time=1", self.container_name],
                        capture_output=True,
                        timeout=10,
                        env=env,
                    )
                except KeyboardInterrupt:
                    _thread.interrupt_main()
                except Exception:
                    pass
            _thread.interrupt_main()
            return 130

        except Exception as e:
            print(f"Error during execution: {e}")
            return 1

        finally:
            if output_handle:
                output_handle.close()


def map_board_to_machine(board_id: str) -> str:
    """Map board ID to QEMU machine type.

    Args:
        board_id: Board identifier (e.g., 'esp32s3', 'esp32-c6-devkitc-1')

    Returns:
        QEMU machine type (esp32, esp32c3, esp32s3)
    """
    board_lower = board_id.lower()

    # Map boards to QEMU machine types
    if "esp32s3" in board_lower or "s3" in board_lower:
        return "esp32s3"
    elif "esp32c3" in board_lower or "c3" in board_lower:
        return "esp32c3"
    elif "esp32c6" in board_lower or "c6" in board_lower:
        # ESP32-C6 uses RISC-V, similar to C3 but not fully supported in QEMU yet
        # Fall back to esp32c3 for now
        print("Note: ESP32-C6 QEMU support is limited, using esp32c3 emulation")
        return "esp32c3"
    else:
        return "esp32"


def main() -> int:
    """Main entry point for testing QEMU runner."""
    import argparse

    parser = argparse.ArgumentParser(description="Run ESP32 firmware in QEMU using Docker")
    parser.add_argument("firmware_path", type=Path, help="Path to firmware.bin file")
    parser.add_argument(
        "--machine",
        type=str,
        default="esp32s3",
        help="QEMU machine type: esp32, esp32c3, esp32s3 (default: esp32s3)",
    )
    parser.add_argument("--timeout", type=int, default=30, help="Timeout in seconds (default: 30)")
    parser.add_argument("--flash-size", type=int, default=4, help="Flash size in MB (default: 4)")
    parser.add_argument("--interrupt-regex", type=str, help="Regex pattern to detect in output")
    parser.add_argument("--output-file", type=Path, help="File to write QEMU output to")
    parser.add_argument("--skip-pull", action="store_true", help="Skip pulling Docker image")
    parser.add_argument("-v", "--verbose", action="store_true", help="Verbose output")

    args = parser.parse_args()

    if not args.firmware_path.exists():
        print(f"ERROR: Firmware path does not exist: {args.firmware_path}")
        return 1

    runner = QEMURunner(verbose=args.verbose)
    return runner.run(
        firmware_path=args.firmware_path,
        machine=args.machine,
        timeout=args.timeout,
        flash_size=args.flash_size,
        interrupt_regex=args.interrupt_regex,
        output_file=args.output_file,
        skip_pull=args.skip_pull,
    )


if __name__ == "__main__":
    sys.exit(main())
