"""fbuild — PlatformIO-compatible embedded build tool (Rust implementation).

Drop-in replacement for the Python fbuild package. All classes are
implemented in Rust via PyO3 and re-exported here for API compatibility.

Usage::

    from fbuild import Daemon, DaemonConnection, connect_daemon, __version__
"""

from fbuild._native import (  # noqa: F401
    Daemon,
    DaemonConnection,
    __version__,
    connect_daemon,
)

__all__ = [
    "__version__",
    "Daemon",
    "DaemonConnection",
    "connect_daemon",
]
