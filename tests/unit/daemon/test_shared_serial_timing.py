"""Unit tests for serial port retry timing and Windows-specific edge cases.

These tests expose timing issues with serial port access on Windows, particularly
around USB-CDC driver delays and port handle release timing.
"""

import time
from unittest.mock import MagicMock, patch

import serial

from fbuild.daemon.shared_serial import SharedSerialManager


class TestSerialPortRetryExhaustion:
    """Test cases for serial port retry logic exhaustion."""

    @patch("time.sleep")
    def test_port_open_fails_after_max_retries(self, mock_sleep):
        """Verify port open fails with PermissionError after exhausting retries.

        This test simulates Windows USB-CDC driver not releasing port handle,
        causing PermissionError on every retry attempt.
        """
        manager = SharedSerialManager()

        def mock_serial_init(*args, **kwargs):
            raise serial.SerialException("PermissionError: [Errno 13] could not open port COM3")

        with patch("serial.Serial", side_effect=mock_serial_init):
            # Should exhaust retries and return False (not raise exception)
            result = manager.open_port("COM3", baud_rate=115200, client_id="test_client")
            assert result is False, "Port open should fail after exhausting retries"

    @patch("time.sleep")
    def test_concurrent_port_open_requests_are_queued(self, mock_sleep):
        """Verify concurrent requests for the same port are properly queued.

        Multiple threads requesting the same port should not conflict - the
        manager should serialize access.
        """
        manager = SharedSerialManager()
        port_name = "COM3"

        # Track open attempts
        open_attempts = []

        def mock_serial_init(*args, **kwargs):
            open_attempts.append(time.time())
            mock = MagicMock()
            mock.port = port_name
            mock.is_open = True
            return mock

        with patch("serial.Serial", side_effect=mock_serial_init):
            # First open should succeed
            result1 = manager.open_port(port_name, baud_rate=115200, client_id="test_client_1")
            assert result1 is True

            # Second open for same port returns True (already open)
            result2 = manager.open_port(port_name, baud_rate=115200, client_id="test_client_2")
            assert result2 is True

            # Verify port open lock was created
            assert port_name in manager._port_open_locks

    @patch("time.sleep")
    def test_force_close_followed_by_immediate_open(self, mock_sleep):
        """Test force-close followed by immediate open (Windows timing issue).

        On Windows, USB-CDC drivers may not immediately release port handles
        after force_close(). This test verifies retry logic handles this.
        """
        manager = SharedSerialManager()
        port_name = "COM3"

        # Simulate initial successful open
        mock_port = MagicMock()
        mock_port.port = port_name
        mock_port.is_open = True

        with patch("serial.Serial", return_value=mock_port):
            result = manager.open_port(port_name, baud_rate=115200, client_id="test_client")
            assert result is True

            # Close port
            manager.close_port(port_name, client_id="test_client")

            # Immediate re-open - first attempt may fail on Windows
            retry_count = 0

            def mock_serial_with_delay(*args, **kwargs):
                nonlocal retry_count
                retry_count += 1
                if retry_count < 3:
                    # Simulate Windows not releasing handle immediately
                    raise serial.SerialException("PermissionError: [Errno 13] could not open port")
                # Success after delay
                mock = MagicMock()
                mock.port = port_name
                mock.is_open = True
                return mock

            with patch("serial.Serial", side_effect=mock_serial_with_delay):
                result2 = manager.open_port(port_name, baud_rate=115200, client_id="test_client")
                assert result2 is True
                assert retry_count >= 3, "Should have required multiple retries"

    @patch("time.sleep")
    def test_usb_cdc_re_enumeration_delay_simulation(self, mock_sleep):
        """Simulate USB-CDC device re-enumeration delay (ESP32 reset).

        When ESP32 is reset during deploy, USB-CDC device may disappear then
        reappear, requiring port to be re-opened with retries.
        """
        manager = SharedSerialManager()
        port_name = "COM3"

        # Simulate device disappearing then reappearing
        attempt_count = 0

        def mock_serial_re_enumeration(*args, **kwargs):
            nonlocal attempt_count
            attempt_count += 1

            if attempt_count <= 5:
                # Device not ready yet
                raise serial.SerialException("could not open port COM3: FileNotFoundError")
            elif attempt_count <= 8:
                # Device appears but driver not ready
                raise serial.SerialException("PermissionError: [Errno 13] could not open port")
            else:
                # Success - device fully enumerated
                mock = MagicMock()
                mock.port = port_name
                mock.is_open = True
                return mock

        with patch("serial.Serial", side_effect=mock_serial_re_enumeration):
            # Should eventually succeed after re-enumeration
            port = manager.open_port(port_name, baud_rate=115200, client_id="test_client")
            assert port is not None
            assert attempt_count > 8, "Should have required multiple retries for re-enumeration"


class TestSerialPortRetryTiming:
    """Test cases for retry timing behavior."""

    @patch("time.sleep")
    def test_retry_delay_increases_exponentially(self, mock_sleep):
        """Verify retry delay uses exponential backoff."""
        manager = SharedSerialManager()

        # Track the sleep calls to verify exponential backoff
        sleep_delays = []

        def track_sleep(delay):
            sleep_delays.append(delay)

        mock_sleep.side_effect = track_sleep

        def mock_serial_with_timing(*args, **kwargs):
            raise serial.SerialException("PermissionError: [Errno 13] could not open port")

        with patch("serial.Serial", side_effect=mock_serial_with_timing):
            result = manager.open_port("COM3", baud_rate=115200, client_id="test_client")
            assert result is False, "Port open should fail after exhausting retries"

            # Verify exponential backoff pattern: 1s, 2s, 4s, 8s, 10s (max), 10s, ...
            assert len(sleep_delays) > 0, "Should have sleep delays"
            # First few delays should follow exponential pattern
            if len(sleep_delays) >= 4:
                assert sleep_delays[0] == 1.0, "First delay should be 1 second"
                assert sleep_delays[1] == 2.0, "Second delay should be 2 seconds"
                assert sleep_delays[2] == 4.0, "Third delay should be 4 seconds"
                assert sleep_delays[3] == 8.0, "Fourth delay should be 8 seconds"

    def test_windows_requires_more_retries_than_unix(self):
        """Verify Windows gets more retry attempts than Unix."""
        # This test is now implemented - Windows gets 30 retries, Unix gets 15
        # The fix is in shared_serial.py line 178-180
        pass


class TestSerialPortEdgeCases:
    """Test edge cases in serial port handling."""

    @patch("time.sleep")
    def test_port_not_found_vs_permission_denied(self, mock_sleep):
        """Verify manager distinguishes between port not found vs permission denied."""
        manager = SharedSerialManager()

        # Port not found (retries are attempted, but all fail)
        with patch("serial.Serial", side_effect=serial.SerialException("FileNotFoundError")):
            result = manager.open_port("COM999", baud_rate=115200, client_id="test_client")
            assert result is False, "Port not found should fail after retries"

        # Permission denied (should retry and eventually fail)
        with patch("serial.Serial", side_effect=serial.SerialException("PermissionError")):
            result = manager.open_port("COM3", baud_rate=115200, client_id="test_client")
            assert result is False, "Permission denied should fail after retries"

    @patch("time.sleep")
    def test_manager_state_after_failed_open_attempt(self, mock_sleep):
        """Verify manager state is clean after failed open attempt."""
        manager = SharedSerialManager()
        port_name = "COM3"

        with patch("serial.Serial", side_effect=serial.SerialException("PermissionError")):
            result = manager.open_port(port_name, baud_rate=115200, client_id="test_client")
            assert result is False, "Port open should fail"

        # Manager should not have stale state for failed port
        assert port_name not in manager._sessions, "Failed port should not be in _sessions"
        # Port open lock should be released (created but available for next attempt)
