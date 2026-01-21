"""Pytest configuration and fixtures for fbuild tests.

This conftest addresses Python 3.13 compatibility issues with pytest's capture fixtures.
Python 3.13 changed how stdout/stderr are handled, causing "I/O operation on closed file"
errors during test teardown. This is a known issue: https://github.com/pytest-dev/pytest/issues/11439
"""

import sys
import pytest
import warnings

# Suppress ResourceWarnings from file cleanup in Python 3.13
if sys.version_info >= (3, 13):
    warnings.filterwarnings("ignore", category=ResourceWarning)


@pytest.fixture(autouse=True)
def _restore_stdio():  # noqa: PT004
    """Ensure stdout/stderr are always restored after each test.

    This prevents "I/O operation on closed file" errors in Python 3.13
    when tests raise exceptions that close stdout/stderr.
    """
    yield

    # Restore if they were closed during the test
    if sys.stdout.closed:
        sys.stdout = sys.__stdout__
    if sys.stderr.closed:
        sys.stderr = sys.__stderr__


@pytest.hookimpl(hookwrapper=True, tryfirst=True)
def pytest_runtest_call(item):  # noqa: ARG001
    """Wrap test execution to handle stdout/stderr closure gracefully."""
    yield

    # After test execution, ensure streams aren't closed
    if hasattr(sys.stdout, "closed") and sys.stdout.closed:
        sys.stdout = sys.__stdout__
    if hasattr(sys.stderr, "closed") and sys.stderr.closed:
        sys.stderr = sys.__stderr__


@pytest.hookimpl(hookwrapper=True, trylast=True)
def pytest_runtest_teardown(item):  # noqa: ARG001
    """Ensure streams are restored during teardown phase."""
    yield

    # Final restoration after teardown
    if hasattr(sys.stdout, "closed") and sys.stdout.closed:
        sys.stdout = sys.__stdout__
    if hasattr(sys.stderr, "closed") and sys.stderr.closed:
        sys.stderr = sys.__stderr__
