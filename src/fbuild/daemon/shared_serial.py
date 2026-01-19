"""
Shared Serial Manager - Centralized serial port management for multiple clients.

This module provides the SharedSerialManager class which allows multiple clients
to read serial output from a device (broadcast), while only one client can write
to it at a time (exclusive access). Key features:

- Centralized serial port management on the daemon
- Multiple "reader" clients can attach and receive serial output (broadcast)
- Single "writer" client has exclusive input access
- Output is buffered and distributed to all attached readers
- Thread-safe operations
- Handle reader/writer attach/detach

Example:
    >>> manager = SharedSerialManager()
    >>> # Open port and attach as reader
    >>> manager.open_port("COM3", 115200, client_id="client_1")
    >>> manager.attach_reader("COM3", "client_1")
    >>> # Another client can also read
    >>> manager.attach_reader("COM3", "client_2")
    >>> # Only one writer at a time
    >>> manager.acquire_writer("COM3", "client_1", timeout=5.0)
    >>> manager.write("COM3", "client_1", b"hello\\n")
    >>> manager.release_writer("COM3", "client_1")
    >>> # Get buffered output
    >>> lines = manager.read_buffer("COM3", "client_1")
"""

import logging
import threading
import time
from collections import deque
from dataclasses import dataclass, field
from typing import Any, Callable

# Default buffer size (number of lines)
DEFAULT_BUFFER_SIZE = 10000

# Serial read timeout in seconds
SERIAL_READ_TIMEOUT = 0.1

# Writer acquisition timeout default
DEFAULT_WRITER_TIMEOUT = 10.0


@dataclass
class SerialSession:
    """Information about an active serial session.

    Attributes:
        port: Serial port identifier (e.g., "COM3", "/dev/ttyUSB0")
        baud_rate: Baud rate for the serial connection
        is_open: Whether the serial port is currently open
        writer_client_id: Client ID that has exclusive write access (None if no writer)
        reader_client_ids: Set of client IDs attached as readers
        output_buffer: Deque of recent output lines (thread-safe)
        total_bytes_read: Total bytes read from the serial port
        total_bytes_written: Total bytes written to the serial port
        started_at: Unix timestamp when the session was started
        owner_client_id: Client ID that opened the port (owner)
    """

    port: str
    baud_rate: int
    is_open: bool = False
    writer_client_id: str | None = None
    reader_client_ids: set[str] = field(default_factory=set)
    output_buffer: deque[str] = field(default_factory=lambda: deque(maxlen=DEFAULT_BUFFER_SIZE))
    total_bytes_read: int = 0
    total_bytes_written: int = 0
    started_at: float = field(default_factory=time.time)
    owner_client_id: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert session info to dictionary for JSON serialization."""
        return {
            "port": self.port,
            "baud_rate": self.baud_rate,
            "is_open": self.is_open,
            "writer_client_id": self.writer_client_id,
            "reader_client_ids": list(self.reader_client_ids),
            "buffer_size": len(self.output_buffer),
            "total_bytes_read": self.total_bytes_read,
            "total_bytes_written": self.total_bytes_written,
            "started_at": self.started_at,
            "owner_client_id": self.owner_client_id,
            "uptime_seconds": time.time() - self.started_at,
        }


class SharedSerialManager:
    """Manages shared serial port access for multiple clients.

    This class provides centralized serial port management where:
    - Multiple clients can attach as readers and receive output (broadcast)
    - Only one client can have write access at a time (exclusive)
    - Output is buffered and available for clients to poll
    - All operations are thread-safe

    The manager maintains a background thread per port for reading serial data
    and distributing it to all attached readers.

    Example:
        >>> manager = SharedSerialManager()
        >>>
        >>> # Open port (becomes owner)
        >>> manager.open_port("COM3", 115200, "client_1")
        >>>
        >>> # Attach as reader to get output
        >>> manager.attach_reader("COM3", "client_1")
        >>> manager.attach_reader("COM3", "client_2")  # Another client
        >>>
        >>> # Get write access
        >>> if manager.acquire_writer("COM3", "client_1", timeout=5.0):
        ...     manager.write("COM3", "client_1", b"test\\n")
        ...     manager.release_writer("COM3", "client_1")
        >>>
        >>> # Read buffered output
        >>> lines = manager.read_buffer("COM3", "client_1", max_lines=100)
        >>>
        >>> # Cleanup when done
        >>> manager.detach_reader("COM3", "client_2")
        >>> manager.close_port("COM3", "client_1")
    """

    def __init__(self, max_buffer_size: int = DEFAULT_BUFFER_SIZE) -> None:
        """Initialize the SharedSerialManager.

        Args:
            max_buffer_size: Maximum number of lines to buffer per session
        """
        self._lock = threading.Lock()  # Master lock for sessions dictionary
        self._sessions: dict[str, SerialSession] = {}  # port -> session
        self._serial_ports: dict[str, Any] = {}  # port -> pyserial.Serial object
        self._reader_threads: dict[str, threading.Thread] = {}  # port -> reader thread
        self._stop_events: dict[str, threading.Event] = {}  # port -> stop event
        self._writer_locks: dict[str, threading.Lock] = {}  # port -> writer lock
        self._writer_conditions: dict[str, threading.Condition] = {}  # port -> writer condition
        self._max_buffer_size = max_buffer_size
        self._reader_callbacks: dict[str, dict[str, Callable[[str, str], None]]] = {}  # port -> {client_id -> callback}

    def open_port(self, port: str, baud_rate: int, client_id: str) -> bool:
        """Open a serial port if not already open.

        The client that opens the port becomes the owner. Only the owner or the
        last reader can close the port.

        Args:
            port: Serial port identifier (e.g., "COM3", "/dev/ttyUSB0")
            baud_rate: Baud rate for the serial connection
            client_id: Unique identifier for the client

        Returns:
            True if port was opened successfully or was already open, False on error
        """
        with self._lock:
            # Check if port is already open
            if port in self._sessions and self._sessions[port].is_open:
                logging.info(f"Port {port} already open (requested by {client_id})")
                return True

            # Try to open the port
            try:
                import serial

                ser = serial.Serial(
                    port,
                    baud_rate,
                    timeout=SERIAL_READ_TIMEOUT,
                )

                # Create session
                session = SerialSession(
                    port=port,
                    baud_rate=baud_rate,
                    is_open=True,
                    owner_client_id=client_id,
                    started_at=time.time(),
                    output_buffer=deque(maxlen=self._max_buffer_size),
                )
                self._sessions[port] = session
                self._serial_ports[port] = ser
                self._writer_locks[port] = threading.Lock()
                self._writer_conditions[port] = threading.Condition(self._writer_locks[port])
                self._reader_callbacks[port] = {}

                # Start background reader thread
                stop_event = threading.Event()
                self._stop_events[port] = stop_event
                reader_thread = threading.Thread(
                    target=self._serial_reader_loop,
                    args=(port, ser, stop_event),
                    name=f"SerialReader-{port}",
                    daemon=True,
                )
                self._reader_threads[port] = reader_thread
                reader_thread.start()

                logging.info(f"Opened serial port {port} at {baud_rate} baud (owner: {client_id})")
                return True

            except ImportError:
                logging.error("pyserial not installed. Install with: pip install pyserial")
                return False
            except KeyboardInterrupt:  # noqa: KBI002
                raise
            except Exception as e:
                logging.error(f"Failed to open serial port {port}: {e}")
                return False

    def close_port(self, port: str, client_id: str) -> bool:
        """Close a serial port.

        The port can be closed by the owner or if there are no more readers.

        Args:
            port: Serial port identifier
            client_id: Client requesting the close

        Returns:
            True if port was closed, False if not allowed or not found
        """
        with self._lock:
            if port not in self._sessions:
                logging.warning(f"Cannot close unknown port: {port}")
                return False

            session = self._sessions[port]

            # Check if client is allowed to close
            is_owner = session.owner_client_id == client_id
            no_readers = len(session.reader_client_ids) == 0
            is_last_reader = session.reader_client_ids == {client_id}

            if not (is_owner or no_readers or is_last_reader):
                logging.warning(f"Client {client_id} not allowed to close port {port} (owner: {session.owner_client_id}, readers: {session.reader_client_ids})")
                return False

            return self._close_port_internal(port)

    def _close_port_internal(self, port: str) -> bool:
        """Internal method to close a port (caller must hold _lock).

        Args:
            port: Serial port identifier

        Returns:
            True if port was closed, False on error
        """
        if port not in self._sessions:
            return False

        session = self._sessions[port]

        # Stop reader thread
        if port in self._stop_events:
            self._stop_events[port].set()
            del self._stop_events[port]

        # Wait for reader thread to finish (with timeout)
        if port in self._reader_threads:
            thread = self._reader_threads[port]
            # Release lock while waiting to avoid deadlock
            self._lock.release()
            try:
                thread.join(timeout=2.0)
            finally:
                self._lock.acquire()
            del self._reader_threads[port]

        # Close serial port
        if port in self._serial_ports:
            try:
                self._serial_ports[port].close()
            except KeyboardInterrupt:  # noqa: KBI002
                raise
            except Exception as e:
                logging.error(f"Error closing serial port {port}: {e}")
            del self._serial_ports[port]

        # Cleanup synchronization objects
        if port in self._writer_locks:
            del self._writer_locks[port]
        if port in self._writer_conditions:
            del self._writer_conditions[port]
        if port in self._reader_callbacks:
            del self._reader_callbacks[port]

        # Mark session as closed and remove
        session.is_open = False
        del self._sessions[port]

        logging.info(f"Closed serial port {port}")
        return True

    def attach_reader(self, port: str, client_id: str, callback: Callable[[str, str], None] | None = None) -> bool:
        """Attach as a reader to receive serial output.

        Readers receive all output from the serial port. Multiple readers can
        be attached simultaneously.

        Args:
            port: Serial port identifier
            client_id: Unique identifier for the client
            callback: Optional callback function(port, line) called for each line received

        Returns:
            True if successfully attached, False if port not open
        """
        with self._lock:
            if port not in self._sessions or not self._sessions[port].is_open:
                logging.warning(f"Cannot attach reader to closed/unknown port: {port}")
                return False

            session = self._sessions[port]
            session.reader_client_ids.add(client_id)

            if callback is not None:
                self._reader_callbacks[port][client_id] = callback

            logging.debug(f"Client {client_id} attached as reader to {port}")
            return True

    def detach_reader(self, port: str, client_id: str) -> bool:
        """Detach as a reader and stop receiving output.

        Args:
            port: Serial port identifier
            client_id: Client identifier to detach

        Returns:
            True if successfully detached, False if not found
        """
        with self._lock:
            if port not in self._sessions:
                logging.warning(f"Cannot detach reader from unknown port: {port}")
                return False

            session = self._sessions[port]
            if client_id not in session.reader_client_ids:
                logging.warning(f"Client {client_id} is not a reader on port {port}")
                return False

            session.reader_client_ids.discard(client_id)

            if port in self._reader_callbacks and client_id in self._reader_callbacks[port]:
                del self._reader_callbacks[port][client_id]

            logging.debug(f"Client {client_id} detached from {port}")
            return True

    def acquire_writer(self, port: str, client_id: str, timeout: float = DEFAULT_WRITER_TIMEOUT) -> bool:
        """Acquire exclusive write access to the serial port.

        Only one client can have write access at a time. If another client
        holds write access, this will block until the timeout expires.

        Args:
            port: Serial port identifier
            client_id: Client requesting write access
            timeout: Maximum time to wait for write access (seconds)

        Returns:
            True if write access acquired, False if timeout or port not open
        """
        with self._lock:
            if port not in self._sessions or not self._sessions[port].is_open:
                logging.warning(f"Cannot acquire writer on closed/unknown port: {port}")
                return False

            session = self._sessions[port]
            condition = self._writer_conditions[port]

        # Use condition variable to wait for writer access
        with condition:
            deadline = time.time() + timeout

            while True:
                with self._lock:
                    # Check if port is still valid
                    if port not in self._sessions or not self._sessions[port].is_open:
                        return False

                    session = self._sessions[port]

                    # Check if we can acquire
                    if session.writer_client_id is None:
                        session.writer_client_id = client_id
                        logging.info(f"Client {client_id} acquired writer on {port}")
                        return True

                    # Check if we already have it
                    if session.writer_client_id == client_id:
                        logging.debug(f"Client {client_id} already has writer on {port}")
                        return True

                # Wait with timeout
                remaining = deadline - time.time()
                if remaining <= 0:
                    logging.warning(f"Timeout acquiring writer on {port} for {client_id} (current writer: {session.writer_client_id})")
                    return False

                condition.wait(timeout=min(remaining, 0.5))

    def release_writer(self, port: str, client_id: str) -> bool:
        """Release exclusive write access.

        Args:
            port: Serial port identifier
            client_id: Client releasing write access

        Returns:
            True if released successfully, False if not the current writer
        """
        with self._lock:
            if port not in self._sessions:
                logging.warning(f"Cannot release writer on unknown port: {port}")
                return False

            session = self._sessions[port]

            if session.writer_client_id != client_id:
                logging.warning(f"Client {client_id} is not the writer on {port} (current: {session.writer_client_id})")
                return False

            session.writer_client_id = None
            condition = self._writer_conditions.get(port)

        # Notify waiting writers
        if condition:
            with condition:
                condition.notify_all()

        logging.info(f"Client {client_id} released writer on {port}")
        return True

    def write(self, port: str, client_id: str, data: bytes) -> int:
        """Write data to the serial port.

        Only the current writer can write to the port.

        Args:
            port: Serial port identifier
            client_id: Client attempting to write (must be current writer)
            data: Bytes to write to the serial port

        Returns:
            Number of bytes written, or -1 on error
        """
        with self._lock:
            if port not in self._sessions or not self._sessions[port].is_open:
                logging.warning(f"Cannot write to closed/unknown port: {port}")
                return -1

            session = self._sessions[port]

            if session.writer_client_id != client_id:
                logging.warning(f"Client {client_id} cannot write to {port} (current writer: {session.writer_client_id})")
                return -1

            if port not in self._serial_ports:
                logging.error(f"Serial port object not found for {port}")
                return -1

            ser = self._serial_ports[port]

        try:
            bytes_written = ser.write(data)
            with self._lock:
                if port in self._sessions:
                    self._sessions[port].total_bytes_written += bytes_written
            logging.debug(f"Wrote {bytes_written} bytes to {port}")
            return bytes_written
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error writing to {port}: {e}")
            return -1

    def read_buffer(self, port: str, client_id: str, max_lines: int = 100) -> list[str]:
        """Read recent output from the buffer.

        This does not remove lines from the buffer - all readers see the same
        buffered output.

        Args:
            port: Serial port identifier
            client_id: Client requesting output (must be attached reader)
            max_lines: Maximum number of lines to return

        Returns:
            List of recent output lines (most recent last)
        """
        with self._lock:
            if port not in self._sessions:
                logging.warning(f"Cannot read buffer from unknown port: {port}")
                return []

            session = self._sessions[port]

            # Verify client is a reader
            if client_id not in session.reader_client_ids:
                logging.warning(f"Client {client_id} is not a reader on {port}")
                return []

            # Return copy of buffer (thread-safe snapshot)
            buffer = session.output_buffer
            if max_lines >= len(buffer):
                return list(buffer)
            else:
                return list(buffer)[-max_lines:]

    def get_session_info(self, port: str) -> dict[str, Any] | None:
        """Get information about a serial session.

        Args:
            port: Serial port identifier

        Returns:
            Dictionary with session info, or None if port not found
        """
        with self._lock:
            if port not in self._sessions:
                return None
            return self._sessions[port].to_dict()

    def get_all_sessions(self) -> dict[str, dict[str, Any]]:
        """Get information about all active serial sessions.

        Returns:
            Dictionary mapping port names to session info dictionaries
        """
        with self._lock:
            return {port: session.to_dict() for port, session in self._sessions.items()}

    def disconnect_client(self, client_id: str) -> None:
        """Cleanup all sessions for a disconnected client.

        This removes the client from all reader lists, releases any writer
        locks they hold, and closes ports they own if no other clients remain.

        Args:
            client_id: Client identifier to clean up
        """
        with self._lock:
            ports_to_close = []

            for port, session in self._sessions.items():
                # Release writer if held by this client
                if session.writer_client_id == client_id:
                    session.writer_client_id = None
                    if port in self._writer_conditions:
                        condition = self._writer_conditions[port]
                        # Notify outside the lock
                        self._lock.release()
                        try:
                            with condition:
                                condition.notify_all()
                        finally:
                            self._lock.acquire()
                    logging.info(f"Released writer on {port} for disconnected client {client_id}")

                # Remove from readers
                if client_id in session.reader_client_ids:
                    session.reader_client_ids.discard(client_id)
                    logging.debug(f"Removed {client_id} from readers on {port}")

                # Remove callbacks
                if port in self._reader_callbacks and client_id in self._reader_callbacks[port]:
                    del self._reader_callbacks[port][client_id]

                # Mark port for closure if owner and no other readers
                if session.owner_client_id == client_id and len(session.reader_client_ids) == 0:
                    ports_to_close.append(port)

            # Close ports owned by this client with no remaining readers
            for port in ports_to_close:
                logging.info(f"Closing port {port} owned by disconnected client {client_id}")
                self._close_port_internal(port)

    def broadcast_output(self, port: str, data: bytes) -> None:
        """Internal: Distribute received data to all attached readers.

        This method is called by the background reader thread when data is
        received from the serial port.

        Args:
            port: Serial port identifier
            data: Raw bytes received from serial port
        """
        try:
            # Decode bytes to string, handling errors gracefully
            text = data.decode("utf-8", errors="replace").rstrip()
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception:
            text = str(data)

        if not text:
            return

        # Split into lines and add to buffer
        lines = text.split("\n")

        callbacks_to_call: list[tuple[Callable[[str, str], None], str]] = []

        with self._lock:
            if port not in self._sessions:
                return

            session = self._sessions[port]
            session.total_bytes_read += len(data)

            for line in lines:
                if line:  # Skip empty lines
                    session.output_buffer.append(line)

            # Collect callbacks to call (call outside lock to avoid deadlocks)
            if port in self._reader_callbacks:
                for callback in self._reader_callbacks[port].values():
                    for line in lines:
                        if line:
                            callbacks_to_call.append((callback, line))

        # Call callbacks outside the lock
        for callback, line in callbacks_to_call:
            try:
                callback(port, line)
            except KeyboardInterrupt:  # noqa: KBI002
                raise
            except Exception as e:
                logging.error(f"Error in reader callback for {port}: {e}")

    def _serial_reader_loop(self, port: str, ser: Any, stop_event: threading.Event) -> None:
        """Background thread that reads from serial port and broadcasts output.

        This method runs in a dedicated thread per port and continuously reads
        data from the serial port, broadcasting it to all attached readers.

        Args:
            port: Serial port identifier
            ser: pyserial.Serial object
            stop_event: Event to signal thread shutdown
        """
        logging.debug(f"Serial reader thread started for {port}")

        try:
            while not stop_event.is_set():
                try:
                    # Check if there's data available
                    if ser.in_waiting:
                        data = ser.readline()
                        if data:
                            self.broadcast_output(port, data)
                    else:
                        # Small sleep to avoid busy-waiting
                        time.sleep(0.01)
                except KeyboardInterrupt:  # noqa: KBI002
                    raise
                except Exception as e:
                    if not stop_event.is_set():
                        logging.error(f"Error reading from {port}: {e}")
                        # Brief sleep before retry
                        time.sleep(0.1)
        except KeyboardInterrupt:  # noqa: KBI002
            logging.debug(f"Serial reader thread for {port} interrupted")
        except Exception as e:
            logging.error(f"Fatal error in reader thread for {port}: {e}")
        finally:
            logging.debug(f"Serial reader thread stopped for {port}")

    def get_session_count(self) -> int:
        """Get the number of active serial sessions.

        Returns:
            Number of open serial sessions
        """
        with self._lock:
            return len(self._sessions)

    def shutdown(self) -> None:
        """Shutdown the manager and close all serial sessions.

        This method should be called during daemon shutdown to ensure all
        serial ports are properly closed and threads are stopped.
        """
        logging.info("Shutting down SharedSerialManager...")

        with self._lock:
            ports = list(self._sessions.keys())

        # Close all ports (close_port handles locking internally)
        for port in ports:
            with self._lock:
                if port in self._sessions:
                    self._close_port_internal(port)

        logging.info("SharedSerialManager shutdown complete")

    def reset_device(self, port: str, client_id: str) -> bool:
        """Reset the device connected to the serial port.

        Uses DTR/RTS toggling to reset the device, which is common for
        ESP32 and similar microcontrollers.

        Args:
            port: Serial port identifier
            client_id: Client requesting the reset (must be writer)

        Returns:
            True if reset was successful, False on error or unauthorized
        """
        with self._lock:
            if port not in self._sessions or not self._sessions[port].is_open:
                logging.warning(f"Cannot reset device on closed/unknown port: {port}")
                return False

            session = self._sessions[port]

            # Must be the writer to reset
            if session.writer_client_id != client_id:
                logging.warning(f"Client {client_id} cannot reset device on {port} (current writer: {session.writer_client_id})")
                return False

            if port not in self._serial_ports:
                logging.error(f"Serial port object not found for {port}")
                return False

            ser = self._serial_ports[port]

        try:
            # Toggle DTR/RTS to reset device
            ser.setDTR(False)
            ser.setRTS(True)
            time.sleep(0.1)
            ser.setRTS(False)
            time.sleep(0.1)
            ser.setDTR(True)

            logging.info(f"Reset device on {port}")
            return True
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error resetting device on {port}: {e}")
            return False

    def clear_buffer(self, port: str, client_id: str) -> bool:
        """Clear the output buffer for a port.

        Only the port owner can clear the buffer.

        Args:
            port: Serial port identifier
            client_id: Client requesting the clear (must be owner)

        Returns:
            True if buffer was cleared, False on error
        """
        with self._lock:
            if port not in self._sessions:
                logging.warning(f"Cannot clear buffer on unknown port: {port}")
                return False

            session = self._sessions[port]

            # Only owner can clear buffer
            if session.owner_client_id != client_id:
                logging.warning(f"Client {client_id} cannot clear buffer on {port} (owner: {session.owner_client_id})")
                return False

            session.output_buffer.clear()
            logging.debug(f"Cleared buffer on {port}")
            return True

    def set_baud_rate(self, port: str, client_id: str, baud_rate: int) -> bool:
        """Change the baud rate for an open port.

        Only the port owner can change the baud rate.

        Args:
            port: Serial port identifier
            client_id: Client requesting the change (must be owner)
            baud_rate: New baud rate

        Returns:
            True if baud rate was changed, False on error
        """
        with self._lock:
            if port not in self._sessions or not self._sessions[port].is_open:
                logging.warning(f"Cannot set baud rate on closed/unknown port: {port}")
                return False

            session = self._sessions[port]

            # Only owner can change baud rate
            if session.owner_client_id != client_id:
                logging.warning(f"Client {client_id} cannot set baud rate on {port} (owner: {session.owner_client_id})")
                return False

            if port not in self._serial_ports:
                logging.error(f"Serial port object not found for {port}")
                return False

            ser = self._serial_ports[port]

        try:
            ser.baudrate = baud_rate
            with self._lock:
                if port in self._sessions:
                    self._sessions[port].baud_rate = baud_rate
            logging.info(f"Changed baud rate on {port} to {baud_rate}")
            return True
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.error(f"Error setting baud rate on {port}: {e}")
            return False
