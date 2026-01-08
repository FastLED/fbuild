"""Board configuration loading utilities.

This module provides utilities for loading board configurations with support
for both built-in boards and platform-specific boards (ESP32, etc.).
"""

from typing import Dict

from .board_config import BoardConfig, BoardConfigError


class BoardConfigLoader:
    """Utility class for loading board configurations with platform detection."""

    @staticmethod
    def load_board_config(board_id: str, env_config: Dict) -> BoardConfig:
        """
        Load board configuration with platform-specific handling.

        This method first attempts to load the board from built-in defaults.
        If that fails and the platform is ESP32, it creates a minimal BoardConfig
        with MCU detection based on the board ID.

        Args:
            board_id: Board identifier (e.g., 'uno', 'esp32dev')
            env_config: Environment configuration from platformio.ini

        Returns:
            BoardConfig instance

        Raises:
            BoardConfigError: If board configuration cannot be loaded
        """
        # Extract overrides from env_config (board_build.*)
        overrides = {}
        for key, value in env_config.items():
            if key.startswith("board_build."):
                override_key = key[12:]  # Remove 'board_build.' prefix
                overrides[override_key] = value

        try:
            # Try to load from built-in defaults first
            return BoardConfig.from_board_id(board_id, overrides)
        except BoardConfigError:
            # Check if this is an ESP32 board by looking at platform URL
            platform_url = env_config.get("platform", "")
            if "espressif32" in platform_url or "esp32" in platform_url.lower():
                # For ESP32 boards, create a minimal BoardConfig
                # The actual board JSON will be loaded by ESP32Platform
                mcu = BoardConfigLoader._detect_esp32_mcu(board_id)

                return BoardConfig(
                    name=board_id,
                    mcu=mcu,
                    f_cpu="240000000L",  # Default ESP32 frequency
                    board="ESP32_DEV",
                    core="esp32",
                    variant="esp32",
                )

            # If not ESP32, re-raise original error
            raise BoardConfigError(
                f"Unknown board: {board_id}\n"
                + "Supported AVR boards: uno, mega, nano, leonardo\n"
                + "For ESP32 boards, ensure platform URL contains 'espressif32'"
            )

    @staticmethod
    def _detect_esp32_mcu(board_id: str) -> str:
        """
        Detect ESP32 MCU variant from board ID.

        Args:
            board_id: Board identifier

        Returns:
            MCU identifier (esp32, esp32s3, esp32c3, etc.)
        """
        board_id_lower = board_id.lower()

        # Check for specific ESP32 variants
        if "esp32s3" in board_id_lower or "esp32-s3" in board_id_lower:
            return "esp32s3"
        elif "esp32s2" in board_id_lower or "esp32-s2" in board_id_lower:
            return "esp32s2"
        elif "esp32c3" in board_id_lower or "esp32-c3" in board_id_lower:
            return "esp32c3"
        elif "esp32c6" in board_id_lower or "esp32-c6" in board_id_lower:
            return "esp32c6"
        elif "esp32c2" in board_id_lower or "esp32-c2" in board_id_lower:
            return "esp32c2"
        elif "esp32h2" in board_id_lower or "esp32-h2" in board_id_lower:
            return "esp32h2"
        else:
            # Default to esp32
            return "esp32"
