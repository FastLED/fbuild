"""
Utilities for serial port detection and management.

This module provides shared functionality for detecting and working with
serial ports across different platforms and device types.
"""

from typing import Optional


def detect_serial_port(verbose: bool = False) -> Optional[str]:
    """Auto-detect serial port for embedded devices.

    This function scans available serial ports and attempts to identify
    devices commonly used for embedded development (ESP32, Arduino, etc.).

    Args:
        verbose: Whether to show verbose output

    Returns:
        Serial port name (e.g., "COM3", "/dev/ttyUSB0") or None if not found
    """
    try:
        import serial.tools.list_ports

        ports = list(serial.tools.list_ports.comports())

        # Look for ESP32, Arduino, or generic USB-SERIAL devices
        for port in ports:
            description = (port.description or "").lower()
            manufacturer = (port.manufacturer or "").lower()

            # Common USB-to-serial chip identifiers
            if any(
                x in description or x in manufacturer
                for x in [
                    "cp210",  # Silicon Labs CP210x
                    "ch340",  # WCH CH340
                    "usb-serial",  # Generic USB-serial
                    "uart",  # UART devices
                    "esp32",  # ESP32 devices
                    "arduino",  # Arduino devices
                    "ftdi",  # FTDI chips
                ]
            ):
                return port.device

        # If no specific match, return first port
        if ports:
            return ports[0].device

    except ImportError:
        if verbose:
            print("pyserial not installed. Cannot auto-detect port.")
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise  # Never reached, but satisfies type checker
    except Exception as e:
        if verbose:
            print(f"Port detection failed: {e}")

    return None
