"""
Docker utilities for QEMU deployment.

This module provides utilities for managing Docker containers for ESP32 QEMU emulation,
including automatic Docker daemon startup detection and image management.

Enhanced Docker auto-start functionality based on patterns from fastled-wasm:
- WSL2 backend detection on Windows
- Docker Desktop restart workflow
- Retry logic for Docker client connection
- Cross-platform support (Windows, macOS, Linux)
"""

import os
import platform
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional

from fbuild.interrupt_utils import handle_keyboard_interrupt_properly


def get_docker_env() -> dict[str, str]:
    """Get environment for Docker commands, handling Git Bash/MSYS2 path conversion."""
    env = os.environ.copy()
    # Set UTF-8 encoding environment variables for Windows
    env["PYTHONIOENCODING"] = "utf-8"
    env["PYTHONUTF8"] = "1"
    # Only set MSYS_NO_PATHCONV if we're in a Git Bash/MSYS2 environment
    if "MSYSTEM" in os.environ or os.environ.get("TERM") == "xterm" or "bash.exe" in os.environ.get("SHELL", ""):
        env["MSYS_NO_PATHCONV"] = "1"
    return env


def check_docker_daemon_running() -> bool:
    """Check if Docker daemon is running.

    Returns:
        True if Docker daemon is running, False otherwise
    """
    try:
        result = subprocess.run(
            ["docker", "info"],
            capture_output=True,
            timeout=10,
            env=get_docker_env(),
        )
        return result.returncode == 0
    except (subprocess.SubprocessError, FileNotFoundError, subprocess.TimeoutExpired):
        return False


def check_docker_installed() -> bool:
    """Check if Docker is installed.

    Returns:
        True if Docker is installed, False otherwise
    """
    try:
        result = subprocess.run(
            ["docker", "--version"],
            capture_output=True,
            timeout=5,
            env=get_docker_env(),
        )
        return result.returncode == 0
    except (subprocess.SubprocessError, FileNotFoundError, subprocess.TimeoutExpired):
        return False


def get_docker_desktop_path() -> Optional[str]:
    """Get the path to Docker Desktop executable.

    Returns:
        Path to Docker Desktop or None if not found
    """
    system = platform.system()

    if system == "Windows":
        # Common locations for Docker Desktop on Windows
        home_dir = Path.home()
        paths = [
            r"C:\Program Files\Docker\Docker\Docker Desktop.exe",
            r"C:\Program Files (x86)\Docker\Docker\Docker Desktop.exe",
            os.path.expandvars(r"%ProgramFiles%\Docker\Docker\Docker Desktop.exe"),
            os.path.expandvars(r"%LocalAppData%\Programs\Docker\Docker\Docker Desktop.exe"),
            str(home_dir / "AppData" / "Local" / "Docker" / "Docker Desktop.exe"),
            str(home_dir / "AppData" / "Local" / "Programs" / "Docker" / "Docker" / "Docker Desktop.exe"),
        ]
        for path in paths:
            if os.path.exists(path):
                return path

    elif system == "Darwin":  # macOS
        paths = [
            "/Applications/Docker.app/Contents/MacOS/Docker Desktop",
            "/Applications/Docker.app",
        ]
        for path in paths:
            if os.path.exists(path):
                return path

    elif system == "Linux":
        # On Linux, Docker usually runs as a service, not a desktop app
        return None

    return None


def _check_wsl2_docker_backend() -> tuple[bool, str]:
    """Check if Docker's WSL2 backend (docker-desktop) is running on Windows.

    Returns:
        tuple[bool, str]: (is_running, status_message)
    """
    if platform.system() != "Windows":
        return True, "Not Windows - WSL2 check not applicable"

    try:
        result = subprocess.run(
            ["wsl", "--list", "--verbose"],
            capture_output=True,
            text=True,
            timeout=10,
            env=get_docker_env(),
        )

        if result.returncode != 0:
            return False, "WSL2 not installed (wsl command failed)"

        output = result.stdout

        # Handle WSL output encoding issues (Git Bash adds spaces between characters)
        cleaned_lines = []
        for line in output.split("\n"):
            cleaned = line.replace("\x00", "").strip()
            # Remove spaces between single characters: "d o c k e r" -> "docker"
            if " " in cleaned and len([c for c in cleaned.split() if len(c) == 1]) > 3:
                cleaned = cleaned.replace(" ", "")
            cleaned_lines.append(cleaned)

        # Detect status: running/stopped
        for line in cleaned_lines:
            if "docker-desktop" in line.lower():
                if "running" in line.lower():
                    return True, "docker-desktop WSL2 backend is running"
                elif "stopped" in line.lower():
                    return False, "docker-desktop WSL2 backend is stopped"

        return False, "docker-desktop WSL2 distribution not found"

    except FileNotFoundError:
        return False, "WSL2 not installed (wsl command not found)"
    except subprocess.TimeoutExpired:
        return False, "WSL2 command timed out"
    except KeyboardInterrupt as ke:
        handle_keyboard_interrupt_properly(ke)
        return False, "Interrupted by user"  # Will not reach here
    except Exception as e:
        return False, f"Error checking WSL2 backend: {e}"


def _kill_docker_desktop_windows() -> bool:
    """Forcefully terminate Docker Desktop and backend processes on Windows.

    Returns:
        bool: True if processes were killed successfully
    """
    if platform.system() != "Windows":
        return False

    try:
        # Kill Docker Desktop GUI
        subprocess.run(
            ["taskkill", "/F", "/IM", "Docker Desktop.exe"],
            capture_output=True,
            timeout=10,
        )

        # Kill Docker backend engine
        subprocess.run(
            ["taskkill", "/F", "/IM", "com.docker.backend.exe"],
            capture_output=True,
            timeout=10,
        )

        time.sleep(3)  # Allow cleanup
        return True
    except KeyboardInterrupt as ke:
        handle_keyboard_interrupt_properly(ke)
        return False  # Will not reach here
    except Exception:
        return False


def _restart_docker_desktop_windows() -> tuple[bool, str]:
    """Complete Docker Desktop restart workflow on Windows.

    Returns:
        tuple[bool, str]: (success, message)
    """
    print("  Attempting to restart Docker Desktop to fix WSL2 backend...")

    docker_path = get_docker_desktop_path()
    if not docker_path:
        return False, "Docker Desktop executable not found"

    print("  Stopping Docker Desktop...")
    _kill_docker_desktop_windows()

    time.sleep(5)  # Wait for cleanup

    print("  Starting Docker Desktop...")
    try:
        subprocess.Popen(
            [docker_path],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            creationflags=subprocess.DETACHED_PROCESS | subprocess.CREATE_NO_WINDOW,
        )
    except KeyboardInterrupt as ke:
        handle_keyboard_interrupt_properly(ke)
        return False, "Interrupted by user"  # Will not reach here
    except Exception as e:
        return False, f"Failed to start Docker Desktop: {e}"

    print("  Waiting for Docker Desktop and WSL2 backend to initialize...")

    # Poll for up to 2 minutes
    for attempt in range(120):
        time.sleep(1)

        # Check if Docker engine is available
        if check_docker_daemon_running():
            # Also check WSL2 backend
            wsl_running, _ = _check_wsl2_docker_backend()
            if wsl_running:
                return True, "Docker Desktop restarted successfully - WSL2 backend is running"

        # Progress indicator every 15 seconds
        if (attempt + 1) % 15 == 0:
            print(f"  Still waiting ({attempt + 1}s)...")

    return False, "Docker Desktop started but WSL2 backend failed to initialize within 2 minutes"


def start_docker_daemon() -> bool:
    """Attempt to start the Docker daemon.

    This function tries to start Docker Desktop on Windows/macOS
    or the Docker service on Linux. On Windows, it includes WSL2
    backend detection and restart workflow.

    Returns:
        True if Docker daemon started successfully, False otherwise
    """
    system = platform.system()

    if check_docker_daemon_running():
        return True

    print("Docker daemon is not running. Attempting to start...")

    if system == "Windows":
        # Check WSL2 backend status on Windows
        print("  Checking Docker Desktop WSL2 backend status...")
        wsl_running, wsl_message = _check_wsl2_docker_backend()
        print(f"  WSL2 Status: {wsl_message}")

        # Detect split-brain state and trigger restart
        if not wsl_running and "stopped" in wsl_message.lower():
            print("  Issue detected: Docker Desktop app may be running but WSL2 backend is stopped")
            success, message = _restart_docker_desktop_windows()
            if success:
                print(f"  {message}")
                return True
            else:
                print(f"  {message}")
                return False

        docker_path = get_docker_desktop_path()
        if docker_path:
            try:
                # Start Docker Desktop without waiting
                subprocess.Popen(
                    [docker_path],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    creationflags=subprocess.DETACHED_PROCESS | subprocess.CREATE_NO_WINDOW,
                )
                success = _wait_for_docker_daemon(timeout=120)  # 2 minute timeout

                if not success:
                    # If startup failed, try full restart
                    print("  Docker Desktop didn't start properly, attempting full restart...")
                    success, message = _restart_docker_desktop_windows()
                    if success:
                        print(f"  {message}")
                        return True
                    else:
                        print(f"  {message}")
                        return False
                return success
            except KeyboardInterrupt as ke:
                handle_keyboard_interrupt_properly(ke)
                return False
            except Exception as e:
                print(f"Failed to start Docker Desktop: {e}")
                return False
        else:
            print("Docker Desktop not found. Please start Docker Desktop manually.")
            return False

    elif system == "Darwin":
        docker_path = get_docker_desktop_path()
        if docker_path:
            try:
                subprocess.Popen(
                    ["open", "-a", "Docker"],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                return _wait_for_docker_daemon()
            except KeyboardInterrupt as ke:
                handle_keyboard_interrupt_properly(ke)
                return False
            except Exception as e:
                print(f"Failed to start Docker Desktop: {e}")
                return False
        else:
            print("Docker Desktop not found. Please install Docker Desktop.")
            return False

    elif system == "Linux":
        try:
            # Try to start the Docker service using systemctl
            result = subprocess.run(
                ["sudo", "systemctl", "start", "docker"],
                capture_output=True,
                timeout=30,
            )
            if result.returncode == 0:
                return _wait_for_docker_daemon()
            else:
                # Try without sudo (if user has permissions)
                result = subprocess.run(
                    ["systemctl", "start", "docker"],
                    capture_output=True,
                    timeout=30,
                )
                if result.returncode == 0:
                    return _wait_for_docker_daemon()
        except KeyboardInterrupt as ke:
            handle_keyboard_interrupt_properly(ke)
            return False
        except Exception as e:
            print(f"Failed to start Docker service: {e}")

        print("Failed to start Docker service. Try running: sudo systemctl start docker")
        return False

    return False


def _wait_for_docker_daemon(timeout: int = 60) -> bool:
    """Wait for Docker daemon to become available.

    Args:
        timeout: Maximum time to wait in seconds

    Returns:
        True if Docker daemon is now running, False if timeout
    """
    print(f"Waiting for Docker daemon to start (timeout: {timeout}s)...")
    start_time = time.time()

    while (time.time() - start_time) < timeout:
        if check_docker_daemon_running():
            print("Docker daemon is now running!")
            return True
        time.sleep(2)
        sys.stdout.write(".")
        sys.stdout.flush()

    print("\nTimeout waiting for Docker daemon to start.")
    return False


def ensure_docker_available() -> bool:
    """Ensure Docker is available, starting daemon if necessary.

    Returns:
        True if Docker is available, False otherwise
    """
    if not check_docker_installed():
        print("Docker is not installed.")
        print()
        print("Install Docker:")
        print("  - Windows/Mac: https://www.docker.com/products/docker-desktop")
        print("  - Linux: https://docs.docker.com/engine/install/")
        return False

    if check_docker_daemon_running():
        return True

    return start_docker_daemon()


def check_docker_image_exists(image_name: str) -> bool:
    """Check if a Docker image exists locally.

    Args:
        image_name: Name of the Docker image to check

    Returns:
        True if image exists, False otherwise
    """
    try:
        result = subprocess.run(
            ["docker", "images", "-q", image_name],
            capture_output=True,
            text=True,
            timeout=10,
            env=get_docker_env(),
        )
        return bool(result.stdout.strip())
    except (subprocess.SubprocessError, FileNotFoundError, subprocess.TimeoutExpired):
        return False


def pull_docker_image(image_name: str, timeout: int = 600) -> bool:
    """Pull a Docker image.

    Args:
        image_name: Name of the Docker image to pull
        timeout: Timeout in seconds (default 10 minutes)

    Returns:
        True if image was pulled successfully, False otherwise
    """
    print(f"Pulling Docker image: {image_name}")
    print("This may take a few minutes on first run...")

    try:
        result = subprocess.run(
            ["docker", "pull", image_name],
            timeout=timeout,
            env=get_docker_env(),
        )
        if result.returncode == 0:
            print(f"Successfully pulled {image_name}")
            return True
        else:
            print(f"Failed to pull {image_name}")
            return False
    except KeyboardInterrupt as ke:
        handle_keyboard_interrupt_properly(ke)
        return False  # Will not reach here, but satisfies type checker
    except subprocess.TimeoutExpired:
        print(f"Timeout pulling {image_name}")
        return False
    except Exception as e:
        print(f"Error pulling {image_name}: {e}")
        return False


def ensure_docker_image(image_name: str, fallback_images: Optional[list[str]] = None) -> bool:
    """Ensure a Docker image is available, pulling if necessary.

    Args:
        image_name: Name of the Docker image to ensure
        fallback_images: Optional list of fallback images to try if primary fails

    Returns:
        True if image is available, False otherwise
    """
    if check_docker_image_exists(image_name):
        print(f"Image {image_name} already available locally")
        return True

    if pull_docker_image(image_name):
        return True

    # Try fallback images
    if fallback_images:
        for fallback in fallback_images:
            print(f"Trying fallback image: {fallback}")
            if check_docker_image_exists(fallback):
                return True
            if pull_docker_image(fallback):
                return True

    return False
