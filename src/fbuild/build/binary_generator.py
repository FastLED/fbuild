"""Binary Generation Utilities.

This module provides utilities for generating firmware binaries from ELF files,
including ESP32-specific bootloader and partition table generation.

Design:
    - Separates binary generation logic from linker
    - Supports both objcopy (AVR) and esptool (ESP32) workflows
    - Handles ESP32 bootloader and partition table generation
"""

import subprocess
import sys
from pathlib import Path
from typing import Any, Dict, Optional

from ..subprocess_utils import safe_run


class BinaryGeneratorError(Exception):
    """Raised when binary generation operations fail."""
    pass


class BinaryGenerator:
    """Handles firmware binary generation from ELF files.

    This class provides:
    - ELF to BIN conversion using objcopy or esptool
    - ESP32 bootloader.bin generation
    - ESP32 partitions.bin generation
    """

    def __init__(
        self,
        mcu: str,
        board_config: Dict[str, Any],
        build_dir: Path,
        toolchain: Any = None,
        framework: Any = None,
        show_progress: bool = True,
        env_config: Optional[Dict[str, Any]] = None
    ):
        """Initialize binary generator.

        Args:
            mcu: MCU type (e.g., "esp32c6", "atmega328p")
            board_config: Board configuration dictionary
            build_dir: Directory for build artifacts
            toolchain: Toolchain instance (required for objcopy)
            framework: Framework instance (required for ESP32 bootloader/partitions)
            show_progress: Whether to show generation progress
            env_config: Optional platformio.ini environment configuration
        """
        self.mcu = mcu
        self.board_config = board_config
        self.build_dir = build_dir
        self.toolchain = toolchain
        self.framework = framework
        self.show_progress = show_progress
        self.env_config = env_config or {}

    def generate_bin(self, elf_path: Path, output_bin: Optional[Path] = None) -> Path:
        """Generate firmware.bin from firmware.elf.

        Args:
            elf_path: Path to firmware.elf
            output_bin: Optional path for output .bin file

        Returns:
            Path to generated firmware.bin

        Raises:
            BinaryGeneratorError: If conversion fails
        """
        if not elf_path.exists():
            raise BinaryGeneratorError(f"ELF file not found: {elf_path}")

        # Generate output path if not provided
        if output_bin is None:
            output_bin = self.build_dir / "firmware.bin"

        # For ESP32 platforms, use esptool.py elf2image instead of objcopy
        # This generates a properly formatted ESP32 flash image without memory gaps
        if self.mcu.startswith("esp32"):
            return self._generate_bin_esp32(elf_path, output_bin)
        else:
            return self._generate_bin_objcopy(elf_path, output_bin)

    def _generate_bin_esp32(self, elf_path: Path, output_bin: Path) -> Path:
        """Generate firmware.bin for ESP32 using esptool.py elf2image.

        Args:
            elf_path: Path to firmware.elf
            output_bin: Path for output .bin file

        Returns:
            Path to generated firmware.bin

        Raises:
            BinaryGeneratorError: If conversion fails
        """
        # Get chip type from MCU
        chip = self.mcu  # e.g., "esp32c6", "esp32s3"

        # Get flash parameters from board config
        flash_mode = self.board_config.get("build", {}).get("flash_mode", "dio")
        flash_freq = self.board_config.get("build", {}).get("f_flash", "80m")
        flash_size = self.board_config.get("build", {}).get("flash_size", "4MB")

        # Convert frequency to esptool format if needed
        flash_freq = self._normalize_flash_freq(flash_freq)

        # Build esptool.py elf2image command
        cmd = [
            sys.executable,
            "-m",
            "esptool",
            "--chip",
            chip,
            "elf2image",
            "--flash-mode",
            flash_mode,
            "--flash-freq",
            flash_freq,
            "--flash-size",
            flash_size,
            "--elf-sha256-offset",
            "0xb0",
            "-o",
            str(output_bin),
            str(elf_path)
        ]

        if self.show_progress:
            print("Generating firmware.bin using esptool.py elf2image...")

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=False,  # Don't decode as text - esptool may output binary data
                timeout=60
            )

            if result.returncode != 0:
                error_msg = "Binary generation failed\n"
                stderr = result.stderr.decode('utf-8', errors='replace') if result.stderr else ""
                stdout = result.stdout.decode('utf-8', errors='replace') if result.stdout else ""
                error_msg += f"stderr: {stderr}\n"
                error_msg += f"stdout: {stdout}"
                raise BinaryGeneratorError(error_msg)

            if not output_bin.exists():
                raise BinaryGeneratorError(f"firmware.bin was not created: {output_bin}")

            if self.show_progress:
                size = output_bin.stat().st_size
                print(f"✓ Created firmware.bin: {size:,} bytes ({size / 1024:.2f} KB)")

            return output_bin

        except subprocess.TimeoutExpired as e:
            raise BinaryGeneratorError("Binary generation timeout") from e
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise BinaryGeneratorError(f"Failed to generate binary: {e}") from e

    def _generate_bin_objcopy(self, elf_path: Path, output_bin: Path) -> Path:
        """Generate firmware.bin using objcopy (for non-ESP32 platforms).

        Args:
            elf_path: Path to firmware.elf
            output_bin: Path for output .bin file

        Returns:
            Path to generated firmware.bin

        Raises:
            BinaryGeneratorError: If conversion fails
        """
        if self.toolchain is None:
            raise BinaryGeneratorError("Toolchain required for objcopy binary generation")

        # Get objcopy tool
        objcopy_path = self.toolchain.get_objcopy_path()
        if objcopy_path is None or not objcopy_path.exists():
            raise BinaryGeneratorError(
                f"objcopy not found: {objcopy_path}. " +
                "Ensure toolchain is installed."
            )

        # Build objcopy command
        cmd = [
            str(objcopy_path),
            "-O", "binary",
            str(elf_path),
            str(output_bin)
        ]

        # Execute objcopy
        if self.show_progress:
            print("Generating firmware.bin...")

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=True,
                timeout=30
            )

            if result.returncode != 0:
                error_msg = "Binary generation failed\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise BinaryGeneratorError(error_msg)

            if not output_bin.exists():
                raise BinaryGeneratorError(f"firmware.bin was not created: {output_bin}")

            if self.show_progress:
                size = output_bin.stat().st_size
                print(f"✓ Created firmware.bin: {size:,} bytes ({size / 1024 / 1024:.2f} MB)")

            return output_bin

        except subprocess.TimeoutExpired as e:
            raise BinaryGeneratorError("Binary generation timeout") from e
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise BinaryGeneratorError(f"Failed to generate binary: {e}") from e

    def generate_bootloader(self, output_bin: Optional[Path] = None) -> Path:
        """Generate bootloader.bin from bootloader ELF file.

        Args:
            output_bin: Optional path for output bootloader.bin

        Returns:
            Path to generated bootloader.bin

        Raises:
            BinaryGeneratorError: If generation fails
        """
        if not self.mcu.startswith("esp32"):
            raise BinaryGeneratorError(
                f"Bootloader generation only supported for ESP32 platforms, not {self.mcu}"
            )

        if self.framework is None:
            raise BinaryGeneratorError("Framework required for bootloader generation")

        # Generate output path if not provided
        if output_bin is None:
            output_bin = self.build_dir / "bootloader.bin"

        # Get flash parameters from board config
        flash_mode = self.board_config.get("build", {}).get("flash_mode", "dio")
        flash_freq = self.board_config.get("build", {}).get("f_flash", "80m")
        flash_size = self.board_config.get("build", {}).get("flash_size", "4MB")

        # Convert frequency to esptool format if needed
        flash_freq = self._normalize_flash_freq(flash_freq)

        # Find bootloader ELF file in framework SDK
        bootloader_name = f"bootloader_{flash_mode}_{flash_freq.replace('m', 'm')}.elf"
        sdk_bin_dir = self.framework.get_sdk_dir() / self.mcu / "bin"
        bootloader_elf = sdk_bin_dir / bootloader_name

        if not bootloader_elf.exists():
            raise BinaryGeneratorError(
                f"Bootloader ELF not found: {bootloader_elf}"
            )

        # CRITICAL FIX: ESP32-C6/C3/C2/H2 bootloaders MUST be generated in DIO mode
        # even if the application uses QIO. The ROM bootloader can only load the
        # second-stage bootloader in DIO mode. QIO is enabled later by the second-stage
        # bootloader for the application. This is a known issue with esptool v4.7+.
        # See: https://github.com/espressif/arduino-esp32/discussions/10418
        bootloader_flash_mode = flash_mode
        if self.mcu in ["esp32c6", "esp32c3", "esp32c2", "esp32h2"]:
            bootloader_flash_mode = "dio"

        # Generate bootloader.bin using esptool.py elf2image
        cmd = [
            sys.executable,
            "-m",
            "esptool",
            "--chip",
            self.mcu,
            "elf2image",
            "--flash-mode",
            bootloader_flash_mode,
            "--flash-freq",
            flash_freq,
            "--flash-size",
            flash_size,
            "-o",
            str(output_bin),
            str(bootloader_elf)
        ]

        if self.show_progress:
            print("Generating bootloader.bin...")

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=False,
                timeout=60
            )

            if result.returncode != 0:
                error_msg = "Bootloader generation failed\n"
                stderr = result.stderr.decode('utf-8', errors='replace') if result.stderr else ""
                stdout = result.stdout.decode('utf-8', errors='replace') if result.stdout else ""
                error_msg += f"stderr: {stderr}\n"
                error_msg += f"stdout: {stdout}"
                raise BinaryGeneratorError(error_msg)

            if not output_bin.exists():
                raise BinaryGeneratorError(f"bootloader.bin was not created: {output_bin}")

            if self.show_progress:
                size = output_bin.stat().st_size
                print(f"✓ Created bootloader.bin: {size:,} bytes ({size / 1024:.2f} KB)")

            return output_bin

        except subprocess.TimeoutExpired as e:
            raise BinaryGeneratorError("Bootloader generation timeout") from e
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise BinaryGeneratorError(f"Failed to generate bootloader: {e}") from e

    def generate_partition_table(self, output_bin: Optional[Path] = None) -> Path:
        """Generate partitions.bin from partition CSV file.

        Args:
            output_bin: Optional path for output partitions.bin

        Returns:
            Path to generated partitions.bin

        Raises:
            BinaryGeneratorError: If generation fails
        """
        if not self.mcu.startswith("esp32"):
            raise BinaryGeneratorError(
                f"Partition table generation only supported for ESP32 platforms, not {self.mcu}"
            )

        if self.framework is None:
            raise BinaryGeneratorError("Framework required for partition table generation")

        # Generate output path if not provided
        if output_bin is None:
            output_bin = self.build_dir / "partitions.bin"

        # Get partition table name from env_config (board_build.partitions)
        # Default to default.csv if not specified
        partition_table = self.env_config.get("board_build.partitions", "default.csv")

        # If partition_table doesn't have .csv extension, add it
        if not partition_table.endswith(".csv"):
            partition_table += ".csv"

        # Find partition CSV file in framework
        partitions_csv = self.framework.framework_path / "tools" / "partitions" / partition_table

        if not partitions_csv.exists():
            raise BinaryGeneratorError(
                f"Partition CSV not found: {partitions_csv}\n" +
                f"Requested partition table: {partition_table}"
            )

        # Find gen_esp32part.py tool - also in framework
        gen_tool = self.framework.framework_path / "tools" / "gen_esp32part.py"

        if not gen_tool.exists():
            raise BinaryGeneratorError(
                f"Partition generation tool not found: {gen_tool}"
            )

        # Generate partition table using gen_esp32part.py
        cmd = [
            sys.executable,
            str(gen_tool),
            "-q",
            str(partitions_csv),
            str(output_bin)
        ]

        if self.show_progress:
            print("Generating partitions.bin...")

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=True,
                timeout=30
            )

            if result.returncode != 0:
                error_msg = "Partition table generation failed\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise BinaryGeneratorError(error_msg)

            if not output_bin.exists():
                raise BinaryGeneratorError(f"partitions.bin was not created: {output_bin}")

            if self.show_progress:
                size = output_bin.stat().st_size
                print(f"✓ Created partitions.bin: {size:,} bytes")

            return output_bin

        except subprocess.TimeoutExpired as e:
            raise BinaryGeneratorError("Partition table generation timeout") from e
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise BinaryGeneratorError(f"Failed to generate partition table: {e}") from e

    def generate_merged_bin(self, output_bin: Optional[Path] = None) -> Path:
        """Generate merged.bin combining bootloader, partition table, and firmware.

        This creates a single binary file that can be flashed to address 0x0 and contains
        all components at their correct offsets. Useful for QEMU and factory programming.

        Args:
            output_bin: Optional path for output merged.bin

        Returns:
            Path to generated merged.bin

        Raises:
            BinaryGeneratorError: If generation fails
        """
        if not self.mcu.startswith("esp32"):
            raise BinaryGeneratorError(
                f"Merged bin generation only supported for ESP32 platforms, not {self.mcu}"
            )

        # Generate output path if not provided
        if output_bin is None:
            output_bin = self.build_dir / "merged.bin"

        # Generate all required components first
        firmware_bin = self.build_dir / "firmware.bin"
        bootloader_bin = self.build_dir / "bootloader.bin"
        partitions_bin = self.build_dir / "partitions.bin"

        # Ensure all components exist
        if not firmware_bin.exists():
            raise BinaryGeneratorError(f"firmware.bin not found: {firmware_bin}")
        if not bootloader_bin.exists():
            raise BinaryGeneratorError(f"bootloader.bin not found: {bootloader_bin}")
        if not partitions_bin.exists():
            raise BinaryGeneratorError(f"partitions.bin not found: {partitions_bin}")

        # Get flash offsets for each component based on MCU type
        offsets = self._get_flash_offsets()

        # Use esptool.py merge_bin to create merged binary
        flash_size = self.board_config.get("build", {}).get("flash_size", "4MB")

        cmd = [
            sys.executable,
            "-m",
            "esptool",
            "--chip",
            self.mcu,
            "merge_bin",
            "--flash-mode",
            self.board_config.get("build", {}).get("flash_mode", "dio"),
            "--flash-freq",
            self._normalize_flash_freq(self.board_config.get("build", {}).get("f_flash", "80m")),
            "--flash-size",
            flash_size,
            "-o",
            str(output_bin),
            str(offsets["bootloader"]),
            str(bootloader_bin),
            str(offsets["partitions"]),
            str(partitions_bin),
            str(offsets["firmware"]),
            str(firmware_bin)
        ]

        if self.show_progress:
            print("Generating merged.bin...")

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=False,
                timeout=60
            )

            if result.returncode != 0:
                error_msg = "Merged bin generation failed\n"
                stderr = result.stderr.decode('utf-8', errors='replace') if result.stderr else ""
                stdout = result.stdout.decode('utf-8', errors='replace') if result.stdout else ""
                error_msg += f"stderr: {stderr}\n"
                error_msg += f"stdout: {stdout}"
                raise BinaryGeneratorError(error_msg)

            if not output_bin.exists():
                raise BinaryGeneratorError(f"merged.bin was not created: {output_bin}")

            if self.show_progress:
                size = output_bin.stat().st_size
                print(f"✓ Created merged.bin: {size:,} bytes ({size / 1024:.2f} KB)")
                print("  Flash offsets:")
                print(f"    Bootloader:    {offsets['bootloader']}")
                print(f"    Partitions:    {offsets['partitions']}")
                print(f"    Firmware:      {offsets['firmware']}")

            return output_bin

        except subprocess.TimeoutExpired as e:
            raise BinaryGeneratorError("Merged bin generation timeout") from e
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise BinaryGeneratorError(f"Failed to generate merged bin: {e}") from e

    def _get_flash_offsets(self) -> Dict[str, str]:
        """Get flash offsets for bootloader, partition table, and firmware based on MCU.

        Returns:
            Dictionary with 'bootloader', 'partitions', and 'firmware' offset strings
        """
        # Default offsets for most ESP32 variants
        offsets = {
            "bootloader": "0x0",
            "partitions": "0x8000",
            "firmware": "0x10000"
        }

        # ESP32-C3 and ESP32-C2 use different bootloader offset
        if self.mcu in ["esp32c3", "esp32c2"]:
            offsets["bootloader"] = "0x0"

        # ESP32-S2 uses standard offsets
        # ESP32-S3 uses standard offsets
        # ESP32-C6 uses standard offsets
        # ESP32-H2 uses standard offsets

        return offsets

    @staticmethod
    def _normalize_flash_freq(flash_freq: Any) -> str:
        """Normalize flash frequency to esptool format.

        Args:
            flash_freq: Flash frequency (int/float in Hz, or string like "80m" or "80000000L")

        Returns:
            Normalized frequency string (e.g., "80m")
        """
        if isinstance(flash_freq, (int, float)):
            # Convert Hz to MHz format like "80m"
            return f"{int(flash_freq // 1000000)}m"
        elif isinstance(flash_freq, str) and flash_freq.endswith('L'):
            # Handle string representation of long integers like "80000000L"
            freq_value = int(flash_freq.rstrip('L'))
            return f"{freq_value // 1000000}m"
        else:
            return str(flash_freq)
