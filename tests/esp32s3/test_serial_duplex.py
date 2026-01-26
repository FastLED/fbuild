#!/usr/bin/env python3
"""Test script for serial_duplex_test.ino

This script tests bidirectional serial communication with the ESP32-S3
to help diagnose serial port locking issues.

Usage:
    python test_serial_duplex.py COM13
    python test_serial_duplex.py /dev/ttyUSB0
"""

import json
import sys
import time

try:
    import serial
except ImportError:
    print("Error: pyserial not installed")
    print("Install with: pip install pyserial")
    sys.exit(1)


class SerialDuplexTester:
    """Test bidirectional serial communication with ESP32-S3."""

    def __init__(self, port: str, baudrate: int = 115200, timeout: float = 2.0):
        """Initialize serial connection.

        Args:
            port: Serial port (e.g., COM13, /dev/ttyUSB0)
            baudrate: Baud rate (default: 115200)
            timeout: Read timeout in seconds (default: 2.0)
        """
        self.port = port
        self.baudrate = baudrate
        self.timeout = timeout
        self.ser = None

    def connect(self) -> bool:
        """Connect to serial port.

        Returns:
            True if connection successful, False otherwise
        """
        try:
            self.ser = serial.Serial(
                port=self.port,
                baudrate=self.baudrate,
                timeout=self.timeout,
                write_timeout=2.0,
            )
            print(f"✓ Connected to {self.port} at {self.baudrate} baud")

            # Wait for device to be ready
            time.sleep(0.5)

            # Flush any startup messages
            self.ser.reset_input_buffer()
            self.ser.reset_output_buffer()

            return True

        except serial.SerialException as e:
            print(f"✗ Failed to connect to {self.port}: {e}")
            return False

    def disconnect(self):
        """Disconnect from serial port."""
        if self.ser and self.ser.is_open:
            self.ser.close()
            print(f"✓ Disconnected from {self.port}")

    def send_command(self, cmd: str, data: str | None = None) -> dict | None:
        """Send command and read response.

        Args:
            cmd: Command name
            data: Optional data payload

        Returns:
            Parsed JSON response or None if error
        """
        if not self.ser or not self.ser.is_open:
            print("✗ Serial port not open")
            return None

        # Build command JSON
        if data:
            cmd_json = json.dumps({"cmd": cmd, "data": data})
        else:
            cmd_json = json.dumps({"cmd": cmd})

        # Send command
        try:
            self.ser.write((cmd_json + "\n").encode("utf-8"))
            self.ser.flush()
            print(f"→ {cmd_json}")

            # Read response (with timeout)
            line = self.ser.readline().decode("utf-8", errors="ignore").strip()

            if not line:
                print("✗ No response (timeout)")
                return None

            # Parse JSON response
            try:
                response = json.loads(line)
                print(f"← {json.dumps(response)}")
                return response

            except json.JSONDecodeError:
                print(f"✗ Invalid JSON response: {line}")
                return None

        except serial.SerialException as e:
            print(f"✗ Serial error: {e}")
            return None

    def run_tests(self):
        """Run a series of test commands."""
        print("\n" + "=" * 60)
        print("Running Serial Duplex Tests")
        print("=" * 60 + "\n")

        tests = [
            ("ping", None, "pong"),
            ("info", None, None),  # Response varies
            ("echo", "Hello ESP32!", "Hello ESP32!"),
            ("led_on", None, "LED is ON"),
            ("led_off", None, "LED is OFF"),
            ("toggle", None, None),  # Response varies
            ("blink", None, "blinked"),
        ]

        passed = 0
        failed = 0

        for cmd, data, expected_response in tests:
            print(f"\nTest: {cmd}" + (f" (data={data})" if data else ""))
            response = self.send_command(cmd, data)

            if response is None:
                print("  ✗ FAIL: No response")
                failed += 1
                continue

            if response.get("status") != "ok":
                print(f"  ✗ FAIL: Error response: {response}")
                failed += 1
                continue

            if expected_response and response.get("response") != expected_response:
                print(f"  ✗ FAIL: Expected '{expected_response}', got '{response.get('response')}'")
                failed += 1
                continue

            print("  ✓ PASS")
            passed += 1

            # Small delay between commands
            time.sleep(0.1)

        print("\n" + "=" * 60)
        print(f"Results: {passed} passed, {failed} failed")
        print("=" * 60 + "\n")

        return failed == 0

    def interactive_mode(self):
        """Interactive command mode."""
        print("\n" + "=" * 60)
        print("Interactive Mode")
        print("=" * 60)
        print("Commands: ping, info, echo, led_on, led_off, toggle, blink")
        print("Format: cmd [data]")
        print("Type 'quit' to exit")
        print("=" * 60 + "\n")

        while True:
            try:
                user_input = input("> ").strip()

                if not user_input:
                    continue

                if user_input.lower() in ("quit", "exit", "q"):
                    break

                # Parse input
                parts = user_input.split(maxsplit=1)
                cmd = parts[0]
                data = parts[1] if len(parts) > 1 else None

                # Send command
                response = self.send_command(cmd, data)

                if response and response.get("status") == "ok":
                    print(f"  ✓ {response.get('response', 'OK')}")
                elif response and response.get("status") == "error":
                    print(f"  ✗ Error: {response.get('error', 'unknown')}")
                else:
                    print("  ✗ No valid response")

            except KeyboardInterrupt:
                print("\n\nInterrupted by user")
                break
            except Exception as e:
                print(f"  ✗ Error: {e}")


def main():
    """Main entry point."""
    if len(sys.argv) < 2:
        print("Usage: python test_serial_duplex.py <port> [--interactive]")
        print("Example: python test_serial_duplex.py COM13")
        print("Example: python test_serial_duplex.py /dev/ttyUSB0 --interactive")
        sys.exit(1)

    port = sys.argv[1]
    interactive = "--interactive" in sys.argv or "-i" in sys.argv

    tester = SerialDuplexTester(port)

    try:
        if not tester.connect():
            sys.exit(1)

        if interactive:
            tester.interactive_mode()
        else:
            success = tester.run_tests()
            sys.exit(0 if success else 1)

    except KeyboardInterrupt:
        print("\n\nInterrupted by user")
        sys.exit(130)

    finally:
        tester.disconnect()


if __name__ == "__main__":
    main()
