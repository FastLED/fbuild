"""Platform configuration loader for MCU-specific compiler/linker settings.

This module provides access to JSON configuration files for various MCU platforms.
Uses importlib.resources for proper package data access when installed as a wheel.

Configs are organized by vendor:
    esp/      - Espressif (ESP32, ESP32-C3, ESP32-S3, etc.)
    teensy/   - PJRC Teensy (imxrt1062, etc.)
    rp/       - Raspberry Pi (RP2040, RP2350)
    stm32/    - STMicroelectronics (STM32F1, STM32F4, etc.)

The manifest.json file contains metadata about all available configs.
"""

from __future__ import annotations

import json
from importlib import resources
from typing import Any

# Vendor directories to search
VENDOR_DIRS = ["esp", "teensy", "rp", "stm32"]


def load_manifest() -> dict[str, Any] | None:
    """Load the platform configs manifest.

    Returns:
        The manifest dictionary if found, None otherwise.
    """
    try:
        pkg_files = resources.files(__package__)
        manifest_file = pkg_files.joinpath("manifest.json")

        if manifest_file.is_file():
            with manifest_file.open("r", encoding="utf-8") as f:
                return json.load(f)
    except (FileNotFoundError, TypeError):
        pass

    return None


def load_config(mcu: str) -> dict[str, Any] | None:
    """Load platform configuration for the specified MCU.

    Searches all vendor subdirectories for the matching config file.

    Args:
        mcu: The MCU identifier (e.g., 'esp32', 'esp32c6', 'rp2040', 'stm32f4', 'imxrt1062')

    Returns:
        The configuration dictionary if found, None otherwise.
    """
    config_name = f"{mcu}.json"

    try:
        pkg_files = resources.files(__package__)

        # Search in vendor subdirectories
        for vendor in VENDOR_DIRS:
            vendor_dir = pkg_files.joinpath(vendor)
            try:
                config_file = vendor_dir.joinpath(config_name)
                if config_file.is_file():
                    with config_file.open("r", encoding="utf-8") as f:
                        return json.load(f)
            except (FileNotFoundError, TypeError, AttributeError):
                continue

    except (FileNotFoundError, TypeError):
        pass

    return None


def list_available_configs() -> list[str]:
    """List all available platform configuration MCU names.

    Returns:
        List of MCU names that have configuration files (without .json extension).
    """
    configs = []
    try:
        pkg_files = resources.files(__package__)

        for vendor in VENDOR_DIRS:
            try:
                vendor_dir = pkg_files.joinpath(vendor)
                for f in vendor_dir.iterdir():
                    if f.name.endswith(".json") and f.is_file():
                        configs.append(f.name[:-5])  # Remove .json extension
            except (TypeError, AttributeError, FileNotFoundError):
                continue

    except (TypeError, AttributeError):
        pass

    return sorted(configs)


def list_configs_by_vendor() -> dict[str, list[str]]:
    """List available configs organized by vendor.

    Returns:
        Dictionary mapping vendor names to lists of MCU names.
    """
    result: dict[str, list[str]] = {}
    try:
        pkg_files = resources.files(__package__)

        for vendor in VENDOR_DIRS:
            try:
                vendor_dir = pkg_files.joinpath(vendor)
                mcus = []
                for f in vendor_dir.iterdir():
                    if f.name.endswith(".json") and f.is_file():
                        mcus.append(f.name[:-5])
                if mcus:
                    result[vendor] = sorted(mcus)
            except (TypeError, AttributeError, FileNotFoundError):
                continue

    except (TypeError, AttributeError):
        pass

    return result
