"""
Firmware deployment module for uploading to embedded devices.

This module handles flashing firmware to ESP32 devices using esptool.
Includes automatic crash-loop recovery for devices stuck in rapid reboot cycles.
"""

import io
import random
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Optional

from fbuild.config import PlatformIOConfig
from fbuild.packages import Cache

from ..subprocess_utils import get_python_executable, safe_popen
from .deployer import DeploymentError, DeploymentResult, IDeployer
from .esptool_utils import is_crash_loop_error
from .platform_utils import get_filtered_env
from .serial_utils import detect_serial_port


def run_with_watchdog_timeout(
    cmd: list[str],
    timeout: int,
    inactivity_timeout: int = 30,
    verbose: bool = False,
    **kwargs,
) -> subprocess.CompletedProcess:
    """Run a command with both total timeout and inactivity timeout.

    This function provides more robust timeout handling than subprocess.run(timeout=N)
    on Windows, where kernel-level I/O operations (e.g., serial port reads) can block
    process termination signals indefinitely.

    Args:
        cmd: Command to execute
        timeout: Maximum total execution time in seconds
        inactivity_timeout: Maximum time without output in seconds (default: 30)
        verbose: Whether to print verbose output
        **kwargs: Additional arguments passed to subprocess.Popen

    Returns:
        CompletedProcess with returncode, stdout, stderr

    Raises:
        subprocess.TimeoutExpired: If timeout or inactivity_timeout is exceeded

    Note:
        On Windows, if the process blocks in kernel I/O, this function will:
        1. Attempt graceful termination (SIGTERM)
        2. Wait 5 seconds for process to exit
        3. Force kill (SIGKILL/TerminateProcess) if still running
        4. Raise TimeoutExpired with appropriate error message
    """
    start_time = time.time()
    last_output_time = start_time

    # Ensure we capture output for monitoring
    if "stdout" not in kwargs:
        kwargs["stdout"] = subprocess.PIPE
    if "stderr" not in kwargs:
        kwargs["stderr"] = subprocess.PIPE

    # Use safe_popen to apply platform-specific flags
    process = safe_popen(cmd, **kwargs)

    stdout_data = io.BytesIO()
    stderr_data = io.BytesIO()

    # Monitoring thread flags
    timed_out = False
    timeout_reason: Optional[str] = None

    def monitor_output():
        """Monitor process output and enforce timeouts."""
        nonlocal last_output_time, timed_out, timeout_reason

        while process.poll() is None:
            elapsed = time.time() - start_time
            time_since_output = time.time() - last_output_time

            # Check total timeout
            if elapsed > timeout:
                timed_out = True
                timeout_reason = f"Total timeout ({timeout}s) exceeded"
                if verbose:
                    print(f"\n⚠️  {timeout_reason}", flush=True)
                break

            # Check inactivity timeout
            if time_since_output > inactivity_timeout:
                timed_out = True
                timeout_reason = f"No output for {inactivity_timeout}s (process may be stuck in kernel I/O)"
                if verbose:
                    print(f"\n⚠️  {timeout_reason}", flush=True)
                break

            time.sleep(0.1)

    # Start monitoring thread
    monitor_thread = threading.Thread(target=monitor_output, daemon=True)
    monitor_thread.start()

    # Read output in real-time using threads to avoid blocking
    # We need separate reader threads because read() blocks
    import queue

    output_queue = queue.Queue()

    def read_stream(stream, stream_name):
        """Read from a stream in a separate thread."""
        try:
            while True:
                chunk = stream.read(1024)
                if not chunk:
                    break
                output_queue.put((stream_name, chunk))
        except Exception:
            pass  # Stream closed or error

    # Start reader threads
    stdout_thread = None
    stderr_thread = None

    if process.stdout:
        stdout_thread = threading.Thread(target=read_stream, args=(process.stdout, "stdout"), daemon=True)
        stdout_thread.start()

    if process.stderr:
        stderr_thread = threading.Thread(target=read_stream, args=(process.stderr, "stderr"), daemon=True)
        stderr_thread.start()

    # Process output from queue
    try:
        while process.poll() is None and not timed_out:
            try:
                # Non-blocking queue get with timeout
                stream_name, chunk = output_queue.get(timeout=0.1)

                if stream_name == "stdout":
                    stdout_data.write(chunk)
                    if verbose:
                        sys.stdout.buffer.write(chunk)
                        sys.stdout.flush()
                else:  # stderr
                    stderr_data.write(chunk)
                    if verbose:
                        sys.stderr.buffer.write(chunk)
                        sys.stderr.flush()

                last_output_time = time.time()
            except queue.Empty:
                # No output available, continue checking timeout
                pass

        # If timed out, forcefully terminate process
        if timed_out:
            if verbose:
                print("⏳ Attempting to terminate process...", flush=True)

            # Try graceful termination first
            process.terminate()

            # Wait up to 5 seconds for graceful exit
            try:
                process.wait(timeout=5)
                if verbose:
                    print("✓ Process terminated gracefully", flush=True)
            except subprocess.TimeoutExpired:
                # Graceful termination failed - force kill
                if verbose:
                    print("⚠️  Graceful termination failed, force killing process...", flush=True)

                if sys.platform == "win32":
                    # On Windows, use TerminateProcess (more forceful than kill())
                    import ctypes

                    kernel32 = ctypes.windll.kernel32
                    handle = int(process._handle)
                    kernel32.TerminateProcess(handle, 1)
                else:
                    # On Unix, use SIGKILL
                    process.kill()

                # Wait for process to exit
                process.wait()

                if verbose:
                    print("✓ Process force killed", flush=True)

            # Give reader threads a moment to finish reading any buffered data
            time.sleep(0.2)

            # Drain any remaining output from the queue before raising exception
            while True:
                try:
                    stream_name, chunk = output_queue.get(timeout=0.1)
                    if stream_name == "stdout":
                        stdout_data.write(chunk)
                    else:
                        stderr_data.write(chunk)
                except queue.Empty:
                    break

            # Raise TimeoutExpired with detailed message
            raise subprocess.TimeoutExpired(
                cmd=cmd,
                timeout=timeout if timeout_reason and "Total timeout" in timeout_reason else inactivity_timeout,
                output=stdout_data.getvalue(),
                stderr=stderr_data.getvalue(),
            )

        # Drain remaining output from queue
        # Give reader threads a moment to finish
        time.sleep(0.1)
        while not output_queue.empty():
            try:
                stream_name, chunk = output_queue.get_nowait()
                if stream_name == "stdout":
                    stdout_data.write(chunk)
                    if verbose:
                        sys.stdout.buffer.write(chunk)
                        sys.stdout.flush()
                else:
                    stderr_data.write(chunk)
                    if verbose:
                        sys.stderr.buffer.write(chunk)
                        sys.stderr.flush()
            except queue.Empty:
                break

    finally:
        # Ensure process is cleaned up
        if process.poll() is None:
            process.kill()
            process.wait()

        # Wait for monitor thread to finish
        monitor_thread.join(timeout=1.0)

    # Return CompletedProcess-like result
    return subprocess.CompletedProcess(
        args=cmd,
        returncode=process.returncode,
        stdout=stdout_data.getvalue(),
        stderr=stderr_data.getvalue(),
    )


class ESP32Deployer(IDeployer):
    """Handles firmware deployment to embedded devices."""

    def __init__(self, verbose: bool = False):
        """Initialize deployer.

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
        """Deploy firmware to a device.

        Args:
            project_dir: Path to project directory
            env_name: Environment name to deploy
            port: Serial port to use (auto-detect if None)

        Returns:
            DeploymentResult with success status and message
        """
        try:
            # Load platformio.ini
            ini_path = project_dir / "platformio.ini"
            if not ini_path.exists():
                raise DeploymentError(f"platformio.ini not found in {project_dir}")

            config = PlatformIOConfig(ini_path)
            env_config = config.get_env_config(env_name)

            # Get board and platform
            board_id = env_config.get("board")
            platform_url = env_config.get("platform")

            if not board_id or not platform_url:
                raise DeploymentError("Board or platform not specified in platformio.ini")

            # Determine platform type
            if "espressif32" in platform_url or board_id.startswith("esp32"):
                return self._deploy_esp32(project_dir, env_name, board_id, port, platform_url)
            else:
                raise DeploymentError(f"Deployment not supported for board: {board_id}")

        except DeploymentError as e:
            return DeploymentResult(success=False, message=str(e))
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            return DeploymentResult(success=False, message=f"Unexpected deployment error: {e}")

    def _deploy_esp32(
        self,
        project_dir: Path,
        env_name: str,
        board_id: str,
        port: Optional[str],
        platform_url: str,
    ) -> DeploymentResult:
        """Deploy firmware to ESP32 device.

        Args:
            project_dir: Path to project directory
            env_name: Environment name
            board_id: Board identifier
            port: Serial port (auto-detect if None)
            platform_url: Platform package URL

        Returns:
            DeploymentResult with success status
        """
        # Get build directory
        build_dir = project_dir / ".fbuild" / "build" / env_name
        firmware_bin = (build_dir / "firmware.bin").absolute()
        bootloader_bin = (build_dir / "bootloader.bin").absolute()
        partitions_bin = (build_dir / "partitions.bin").absolute()

        if not firmware_bin.exists():
            raise DeploymentError(f"Firmware not found at {firmware_bin}. Run 'fbuild build' first.")

        # Get cache and ensure platform/toolchain packages
        cache = Cache(project_dir)

        # Import ESP32 packages
        from fbuild.packages.framework_esp32 import FrameworkESP32
        from fbuild.packages.platform_esp32 import PlatformESP32

        # Ensure platform is downloaded first (needed to get board JSON)
        platform = PlatformESP32(cache, platform_url, show_progress=self.verbose)
        platform.ensure_platform()

        # Get board JSON to determine MCU and required packages
        board_json = platform.get_board_json(board_id)
        mcu = board_json.get("build", {}).get("mcu", "esp32")
        packages = platform.get_required_packages(mcu)

        # Initialize framework
        framework_url = packages.get("framework-arduinoespressif32")
        libs_url = packages.get("framework-arduinoespressif32-libs")
        if not framework_url or not libs_url:
            raise DeploymentError("Framework URLs not found in platform package")

        framework = FrameworkESP32(cache, framework_url, libs_url, show_progress=self.verbose)
        framework.ensure_framework()

        # Auto-detect port if not specified
        if not port:
            port = detect_serial_port(verbose=self.verbose)
            if not port:
                raise DeploymentError("No serial port specified and auto-detection failed. " + "Use --port to specify a port.")

        if self.verbose:
            print(f"Using port: {port}")

        # NOTE: We deliberately skip port pre-verification here.
        # Raw serial.Serial() cannot recover a stuck Windows USB-CDC driver.
        # esptool's connect sequence uses DTR/RTS hardware signals to reset
        # the ESP32, which clears stuck driver states automatically.
        # Let esptool handle all port access and recovery.

        # Determine chip type and flash parameters from board JSON
        chip = self._get_chip_type(mcu)
        flash_mode = board_json.get("build", {}).get("flash_mode", "dio")

        # Get flash frequency and convert to esptool format
        f_flash = board_json.get("build", {}).get("f_flash", "80000000L")
        if isinstance(f_flash, str) and f_flash.endswith("L"):
            freq_value = int(f_flash.rstrip("L"))
            flash_freq = f"{freq_value // 1000000}m"
        elif isinstance(f_flash, (int, float)):
            flash_freq = f"{int(f_flash // 1000000)}m"
        else:
            flash_freq = "80m"

        flash_size = "detect"

        # CRITICAL FIX: ESP32-C6/C3/C2/H2/S3 ROM bootloader can only load the second-stage
        # bootloader in DIO mode. Must use DIO for flashing even if app uses QIO.
        # See: https://github.com/espressif/arduino-esp32/discussions/10418
        if mcu in ["esp32c6", "esp32c3", "esp32c2", "esp32h2", "esp32s3"]:
            flash_mode = "dio"

        # Determine bootloader offset based on MCU
        # ESP32/ESP32-S2: 0x1000, ESP32-P4: 0x2000, others: 0x0
        if mcu in ["esp32", "esp32s2"]:
            bootloader_offset = "0x1000"
        elif mcu == "esp32p4":
            bootloader_offset = "0x2000"
        else:
            bootloader_offset = "0x0"

        # Find boot_app0.bin in framework tools
        boot_app0_bin = framework.framework_path / "tools" / "partitions" / "boot_app0.bin"

        # Build esptool command to flash multiple binaries at different offsets
        # Flash layout: bootloader @ offset, partition table @ 0x8000, boot_app0 @ 0xe000, app @ 0x10000
        cmd = [
            get_python_executable(),
            "-m",
            "esptool",
            "--chip",
            chip,
            "--port",
            port,
            "--baud",
            "460800",
            "--before",
            "default_reset",  # Use DTR/RTS to reset chip into bootloader (recovers stuck USB-CDC)
            "--after",
            "hard_reset",  # Reset chip after upload to run new firmware
            "write_flash",
            "-z",  # Compress
            "--flash-mode",
            flash_mode,
            "--flash-freq",
            flash_freq,
            "--flash-size",
            flash_size,
        ]

        # Add bootloader if it exists
        if bootloader_bin.exists():
            cmd.extend([bootloader_offset, str(bootloader_bin)])
        else:
            if self.verbose:
                print("Warning: bootloader.bin not found, skipping")

        # Add partition table if it exists
        if partitions_bin.exists():
            cmd.extend(["0x8000", str(partitions_bin)])
        else:
            if self.verbose:
                print("Warning: partitions.bin not found, skipping")

        # Add boot_app0.bin if it exists
        if boot_app0_bin.exists():
            cmd.extend(["0xe000", str(boot_app0_bin)])
        else:
            if self.verbose:
                print("Warning: boot_app0.bin not found, skipping")

        # Add application firmware at 0x10000
        cmd.extend(["0x10000", str(firmware_bin)])

        if self.verbose:
            print("Flashing firmware to device...")
            print(f"  Bootloader: {bootloader_offset}")
            print("  Partition table: 0x8000")
            print("  Boot app: 0xe000")
            print("  Application: 0x10000")
            print(f"Running: {' '.join(cmd)}")

        # Execute esptool with crash-loop recovery
        # Use 120 second timeout to prevent hanging if device is unresponsive
        # Use 30 second inactivity timeout to detect stuck I/O operations
        upload_timeout = 120
        inactivity_timeout = 30

        # Crash-loop recovery parameters
        max_recovery_attempts = 20
        min_delay_ms = 100
        max_delay_ms = 1500

        result = None
        recovery_mode_activated = False

        for attempt in range(1, max_recovery_attempts + 1):
            try:
                if sys.platform == "win32":
                    # Use filtered environment to avoid MSYS issues
                    env = get_filtered_env()
                    # Use watchdog timeout for better Windows serial port handling
                    result = run_with_watchdog_timeout(
                        cmd,
                        timeout=upload_timeout,
                        inactivity_timeout=inactivity_timeout,
                        verbose=self.verbose,
                        cwd=project_dir,
                        env=env,
                        shell=False,
                    )
                else:
                    # Use watchdog timeout for consistency across platforms
                    result = run_with_watchdog_timeout(
                        cmd,
                        timeout=upload_timeout,
                        inactivity_timeout=inactivity_timeout,
                        verbose=self.verbose,
                        cwd=project_dir,
                        shell=False,
                    )
            except subprocess.TimeoutExpired as e:
                # Check if we should retry
                if attempt == 1:
                    # First attempt timeout - might be crash-loop or stuck I/O
                    recovery_mode_activated = True
                    if self.verbose:
                        print(f"\nConnection timeout detected. Attempting recovery (attempt {attempt}/{max_recovery_attempts})...")

                if recovery_mode_activated and attempt < max_recovery_attempts:
                    delay_ms = random.randint(min_delay_ms, max_delay_ms)
                    time.sleep(delay_ms / 1000.0)
                    continue
                else:
                    # Build detailed error message
                    error_msg = f"Upload timed out after {upload_timeout}s."
                    if e.stderr:
                        stderr_text = e.stderr.decode("utf-8", errors="replace") if isinstance(e.stderr, bytes) else str(e.stderr)
                        if "No output for" in stderr_text:
                            error_msg += "\n\n⚠️  Process stuck in kernel I/O (Windows serial port driver issue)."
                            error_msg += "\n\nThis is a known Windows USB-CDC driver limitation where serial port"
                            error_msg += "\nread/write operations can block indefinitely in kernel space."
                            error_msg += "\n\nSuggestions:"
                            error_msg += "\n  1. Unplug and replug the USB cable"
                            error_msg += "\n  2. Try a different USB port"
                            error_msg += "\n  3. Reset the device (hold BOOT button, press RESET)"
                            error_msg += "\n  4. Check Device Manager for driver issues (yellow exclamation marks)"
                            error_msg += "\n  5. Update USB-CDC drivers (esp32s3 CDC: CH343/CH340, others: CP210x/FTDI)"
                        else:
                            error_msg += "\n\nDevice may be unresponsive or not in download mode."
                            error_msg += "\n\nSuggestions:"
                            error_msg += "\n  1. Try resetting the device"
                            error_msg += "\n  2. Check USB cable connection"
                            error_msg += "\n  3. Verify correct port is selected"
                    else:
                        error_msg += " Device may be unresponsive or not in download mode. Try resetting the device."

                    return DeploymentResult(
                        success=False,
                        message=error_msg,
                        port=port,
                    )

            # Check result
            if result and result.returncode == 0:
                # Success!
                if recovery_mode_activated and self.verbose:
                    print(f"✓ Recovery successful on attempt {attempt}")
                break  # Exit retry loop

            # Check if this is a crash-loop error
            if result:
                error_output = ""
                if result.stderr:
                    error_output += result.stderr.decode("utf-8", errors="replace")
                if result.stdout:
                    error_output += result.stdout.decode("utf-8", errors="replace")

                is_crash_loop = is_crash_loop_error(error_output)

                if is_crash_loop and attempt == 1:
                    # First attempt failed with crash-loop error - activate recovery
                    recovery_mode_activated = True
                    if self.verbose:
                        print(f"\nCrash-loop detected on {port}. Attempting recovery...")
                        print("This may take several attempts to catch the bootloader window.")

                if recovery_mode_activated and attempt < max_recovery_attempts:
                    # Continue recovery attempts
                    if self.verbose:
                        print(f"Attempt {attempt}/{max_recovery_attempts}: Resetting port and waiting for bootloader window...", flush=True)

                    # Reset device to release Windows USB-CDC port lock
                    # This is crucial on Windows where the USB-CDC driver doesn't
                    # release port handles immediately after esptool closes the port
                    from fbuild.deploy.esptool_utils import reset_esp32_device

                    reset_esp32_device(port, chip=chip, verbose=self.verbose)

                    # Add delay to let Windows driver fully release port
                    delay_ms = random.randint(min_delay_ms, max_delay_ms)
                    time.sleep(delay_ms / 1000.0)
                    continue
                elif not is_crash_loop:
                    # Non-crash-loop error - fail immediately
                    error_msg = "Upload failed"
                    if result.stderr:
                        error_msg = result.stderr.decode("utf-8", errors="replace")
                    return DeploymentResult(success=False, message=f"Deployment failed: {error_msg}", port=port)
                else:
                    # Exhausted all recovery attempts
                    break

        # Final result check
        if not result or result.returncode != 0:
            error_msg = "Upload failed"
            if result and result.stderr:
                error_msg = result.stderr.decode("utf-8", errors="replace")

            if recovery_mode_activated:
                error_msg += f"\n\nRecovery failed after {max_recovery_attempts} attempts."
                error_msg += "\nSuggestions:"
                error_msg += "\n  1. Manually hold the BOOT button and press RESET while deploying"
                error_msg += "\n  2. Check power supply (ensure sufficient current for your device)"
                error_msg += "\n  3. Try disconnecting and reconnecting the USB cable"

            return DeploymentResult(success=False, message=f"Deployment failed: {error_msg}", port=port)

        # Wait for USB-CDC driver to re-enumerate after hard_reset
        # Windows needs significantly longer for ESP32-S3 USB-Serial/JTAG to re-enumerate
        # This prevents "Failed to attach" errors when immediately trying to open serial monitor
        if sys.platform == "win32":
            if self.verbose:
                print("⏳ Waiting for Windows USB-CDC driver to re-enumerate port...")
            time.sleep(5.0)  # 5 second delay for Windows port re-enumeration (increased for ESP32-S3)
        else:
            # Linux/Mac typically faster, but still add small delay for safety
            time.sleep(2.0)

        return DeploymentResult(success=True, message="Firmware uploaded successfully", port=port)

    def _get_chip_type(self, mcu: str) -> str:
        """Get chip type string for esptool from MCU name.

        Args:
            mcu: MCU type (e.g., "esp32c6", "esp32s3")

        Returns:
            Chip type for esptool (e.g., "esp32c6", "esp32s3")
        """
        # Map MCU names to esptool chip types
        return mcu  # Usually they match directly
