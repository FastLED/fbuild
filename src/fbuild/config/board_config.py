"""
Board configuration loader for Arduino boards.

This module provides functionality to load and parse board-specific configurations
from boards.txt files, extracting MCU type, frequency, and other build parameters.
"""

from pathlib import Path
from typing import Dict, Optional


class BoardConfigError(Exception):
    """Exception raised for board configuration errors."""

    pass


class BoardConfig:
    """
    Loader and parser for Arduino boards.txt configuration.

    boards.txt files contain board-specific definitions like MCU type,
    frequency, fuse settings, and other build parameters.

    Example boards.txt entry:
        uno.name=Arduino Uno
        uno.build.mcu=atmega328p
        uno.build.f_cpu=16000000L
        uno.build.board=AVR_UNO
        uno.build.core=arduino
        uno.build.variant=standard

    Usage:
        # Load from boards.txt file
        config = BoardConfig.from_boards_txt(Path("boards.txt"), "uno")

        # Or create directly with known values
        config = BoardConfig(
            name="uno",
            mcu="atmega328p",
            f_cpu="16000000L",
            board="AVR_UNO",
            core="arduino",
            variant="standard"
        )
    """

    # Default Arduino Uno configuration (used as fallback)
    UNO_DEFAULTS = {
        "name": "Arduino Uno",
        "mcu": "atmega328p",
        "f_cpu": "16000000L",
        "board": "AVR_UNO",
        "core": "arduino",
        "variant": "standard",
        "vid": "0x2341",
        "pid": "0x0043",
    }

    # Other common boards with defaults
    BOARD_DEFAULTS = {
        "uno": UNO_DEFAULTS,
        "mega": {
            "name": "Arduino Mega",
            "mcu": "atmega2560",
            "f_cpu": "16000000L",
            "board": "AVR_MEGA2560",
            "core": "arduino",
            "variant": "mega",
        },
        "nano": {
            "name": "Arduino Nano",
            "mcu": "atmega328p",
            "f_cpu": "16000000L",
            "board": "AVR_NANO",
            "core": "arduino",
            "variant": "eightanaloginputs",
        },
        "leonardo": {
            "name": "Arduino Leonardo",
            "mcu": "atmega32u4",
            "f_cpu": "16000000L",
            "board": "AVR_LEONARDO",
            "core": "arduino",
            "variant": "leonardo",
        },
        "esp32-c3-devkitm-1": {
            "name": "ESP32-C3 DevKit M-1",
            "mcu": "esp32c3",
            "f_cpu": "160000000L",
            "board": "ESP32_DEV",
            "core": "esp32",
            "variant": "esp32c3",
        },
        "esp32-c6-devkitc-1": {
            "name": "ESP32-C6 DevKit C-1",
            "mcu": "esp32c6",
            "f_cpu": "160000000L",
            "board": "ESP32_DEV",
            "core": "esp32",
            "variant": "esp32c6",
        },
        "esp32-c6-devkitm-1": {
            "name": "ESP32-C6 DevKit M-1",
            "mcu": "esp32c6",
            "f_cpu": "160000000L",
            "board": "ESP32_DEV",
            "core": "esp32",
            "variant": "esp32c6",
        },
        "esp32-c5-devkitc-1": {
            "name": "ESP32-C5 DevKit C-1",
            "mcu": "esp32c5",
            "f_cpu": "160000000L",
            "board": "ESP32_DEV",
            "core": "esp32",
            "variant": "esp32c5",
        },
        "teensy36": {
            "name": "Teensy 3.6",
            "mcu": "mk66fx1m0",
            "f_cpu": "180000000L",
            "board": "TEENSY36",
            "core": "teensy3",
            "variant": "teensy36",
        },
        "teensy40": {
            "name": "Teensy 4.0",
            "mcu": "imxrt1062",
            "f_cpu": "600000000L",
            "board": "TEENSY40",
            "core": "teensy4",
            "variant": "teensy40",
        },
        "teensy41": {
            "name": "Teensy 4.1",
            "mcu": "imxrt1062",
            "f_cpu": "600000000L",
            "board": "TEENSY41",
            "core": "teensy4",
            "variant": "teensy41",
        },
        "rpipico": {
            "name": "Raspberry Pi Pico",
            "mcu": "rp2040",
            "f_cpu": "133000000L",
            "board": "RASPBERRY_PI_PICO",
            "core": "rp2040",
            "variant": "rpipico",
        },
        "rpipicow": {
            "name": "Raspberry Pi Pico W",
            "mcu": "rp2040",
            "f_cpu": "133000000L",
            "board": "RASPBERRY_PI_PICO_W",
            "core": "rp2040",
            "variant": "rpipicow",
        },
        "rpipico2": {
            "name": "Raspberry Pi Pico 2",
            "mcu": "rp2350",
            "f_cpu": "150000000L",
            "board": "RASPBERRY_PI_PICO_2",
            "core": "rp2040",
            "variant": "rpipico2",
        },
        "rpipico2w": {
            "name": "Raspberry Pi Pico 2 W",
            "mcu": "rp2350",
            "f_cpu": "150000000L",
            "board": "RASPBERRY_PI_PICO_2_W",
            "core": "rp2040",
            "variant": "rpipico2w",
        },
        # STM32 boards
        "nucleo_f446re": {
            "name": "ST Nucleo F446RE",
            "mcu": "stm32f446ret6",
            "f_cpu": "180000000L",
            "board": "NUCLEO_F446RE",
            "core": "arduino",
            "variant": "STM32F4xx/F446R(C-E)T_NUCLEO",
        },
        "nucleo_f411re": {
            "name": "ST Nucleo F411RE",
            "mcu": "stm32f411ret6",
            "f_cpu": "100000000L",
            "board": "NUCLEO_F411RE",
            "core": "arduino",
            "variant": "STM32F4xx/F411R(C-E)T",
        },
        "nucleo_f103rb": {
            "name": "ST Nucleo F103RB",
            "mcu": "stm32f103rbt6",
            "f_cpu": "72000000L",
            "board": "NUCLEO_F103RB",
            "core": "arduino",
            "variant": "STM32F1xx/F103R(8-B)T",
        },
        "nucleo_l476rg": {
            "name": "ST Nucleo L476RG",
            "mcu": "stm32l476rgt6",
            "f_cpu": "80000000L",
            "board": "NUCLEO_L476RG",
            "core": "arduino",
            "variant": "STM32L4xx/L476R(C-E-G)T",
        },
        "bluepill_f103c8": {
            "name": "Blue Pill F103C8",
            "mcu": "stm32f103c8t6",
            "f_cpu": "72000000L",
            "board": "BLUEPILL_F103C8",
            "core": "arduino",
            "variant": "STM32F1xx/F103C8T_BLUEPILL",
        },
        "blackpill_f411ce": {
            "name": "Black Pill F411CE",
            "mcu": "stm32f411ceu6",
            "f_cpu": "100000000L",
            "board": "BLACKPILL_F411CE",
            "core": "arduino",
            "variant": "STM32F4xx/F411C(C-E)(U-Y)",
        },
    }

    def __init__(
        self,
        name: str,
        mcu: str,
        f_cpu: str,
        board: str,
        core: str = "arduino",
        variant: str = "standard",
        vid: Optional[str] = None,
        pid: Optional[str] = None,
        extra_flags: Optional[Dict[str, str]] = None,
    ):
        """
        Initialize board configuration.

        Args:
            name: Human-readable board name (e.g., "Arduino Uno")
            mcu: Microcontroller type (e.g., "atmega328p")
            f_cpu: CPU frequency (e.g., "16000000L")
            board: Board identifier for defines (e.g., "AVR_UNO")
            core: Core library name (default: "arduino")
            variant: Variant directory name (default: "standard")
            vid: USB Vendor ID (optional)
            pid: USB Product ID (optional)
            extra_flags: Additional board-specific flags (optional)
        """
        self.name = name
        self.mcu = mcu
        self.f_cpu = f_cpu
        self.board = board
        self.core = core
        self.variant = variant
        self.vid = vid
        self.pid = pid
        self.extra_flags = extra_flags or {}

    @property
    def platform(self) -> str:
        """
        Detect platform type from MCU.

        Returns:
            Platform identifier: "avr", "esp32", "teensy", "raspberrypi", or "ststm32"
        """
        if self.mcu.startswith("atmega"):
            return "avr"
        elif self.mcu.startswith("esp32"):
            return "esp32"
        elif self.mcu.startswith("imxrt"):
            return "teensy"
        elif self.mcu.startswith("rp20"):
            return "raspberrypi"
        elif self.mcu.startswith("stm32"):
            return "ststm32"
        else:
            # Default to AVR for unknown
            return "avr"

    @classmethod
    def from_boards_txt(
        cls,
        boards_txt_path: Path,
        board_id: str,
        overrides: Optional[Dict[str, str]] = None,
    ) -> "BoardConfig":
        """
        Load board configuration from a boards.txt file.

        Args:
            boards_txt_path: Path to boards.txt file
            board_id: Board identifier (e.g., "uno", "mega")
            overrides: Optional overrides from platformio.ini
                      (e.g., board_build.mcu, board_build.f_cpu)

        Returns:
            BoardConfig instance

        Raises:
            BoardConfigError: If file not found or board not defined

        Example:
            config = BoardConfig.from_boards_txt(
                Path("hardware/arduino/avr/boards.txt"),
                "uno"
            )
        """
        if not boards_txt_path.exists():
            raise BoardConfigError(f"boards.txt not found: {boards_txt_path}")

        # Parse boards.txt
        board_data = cls._parse_boards_txt(boards_txt_path, board_id)

        if not board_data:
            raise BoardConfigError(f"Board '{board_id}' not found in {boards_txt_path}")

        # Apply overrides from platformio.ini (board_build.*)
        if overrides:
            board_data.update(overrides)

        # Extract required fields with validation
        try:
            return cls(
                name=board_data.get("name", board_id),
                mcu=board_data["mcu"],
                f_cpu=board_data["f_cpu"],
                board=board_data["board"],
                core=board_data.get("core", "arduino"),
                variant=board_data.get("variant", "standard"),
                vid=board_data.get("vid"),
                pid=board_data.get("pid"),
                extra_flags={
                    k: v
                    for k, v in board_data.items()
                    if k
                    not in {
                        "name",
                        "mcu",
                        "f_cpu",
                        "board",
                        "core",
                        "variant",
                        "vid",
                        "pid",
                    }
                },
            )
        except KeyError as e:
            raise BoardConfigError(f"Board '{board_id}' is missing required field: {e}") from e

    @classmethod
    def from_board_id(cls, board_id: str, overrides: Optional[Dict[str, str]] = None) -> "BoardConfig":
        """
        Load board configuration using built-in defaults.

        This is a fallback when boards.txt is not available. It uses
        hardcoded configurations for common Arduino boards.

        Args:
            board_id: Board identifier (e.g., "uno", "mega")
            overrides: Optional overrides (e.g., from platformio.ini)

        Returns:
            BoardConfig instance

        Raises:
            BoardConfigError: If board_id not recognized

        Example:
            config = BoardConfig.from_board_id("uno")
        """
        # Normalize board_id (handle common variations)
        board_id = board_id.lower()
        if board_id == "megaatmega2560":
            board_id = "mega"

        if board_id not in cls.BOARD_DEFAULTS:
            raise BoardConfigError(f"Unknown board: {board_id}. " + f"Supported boards: {', '.join(cls.BOARD_DEFAULTS.keys())}")

        board_data = cls.BOARD_DEFAULTS[board_id].copy()

        # Apply overrides
        if overrides:
            board_data.update(overrides)

        return cls(
            name=board_data["name"],
            mcu=board_data["mcu"],
            f_cpu=board_data["f_cpu"],
            board=board_data["board"],
            core=board_data.get("core", "arduino"),
            variant=board_data.get("variant", "standard"),
            vid=board_data.get("vid"),
            pid=board_data.get("pid"),
        )

    @staticmethod
    def _parse_boards_txt(boards_txt_path: Path, board_id: str) -> Dict[str, str]:
        """
        Parse boards.txt file and extract configuration for a specific board.

        Args:
            boards_txt_path: Path to boards.txt
            board_id: Board identifier to extract

        Returns:
            Dictionary of board configuration keys and values

        Example boards.txt format:
            uno.name=Arduino Uno
            uno.build.mcu=atmega328p
            uno.build.f_cpu=16000000L
        """
        board_data = {}
        prefix = f"{board_id}."

        try:
            with open(boards_txt_path, "r", encoding="utf-8") as f:
                for line in f:
                    line = line.strip()

                    # Skip comments and empty lines
                    if not line or line.startswith("#"):
                        continue

                    # Parse key=value pairs
                    if "=" not in line:
                        continue

                    key, value = line.split("=", 1)
                    key = key.strip()
                    value = value.strip()

                    # Only process lines for our board
                    if not key.startswith(prefix):
                        continue

                    # Remove board prefix and extract the field name
                    # uno.build.mcu -> mcu (from build.*)
                    # uno.name -> name
                    field = key[len(prefix) :]

                    # Handle build.* fields
                    if field.startswith("build."):
                        field = field[6:]  # Remove 'build.' prefix

                    # Handle upload.* fields (vid, pid)
                    elif field.startswith("upload."):
                        field = field[7:]  # Remove 'upload.' prefix

                    board_data[field] = value

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception as e:
            raise BoardConfigError(f"Failed to parse {boards_txt_path}: {e}") from e

        return board_data

    def get_defines(self) -> Dict[str, str]:
        """
        Get preprocessor defines for this board configuration.

        Returns:
            Dictionary of define names to values

        Example:
            {
                'PLATFORMIO': '60118',
                'F_CPU': '16000000L',
                'ARDUINO': '10819',
                'ARDUINO_AVR_UNO': '',
                'ARDUINO_ARCH_AVR': '',
                '__AVR_ATmega328P__': ''
            }
        """
        defines = {
            "PLATFORMIO": "60118",  # PlatformIO version identifier
            "F_CPU": self.f_cpu,
            "ARDUINO": "10819",  # Arduino version (1.8.19)
            f"ARDUINO_{self.board}": "",
            "ARDUINO_ARCH_AVR": "",
        }

        # Add MCU-specific define (e.g., __AVR_ATmega328P__)
        mcu_define = f"__AVR_{self.mcu.upper()}__"
        defines[mcu_define] = ""

        # Add any extra defines from board configuration
        for key, value in self.extra_flags.items():
            if key.startswith("define_"):
                define_name = key[7:].upper()
                defines[define_name] = value

        return defines

    def get_include_paths(self, core_path: Path) -> list[Path]:
        """
        Get include paths for compilation.

        Args:
            core_path: Path to Arduino core installation

        Returns:
            List of include directory paths

        Example:
            [
                Path('/path/to/cores/arduino'),
                Path('/path/to/variants/standard')
            ]
        """
        return [
            core_path / "cores" / self.core,
            core_path / "variants" / self.variant,
        ]

    def get_core_sources_dir(self, core_path: Path) -> Path:
        """
        Get path to core source files directory.

        Args:
            core_path: Path to Arduino core installation

        Returns:
            Path to core sources (e.g., cores/arduino)
        """
        return core_path / "cores" / self.core

    def get_variant_dir(self, core_path: Path) -> Path:
        """
        Get path to board variant directory.

        Args:
            core_path: Path to Arduino core installation

        Returns:
            Path to variant directory (e.g., variants/standard)
        """
        return core_path / "variants" / self.variant

    def __repr__(self) -> str:
        """String representation of board configuration."""
        return f"BoardConfig(name='{self.name}', mcu='{self.mcu}', f_cpu='{self.f_cpu}', board='{self.board}')"
