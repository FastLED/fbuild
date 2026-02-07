"""Standalone demo script for the parallel package pipeline TUI.

Simulates a parallel installation with multiple packages transitioning
through download -> unpack -> install -> done phases to demonstrate
the Docker pull-style progress display.

Usage:
    python scripts/demo_parallel_install.py
"""

import sys
import time
import threading
from pathlib import Path

# Add src to path for direct script execution
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))

from fbuild.packages.pipeline.models import TaskPhase
from fbuild.packages.pipeline.progress_display import PipelineProgressDisplay


def _simulate_task(
    display: PipelineProgressDisplay,
    name: str,
    download_size: int,
    download_speed: float,
    unpack_files: int,
    install_steps: int,
    start_delay: float,
) -> None:
    """Simulate a package going through all pipeline phases.

    Args:
        display: Progress display instance.
        name: Task name.
        download_size: Simulated download size in bytes.
        download_speed: Simulated download speed in bytes/sec.
        unpack_files: Number of files to simulate extracting.
        install_steps: Number of install verification steps.
        start_delay: Delay before starting (simulates dependency wait).
    """
    time.sleep(start_delay)

    # Download phase
    downloaded = 0
    chunk_size = max(1, download_size // 20)
    while downloaded < download_size:
        downloaded = min(downloaded + chunk_size, download_size)
        speed_str = f"{download_speed / (1024 * 1024):.1f} MB/s"
        display.on_progress(name, TaskPhase.DOWNLOADING, downloaded, download_size, speed_str)
        time.sleep(0.15)

    # Unpack phase
    for i in range(unpack_files):
        display.on_progress(name, TaskPhase.UNPACKING, i + 1, unpack_files, f"Extracting files ({i + 1}/{unpack_files})")
        time.sleep(0.08)

    # Install phase
    install_messages = [
        "Verifying package contents...",
        "Checking binary compatibility...",
        "Generating fingerprint...",
        "Registering package...",
    ]
    for i in range(install_steps):
        msg = install_messages[i % len(install_messages)]
        display.on_progress(name, TaskPhase.INSTALLING, i + 1, install_steps, msg)
        time.sleep(0.3)

    # Done
    display.on_progress(name, TaskPhase.DONE, 1, 1, "Complete")


def main() -> None:
    """Run the TUI demo with simulated packages."""
    # Define simulated packages (AVR-like task graph)
    packages = [
        {"name": "platform-atmelavr", "version": "5.0.0", "download_size": 8_000_000, "download_speed": 3_200_000, "unpack_files": 15, "install_steps": 3, "start_delay": 0.0},
        {"name": "toolchain-atmelavr", "version": "3.1.0", "download_size": 45_000_000, "download_speed": 5_000_000, "unpack_files": 40, "install_steps": 4, "start_delay": 0.0},
        {"name": "framework-arduino-avr", "version": "4.2.0", "download_size": 12_000_000, "download_speed": 4_000_000, "unpack_files": 25, "install_steps": 3, "start_delay": 0.0},
        {"name": "Wire", "version": "1.0", "download_size": 500_000, "download_speed": 2_000_000, "unpack_files": 5, "install_steps": 2, "start_delay": 2.0},
        {"name": "SPI", "version": "1.0", "download_size": 400_000, "download_speed": 2_000_000, "unpack_files": 4, "install_steps": 2, "start_delay": 2.0},
        {"name": "Servo", "version": "1.1.8", "download_size": 600_000, "download_speed": 2_500_000, "unpack_files": 6, "install_steps": 2, "start_delay": 3.5},
    ]

    display = PipelineProgressDisplay(
        console=None,
        env_name="uno",
        refresh_per_second=10,
    )

    # Register all tasks
    for pkg in packages:
        display.register_task(pkg["name"], pkg["version"])

    # Run simulation with live display
    with display:
        threads = []
        for pkg in packages:
            t = threading.Thread(
                target=_simulate_task,
                args=(
                    display,
                    pkg["name"],
                    pkg["download_size"],
                    pkg["download_speed"],
                    pkg["unpack_files"],
                    pkg["install_steps"],
                    pkg["start_delay"],
                ),
            )
            threads.append(t)
            t.start()

        # Refresh display while threads are running
        while any(t.is_alive() for t in threads):
            display.update()
            time.sleep(0.1)

        # Final update
        display.update()

        # Wait for all threads
        for t in threads:
            t.join(timeout=30)

    print("\nDemo complete!")


if __name__ == "__main__":
    main()
