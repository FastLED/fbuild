"""
Serial monitor module for embedded devices.

This module provides serial monitoring capabilities with optional halt conditions.
"""

import re
import sys
import time
from pathlib import Path
from typing import Optional

from fbuild.cli_utils import safe_print
from fbuild.config import PlatformIOConfig


class MonitorError(Exception):
    """Raised when monitor operations fail."""

    pass


class SerialMonitor:
    """Serial monitor for embedded devices."""

    def __init__(self, verbose: bool = False):
        """Initialize serial monitor.

        Args:
            verbose: Whether to show verbose output
        """
        self.verbose = verbose

    def monitor(
        self,
        project_dir: Path,
        env_name: str,
        port: Optional[str] = None,
        baud: int = 115200,
        timeout: Optional[int] = None,
        halt_on_error: Optional[str] = None,
        halt_on_success: Optional[str] = None,
        expect: Optional[str] = None,
    ) -> int:
        """Monitor serial output from device.

        Args:
            project_dir: Path to project directory
            env_name: Environment name
            port: Serial port to use (auto-detect if None)
            baud: Baud rate (default: 115200)
            timeout: Timeout in seconds (None for infinite)
            halt_on_error: String pattern that triggers error exit
            halt_on_success: String pattern that triggers success exit
            expect: Expected pattern - checked at timeout/success for exit code

        Returns:
            Exit code (0 for success, 1 for error)
        """
        try:
            import serial
        except ImportError:
            print("Error: pyserial not installed. Install with: pip install pyserial")
            return 1

        # Load platformio.ini to get board config
        ini_path = project_dir / "platformio.ini"
        if not ini_path.exists():
            print(f"Error: platformio.ini not found in {project_dir}")
            return 1

        config = PlatformIOConfig(ini_path)

        try:
            env_config = config.get_env_config(env_name)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            print(f"Error: {e}")
            return 1

        # Get monitor baud rate from config if specified
        monitor_speed = env_config.get("monitor_speed")
        if monitor_speed:
            try:
                baud = int(monitor_speed)
            except ValueError:
                pass

        # Auto-detect port if not specified
        if not port:
            port = self._detect_serial_port()
            if not port:
                print("Error: No serial port specified and auto-detection failed. " + "Use --port to specify a port.")
                return 1

        print(f"Opening serial port {port} at {baud} baud...")

        ser = None
        try:
            # Open serial port
            ser = serial.Serial(
                port,
                baud,
                timeout=0.1,  # Short timeout for readline
            )

            # Reset the device to ensure we catch all output from the start
            # This is necessary because the device may have already booted
            # between esptool finishing and the monitor starting
            ser.setDTR(False)  # type: ignore[attr-defined]
            ser.setRTS(True)  # type: ignore[attr-defined]
            time.sleep(0.1)
            ser.setRTS(False)  # type: ignore[attr-defined]
            time.sleep(0.1)
            ser.setDTR(True)  # type: ignore[attr-defined]

            print(f"Connected to {port}")
            print("--- Serial Monitor (Ctrl+C to exit) ---")
            print()

            # Give device a moment to start booting after reset
            time.sleep(0.2)

            start_time = time.time()

            # Track statistics
            expect_found = False
            halt_on_error_found = False
            halt_on_success_found = False

            while True:
                # Check timeout
                if timeout and (time.time() - start_time) > timeout:
                    print()
                    print(f"--- Monitor timeout after {timeout} seconds ---")

                    # Print statistics
                    if expect or halt_on_error or halt_on_success:
                        safe_print("\n--- Test Results ---")
                        if expect:
                            if expect_found:
                                safe_print(f"✓ Expected pattern found: '{expect}'")
                            else:
                                safe_print(f"✗ Expected pattern NOT found: '{expect}'")
                        if halt_on_error:
                            if halt_on_error_found:
                                safe_print(f"✗ Error pattern found: '{halt_on_error}'")
                            else:
                                safe_print(f"✓ Error pattern not found: '{halt_on_error}'")
                        if halt_on_success:
                            if halt_on_success_found:
                                safe_print(f"✓ Success pattern found: '{halt_on_success}'")
                            else:
                                safe_print(f"✗ Success pattern NOT found: '{halt_on_success}'")

                    ser.close()

                    # Check expect keyword for exit code
                    if expect:
                        return 0 if expect_found else 1
                    else:
                        # Legacy behavior when no expect is specified
                        if halt_on_error or halt_on_success:
                            return 1  # Error: pattern was expected but not found
                        else:
                            return 0  # Success: just a timed monitoring session

                # Read line from serial
                try:
                    if ser.in_waiting:
                        line = ser.readline()
                        try:
                            text = line.decode("utf-8", errors="replace").rstrip()
                        except KeyboardInterrupt as ke:
                            from fbuild.interrupt_utils import (
                                handle_keyboard_interrupt_properly,
                            )

                            handle_keyboard_interrupt_properly(ke)
                        except Exception:
                            text = str(line)

                        # Print the line
                        safe_print(text)
                        sys.stdout.flush()

                        # Check for expect pattern (track but don't halt)
                        if expect and re.search(expect, text, re.IGNORECASE):
                            expect_found = True

                        # Check halt conditions
                        if halt_on_error and re.search(halt_on_error, text, re.IGNORECASE):
                            halt_on_error_found = True
                            print()
                            print(f"--- Found error pattern: '{halt_on_error}' ---")

                            # Print statistics
                            if expect or halt_on_success:
                                safe_print("\n--- Test Results ---")
                                if expect:
                                    if expect_found:
                                        safe_print(f"✓ Expected pattern found: '{expect}'")
                                    else:
                                        safe_print(f"✗ Expected pattern NOT found: '{expect}'")
                                if halt_on_success:
                                    if halt_on_success_found:
                                        safe_print(f"✓ Success pattern found: '{halt_on_success}'")
                                    else:
                                        safe_print(f"✗ Success pattern NOT found: '{halt_on_success}'")
                                safe_print(f"✗ Error pattern found: '{halt_on_error}'")

                            ser.close()
                            return 1

                        if halt_on_success and re.search(halt_on_success, text, re.IGNORECASE):
                            halt_on_success_found = True
                            print()
                            print(f"--- Found success pattern: '{halt_on_success}' ---")

                            # Print statistics
                            if expect or halt_on_error:
                                safe_print("\n--- Test Results ---")
                                if expect:
                                    if expect_found:
                                        safe_print(f"✓ Expected pattern found: '{expect}'")
                                    else:
                                        safe_print(f"✗ Expected pattern NOT found: '{expect}'")
                                safe_print(f"✓ Success pattern found: '{halt_on_success}'")
                                if halt_on_error:
                                    if halt_on_error_found:
                                        safe_print(f"✗ Error pattern found: '{halt_on_error}'")
                                    else:
                                        safe_print(f"✓ Error pattern not found: '{halt_on_error}'")

                            ser.close()

                            # Check expect keyword for exit code
                            if expect:
                                return 0 if expect_found else 1
                            else:
                                return 0
                    else:
                        time.sleep(0.01)

                except serial.SerialException as e:
                    print(f"\nError reading from serial port: {e}")
                    ser.close()
                    return 1

        except serial.SerialException as e:
            print(f"Error opening serial port {port}: {e}")
            return 1
        except KeyboardInterrupt:  # noqa: KBI002
            print()
            print("--- Monitor interrupted ---")
            if ser is not None:
                ser.close()
            return 0

    def _detect_serial_port(self) -> Optional[str]:
        """Auto-detect serial port for device.

        Returns:
            Serial port name or None if not found
        """
        try:
            import serial.tools.list_ports

            ports = list(serial.tools.list_ports.comports())

            # Look for ESP32 or USB-SERIAL devices
            for port in ports:
                description = (port.description or "").lower()
                manufacturer = (port.manufacturer or "").lower()

                if any(x in description or x in manufacturer for x in ["cp210", "ch340", "usb-serial", "uart", "esp32"]):
                    return port.device

            # If no specific match, return first port
            if ports:
                return ports[0].device

        except ImportError:
            if self.verbose:
                print("pyserial not installed. Cannot auto-detect port.")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            if self.verbose:
                print(f"Port detection failed: {e}")

        return None
