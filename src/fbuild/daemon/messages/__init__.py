"""
Typed message protocol for fbuild daemon operations.

This package defines typed dataclasses for all client-daemon communication,
ensuring type safety and validation.

Organization:
- _base: Core enumerations (DaemonState, OperationType)
- operations: Build, deploy, monitor, and dependency installation requests
- locks: Lock management (acquire, release, status)
- firmware: Firmware ledger (query, record)
- serial: Serial session management (attach, detach, read, write)
- monitor: Serial monitor API (attach, detach, poll)
- device: Device management (lease, release, preempt, list, status)
- client: Client connection management (connect, heartbeat, disconnect)
- status: Daemon status and identity
"""

# Base enumerations
from fbuild.daemon.messages._base import DaemonState, OperationType

# Client connection management
from fbuild.daemon.messages.client import (
    ClientConnectRequest,
    ClientDisconnectRequest,
    ClientHeartbeatRequest,
    ClientResponse,
)

# Device management
from fbuild.daemon.messages.device import (
    DeviceLeaseRequest,
    DeviceLeaseResponse,
    DeviceLeaseType,
    DeviceListRequest,
    DeviceListResponse,
    DevicePreemptNotification,
    DevicePreemptRequest,
    DeviceReleaseRequest,
    DeviceStatusRequest,
    DeviceStatusResponse,
)

# Firmware ledger
from fbuild.daemon.messages.firmware import (
    FirmwareQueryRequest,
    FirmwareQueryResponse,
    FirmwareRecordRequest,
)

# Lock management
from fbuild.daemon.messages.locks import (
    LockAcquireRequest,
    LockReleaseRequest,
    LockResponse,
    LockStatusRequest,
    LockType,
)

# Serial monitor API
from fbuild.daemon.messages.monitor import (
    SerialMonitorAttachRequest,
    SerialMonitorDetachRequest,
    SerialMonitorPollRequest,
    SerialMonitorResponse,
)

# Operation requests
from fbuild.daemon.messages.operations import (
    BuildRequest,
    DeployRequest,
    InstallDependenciesRequest,
    MonitorRequest,
)

# Serial session management
from fbuild.daemon.messages.serial import (
    SerialAttachRequest,
    SerialBufferRequest,
    SerialDetachRequest,
    SerialSessionResponse,
    SerialWriteRequest,
)

# Daemon status and identity
from fbuild.daemon.messages.status import (
    DaemonIdentity,
    DaemonIdentityRequest,
    DaemonIdentityResponse,
    DaemonStatus,
)

__all__ = [
    # Base enumerations
    "DaemonState",
    "OperationType",
    # Operation requests
    "BuildRequest",
    "DeployRequest",
    "MonitorRequest",
    "InstallDependenciesRequest",
    # Lock management
    "LockType",
    "LockAcquireRequest",
    "LockReleaseRequest",
    "LockStatusRequest",
    "LockResponse",
    # Firmware ledger
    "FirmwareQueryRequest",
    "FirmwareQueryResponse",
    "FirmwareRecordRequest",
    # Serial session management
    "SerialAttachRequest",
    "SerialDetachRequest",
    "SerialWriteRequest",
    "SerialBufferRequest",
    "SerialSessionResponse",
    # Serial monitor API
    "SerialMonitorAttachRequest",
    "SerialMonitorDetachRequest",
    "SerialMonitorPollRequest",
    "SerialMonitorResponse",
    # Device management
    "DeviceLeaseType",
    "DeviceLeaseRequest",
    "DeviceReleaseRequest",
    "DevicePreemptRequest",
    "DeviceListRequest",
    "DeviceStatusRequest",
    "DeviceLeaseResponse",
    "DeviceListResponse",
    "DeviceStatusResponse",
    "DevicePreemptNotification",
    # Client connection management
    "ClientConnectRequest",
    "ClientHeartbeatRequest",
    "ClientDisconnectRequest",
    "ClientResponse",
    # Daemon status and identity
    "DaemonStatus",
    "DaemonIdentity",
    "DaemonIdentityRequest",
    "DaemonIdentityResponse",
]
