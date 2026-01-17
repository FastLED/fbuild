"""
Pytest configuration for fbuild test suite.

This configuration enables the --full flag to run integration tests.
"""

import pytest


def pytest_addoption(parser):
    """Add custom command-line options."""
    parser.addoption(
        "--full",
        action="store_true",
        default=False,
        help="Run full test suite including integration tests (slow)",
    )


def pytest_configure(config):
    """Configure pytest based on command-line options."""
    if config.getoption("--full"):
        # Remove the default marker expression that excludes integration/concurrent_safety tests
        markexpr = config.getoption("-m", "")
        if markexpr in ("not integration", "not integration and not concurrent_safety"):
            # Clear the marker expression to run all tests
            config.option.markexpr = ""
