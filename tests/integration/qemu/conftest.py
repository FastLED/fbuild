"""
Pytest configuration for QEMU integration tests.

This module provides fixtures and configuration for running QEMU tests
with Docker containers.
"""

import subprocess
from pathlib import Path

import pytest


def pytest_configure(config):
    """Configure pytest markers for QEMU tests."""
    config.addinivalue_line("markers", "qemu: mark test as requiring QEMU emulation (requires Docker)")
    config.addinivalue_line("markers", "integration: mark test as integration test")


@pytest.fixture(scope="session")
def docker_available():
    """Session-scoped fixture to check Docker availability.

    Returns True if Docker is available, otherwise skips the test.
    """
    try:
        result = subprocess.run(
            ["docker", "version"],
            capture_output=True,
            timeout=10,
        )
        if result.returncode != 0:
            pytest.skip("Docker is not available or not running")
        return True
    except (subprocess.SubprocessError, FileNotFoundError, subprocess.TimeoutExpired):
        pytest.skip("Docker is not installed")


@pytest.fixture(scope="session")
def docker_image_ready(docker_available):
    """Session-scoped fixture to ensure Docker image is pulled.

    This fixture ensures that the espressif/idf:latest image is available
    before running any QEMU tests.
    """
    image_name = "espressif/idf:latest"

    # Check if image exists
    result = subprocess.run(
        ["docker", "images", "-q", image_name],
        capture_output=True,
        text=True,
        timeout=10,
    )

    if result.stdout.strip():
        return True

    # Pull image
    print(f"Pulling Docker image {image_name}...")
    result = subprocess.run(
        ["docker", "pull", image_name],
        capture_output=True,
        timeout=600,  # 10 minute timeout
    )

    if result.returncode != 0:
        pytest.skip(f"Failed to pull Docker image {image_name}")

    return True


@pytest.fixture
def tests_dir():
    """Return the path to the tests directory."""
    return Path(__file__).parent.parent.parent


@pytest.fixture
def esp32s3_project(tests_dir):
    """Fixture providing ESP32-S3 test project path."""
    project = tests_dir / "esp32s3"
    if not project.exists():
        pytest.skip("ESP32-S3 test project not found")
    return project


@pytest.fixture
def esp32dev_project(tests_dir):
    """Fixture providing ESP32-DEV test project path."""
    project = tests_dir / "esp32dev"
    if not project.exists():
        pytest.skip("ESP32-DEV test project not found")
    return project


@pytest.fixture
def esp32c6_project(tests_dir):
    """Fixture providing ESP32-C6 test project path."""
    project = tests_dir / "esp32c6"
    if not project.exists():
        pytest.skip("ESP32-C6 test project not found")
    return project
