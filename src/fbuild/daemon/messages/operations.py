"""
Operation request messages for build, deploy, monitor, and dependency installation.

This module defines the primary operation requests that clients send to the daemon.
"""

import time
from dataclasses import asdict, dataclass, field
from typing import Any

from fbuild.daemon.message_protocol import deserialize_dataclass, serialize_dataclass


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
        jobs: Number of parallel compilation jobs (None = CPU count)
        timestamp: Unix timestamp when request was created
        request_id: Unique identifier for this request
    """

    project_dir: str
    environment: str
    clean_build: bool
    verbose: bool
    caller_pid: int
    caller_cwd: str
    jobs: int | None = None
    timestamp: float = field(default_factory=time.time)
    request_id: str = field(default_factory=lambda: f"build_{int(time.time() * 1000)}")

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return serialize_dataclass(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "BuildRequest":
        """Create BuildRequest from dictionary."""
        return deserialize_dataclass(cls, data)


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
        skip_build: Whether to skip the build phase (upload-only mode)
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
    skip_build: bool = False
    timestamp: float = field(default_factory=time.time)
    request_id: str = field(default_factory=lambda: f"deploy_{int(time.time() * 1000)}")

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return serialize_dataclass(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "DeployRequest":
        """Create DeployRequest from dictionary."""
        return deserialize_dataclass(cls, data)


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
