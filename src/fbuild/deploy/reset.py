"""
Device reset functionality for embedded platforms.

Sends platform-appropriate reset signals to connected devices via serial port.
Supports Teensy (134 baud magic), ESP32 (DTR/RTS sequence), and AVR (DTR toggle).
"""

import time

from fbuild.output import log


def reset_device(platform: str, port: str, verbose: bool) -> bool:
    """Reset an embedded device via serial port.

    Args:
        platform: Platform identifier ("teensy", "esp32", "avr", etc.)
        port: Serial port name (e.g., "COM5", "/dev/ttyUSB0")
        verbose: Whether to show verbose output

    Returns:
        True if reset succeeded, False otherwise
    """
    if platform == "teensy":
        return _reset_teensy(port, verbose)
    elif platform == "esp32":
        return _reset_esp32(port, verbose)
    elif platform == "avr":
        return _reset_avr(port, verbose)
    else:
        return _reset_generic(port, verbose)


def _reset_teensy(port: str, verbose: bool) -> bool:
    """Reset Teensy via 134 baud magic.

    Opening the serial port at 134 baud triggers the Teensy bootloader's
    soft reboot mechanism. This is the standard Teensy reset method.

    Args:
        port: Serial port name
        verbose: Whether to show verbose output

    Returns:
        True if reset succeeded, False otherwise
    """
    try:
        import serial

        if verbose:
            log(f"Teensy reset: opening {port} at 134 baud (magic reboot)")

        ser = serial.Serial(port, baudrate=134)
        time.sleep(0.1)
        ser.close()

        log(f"Reset signal sent to Teensy on {port}")
        return True

    except ImportError:
        log("Error: pyserial not installed. Run: pip install pyserial")
        return False
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise  # Never reached, but satisfies type checker
    except Exception as e:
        if verbose:
            log(f"Teensy 134-baud reset failed: {e}")
        log(f"Failed to reset Teensy on {port}: {e}")
        return False


def _reset_esp32(port: str, verbose: bool) -> bool:
    """Reset ESP32 via DTR/RTS sequence.

    Uses the same reset sequence as esptool to toggle the EN (reset) pin
    via the DTR/RTS lines on the USB-to-serial adapter.

    Args:
        port: Serial port name
        verbose: Whether to show verbose output

    Returns:
        True if reset succeeded, False otherwise
    """
    try:
        import serial

        if verbose:
            log(f"ESP32 reset: DTR/RTS sequence on {port}")

        ser = serial.Serial(port, baudrate=115200)

        # Reset sequence: pull EN low via RTS, then release
        ser.dtr = False
        ser.rts = True
        time.sleep(0.1)
        ser.dtr = True
        ser.rts = False
        time.sleep(0.05)
        ser.dtr = False

        ser.close()

        log(f"Reset signal sent to ESP32 on {port}")
        return True

    except ImportError:
        log("Error: pyserial not installed. Run: pip install pyserial")
        return False
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise  # Never reached, but satisfies type checker
    except Exception as e:
        log(f"Failed to reset ESP32 on {port}: {e}")
        return False


def _reset_avr(port: str, verbose: bool) -> bool:
    """Reset AVR/Arduino via DTR toggle.

    Standard Arduino reset: toggling DTR pulses the reset pin through
    the capacitor on the board.

    Args:
        port: Serial port name
        verbose: Whether to show verbose output

    Returns:
        True if reset succeeded, False otherwise
    """
    try:
        import serial

        if verbose:
            log(f"AVR reset: DTR toggle on {port}")

        ser = serial.Serial(port, baudrate=115200)
        ser.dtr = False
        time.sleep(0.1)
        ser.dtr = True
        time.sleep(0.1)
        ser.close()

        log(f"Reset signal sent to AVR on {port}")
        return True

    except ImportError:
        log("Error: pyserial not installed. Run: pip install pyserial")
        return False
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise  # Never reached, but satisfies type checker
    except Exception as e:
        log(f"Failed to reset AVR on {port}: {e}")
        return False


def _reset_generic(port: str, verbose: bool) -> bool:
    """Reset device via generic DTR toggle.

    Fallback reset method that works for most devices with DTR-based reset.

    Args:
        port: Serial port name
        verbose: Whether to show verbose output

    Returns:
        True if reset succeeded, False otherwise
    """
    try:
        import serial

        if verbose:
            log(f"Generic reset: DTR toggle on {port}")

        ser = serial.Serial(port, baudrate=115200)
        ser.dtr = False
        time.sleep(0.1)
        ser.dtr = True
        time.sleep(0.1)
        ser.close()

        log(f"Reset signal sent to device on {port}")
        return True

    except ImportError:
        log("Error: pyserial not installed. Run: pip install pyserial")
        return False
    except KeyboardInterrupt as ke:
        from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

        handle_keyboard_interrupt_properly(ke)
        raise  # Never reached, but satisfies type checker
    except Exception as e:
        log(f"Failed to reset device on {port}: {e}")
        return False
