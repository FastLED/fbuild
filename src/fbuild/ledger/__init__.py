"""
Board Ledger - Track attached chip/port mappings.

This module provides persistent caching of chip type detections for serial ports,
enabling faster operations by avoiding repeated esptool calls.

Example:
    >>> from fbuild.ledger import BoardLedger, detect_and_cache
    >>>
    >>> # Detect chip and cache result
    >>> result = detect_and_cache("COM3")
    >>> print(f"Chip: {result.chip_type}, Env: {result.environment}, Cached: {result.was_cached}")
    >>>
    >>> # Manual ledger operations
    >>> ledger = BoardLedger()
    >>> ledger.set_chip("COM4", "ESP32-C6")
    >>> chip = ledger.get_chip("COM4")
    >>> env = ledger.get_environment("COM4")
"""

from .board_ledger import (
    CHIP_TO_ENVIRONMENT,
    STALE_THRESHOLD_SECONDS,
    VALID_CHIP_TYPES,
    BoardLedger,
    BoardLedgerError,
    ChipDetectionError,
    DetectionResult,
    LedgerEntry,
    detect_and_cache,
    detect_chip_with_esptool,
    get_environment_for_chip,
)

__all__ = [
    # Main class
    "BoardLedger",
    # Data classes
    "LedgerEntry",
    "DetectionResult",
    # Exceptions
    "BoardLedgerError",
    "ChipDetectionError",
    # Functions
    "detect_and_cache",
    "detect_chip_with_esptool",
    "get_environment_for_chip",
    # Constants
    "CHIP_TO_ENVIRONMENT",
    "VALID_CHIP_TYPES",
    "STALE_THRESHOLD_SECONDS",
]
