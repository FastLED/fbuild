"""
Firmware deployment functionality for fbuild.

This module provides deployment capabilities for uploading firmware to devices.
"""

from fbuild.deploy.deployer import DeploymentError, DeploymentResult, IDeployer
from fbuild.deploy.deployer_esp32 import ESP32Deployer
from fbuild.deploy.monitor import SerialMonitor
from fbuild.deploy.qemu_runner import QEMURunner, check_docker_available, map_board_to_machine

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
