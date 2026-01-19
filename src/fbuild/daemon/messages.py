"""
Typed message protocol for fbuild daemon operations.

This module defines typed dataclasses for all client-daemon communication,
ensuring type safety and validation.

Supports:
- Build operations (compilation and linking)
- Deploy operations (firmware upload)
- Monitor operations (serial monitoring)
- Status updates and progress tracking
- Lock management (acquire, release, query)
- Firmware queries (check if firmware is current)
- Serial session management (attach, detach, write)
"""

import time
from dataclasses import asdict, dataclass, field
from enum import Enum
from typing import Any


class DaemonState(Enum):
    """Daemon state enumeration."""

    IDLE = "idle"
    DEPLOYING = "deploying"
    MONITORING = "monitoring"
    BUILDING = "building"
    COMPLETED = "completed"
    FAILED = "failed"
    UNKNOWN = "unknown"

    @classmethod
    def from_string(cls, value: str) -> "DaemonState":
        """Convert string to DaemonState, defaulting to UNKNOWN if invalid."""
        try:
            return cls(value)
        except ValueError:
            return cls.UNKNOWN


class OperationType(Enum):
    """Type of operation being performed."""

    BUILD = "build"
    DEPLOY = "deploy"
    MONITOR = "monitor"
    BUILD_AND_DEPLOY = "build_and_deploy"

    @classmethod
    def from_string(cls, value: str) -> "OperationType":
        """Convert string to OperationType."""
        return cls(value)


@dataclass
class DeployRequest:
    """Client → Daemon: Deploy request message.

    Attributes:
        project_dir: Absolute path to project directory
        environment: Build environment name
        port: Serial port for deployment (optional, auto-detect if None)
        clean_build: Whether to perform clean build
        monitor_after: Whether to start monitor after deploy
        monitor_timeout: Timeout for monitor in seconds (if monitor_after=True)
        monitor_halt_on_error: Pattern to halt on error (if monitor_after=True)
        monitor_halt_on_success: Pattern to halt on success (if monitor_after=True)
        monitor_expect: Expected pattern to check at timeout/success (if monitor_after=True)
        monitor_show_timestamp: Whether to prefix monitor output lines with elapsed time
        caller_pid: Process ID of requesting client
        caller_cwd: Working directory of requesting client
        timestamp: Unix timestamp when request was created
        request_id: Unique identifier for this request
    """

    project_dir: str
    environment: str
    port: str | None
    clean_build: bool
    monitor_after: bool
    monitor_timeout: float | None
    monitor_halt_on_error: str | None
    monitor_halt_on_success: str | None
    monitor_expect: str | None
    caller_pid: int
    caller_cwd: str
    monitor_show_timestamp: bool = False
    timestamp: float = field(default_factory=time.time)
    request_id: str = field(default_factory=lambda: f"deploy_{int(time.time() * 1000)}")

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DeployRequest":
        """Create DeployRequest from dictionary."""
        return cls(
            project_dir=data["project_dir"],
            environment=data["environment"],
            port=data.get("port"),
            clean_build=data.get("clean_build", False),
            monitor_after=data.get("monitor_after", False),
            monitor_timeout=data.get("monitor_timeout"),
            monitor_halt_on_error=data.get("monitor_halt_on_error"),
            monitor_halt_on_success=data.get("monitor_halt_on_success"),
            monitor_expect=data.get("monitor_expect"),
            caller_pid=data["caller_pid"],
            caller_cwd=data["caller_cwd"],
            monitor_show_timestamp=data.get("monitor_show_timestamp", False),
            timestamp=data.get("timestamp", time.time()),
            request_id=data.get("request_id", f"deploy_{int(time.time() * 1000)}"),
        )


@dataclass
class MonitorRequest:
    """Client → Daemon: Monitor request message.

    Attributes:
        project_dir: Absolute path to project directory
        environment: Build environment name
        port: Serial port for monitoring (optional, auto-detect if None)
        baud_rate: Serial baud rate (optional, use config default if None)
        halt_on_error: Pattern to halt on (error detection)
        halt_on_success: Pattern to halt on (success detection)
        expect: Expected pattern to check at timeout/success
        timeout: Maximum monitoring time in seconds
        caller_pid: Process ID of requesting client
        caller_cwd: Working directory of requesting client
        show_timestamp: Whether to prefix output lines with elapsed time (SS.HH format)
        timestamp: Unix timestamp when request was created
        request_id: Unique identifier for this request
    """

    project_dir: str
    environment: str
    port: str | None
    baud_rate: int | None
    halt_on_error: str | None
    halt_on_success: str | None
    expect: str | None
    timeout: float | None
    caller_pid: int
    caller_cwd: str
    show_timestamp: bool = False
    timestamp: float = field(default_factory=time.time)
    request_id: str = field(default_factory=lambda: f"monitor_{int(time.time() * 1000)}")

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "MonitorRequest":
        """Create MonitorRequest from dictionary."""
        return cls(
            project_dir=data["project_dir"],
            environment=data["environment"],
            port=data.get("port"),
            baud_rate=data.get("baud_rate"),
            halt_on_error=data.get("halt_on_error"),
            halt_on_success=data.get("halt_on_success"),
            expect=data.get("expect"),
            timeout=data.get("timeout"),
            caller_pid=data["caller_pid"],
            caller_cwd=data["caller_cwd"],
            show_timestamp=data.get("show_timestamp", False),
            timestamp=data.get("timestamp", time.time()),
            request_id=data.get("request_id", f"monitor_{int(time.time() * 1000)}"),
        )


@dataclass
class BuildRequest:
    """Client → Daemon: Build request message.

    Attributes:
        project_dir: Absolute path to project directory
        environment: Build environment name
        clean_build: Whether to perform clean build
        verbose: Enable verbose build output
        caller_pid: Process ID of requesting client
        caller_cwd: Working directory of requesting client
        timestamp: Unix timestamp when request was created
        request_id: Unique identifier for this request
    """

    project_dir: str
    environment: str
    clean_build: bool
    verbose: bool
    caller_pid: int
    caller_cwd: str
    timestamp: float = field(default_factory=time.time)
    request_id: str = field(default_factory=lambda: f"build_{int(time.time() * 1000)}")

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "BuildRequest":
        """Create BuildRequest from dictionary."""
        return cls(
            project_dir=data["project_dir"],
            environment=data["environment"],
            clean_build=data.get("clean_build", False),
            verbose=data.get("verbose", False),
            caller_pid=data["caller_pid"],
            caller_cwd=data["caller_cwd"],
            timestamp=data.get("timestamp", time.time()),
            request_id=data.get("request_id", f"build_{int(time.time() * 1000)}"),
        )


@dataclass
class InstallDependenciesRequest:
    """Client → Daemon: Install dependencies request message.

    This request downloads and caches all dependencies (toolchain, platform,
    framework, libraries) without performing a build. Useful for:
    - Pre-warming the cache before builds
    - Ensuring dependencies are available offline
    - Separating dependency installation from compilation

    Attributes:
        project_dir: Absolute path to project directory
        environment: Build environment name
        verbose: Enable verbose output
        caller_pid: Process ID of requesting client
        caller_cwd: Working directory of requesting client
        timestamp: Unix timestamp when request was created
        request_id: Unique identifier for this request
    """

    project_dir: str
    environment: str
    verbose: bool
    caller_pid: int
    caller_cwd: str
    timestamp: float = field(default_factory=time.time)
    request_id: str = field(default_factory=lambda: f"install_deps_{int(time.time() * 1000)}")

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "InstallDependenciesRequest":
        """Create InstallDependenciesRequest from dictionary."""
        return cls(
            project_dir=data["project_dir"],
            environment=data["environment"],
            verbose=data.get("verbose", False),
            caller_pid=data["caller_pid"],
            caller_cwd=data["caller_cwd"],
            timestamp=data.get("timestamp", time.time()),
            request_id=data.get("request_id", f"install_deps_{int(time.time() * 1000)}"),
        )


@dataclass
class DaemonStatus:
    """Daemon → Client: Status update message.

    Attributes:
        state: Current daemon state
        message: Human-readable status message
        updated_at: Unix timestamp of last status update
        operation_in_progress: Whether an operation is actively running
        daemon_pid: Process ID of the daemon
        daemon_started_at: Unix timestamp when daemon started
        caller_pid: Process ID of client whose request is being processed
        caller_cwd: Working directory of client whose request is being processed
        request_id: ID of the request currently being processed
        request_started_at: Unix timestamp when current request started
        environment: Environment being processed
        project_dir: Project directory for current operation
        current_operation: Detailed description of current operation
        operation_type: Type of operation (deploy/monitor)
        output_lines: Recent output lines from the operation
        exit_code: Process exit code (None if still running)
        port: Serial port being used
        ports: Dictionary of active ports with their state information
        locks: Dictionary of lock state information (port_locks, project_locks)
    """

    state: DaemonState
    message: str
    updated_at: float
    operation_in_progress: bool = False
    daemon_pid: int | None = None
    daemon_started_at: float | None = None
    caller_pid: int | None = None
    caller_cwd: str | None = None
    request_id: str | None = None
    request_started_at: float | None = None
    environment: str | None = None
    project_dir: str | None = None
    current_operation: str | None = None
    operation_type: OperationType | None = None
    output_lines: list[str] = field(default_factory=list)
    exit_code: int | None = None
    port: str | None = None
    ports: dict[str, Any] = field(default_factory=dict)
    locks: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        result = asdict(self)
        # Convert enums to string values
        result["state"] = self.state.value
        if self.operation_type:
            result["operation_type"] = self.operation_type.value
        else:
            result["operation_type"] = None
        return result

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DaemonStatus":
        """Create DaemonStatus from dictionary."""
        # Convert state string to enum
        state_str = data.get("state", "unknown")
        state = DaemonState.from_string(state_str)

        # Convert operation_type string to enum
        operation_type = None
        if data.get("operation_type"):
            operation_type = OperationType.from_string(data["operation_type"])

        return cls(
            state=state,
            message=data.get("message", ""),
            updated_at=data.get("updated_at", time.time()),
            operation_in_progress=data.get("operation_in_progress", False),
            daemon_pid=data.get("daemon_pid"),
            daemon_started_at=data.get("daemon_started_at"),
            caller_pid=data.get("caller_pid"),
            caller_cwd=data.get("caller_cwd"),
            request_id=data.get("request_id"),
            request_started_at=data.get("request_started_at"),
            environment=data.get("environment"),
            project_dir=data.get("project_dir"),
            current_operation=data.get("current_operation"),
            operation_type=operation_type,
            output_lines=data.get("output_lines", []),
            exit_code=data.get("exit_code"),
            port=data.get("port"),
            ports=data.get("ports", {}),
            locks=data.get("locks", {}),
        )

    def is_stale(self, timeout_seconds: float = 30.0) -> bool:
        """Check if status hasn't been updated recently."""
        return (time.time() - self.updated_at) > timeout_seconds

    def get_age_seconds(self) -> float:
        """Get age of this status update in seconds."""
        return time.time() - self.updated_at


# =============================================================================
# Lock Management Messages (Iteration 2)
# =============================================================================


class LockType(Enum):
    """Type of lock to acquire."""

    EXCLUSIVE = "exclusive"
    SHARED_READ = "shared_read"


@dataclass
class LockAcquireRequest:
    """Client → Daemon: Request to acquire a configuration lock.

    Attributes:
        client_id: Unique identifier for the requesting client
        project_dir: Absolute path to project directory
        environment: Build environment name
        port: Serial port for the configuration
        lock_type: Type of lock to acquire (exclusive or shared_read)
        description: Human-readable description of the operation
        timeout: Maximum time in seconds to wait for the lock
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    project_dir: str
    environment: str
    port: str
    lock_type: LockType
    description: str = ""
    timeout: float = 300.0  # 5 minutes default
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        result = asdict(self)
        result["lock_type"] = self.lock_type.value
        return result

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LockAcquireRequest":
        """Create LockAcquireRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            project_dir=data["project_dir"],
            environment=data["environment"],
            port=data["port"],
            lock_type=LockType(data["lock_type"]),
            description=data.get("description", ""),
            timeout=data.get("timeout", 300.0),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class LockReleaseRequest:
    """Client → Daemon: Request to release a configuration lock.

    Attributes:
        client_id: Unique identifier for the client releasing the lock
        project_dir: Absolute path to project directory
        environment: Build environment name
        port: Serial port for the configuration
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    project_dir: str
    environment: str
    port: str
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LockReleaseRequest":
        """Create LockReleaseRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            project_dir=data["project_dir"],
            environment=data["environment"],
            port=data["port"],
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class LockStatusRequest:
    """Client → Daemon: Request status of a configuration lock.

    Attributes:
        project_dir: Absolute path to project directory
        environment: Build environment name
        port: Serial port for the configuration
        timestamp: Unix timestamp when request was created
    """

    project_dir: str
    environment: str
    port: str
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LockStatusRequest":
        """Create LockStatusRequest from dictionary."""
        return cls(
            project_dir=data["project_dir"],
            environment=data["environment"],
            port=data["port"],
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class LockResponse:
    """Daemon → Client: Response to a lock request.

    Attributes:
        success: Whether the operation succeeded
        message: Human-readable status message
        lock_state: Current state of the lock ("unlocked", "locked_exclusive", "locked_shared_read")
        holder_count: Number of clients holding the lock
        waiting_count: Number of clients waiting for the lock
        timestamp: Unix timestamp of the response
    """

    success: bool
    message: str
    lock_state: str = "unlocked"
    holder_count: int = 0
    waiting_count: int = 0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "LockResponse":
        """Create LockResponse from dictionary."""
        return cls(
            success=data["success"],
            message=data["message"],
            lock_state=data.get("lock_state", "unlocked"),
            holder_count=data.get("holder_count", 0),
            waiting_count=data.get("waiting_count", 0),
            timestamp=data.get("timestamp", time.time()),
        )


# =============================================================================
# Firmware Ledger Messages (Iteration 2)
# =============================================================================


@dataclass
class FirmwareQueryRequest:
    """Client → Daemon: Query if firmware is current on a device.

    Used to check if a redeploy is needed or if the device already has
    the expected firmware loaded.

    Attributes:
        port: Serial port of the device
        source_hash: Hash of the source files
        build_flags_hash: Hash of the build flags (optional)
        timestamp: Unix timestamp when request was created
    """

    port: str
    source_hash: str
    build_flags_hash: str | None = None
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "FirmwareQueryRequest":
        """Create FirmwareQueryRequest from dictionary."""
        return cls(
            port=data["port"],
            source_hash=data["source_hash"],
            build_flags_hash=data.get("build_flags_hash"),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class FirmwareQueryResponse:
    """Daemon → Client: Response to firmware query.

    Attributes:
        is_current: True if firmware matches what's deployed (no redeploy needed)
        needs_redeploy: True if source or build flags have changed
        firmware_hash: Hash of the currently deployed firmware (if known)
        project_dir: Project directory of the deployed firmware
        environment: Environment of the deployed firmware
        upload_timestamp: When the firmware was last uploaded
        message: Human-readable message
        timestamp: Unix timestamp of the response
    """

    is_current: bool
    needs_redeploy: bool
    firmware_hash: str | None = None
    project_dir: str | None = None
    environment: str | None = None
    upload_timestamp: float | None = None
    message: str = ""
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "FirmwareQueryResponse":
        """Create FirmwareQueryResponse from dictionary."""
        return cls(
            is_current=data["is_current"],
            needs_redeploy=data["needs_redeploy"],
            firmware_hash=data.get("firmware_hash"),
            project_dir=data.get("project_dir"),
            environment=data.get("environment"),
            upload_timestamp=data.get("upload_timestamp"),
            message=data.get("message", ""),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class FirmwareRecordRequest:
    """Client → Daemon: Record a firmware deployment.

    Sent after a successful upload to update the firmware ledger.

    Attributes:
        port: Serial port of the device
        firmware_hash: Hash of the firmware file
        source_hash: Hash of the source files
        project_dir: Absolute path to project directory
        environment: Build environment name
        build_flags_hash: Hash of build flags (optional)
        timestamp: Unix timestamp when request was created
    """

    port: str
    firmware_hash: str
    source_hash: str
    project_dir: str
    environment: str
    build_flags_hash: str | None = None
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "FirmwareRecordRequest":
        """Create FirmwareRecordRequest from dictionary."""
        return cls(
            port=data["port"],
            firmware_hash=data["firmware_hash"],
            source_hash=data["source_hash"],
            project_dir=data["project_dir"],
            environment=data["environment"],
            build_flags_hash=data.get("build_flags_hash"),
            timestamp=data.get("timestamp", time.time()),
        )


# =============================================================================
# Serial Session Messages (Iteration 2)
# =============================================================================


@dataclass
class SerialAttachRequest:
    """Client → Daemon: Request to attach to a serial session.

    Attributes:
        client_id: Unique identifier for the client
        port: Serial port to attach to
        baud_rate: Baud rate for the connection
        as_reader: Whether to attach as a reader (True) or open the port (False)
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    port: str
    baud_rate: int = 115200
    as_reader: bool = True
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialAttachRequest":
        """Create SerialAttachRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            port=data["port"],
            baud_rate=data.get("baud_rate", 115200),
            as_reader=data.get("as_reader", True),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class SerialDetachRequest:
    """Client → Daemon: Request to detach from a serial session.

    Attributes:
        client_id: Unique identifier for the client
        port: Serial port to detach from
        close_port: Whether to close the port if this is the last reader
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    port: str
    close_port: bool = False
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialDetachRequest":
        """Create SerialDetachRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            port=data["port"],
            close_port=data.get("close_port", False),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class SerialWriteRequest:
    """Client → Daemon: Request to write data to a serial port.

    The client must have acquired writer access first.

    Attributes:
        client_id: Unique identifier for the client
        port: Serial port to write to
        data: Base64-encoded data to write
        acquire_writer: Whether to automatically acquire writer access if not held
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    port: str
    data: str  # Base64-encoded bytes
    acquire_writer: bool = True
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialWriteRequest":
        """Create SerialWriteRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            port=data["port"],
            data=data["data"],
            acquire_writer=data.get("acquire_writer", True),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class SerialBufferRequest:
    """Client → Daemon: Request to read buffered serial output.

    Attributes:
        client_id: Unique identifier for the client
        port: Serial port to read from
        max_lines: Maximum number of lines to return
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    port: str
    max_lines: int = 100
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialBufferRequest":
        """Create SerialBufferRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            port=data["port"],
            max_lines=data.get("max_lines", 100),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class SerialSessionResponse:
    """Daemon → Client: Response to serial session operations.

    Attributes:
        success: Whether the operation succeeded
        message: Human-readable status message
        is_open: Whether the port is currently open
        reader_count: Number of clients attached as readers
        has_writer: Whether a client has write access
        buffer_size: Number of lines in the output buffer
        lines: Output lines (for buffer requests)
        bytes_written: Number of bytes written (for write requests)
        timestamp: Unix timestamp of the response
    """

    success: bool
    message: str
    is_open: bool = False
    reader_count: int = 0
    has_writer: bool = False
    buffer_size: int = 0
    lines: list[str] = field(default_factory=list)
    bytes_written: int = 0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerialSessionResponse":
        """Create SerialSessionResponse from dictionary."""
        return cls(
            success=data["success"],
            message=data["message"],
            is_open=data.get("is_open", False),
            reader_count=data.get("reader_count", 0),
            has_writer=data.get("has_writer", False),
            buffer_size=data.get("buffer_size", 0),
            lines=data.get("lines", []),
            bytes_written=data.get("bytes_written", 0),
            timestamp=data.get("timestamp", time.time()),
        )


# =============================================================================
# Client Connection Messages (Iteration 2)
# =============================================================================


@dataclass
class ClientConnectRequest:
    """Client → Daemon: Register a new client connection.

    Attributes:
        client_id: Unique identifier for the client (UUID)
        pid: Process ID of the client
        hostname: Hostname of the client machine
        version: Version of the client software
        timestamp: Unix timestamp when request was created
    """

    client_id: str
    pid: int
    hostname: str = ""
    version: str = ""
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ClientConnectRequest":
        """Create ClientConnectRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            pid=data["pid"],
            hostname=data.get("hostname", ""),
            version=data.get("version", ""),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class ClientHeartbeatRequest:
    """Client → Daemon: Periodic heartbeat to indicate client is alive.

    Attributes:
        client_id: Unique identifier for the client
        timestamp: Unix timestamp when heartbeat was sent
    """

    client_id: str
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ClientHeartbeatRequest":
        """Create ClientHeartbeatRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class ClientDisconnectRequest:
    """Client → Daemon: Graceful disconnect notification.

    Attributes:
        client_id: Unique identifier for the client
        reason: Optional reason for disconnection
        timestamp: Unix timestamp when disconnect was initiated
    """

    client_id: str
    reason: str = ""
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ClientDisconnectRequest":
        """Create ClientDisconnectRequest from dictionary."""
        return cls(
            client_id=data["client_id"],
            reason=data.get("reason", ""),
            timestamp=data.get("timestamp", time.time()),
        )


@dataclass
class ClientResponse:
    """Daemon → Client: Response to client connection operations.

    Attributes:
        success: Whether the operation succeeded
        message: Human-readable status message
        client_id: Client ID (may be assigned by daemon)
        is_registered: Whether the client is currently registered
        total_clients: Total number of connected clients
        timestamp: Unix timestamp of the response
    """

    success: bool
    message: str
    client_id: str = ""
    is_registered: bool = False
    total_clients: int = 0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ClientResponse":
        """Create ClientResponse from dictionary."""
        return cls(
            success=data["success"],
            message=data["message"],
            client_id=data.get("client_id", ""),
            is_registered=data.get("is_registered", False),
            total_clients=data.get("total_clients", 0),
            timestamp=data.get("timestamp", time.time()),
        )
