"""
Interruptible HTTP Client - Wrapper for httpx that handles KeyboardInterrupt properly.

This module provides a wrapper around httpx.Client that makes synchronous HTTP
requests interruptible by CTRL-C on Windows. The standard httpx blocking calls
can get stuck in Windows socket I/O and ignore KeyboardInterrupt signals.

Solution: Use threading with a timeout poll loop that checks for KeyboardInterrupt
between short timeout chunks, allowing the user to cancel stuck requests.

Usage:
    >>> from fbuild.daemon.client.interruptible_http import interruptible_post
    >>> response = interruptible_post(
    ...     url="http://127.0.0.1:8765/api/build",
    ...     json={"project_dir": "/path"},
    ...     timeout=1800.0
    ... )
"""

import logging
import queue
import threading
import time
from typing import Any

import httpx

logger = logging.getLogger(__name__)

# Poll interval for keyboard interrupt checking (seconds)
INTERRUPT_CHECK_INTERVAL = 0.5


class InterruptibleHTTPError(Exception):
    """Exception raised when HTTP request is interrupted or fails."""

    pass


def interruptible_post(
    url: str,
    json: dict[str, Any] | None = None,
    timeout: float = 30.0,
    connect_timeout: float = 5.0,
    check_interval: float = INTERRUPT_CHECK_INTERVAL,
) -> httpx.Response:
    """Make an interruptible HTTP POST request.

    This function wraps httpx.Client.post() to make it properly interruptible
    by KeyboardInterrupt on Windows. It runs the HTTP request in a background
    thread and polls for completion, checking for KeyboardInterrupt between
    polls.

    Args:
        url: URL to POST to
        json: JSON data to send in request body
        timeout: Total request timeout in seconds
        connect_timeout: Connection timeout in seconds
        check_interval: How often to check for KeyboardInterrupt (seconds)

    Returns:
        httpx.Response if successful

    Raises:
        KeyboardInterrupt: If user presses CTRL-C during request
        InterruptibleHTTPError: If request fails or times out

    Example:
        >>> try:
        ...     response = interruptible_post(
        ...         "http://127.0.0.1:8765/api/build",
        ...         json={"project_dir": "/path"},
        ...         timeout=1800.0
        ...     )
        ...     print(response.json())
        ... except KeyboardInterrupt:
        ...     print("Request cancelled by user")
    """
    result_queue: queue.Queue[httpx.Response | Exception] = queue.Queue()

    def http_worker() -> None:
        """Background thread that performs the HTTP request."""
        try:
            with httpx.Client(
                timeout=httpx.Timeout(timeout, connect=connect_timeout),
                follow_redirects=True,
            ) as client:
                response = client.post(url, json=json)
                result_queue.put(response)
        except KeyboardInterrupt:
            # Re-raise to allow proper cleanup (though unlikely in background thread)
            raise
        except Exception as e:
            result_queue.put(e)

    # Start background thread
    worker_thread = threading.Thread(target=http_worker, name="HTTPWorker", daemon=True)
    worker_thread.start()

    # Poll for completion, checking for KeyboardInterrupt
    start_time = time.time()
    while True:
        try:
            # Try to get result with a short timeout to allow interrupt checking
            try:
                result = result_queue.get(timeout=check_interval)

                # Check if result is an exception
                if isinstance(result, Exception):
                    raise InterruptibleHTTPError(f"HTTP request failed: {result}") from result

                return result

            except queue.Empty:
                # No result yet, check if timeout exceeded
                elapsed = time.time() - start_time
                if elapsed > timeout + connect_timeout + 10.0:  # Add buffer
                    raise InterruptibleHTTPError(f"HTTP request timed out after {elapsed:.1f}s (timeout={timeout}s)")

                # Check if thread is still alive
                if not worker_thread.is_alive():
                    # Thread died without putting result - check queue one more time
                    try:
                        result = result_queue.get_nowait()
                        if isinstance(result, Exception):
                            raise InterruptibleHTTPError(f"HTTP request failed: {result}") from result
                        return result
                    except queue.Empty:
                        raise InterruptibleHTTPError("HTTP worker thread died unexpectedly")

                # Continue polling
                continue

        except KeyboardInterrupt:
            # User pressed CTRL-C - raise immediately
            logger.info("HTTP request interrupted by user (CTRL-C)")
            raise


def interruptible_get(
    url: str,
    timeout: float = 30.0,
    connect_timeout: float = 5.0,
    check_interval: float = INTERRUPT_CHECK_INTERVAL,
) -> httpx.Response:
    """Make an interruptible HTTP GET request.

    This function wraps httpx.Client.get() to make it properly interruptible
    by KeyboardInterrupt on Windows.

    Args:
        url: URL to GET
        timeout: Total request timeout in seconds
        connect_timeout: Connection timeout in seconds
        check_interval: How often to check for KeyboardInterrupt (seconds)

    Returns:
        httpx.Response if successful

    Raises:
        KeyboardInterrupt: If user presses CTRL-C during request
        InterruptibleHTTPError: If request fails or times out

    Example:
        >>> response = interruptible_get("http://127.0.0.1:8765/health")
        >>> print(response.json())
    """
    result_queue: queue.Queue[httpx.Response | Exception] = queue.Queue()

    def http_worker() -> None:
        """Background thread that performs the HTTP request."""
        try:
            with httpx.Client(
                timeout=httpx.Timeout(timeout, connect=connect_timeout),
                follow_redirects=True,
            ) as client:
                response = client.get(url)
                result_queue.put(response)
        except KeyboardInterrupt:
            # Re-raise to allow proper cleanup (though unlikely in background thread)
            raise
        except Exception as e:
            result_queue.put(e)

    # Start background thread
    worker_thread = threading.Thread(target=http_worker, name="HTTPWorker", daemon=True)
    worker_thread.start()

    # Poll for completion, checking for KeyboardInterrupt
    start_time = time.time()
    while True:
        try:
            # Try to get result with a short timeout to allow interrupt checking
            try:
                result = result_queue.get(timeout=check_interval)

                # Check if result is an exception
                if isinstance(result, Exception):
                    raise InterruptibleHTTPError(f"HTTP request failed: {result}") from result

                return result

            except queue.Empty:
                # No result yet, check if timeout exceeded
                elapsed = time.time() - start_time
                if elapsed > timeout + connect_timeout + 10.0:  # Add buffer
                    raise InterruptibleHTTPError(f"HTTP request timed out after {elapsed:.1f}s (timeout={timeout}s)")

                # Check if thread is still alive
                if not worker_thread.is_alive():
                    # Thread died without putting result - check queue one more time
                    try:
                        result = result_queue.get_nowait()
                        if isinstance(result, Exception):
                            raise InterruptibleHTTPError(f"HTTP request failed: {result}") from result
                        return result
                    except queue.Empty:
                        raise InterruptibleHTTPError("HTTP worker thread died unexpectedly")

                # Continue polling
                continue

        except KeyboardInterrupt:
            # User pressed CTRL-C - raise immediately
            logger.info("HTTP request interrupted by user (CTRL-C)")
            raise
