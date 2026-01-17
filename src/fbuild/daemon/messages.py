"""
Typed message protocol for fbuild daemon operations.

This module defines typed dataclasses for all client-daemon communication,
ensuring type safety and validation.

Supports:
- Build operations (compilation and linking)
- Deploy operations (firmware upload)
- Monitor operations (serial monitoring)
- Status updates and progress tracking
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
        )

    def is_stale(self, timeout_seconds: float = 30.0) -> bool:
        """Check if status hasn't been updated recently."""
        return (time.time() - self.updated_at) > timeout_seconds

    def get_age_seconds(self) -> float:
        """Get age of this status update in seconds."""
        return time.time() - self.updated_at
