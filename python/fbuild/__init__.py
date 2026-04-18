"""fbuild — PlatformIO-compatible embedded build tool (Rust implementation).

Drop-in replacement for the Python fbuild package. All classes are
implemented in Rust via PyO3 and re-exported here for API compatibility.

Usage::

    from fbuild import Daemon, DaemonConnection, connect_daemon, __version__
"""

from fbuild._native import (  # noqa: F401
    AsyncDaemon,
    AsyncDaemonConnection,
    Daemon,
    DaemonConnection,
    __version__,
    connect_daemon,
    connect_daemon_async,
)

__all__ = [
    "__version__",
    "AsyncDaemon",
    "AsyncDaemonConnection",
    "Daemon",
    "DaemonConnection",
    "connect_daemon",
    "connect_daemon_async",
]
