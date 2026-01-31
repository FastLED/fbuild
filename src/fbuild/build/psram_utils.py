"""PSRAM utilities for ESP32 boards.

This module provides utilities for detecting and handling PSRAM configuration
on ESP32 boards. It's separated from orchestrator_esp32.py to avoid circular
imports with configurable_linker.py.

Key insight for ESP32-S3 SDK variants:

  OPI variants (qio_opi, dio_opi):
    - CONFIG_SPIRAM_BOOT_INIT=1 (tries to init PSRAM at boot)
    - CONFIG_SPIRAM_IGNORE_NOTFOUND=1 (supposedly ignores missing PSRAM)
    - In practice, OPI initialization on boards WITHOUT PSRAM can still crash
      due to GPIO conflicts with the OPI PSRAM interface

  QSPI variants (qio_qspi, dio_qspi):
    - NO CONFIG_SPIRAM_BOOT_INIT (does NOT try to init PSRAM at boot)
    - PSRAM is only initialized if explicitly requested by application
    - Safer for boards WITHOUT PSRAM

For boards WITHOUT PSRAM, we use "qspi" variants because they do NOT attempt
to initialize PSRAM hardware at boot, avoiding GPIO conflicts and crashes.
"""

import logging
from typing import List

logger = logging.getLogger(__name__)

# ESP32 boards without PSRAM hardware
# These boards MUST use SDK variants that do NOT try to init PSRAM at boot
# (which is the "qspi" variants: dio_qspi, qio_qspi)
NO_PSRAM_BOARDS: List[str] = [
    "seeed_xiao_esp32s3",            # Seeed XIAO ESP32-S3 (no PSRAM variant)
    "adafruit_qtpy_esp32s3_nopsram", # Adafruit QT Py ESP32-S3 No PSRAM
    "adafruit_feather_esp32s3_nopsram",  # Adafruit Feather ESP32-S3 No PSRAM
    # Add other no-PSRAM ESP32-S3 boards here as needed
]


def board_has_psram(board_id: str) -> bool:
    """
    Detect if a board has PSRAM (external RAM) available.

    ESP32 boards come in variants with and without PSRAM. Some boards like
    the Seeed XIAO ESP32-S3 have no PSRAM and require different SDK
    configuration to prevent crashes.

    Args:
        board_id: Board identifier (e.g., "seeed_xiao_esp32s3")

    Returns:
        True if board has PSRAM, False otherwise

    Example:
        >>> board_has_psram("seeed_xiao_esp32s3")
        False
        >>> board_has_psram("esp32dev")
        True
    """
    # Normalize board ID to lowercase for case-insensitive comparison
    board_id_lower = board_id.lower()

    # Check if board is in the no-PSRAM list
    return board_id_lower not in NO_PSRAM_BOARDS


def get_psram_mode(board_id: str, board_config: dict) -> str:
    """
    Get the PSRAM mode for a board, with override for boards without PSRAM.

    The memory_type field in board JSON (e.g., "qio_opi") encodes both flash
    mode and PSRAM mode as "flash_psram".

    IMPORTANT: For boards WITHOUT PSRAM, we return "qspi" because the QSPI SDK
    variants (dio_qspi, qio_qspi) do NOT have CONFIG_SPIRAM_BOOT_INIT, meaning
    they do NOT try to initialize PSRAM at boot. This avoids GPIO conflicts.

    The OPI variants (dio_opi, qio_opi) have CONFIG_SPIRAM_BOOT_INIT=1 which
    attempts OPI PSRAM initialization at boot, causing crashes on boards
    without proper PSRAM hardware.

    Args:
        board_id: Board identifier (e.g., "seeed_xiao_esp32s3")
        board_config: Board configuration dict from JSON

    Returns:
        PSRAM mode string ("qspi", "opi", etc.)

    Example:
        >>> get_psram_mode("seeed_xiao_esp32s3", {"build": {"arduino": {"memory_type": "qio_opi"}}})
        "qspi"  # Overridden to use QSPI SDK (no boot PSRAM init)
        >>> get_psram_mode("esp32dev", {"build": {"arduino": {"memory_type": "qio_opi"}}})
        "opi"  # Board has PSRAM, use actual memory_type
    """
    # If board has no PSRAM, use QSPI SDK variant (does NOT try to init PSRAM at boot)
    # OPI variants have CONFIG_SPIRAM_BOOT_INIT=1 which can cause GPIO conflicts and crashes
    # on boards without PSRAM hardware, even with CONFIG_SPIRAM_IGNORE_NOTFOUND=1
    if not board_has_psram(board_id):
        logger.info(f"PSRAM_MODE: Board {board_id} in NO_PSRAM_BOARDS, using 'qspi' (no boot PSRAM init)")
        return "qspi"

    # Extract PSRAM mode from memory_type (format: "flash_psram", e.g., "qio_opi")
    arduino_config = board_config.get("build", {}).get("arduino", {})
    memory_type = arduino_config.get("memory_type", "qio_qspi")

    # Split memory_type into flash and psram parts
    if "_" in memory_type:
        _, psram_mode = memory_type.split("_", 1)
        logger.debug(f"PSRAM_MODE: Board {board_id} has PSRAM, using {psram_mode} from memory_type={memory_type}")
        return psram_mode
    else:
        # Fallback if memory_type format is unexpected
        logger.debug(f"PSRAM_MODE: Board {board_id} has unexpected memory_type={memory_type}, using qspi")
        return "qspi"
