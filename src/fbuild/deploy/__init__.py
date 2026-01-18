"""
Firmware deployment functionality for fbuild.

This module provides deployment capabilities for uploading firmware to devices.
"""

from .deployer import DeploymentError, DeploymentResult, IDeployer
from .deployer_esp32 import ESP32Deployer
from .monitor import SerialMonitor
from .qemu_runner import QEMURunner, check_docker_available, map_board_to_machine

__all__ = [
    "IDeployer",
    "ESP32Deployer",
    "DeploymentResult",
    "DeploymentError",
    "SerialMonitor",
    "QEMURunner",
    "check_docker_available",
    "map_board_to_machine",
]
