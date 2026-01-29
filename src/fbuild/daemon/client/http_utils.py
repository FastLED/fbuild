"""
HTTP Client Utilities for Daemon Communication

This module provides helper functions for HTTP-based communication with the
fbuild daemon FastAPI server, replacing the legacy file-based IPC.

Architecture:
- URL generation based on dev mode
- Port discovery from daemon directory
- HTTP client configuration (timeouts, retries)
- Request/response serialization helpers

Usage:
    >>> from fbuild.daemon.client.http_utils import get_daemon_url, http_client
    >>> url = get_daemon_url("/api/daemon/info")
    >>> response = http_client().get(url)
    >>> print(response.json())
"""

import logging
import os
from typing import Any

import httpx

from fbuild.daemon.paths import DAEMON_DIR

# HTTP client configuration
DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 8765
DEFAULT_DEV_PORT = 8766
DEFAULT_TEST_PORT = 9176  # Default port for testing when FBUILD_DAEMON_PORT is set
DEFAULT_TIMEOUT = 30.0  # seconds
DEFAULT_CONNECT_TIMEOUT = 5.0  # seconds

# Port file for discovery
PORT_FILE = DAEMON_DIR / "daemon.port"

logger = logging.getLogger(__name__)


def get_daemon_port() -> int:
    """Get the daemon port based on environment variables, port file, and dev mode.

    Priority:
    1. FBUILD_DAEMON_PORT environment variable (if set and valid)
    2. Port file (if exists and valid)
    3. Environment-based default (8766 for dev mode, 8765 for production)

    Returns:
        Port number for daemon HTTP server

    Example:
        >>> port = get_daemon_port()
        >>> print(f"Daemon running on port {port}")
        >>> # Or override with environment variable:
        >>> os.environ["FBUILD_DAEMON_PORT"] = "9176"
        >>> port = get_daemon_port()
        >>> print(f"Daemon running on port {port}")  # 9176
    """
    # Priority 1: Check FBUILD_DAEMON_PORT environment variable
    env_port = os.getenv("FBUILD_DAEMON_PORT")
    if env_port:
        try:
            port = int(env_port)
            if 1 <= port <= 65535:
                return port
            logger.warning(f"Invalid port in FBUILD_DAEMON_PORT: {env_port}")
        except ValueError:
            logger.warning(f"Invalid port format in FBUILD_DAEMON_PORT: {env_port}")

    # Priority 2: Try to read port from file (written by daemon on startup)
    if PORT_FILE.exists():
        try:
            port_str = PORT_FILE.read_text().strip()
            port = int(port_str)
            if 1 <= port <= 65535:
                return port
            logger.warning(f"Invalid port in {PORT_FILE}: {port_str}")
        except (ValueError, OSError) as e:
            logger.warning(f"Failed to read port file {PORT_FILE}: {e}")

    # Priority 3: Fall back to default based on dev mode
    if os.getenv("FBUILD_DEV_MODE") == "1":
        return DEFAULT_DEV_PORT
    return DEFAULT_PORT


def get_daemon_base_url() -> str:
    """Get the base URL for the daemon HTTP server.

    Returns:
        Base URL (e.g., "http://127.0.0.1:8765")

    Example:
        >>> base_url = get_daemon_base_url()
        >>> print(base_url)
        http://127.0.0.1:8765
    """
    port = get_daemon_port()
    return f"http://{DEFAULT_HOST}:{port}"


def get_daemon_url(path: str) -> str:
    """Get a full URL for a daemon endpoint.

    Args:
        path: Endpoint path (e.g., "/api/build", "/health")
              Path should start with "/" or be empty

    Returns:
        Full URL (e.g., "http://127.0.0.1:8765/api/build")

    Example:
        >>> url = get_daemon_url("/api/build")
        >>> print(url)
        http://127.0.0.1:8765/api/build
    """
    base_url = get_daemon_base_url()
    if not path:
        return base_url
    if not path.startswith("/"):
        path = "/" + path
    return f"{base_url}{path}"


def http_client(
    timeout: float = DEFAULT_TIMEOUT,
    connect_timeout: float = DEFAULT_CONNECT_TIMEOUT,
) -> httpx.Client:
    """Create a configured HTTP client for daemon communication.

    Args:
        timeout: Total request timeout in seconds (default: 30s)
        connect_timeout: Connection timeout in seconds (default: 5s)

    Returns:
        Configured httpx.Client instance

    Example:
        >>> with http_client() as client:
        >>>     response = client.get(get_daemon_url("/health"))
        >>>     print(response.json())
    """
    return httpx.Client(
        timeout=httpx.Timeout(timeout, connect=connect_timeout),
        follow_redirects=True,
    )


def is_daemon_http_available() -> bool:
    """Check if the daemon HTTP server is available.

    This function performs a quick health check to determine if the daemon
    is running and responding to HTTP requests.

    Returns:
        True if daemon is available, False otherwise

    Example:
        >>> if is_daemon_http_available():
        >>>     print("Daemon is ready")
        >>> else:
        >>>     print("Daemon not running")
    """
    try:
        with http_client(timeout=2.0, connect_timeout=1.0) as client:
            response = client.get(get_daemon_url("/health"))
            return response.status_code == 200
    except (httpx.ConnectError, httpx.TimeoutException, httpx.HTTPError):
        return False


def wait_for_daemon_http(timeout: float = 10.0, poll_interval: float = 0.5) -> bool:
    """Wait for the daemon HTTP server to become available.

    This function polls the daemon health endpoint until it responds or
    the timeout is reached.

    Args:
        timeout: Maximum wait time in seconds (default: 10s)
        poll_interval: Polling interval in seconds (default: 0.5s)

    Returns:
        True if daemon became available, False if timeout reached

    Example:
        >>> if wait_for_daemon_http(timeout=5.0):
        >>>     print("Daemon is ready")
        >>> else:
        >>>     raise RuntimeError("Daemon failed to start")
    """
    import time

    start_time = time.time()
    while time.time() - start_time < timeout:
        if is_daemon_http_available():
            return True
        time.sleep(poll_interval)
    return False


def serialize_request(request: Any) -> dict[str, Any]:
    """Serialize a request object to JSON-compatible dict.

    Args:
        request: Request object implementing to_dict() method
                 (e.g., BuildRequest, DeployRequest)

    Returns:
        Dictionary suitable for JSON serialization

    Example:
        >>> from fbuild.daemon.messages import BuildRequest
        >>> request = BuildRequest(project_dir="/path/to/project", ...)
        >>> data = serialize_request(request)
        >>> # Send as JSON in HTTP request body
    """
    if hasattr(request, "to_dict"):
        return request.to_dict()
    raise TypeError(f"Request object must implement to_dict() method: {type(request)}")


def deserialize_response(data: dict[str, Any], response_class: type) -> Any:
    """Deserialize a JSON response to a response object.

    Args:
        data: JSON response data
        response_class: Response class implementing from_dict() method
                        (e.g., BuildResponse, DaemonStatus)

    Returns:
        Deserialized response object

    Example:
        >>> from fbuild.daemon.messages import BuildResponse
        >>> data = response.json()
        >>> build_response = deserialize_response(data, BuildResponse)
    """
    if hasattr(response_class, "from_dict"):
        return response_class.from_dict(data)
    raise TypeError(f"Response class must implement from_dict() method: {response_class}")


def write_port_file(port: int) -> None:
    """Write the daemon port to the port file.

    This function is called by the daemon on startup to record the port
    it's listening on, enabling clients to discover the port dynamically.

    Args:
        port: Port number the daemon is listening on

    Example:
        >>> # In daemon startup code
        >>> write_port_file(8765)
    """
    DAEMON_DIR.mkdir(parents=True, exist_ok=True)
    PORT_FILE.write_text(str(port))
    logger.info(f"Wrote daemon port {port} to {PORT_FILE}")
