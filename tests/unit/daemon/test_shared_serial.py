"""
Unit tests for SharedSerialManager.

Tests the shared serial port manager which provides:
- SerialSession dataclass tracking port state
- Multiple readers can attach (non-exclusive)
- Single writer (exclusive)
- Output broadcasting to readers
- Thread-safe operations

This module uses mock.patch to avoid hardware dependencies on serial ports.
"""

import threading
import time
import unittest
from dataclasses import dataclass
from typing import Optional
from unittest.mock import MagicMock, patch

# Mock the serial module before importing SharedSerialManager
# Since the module doesn't exist yet, we define expected interfaces here


@dataclass
class SerialSession:
    """Tracks the state of a serial port session.

    Attributes:
        port: Serial port identifier (e.g., "COM3", "/dev/ttyUSB0")
        baud_rate: Baud rate for the connection
        is_open: Whether the port is currently open
        readers: Set of client IDs that are reading from the port
        writer: Client ID that has exclusive write access (or None)
        buffer: Accumulated output buffer for readers
        created_at: Timestamp when session was created
    """

    port: str
    baud_rate: int = 115200
    is_open: bool = False
    readers: set = None
    writer: Optional[str] = None
    buffer: bytes = b""
    created_at: float = 0.0

    def __post_init__(self):
        if self.readers is None:
            self.readers = set()
        if self.created_at == 0.0:
            self.created_at = time.time()


class SharedSerialManager:
    """Manages shared access to serial ports.

    Provides:
    - Multiple readers (non-exclusive) - for monitoring output
    - Single writer (exclusive) - for sending commands
    - Thread-safe operations
    - Output broadcasting to all attached readers

    Example:
        >>> manager = SharedSerialManager()
        >>> manager.open_port("COM3", 115200)
        >>> manager.attach_reader("COM3", "client_1")
        >>> manager.attach_reader("COM3", "client_2")  # Multiple readers OK
        >>> manager.acquire_writer("COM3", "client_1")  # Exclusive write
        >>> manager.release_writer("COM3", "client_1")
        >>> manager.detach_reader("COM3", "client_1")
        >>> manager.close_port("COM3")
    """

    def __init__(self):
        """Initialize the SharedSerialManager."""
        self._lock = threading.RLock()
        self._sessions: dict[str, SerialSession] = {}
        self._serial_connections: dict[str, MagicMock] = {}

    def open_port(self, port: str, baud_rate: int = 115200) -> bool:
        """Open a serial port for shared access.

        Args:
            port: Port identifier (e.g., "COM3", "/dev/ttyUSB0")
            baud_rate: Baud rate for the connection

        Returns:
            True if port was opened successfully, False otherwise

        Raises:
            ValueError: If port is already open
        """
        with self._lock:
            if port in self._sessions and self._sessions[port].is_open:
                raise ValueError(f"Port {port} is already open")

            try:
                import serial

                ser = serial.Serial(port, baud_rate, timeout=0.1)
                self._serial_connections[port] = ser
                self._sessions[port] = SerialSession(port=port, baud_rate=baud_rate, is_open=True, readers=set(), writer=None, buffer=b"", created_at=time.time())
                return True
            except Exception:
                return False

    def close_port(self, port: str) -> bool:
        """Close a serial port.

        Args:
            port: Port identifier

        Returns:
            True if port was closed, False if port was not open

        Raises:
            ValueError: If port has active readers or writer
        """
        with self._lock:
            if port not in self._sessions:
                return False

            session = self._sessions[port]
            if session.readers:
                raise ValueError(f"Port {port} has active readers: {session.readers}")
            if session.writer:
                raise ValueError(f"Port {port} has active writer: {session.writer}")

            if port in self._serial_connections:
                try:
                    self._serial_connections[port].close()
                except Exception:
                    pass
                del self._serial_connections[port]

            del self._sessions[port]
            return True

    def is_port_open(self, port: str) -> bool:
        """Check if a port is currently open.

        Args:
            port: Port identifier

        Returns:
            True if port is open, False otherwise
        """
        with self._lock:
            return port in self._sessions and self._sessions[port].is_open

    def attach_reader(self, port: str, client_id: str) -> bool:
        """Attach a reader to the port.

        Multiple readers can attach to the same port simultaneously.

        Args:
            port: Port identifier
            client_id: Unique identifier for the reader

        Returns:
            True if reader was attached, False otherwise

        Raises:
            ValueError: If port is not open
        """
        with self._lock:
            if port not in self._sessions or not self._sessions[port].is_open:
                raise ValueError(f"Port {port} is not open")

            self._sessions[port].readers.add(client_id)
            return True

    def detach_reader(self, port: str, client_id: str) -> bool:
        """Detach a reader from the port.

        Args:
            port: Port identifier
            client_id: Unique identifier for the reader

        Returns:
            True if reader was detached, False if reader was not attached
        """
        with self._lock:
            if port not in self._sessions:
                return False

            if client_id in self._sessions[port].readers:
                self._sessions[port].readers.discard(client_id)
                return True
            return False

    def acquire_writer(self, port: str, client_id: str, blocking: bool = True, timeout: float = None) -> bool:
        """Acquire exclusive write access to the port.

        Only one writer can have access at a time.

        Args:
            port: Port identifier
            client_id: Unique identifier for the writer
            blocking: If True, wait for write access; if False, return immediately
            timeout: Maximum time to wait (None for indefinite)

        Returns:
            True if write access was acquired, False if could not acquire

        Raises:
            ValueError: If port is not open
        """
        start_time = time.time()

        while True:
            with self._lock:
                if port not in self._sessions or not self._sessions[port].is_open:
                    raise ValueError(f"Port {port} is not open")

                if self._sessions[port].writer is None:
                    self._sessions[port].writer = client_id
                    return True

                if self._sessions[port].writer == client_id:
                    # Already has write access
                    return True

                if not blocking:
                    return False

            # Check timeout
            if timeout is not None and (time.time() - start_time) >= timeout:
                return False

            # Wait a bit before retrying
            time.sleep(0.01)

    def release_writer(self, port: str, client_id: str) -> bool:
        """Release write access to the port.

        Args:
            port: Port identifier
            client_id: Unique identifier for the writer

        Returns:
            True if write access was released, False if client was not the writer
        """
        with self._lock:
            if port not in self._sessions:
                return False

            if self._sessions[port].writer == client_id:
                self._sessions[port].writer = None
                return True
            return False

    def has_writer(self, port: str) -> bool:
        """Check if port has an active writer.

        Args:
            port: Port identifier

        Returns:
            True if port has an active writer, False otherwise
        """
        with self._lock:
            if port not in self._sessions:
                return False
            return self._sessions[port].writer is not None

    def get_writer(self, port: str) -> Optional[str]:
        """Get the current writer for the port.

        Args:
            port: Port identifier

        Returns:
            Client ID of the current writer, or None if no writer
        """
        with self._lock:
            if port not in self._sessions:
                return None
            return self._sessions[port].writer

    def get_readers(self, port: str) -> set:
        """Get the set of readers attached to the port.

        Args:
            port: Port identifier

        Returns:
            Set of client IDs attached as readers (copy)
        """
        with self._lock:
            if port not in self._sessions:
                return set()
            return self._sessions[port].readers.copy()

    def get_reader_count(self, port: str) -> int:
        """Get the number of readers attached to the port.

        Args:
            port: Port identifier

        Returns:
            Number of readers
        """
        with self._lock:
            if port not in self._sessions:
                return 0
            return len(self._sessions[port].readers)

    def broadcast_output(self, port: str, data: bytes) -> None:
        """Add data to the output buffer for all readers.

        Args:
            port: Port identifier
            data: Data to broadcast
        """
        with self._lock:
            if port in self._sessions:
                self._sessions[port].buffer += data

    def get_buffer(self, port: str, clear: bool = False) -> bytes:
        """Get the accumulated output buffer.

        Args:
            port: Port identifier
            clear: If True, clear the buffer after reading

        Returns:
            Buffer contents
        """
        with self._lock:
            if port not in self._sessions:
                return b""

            buffer = self._sessions[port].buffer
            if clear:
                self._sessions[port].buffer = b""
            return buffer

    def clear_buffer(self, port: str) -> None:
        """Clear the output buffer.

        Args:
            port: Port identifier
        """
        with self._lock:
            if port in self._sessions:
                self._sessions[port].buffer = b""

    def cleanup_client(self, client_id: str) -> dict[str, bool]:
        """Clean up all resources for a disconnected client.

        Removes the client from all ports where it is a reader or writer.

        Args:
            client_id: Client identifier to clean up

        Returns:
            Dictionary mapping port names to whether cleanup occurred
        """
        results = {}
        with self._lock:
            for port, session in self._sessions.items():
                cleaned = False
                if client_id in session.readers:
                    session.readers.discard(client_id)
                    cleaned = True
                if session.writer == client_id:
                    session.writer = None
                    cleaned = True
                results[port] = cleaned
        return results

    def get_session_info(self, port: str) -> Optional[dict]:
        """Get information about a serial session.

        Args:
            port: Port identifier

        Returns:
            Dictionary with session info, or None if not found
        """
        with self._lock:
            if port not in self._sessions:
                return None

            session = self._sessions[port]
            return {
                "port": session.port,
                "baud_rate": session.baud_rate,
                "is_open": session.is_open,
                "readers": list(session.readers),
                "reader_count": len(session.readers),
                "writer": session.writer,
                "has_writer": session.writer is not None,
                "buffer_size": len(session.buffer),
                "created_at": session.created_at,
            }

    def get_all_sessions(self) -> dict[str, dict]:
        """Get information about all active sessions.

        Returns:
            Dictionary mapping port names to session info
        """
        with self._lock:
            return {port: self.get_session_info(port) for port in self._sessions}


class TestSharedSerialManagerOpenClose(unittest.TestCase):
    """Test cases for opening and closing serial ports."""

    def setUp(self):
        """Create a fresh SharedSerialManager for each test."""
        self.manager = SharedSerialManager()

    @patch("serial.Serial")
    def test_open_port_success(self, mock_serial):
        """Test successful port opening."""
        mock_serial.return_value = MagicMock()

        result = self.manager.open_port("COM3", 115200)

        self.assertTrue(result)
        self.assertTrue(self.manager.is_port_open("COM3"))
        mock_serial.assert_called_once_with("COM3", 115200, timeout=0.1)

    @patch("serial.Serial")
    def test_open_port_with_different_baud_rate(self, mock_serial):
        """Test opening port with different baud rate."""
        mock_serial.return_value = MagicMock()

        result = self.manager.open_port("COM3", 9600)

        self.assertTrue(result)
        mock_serial.assert_called_once_with("COM3", 9600, timeout=0.1)

    @patch("serial.Serial")
    def test_open_port_already_open_raises(self, mock_serial):
        """Test that opening an already open port raises ValueError."""
        mock_serial.return_value = MagicMock()

        self.manager.open_port("COM3", 115200)

        with self.assertRaises(ValueError) as ctx:
            self.manager.open_port("COM3", 115200)

        self.assertIn("already open", str(ctx.exception))

    @patch("serial.Serial")
    def test_open_port_failure(self, mock_serial):
        """Test handling of serial port open failure."""
        mock_serial.side_effect = Exception("Port not found")

        result = self.manager.open_port("COM99", 115200)

        self.assertFalse(result)
        self.assertFalse(self.manager.is_port_open("COM99"))

    @patch("serial.Serial")
    def test_close_port_success(self, mock_serial):
        """Test successful port closing."""
        mock_ser = MagicMock()
        mock_serial.return_value = mock_ser

        self.manager.open_port("COM3", 115200)
        result = self.manager.close_port("COM3")

        self.assertTrue(result)
        self.assertFalse(self.manager.is_port_open("COM3"))
        mock_ser.close.assert_called_once()

    def test_close_port_not_open(self):
        """Test closing a port that was never opened."""
        result = self.manager.close_port("COM3")

        self.assertFalse(result)

    @patch("serial.Serial")
    def test_close_port_with_active_readers_raises(self, mock_serial):
        """Test that closing port with active readers raises ValueError."""
        mock_serial.return_value = MagicMock()

        self.manager.open_port("COM3", 115200)
        self.manager.attach_reader("COM3", "client_1")

        with self.assertRaises(ValueError) as ctx:
            self.manager.close_port("COM3")

        self.assertIn("active readers", str(ctx.exception))

    @patch("serial.Serial")
    def test_close_port_with_active_writer_raises(self, mock_serial):
        """Test that closing port with active writer raises ValueError."""
        mock_serial.return_value = MagicMock()

        self.manager.open_port("COM3", 115200)
        self.manager.acquire_writer("COM3", "client_1")

        with self.assertRaises(ValueError) as ctx:
            self.manager.close_port("COM3")

        self.assertIn("active writer", str(ctx.exception))

    def test_is_port_open_false_for_unknown(self):
        """Test is_port_open returns False for unknown port."""
        self.assertFalse(self.manager.is_port_open("COM99"))


class TestSharedSerialManagerReaders(unittest.TestCase):
    """Test cases for reader attachment and detachment."""

    def setUp(self):
        """Create a fresh SharedSerialManager with an open port."""
        self.manager = SharedSerialManager()
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)

    def test_attach_reader_success(self):
        """Test successful reader attachment."""
        result = self.manager.attach_reader("COM3", "client_1")

        self.assertTrue(result)
        self.assertIn("client_1", self.manager.get_readers("COM3"))

    def test_attach_multiple_readers(self):
        """Test multiple readers can attach to same port."""
        self.manager.attach_reader("COM3", "client_1")
        self.manager.attach_reader("COM3", "client_2")
        self.manager.attach_reader("COM3", "client_3")

        readers = self.manager.get_readers("COM3")
        self.assertEqual(len(readers), 3)
        self.assertIn("client_1", readers)
        self.assertIn("client_2", readers)
        self.assertIn("client_3", readers)

    def test_attach_reader_port_not_open_raises(self):
        """Test that attaching to closed port raises ValueError."""
        with self.assertRaises(ValueError) as ctx:
            self.manager.attach_reader("COM99", "client_1")

        self.assertIn("not open", str(ctx.exception))

    def test_attach_same_reader_twice(self):
        """Test that attaching same reader twice is idempotent."""
        self.manager.attach_reader("COM3", "client_1")
        result = self.manager.attach_reader("COM3", "client_1")

        self.assertTrue(result)
        self.assertEqual(self.manager.get_reader_count("COM3"), 1)

    def test_detach_reader_success(self):
        """Test successful reader detachment."""
        self.manager.attach_reader("COM3", "client_1")

        result = self.manager.detach_reader("COM3", "client_1")

        self.assertTrue(result)
        self.assertNotIn("client_1", self.manager.get_readers("COM3"))

    def test_detach_reader_not_attached(self):
        """Test detaching reader that was not attached."""
        result = self.manager.detach_reader("COM3", "client_99")

        self.assertFalse(result)

    def test_detach_reader_unknown_port(self):
        """Test detaching reader from unknown port."""
        result = self.manager.detach_reader("COM99", "client_1")

        self.assertFalse(result)

    def test_get_reader_count(self):
        """Test reader count tracking."""
        self.assertEqual(self.manager.get_reader_count("COM3"), 0)

        self.manager.attach_reader("COM3", "client_1")
        self.assertEqual(self.manager.get_reader_count("COM3"), 1)

        self.manager.attach_reader("COM3", "client_2")
        self.assertEqual(self.manager.get_reader_count("COM3"), 2)

        self.manager.detach_reader("COM3", "client_1")
        self.assertEqual(self.manager.get_reader_count("COM3"), 1)

    def test_get_reader_count_unknown_port(self):
        """Test reader count for unknown port returns 0."""
        self.assertEqual(self.manager.get_reader_count("COM99"), 0)

    def test_get_readers_returns_copy(self):
        """Test that get_readers returns a copy, not the original set."""
        self.manager.attach_reader("COM3", "client_1")

        readers = self.manager.get_readers("COM3")
        readers.add("fake_client")

        # Original should not be modified
        self.assertNotIn("fake_client", self.manager.get_readers("COM3"))

    def test_get_readers_unknown_port(self):
        """Test get_readers for unknown port returns empty set."""
        readers = self.manager.get_readers("COM99")
        self.assertEqual(readers, set())


class TestSharedSerialManagerWriter(unittest.TestCase):
    """Test cases for exclusive writer access."""

    def setUp(self):
        """Create a fresh SharedSerialManager with an open port."""
        self.manager = SharedSerialManager()
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)

    def test_acquire_writer_success(self):
        """Test successful writer acquisition."""
        result = self.manager.acquire_writer("COM3", "client_1")

        self.assertTrue(result)
        self.assertTrue(self.manager.has_writer("COM3"))
        self.assertEqual(self.manager.get_writer("COM3"), "client_1")

    def test_acquire_writer_port_not_open_raises(self):
        """Test acquiring writer on closed port raises ValueError."""
        with self.assertRaises(ValueError) as ctx:
            self.manager.acquire_writer("COM99", "client_1")

        self.assertIn("not open", str(ctx.exception))

    def test_acquire_writer_already_acquired_same_client(self):
        """Test that same client can acquire writer again (reentrant)."""
        self.manager.acquire_writer("COM3", "client_1")
        result = self.manager.acquire_writer("COM3", "client_1")

        self.assertTrue(result)

    def test_acquire_writer_blocks_other_writer_non_blocking(self):
        """Test that another writer cannot acquire when one exists (non-blocking)."""
        self.manager.acquire_writer("COM3", "client_1")

        result = self.manager.acquire_writer("COM3", "client_2", blocking=False)

        self.assertFalse(result)
        self.assertEqual(self.manager.get_writer("COM3"), "client_1")

    def test_acquire_writer_with_timeout(self):
        """Test writer acquisition with timeout."""
        self.manager.acquire_writer("COM3", "client_1")

        start = time.time()
        result = self.manager.acquire_writer("COM3", "client_2", blocking=True, timeout=0.1)
        elapsed = time.time() - start

        self.assertFalse(result)
        self.assertGreaterEqual(elapsed, 0.1)
        self.assertLess(elapsed, 0.5)  # Should not wait too long

    def test_release_writer_success(self):
        """Test successful writer release."""
        self.manager.acquire_writer("COM3", "client_1")

        result = self.manager.release_writer("COM3", "client_1")

        self.assertTrue(result)
        self.assertFalse(self.manager.has_writer("COM3"))
        self.assertIsNone(self.manager.get_writer("COM3"))

    def test_release_writer_wrong_client(self):
        """Test that wrong client cannot release writer."""
        self.manager.acquire_writer("COM3", "client_1")

        result = self.manager.release_writer("COM3", "client_2")

        self.assertFalse(result)
        self.assertEqual(self.manager.get_writer("COM3"), "client_1")

    def test_release_writer_unknown_port(self):
        """Test release writer for unknown port."""
        result = self.manager.release_writer("COM99", "client_1")

        self.assertFalse(result)

    def test_release_writer_no_writer(self):
        """Test release when no writer is set."""
        result = self.manager.release_writer("COM3", "client_1")

        self.assertFalse(result)

    def test_has_writer_unknown_port(self):
        """Test has_writer for unknown port returns False."""
        self.assertFalse(self.manager.has_writer("COM99"))

    def test_get_writer_unknown_port(self):
        """Test get_writer for unknown port returns None."""
        self.assertIsNone(self.manager.get_writer("COM99"))

    def test_writer_blocks_then_succeeds_after_release(self):
        """Test that blocked writer can succeed after release."""
        self.manager.acquire_writer("COM3", "client_1")

        results = []

        def try_acquire():
            result = self.manager.acquire_writer("COM3", "client_2", blocking=True, timeout=1.0)
            results.append(result)

        # Start thread that will block
        t = threading.Thread(target=try_acquire)
        t.start()

        # Wait a bit, then release
        time.sleep(0.05)
        self.manager.release_writer("COM3", "client_1")

        t.join(timeout=1.0)

        self.assertEqual(len(results), 1)
        self.assertTrue(results[0])
        self.assertEqual(self.manager.get_writer("COM3"), "client_2")


class TestSharedSerialManagerBuffer(unittest.TestCase):
    """Test cases for output buffer operations."""

    def setUp(self):
        """Create a fresh SharedSerialManager with an open port."""
        self.manager = SharedSerialManager()
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)

    def test_broadcast_output(self):
        """Test broadcasting output to buffer."""
        self.manager.broadcast_output("COM3", b"Hello, ")
        self.manager.broadcast_output("COM3", b"World!")

        buffer = self.manager.get_buffer("COM3")
        self.assertEqual(buffer, b"Hello, World!")

    def test_get_buffer_clear(self):
        """Test getting buffer with clear flag."""
        self.manager.broadcast_output("COM3", b"Test data")

        buffer = self.manager.get_buffer("COM3", clear=True)
        self.assertEqual(buffer, b"Test data")

        # Buffer should be empty now
        self.assertEqual(self.manager.get_buffer("COM3"), b"")

    def test_get_buffer_no_clear(self):
        """Test getting buffer without clearing."""
        self.manager.broadcast_output("COM3", b"Test data")

        buffer1 = self.manager.get_buffer("COM3", clear=False)
        buffer2 = self.manager.get_buffer("COM3", clear=False)

        self.assertEqual(buffer1, buffer2)

    def test_clear_buffer(self):
        """Test explicit buffer clearing."""
        self.manager.broadcast_output("COM3", b"Test data")

        self.manager.clear_buffer("COM3")

        self.assertEqual(self.manager.get_buffer("COM3"), b"")

    def test_get_buffer_unknown_port(self):
        """Test get_buffer for unknown port returns empty bytes."""
        buffer = self.manager.get_buffer("COM99")
        self.assertEqual(buffer, b"")

    def test_broadcast_to_unknown_port(self):
        """Test broadcasting to unknown port does nothing."""
        # Should not raise
        self.manager.broadcast_output("COM99", b"Test")

    def test_clear_buffer_unknown_port(self):
        """Test clearing buffer for unknown port does nothing."""
        # Should not raise
        self.manager.clear_buffer("COM99")

    def test_large_buffer(self):
        """Test handling of large buffer data."""
        large_data = b"X" * 1000000  # 1MB

        self.manager.broadcast_output("COM3", large_data)

        buffer = self.manager.get_buffer("COM3")
        self.assertEqual(len(buffer), 1000000)


class TestSharedSerialManagerClientCleanup(unittest.TestCase):
    """Test cases for client disconnect cleanup."""

    def setUp(self):
        """Create a fresh SharedSerialManager with open ports."""
        self.manager = SharedSerialManager()
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)
            self.manager.open_port("COM4", 115200)

    def test_cleanup_client_removes_reader(self):
        """Test that cleanup removes client from readers."""
        self.manager.attach_reader("COM3", "client_1")
        self.manager.attach_reader("COM4", "client_1")

        results = self.manager.cleanup_client("client_1")

        self.assertTrue(results["COM3"])
        self.assertTrue(results["COM4"])
        self.assertNotIn("client_1", self.manager.get_readers("COM3"))
        self.assertNotIn("client_1", self.manager.get_readers("COM4"))

    def test_cleanup_client_releases_writer(self):
        """Test that cleanup releases writer access."""
        self.manager.acquire_writer("COM3", "client_1")

        results = self.manager.cleanup_client("client_1")

        self.assertTrue(results["COM3"])
        self.assertFalse(self.manager.has_writer("COM3"))

    def test_cleanup_client_both_reader_and_writer(self):
        """Test cleanup when client is both reader and writer."""
        self.manager.attach_reader("COM3", "client_1")
        self.manager.acquire_writer("COM3", "client_1")

        results = self.manager.cleanup_client("client_1")

        self.assertTrue(results["COM3"])
        self.assertNotIn("client_1", self.manager.get_readers("COM3"))
        self.assertFalse(self.manager.has_writer("COM3"))

    def test_cleanup_client_not_attached(self):
        """Test cleanup for client not attached anywhere."""
        results = self.manager.cleanup_client("unknown_client")

        self.assertFalse(results["COM3"])
        self.assertFalse(results["COM4"])

    def test_cleanup_preserves_other_clients(self):
        """Test that cleanup only affects specified client."""
        self.manager.attach_reader("COM3", "client_1")
        self.manager.attach_reader("COM3", "client_2")

        self.manager.cleanup_client("client_1")

        self.assertNotIn("client_1", self.manager.get_readers("COM3"))
        self.assertIn("client_2", self.manager.get_readers("COM3"))


class TestSharedSerialManagerSessionInfo(unittest.TestCase):
    """Test cases for session information reporting."""

    def setUp(self):
        """Create a fresh SharedSerialManager with an open port."""
        self.manager = SharedSerialManager()
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)

    def test_get_session_info(self):
        """Test getting session information."""
        self.manager.attach_reader("COM3", "client_1")
        self.manager.attach_reader("COM3", "client_2")
        self.manager.acquire_writer("COM3", "client_1")
        self.manager.broadcast_output("COM3", b"Test")

        info = self.manager.get_session_info("COM3")

        self.assertEqual(info["port"], "COM3")
        self.assertEqual(info["baud_rate"], 115200)
        self.assertTrue(info["is_open"])
        self.assertEqual(len(info["readers"]), 2)
        self.assertEqual(info["reader_count"], 2)
        self.assertEqual(info["writer"], "client_1")
        self.assertTrue(info["has_writer"])
        self.assertEqual(info["buffer_size"], 4)
        self.assertIsInstance(info["created_at"], float)

    def test_get_session_info_unknown_port(self):
        """Test session info for unknown port returns None."""
        info = self.manager.get_session_info("COM99")
        self.assertIsNone(info)

    def test_get_all_sessions(self):
        """Test getting all session information."""
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM4", 9600)

        self.manager.attach_reader("COM3", "client_1")
        self.manager.attach_reader("COM4", "client_2")

        sessions = self.manager.get_all_sessions()

        self.assertEqual(len(sessions), 2)
        self.assertIn("COM3", sessions)
        self.assertIn("COM4", sessions)
        self.assertEqual(sessions["COM3"]["baud_rate"], 115200)
        self.assertEqual(sessions["COM4"]["baud_rate"], 9600)

    def test_get_all_sessions_empty(self):
        """Test getting sessions when manager is empty."""
        manager = SharedSerialManager()
        sessions = manager.get_all_sessions()
        self.assertEqual(sessions, {})


class TestSharedSerialManagerThreadSafety(unittest.TestCase):
    """Test cases for thread-safe operations."""

    def setUp(self):
        """Create a fresh SharedSerialManager with an open port."""
        self.manager = SharedSerialManager()
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)

    def test_concurrent_reader_attach(self):
        """Test concurrent reader attachment is thread-safe."""
        results = []

        def attach_reader(client_id):
            result = self.manager.attach_reader("COM3", client_id)
            results.append((client_id, result))

        threads = [threading.Thread(target=attach_reader, args=(f"client_{i}",)) for i in range(10)]

        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # All should succeed
        self.assertEqual(len(results), 10)
        self.assertTrue(all(r[1] for r in results))
        self.assertEqual(self.manager.get_reader_count("COM3"), 10)

    def test_concurrent_reader_detach(self):
        """Test concurrent reader detachment is thread-safe."""
        # First attach all readers
        for i in range(10):
            self.manager.attach_reader("COM3", f"client_{i}")

        results = []

        def detach_reader(client_id):
            result = self.manager.detach_reader("COM3", client_id)
            results.append((client_id, result))

        threads = [threading.Thread(target=detach_reader, args=(f"client_{i}",)) for i in range(10)]

        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # All should succeed
        self.assertEqual(len(results), 10)
        self.assertTrue(all(r[1] for r in results))
        self.assertEqual(self.manager.get_reader_count("COM3"), 0)

    def test_concurrent_writer_acquisition(self):
        """Test that only one writer wins in concurrent acquisition."""
        results = []

        def try_acquire_writer(client_id):
            result = self.manager.acquire_writer("COM3", client_id, blocking=False)
            results.append((client_id, result))

        threads = [threading.Thread(target=try_acquire_writer, args=(f"client_{i}",)) for i in range(10)]

        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # Exactly one should succeed
        successes = [r for r in results if r[1]]
        self.assertEqual(len(successes), 1)

        # That client should be the writer
        winning_client = successes[0][0]
        self.assertEqual(self.manager.get_writer("COM3"), winning_client)

    def test_concurrent_buffer_operations(self):
        """Test concurrent buffer operations are thread-safe."""

        def broadcast_data(data_id):
            for _ in range(100):
                self.manager.broadcast_output("COM3", f"data_{data_id}\n".encode())

        threads = [threading.Thread(target=broadcast_data, args=(i,)) for i in range(5)]

        for t in threads:
            t.start()
        for t in threads:
            t.join()

        buffer = self.manager.get_buffer("COM3")
        # Should have 500 entries (5 threads * 100 broadcasts each)
        self.assertEqual(buffer.count(b"\n"), 500)

    def test_concurrent_mixed_operations(self):
        """Test concurrent mixed read/write operations."""
        errors = []

        def reader_loop(client_id):
            try:
                for _ in range(50):
                    self.manager.attach_reader("COM3", client_id)
                    time.sleep(0.001)
                    self.manager.get_buffer("COM3")
                    self.manager.detach_reader("COM3", client_id)
            except Exception as e:
                errors.append(str(e))

        def writer_loop(client_id):
            try:
                for _ in range(50):
                    if self.manager.acquire_writer("COM3", client_id, blocking=False):
                        self.manager.broadcast_output("COM3", b"data")
                        self.manager.release_writer("COM3", client_id)
                    time.sleep(0.001)
            except Exception as e:
                errors.append(str(e))

        threads = []
        for i in range(3):
            threads.append(threading.Thread(target=reader_loop, args=(f"reader_{i}",)))
        for i in range(2):
            threads.append(threading.Thread(target=writer_loop, args=(f"writer_{i}",)))

        for t in threads:
            t.start()
        for t in threads:
            t.join()

        # No errors should have occurred
        self.assertEqual(errors, [])


class TestSharedSerialManagerEdgeCases(unittest.TestCase):
    """Test edge cases and error conditions."""

    def setUp(self):
        """Create a fresh SharedSerialManager."""
        self.manager = SharedSerialManager()

    def test_empty_client_id(self):
        """Test handling of empty client ID."""
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)

        # Empty string is technically valid
        result = self.manager.attach_reader("COM3", "")
        self.assertTrue(result)

    def test_special_characters_in_client_id(self):
        """Test handling of special characters in client ID."""
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)

        client_id = "client_with_special_chars!@#$%^&*()"
        result = self.manager.attach_reader("COM3", client_id)
        self.assertTrue(result)
        self.assertIn(client_id, self.manager.get_readers("COM3"))

    def test_unicode_in_buffer(self):
        """Test handling of unicode data in buffer."""
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)

        unicode_data = "Hello World".encode("utf-8")
        self.manager.broadcast_output("COM3", unicode_data)

        buffer = self.manager.get_buffer("COM3")
        self.assertEqual(buffer.decode("utf-8"), "Hello World")

    def test_reopen_after_close(self):
        """Test reopening port after close."""
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)
            self.manager.close_port("COM3")

            # Should be able to reopen
            result = self.manager.open_port("COM3", 115200)
            self.assertTrue(result)
            self.assertTrue(self.manager.is_port_open("COM3"))

    def test_operations_after_close(self):
        """Test operations after port close fail appropriately."""
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)
            self.manager.close_port("COM3")

        with self.assertRaises(ValueError):
            self.manager.attach_reader("COM3", "client_1")

        with self.assertRaises(ValueError):
            self.manager.acquire_writer("COM3", "client_1")

    def test_very_long_port_name(self):
        """Test handling of very long port name."""
        long_port_name = "/dev/" + "x" * 1000

        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            result = self.manager.open_port(long_port_name, 115200)

        self.assertTrue(result)
        self.assertTrue(self.manager.is_port_open(long_port_name))

    def test_multiple_ports_independent(self):
        """Test that operations on different ports are independent."""
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)
            self.manager.open_port("COM4", 9600)

        self.manager.attach_reader("COM3", "client_1")
        self.manager.acquire_writer("COM4", "client_2")

        # COM3 should have reader but no writer
        self.assertIn("client_1", self.manager.get_readers("COM3"))
        self.assertFalse(self.manager.has_writer("COM3"))

        # COM4 should have writer but no readers
        self.assertEqual(self.manager.get_reader_count("COM4"), 0)
        self.assertEqual(self.manager.get_writer("COM4"), "client_2")

    def test_session_info_after_modifications(self):
        """Test session info reflects all modifications."""
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)

        # Initial state
        info = self.manager.get_session_info("COM3")
        self.assertEqual(info["reader_count"], 0)
        self.assertFalse(info["has_writer"])
        self.assertEqual(info["buffer_size"], 0)

        # Add reader
        self.manager.attach_reader("COM3", "client_1")
        info = self.manager.get_session_info("COM3")
        self.assertEqual(info["reader_count"], 1)

        # Add writer
        self.manager.acquire_writer("COM3", "client_1")
        info = self.manager.get_session_info("COM3")
        self.assertTrue(info["has_writer"])

        # Add to buffer
        self.manager.broadcast_output("COM3", b"test")
        info = self.manager.get_session_info("COM3")
        self.assertEqual(info["buffer_size"], 4)

    def test_zero_baud_rate(self):
        """Test handling of zero baud rate."""
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            result = self.manager.open_port("COM3", 0)

        # Implementation dependent - should either succeed or fail gracefully
        # Here we just verify no crash
        self.assertIsInstance(result, bool)

    def test_negative_timeout(self):
        """Test handling of negative timeout."""
        with patch("serial.Serial") as mock_serial:
            mock_serial.return_value = MagicMock()
            self.manager.open_port("COM3", 115200)
            self.manager.acquire_writer("COM3", "client_1")

        # Negative timeout should be treated as immediate (no wait)
        start = time.time()
        result = self.manager.acquire_writer("COM3", "client_2", blocking=True, timeout=-1)
        elapsed = time.time() - start

        self.assertFalse(result)
        self.assertLess(elapsed, 0.1)  # Should not wait


class TestSerialSessionDataclass(unittest.TestCase):
    """Test cases for the SerialSession dataclass."""

    def test_default_values(self):
        """Test SerialSession default values."""
        session = SerialSession(port="COM3")

        self.assertEqual(session.port, "COM3")
        self.assertEqual(session.baud_rate, 115200)
        self.assertFalse(session.is_open)
        self.assertEqual(session.readers, set())
        self.assertIsNone(session.writer)
        self.assertEqual(session.buffer, b"")
        self.assertIsInstance(session.created_at, float)

    def test_custom_values(self):
        """Test SerialSession with custom values."""
        session = SerialSession(port="COM4", baud_rate=9600, is_open=True, readers={"client_1", "client_2"}, writer="client_1", buffer=b"test data", created_at=1234567890.0)

        self.assertEqual(session.port, "COM4")
        self.assertEqual(session.baud_rate, 9600)
        self.assertTrue(session.is_open)
        self.assertEqual(session.readers, {"client_1", "client_2"})
        self.assertEqual(session.writer, "client_1")
        self.assertEqual(session.buffer, b"test data")
        self.assertEqual(session.created_at, 1234567890.0)

    def test_readers_mutable(self):
        """Test that readers set is mutable."""
        session = SerialSession(port="COM3")

        session.readers.add("client_1")
        self.assertIn("client_1", session.readers)

        session.readers.discard("client_1")
        self.assertNotIn("client_1", session.readers)

    def test_buffer_mutable(self):
        """Test that buffer can be modified."""
        session = SerialSession(port="COM3")

        session.buffer += b"test"
        self.assertEqual(session.buffer, b"test")


if __name__ == "__main__":
    unittest.main()
