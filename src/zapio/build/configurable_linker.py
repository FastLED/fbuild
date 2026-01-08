"""Configurable Linker.

This module provides a generic, configuration-driven linker that can link
for any platform (ESP32, AVR, etc.) based on platform configuration files.

Design:
    - Loads linker flags, scripts, libraries from JSON/Python config
    - Generic implementation replaces platform-specific linker classes
    - Same interface as ESP32Linker for drop-in replacement
"""

import json
import subprocess
from pathlib import Path
from typing import List, Dict, Any, Optional, Union

from ..packages.esp32_platform import ESP32Platform
from ..packages.esp32_toolchain import ESP32Toolchain
from ..packages.esp32_framework import ESP32Framework


class ConfigurableLinkerError(Exception):
    """Raised when configurable linking operations fail."""
    pass


class ConfigurableLinker:
    """Generic linker driven by platform configuration.

    This class handles:
    - Loading platform-specific config from JSON
    - Linker script management
    - Library collection
    - Linking object files into firmware.elf
    - Converting .elf to .bin
    """

    def __init__(
        self,
        platform: ESP32Platform,
        toolchain: ESP32Toolchain,
        framework: ESP32Framework,
        board_id: str,
        build_dir: Path,
        platform_config: Optional[Union[Dict, Path]] = None,
        show_progress: bool = True
    ):
        """Initialize configurable linker.

        Args:
            platform: Platform instance
            toolchain: Toolchain instance
            framework: Framework instance
            board_id: Board identifier (e.g., "esp32-c6-devkitm-1")
            build_dir: Directory for build artifacts
            platform_config: Platform config dict or path to config JSON file
            show_progress: Whether to show linking progress
        """
        self.platform = platform
        self.toolchain = toolchain
        self.framework = framework
        self.board_id = board_id
        self.build_dir = build_dir
        self.show_progress = show_progress

        # Load board configuration
        self.board_config = platform.get_board_json(board_id)

        # Get MCU type from board config
        self.mcu = self.board_config.get("build", {}).get("mcu", "").lower()

        # Load platform configuration
        if platform_config is None:
            # Try to load from default location
            config_path = Path(__file__).parent.parent / "platform_configs" / f"{self.mcu}.json"
            if config_path.exists():
                with open(config_path, 'r') as f:
                    self.config = json.load(f)
            else:
                raise ConfigurableLinkerError(
                    f"No platform configuration found for {self.mcu}. "
                    f"Expected: {config_path}"
                )
        elif isinstance(platform_config, dict):
            self.config = platform_config
        else:
            # Assume it's a path
            with open(platform_config, 'r') as f:
                self.config = json.load(f)

        # Cache for linker paths
        self._linker_scripts_cache: Optional[List[Path]] = None
        self._sdk_libs_cache: Optional[List[Path]] = None

    def get_linker_scripts(self) -> List[Path]:
        """Get list of linker script paths for the MCU.

        Returns:
            List of .ld file paths in linking order
        """
        if self._linker_scripts_cache is not None:
            return self._linker_scripts_cache

        # Get linker script directory
        sdk_ld_dir = self.framework.get_sdk_dir() / self.mcu / "ld"

        if not sdk_ld_dir.exists():
            raise ConfigurableLinkerError(f"Linker script directory not found: {sdk_ld_dir}")

        # Get linker scripts from config
        config_scripts = self.config.get('linker_scripts', [])

        scripts = []
        for script_name in config_scripts:
            script_path = sdk_ld_dir / script_name
            if script_path.exists():
                scripts.append(script_path)
            # For ESP32-S3, sections.ld may be in flash mode subdirectories
            elif self.mcu == "esp32s3" and script_name == "sections.ld":
                flash_mode = self.board_config.get("build", {}).get("flash_mode", "qio")
                psram_mode = self.board_config.get("build", {}).get("psram_mode", "qspi")
                flash_dir = sdk_ld_dir.parent / f"{flash_mode}_{psram_mode}"
                alt_script_path = flash_dir / script_name
                if alt_script_path.exists():
                    scripts.append(alt_script_path)

        if not scripts:
            raise ConfigurableLinkerError(
                f"No linker scripts found for {self.mcu} in {sdk_ld_dir}"
            )

        self._linker_scripts_cache = scripts
        return scripts

    def get_sdk_libraries(self) -> List[Path]:
        """Get list of SDK precompiled libraries.

        Returns:
            List of .a library file paths
        """
        if self._sdk_libs_cache is not None:
            return self._sdk_libs_cache

        # Get flash mode from board configuration
        flash_mode = self.board_config.get("build", {}).get("flash_mode", "qio")

        # Get SDK libraries
        self._sdk_libs_cache = self.framework.get_sdk_libs(self.mcu, flash_mode)
        return self._sdk_libs_cache

    def get_linker_flags(self) -> List[str]:
        """Get linker flags from configuration.

        Returns:
            List of linker flags
        """
        flags = []

        # Get flags from config
        config_flags = self.config.get('linker_flags', [])
        flags.extend(config_flags)

        # Add map file flag with forward slashes for GCC compatibility
        # Use "firmware.map" instead of board_id to avoid special characters
        map_file = self.build_dir / "firmware.map"
        map_file_str = str(map_file).replace('\\', '/')
        flags.append(f'-Wl,-Map={map_file_str}')

        return flags

    def link(
        self,
        object_files: List[Path],
        core_archive: Path,
        output_elf: Optional[Path] = None,
        library_archives: Optional[List[Path]] = None
    ) -> Path:
        """Link object files and libraries into firmware.elf.

        Args:
            object_files: List of object files to link (sketch, libraries)
            core_archive: Path to core.a archive
            output_elf: Optional path for output .elf file
            library_archives: Optional list of library archives to link

        Returns:
            Path to generated firmware.elf

        Raises:
            ConfigurableLinkerError: If linking fails
        """
        if not object_files:
            raise ConfigurableLinkerError("No object files provided for linking")

        if not core_archive.exists():
            raise ConfigurableLinkerError(f"Core archive not found: {core_archive}")

        # Initialize library archives list
        if library_archives is None:
            library_archives = []

        # Get linker tool (use g++ for C++ support)
        linker_path = self.toolchain.get_gxx_path()
        if linker_path is None or not linker_path.exists():
            raise ConfigurableLinkerError(
                f"Linker not found: {linker_path}. "
                "Ensure toolchain is installed."
            )

        # Generate output path if not provided
        if output_elf is None:
            output_elf = self.build_dir / "firmware.elf"

        # Get linker flags
        linker_flags = self.get_linker_flags()

        # Get linker scripts
        linker_scripts = self.get_linker_scripts()

        # Get SDK libraries
        sdk_libs = self.get_sdk_libraries()

        # Build linker command
        cmd = [str(linker_path)]
        cmd.extend(linker_flags)

        # Add linker script directory to library search path
        ld_dir = self.framework.get_sdk_dir() / self.mcu / "ld"
        cmd.append(f"-L{ld_dir}")

        # For ESP32-S3, also add flash mode directory to search path
        if self.mcu == "esp32s3":
            flash_mode = self.board_config.get("build", {}).get("flash_mode", "qio")
            psram_mode = self.board_config.get("build", {}).get("psram_mode", "qspi")
            flash_dir = self.framework.get_sdk_dir() / self.mcu / f"{flash_mode}_{psram_mode}"
            if flash_dir.exists():
                cmd.append(f"-L{flash_dir}")

        # Add linker scripts
        for script in linker_scripts:
            if script.parent == ld_dir or (self.mcu == "esp32s3" and script.parent.name.endswith(("_qspi", "_opi"))):
                cmd.append(f"-T{script.name}")
            else:
                cmd.append(f"-T{script}")

        # Add object files
        cmd.extend([str(obj) for obj in object_files])

        # Add core archive
        cmd.append(str(core_archive))

        # Add SDK library directory to search path
        sdk_lib_dir = self.framework.get_sdk_dir() / self.mcu / "lib"
        if sdk_lib_dir.exists():
            cmd.append(f"-L{sdk_lib_dir}")

        # Group libraries to resolve circular dependencies
        cmd.append("-Wl,--start-group")

        # Add user library archives first
        for lib_archive in library_archives:
            if lib_archive.exists():
                cmd.append(str(lib_archive))

        # Add SDK libraries
        for lib in sdk_libs:
            cmd.append(str(lib))

        # Add standard libraries
        cmd.extend([
            "-lgcc",
            "-lstdc++",
            "-lm",
            "-lc",
        ])

        cmd.append("-Wl,--end-group")

        # Add output
        cmd.extend(["-o", str(output_elf)])

        # Execute linker
        if self.show_progress:
            print("Linking firmware.elf...")
            print(f"  Object files: {len(object_files)}")
            print(f"  Core archive: {core_archive.name}")
            print(f"  SDK libraries: {len(sdk_libs)}")
            print(f"  Linker scripts: {len(linker_scripts)}")

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=120
            )

            if result.returncode != 0:
                error_msg = "Linking failed\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise ConfigurableLinkerError(error_msg)

            if not output_elf.exists():
                raise ConfigurableLinkerError(f"firmware.elf was not created: {output_elf}")

            if self.show_progress:
                size = output_elf.stat().st_size
                print(f"✓ Created firmware.elf: {size:,} bytes ({size / 1024 / 1024:.2f} MB)")

            return output_elf

        except subprocess.TimeoutExpired:
            raise ConfigurableLinkerError("Linking timeout")
        except Exception as e:
            raise ConfigurableLinkerError(f"Failed to link: {e}")

    def generate_bin(self, elf_path: Path, output_bin: Optional[Path] = None) -> Path:
        """Generate firmware.bin from firmware.elf.

        Args:
            elf_path: Path to firmware.elf
            output_bin: Optional path for output .bin file

        Returns:
            Path to generated firmware.bin

        Raises:
            ConfigurableLinkerError: If conversion fails
        """
        if not elf_path.exists():
            raise ConfigurableLinkerError(f"ELF file not found: {elf_path}")

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
            ConfigurableLinkerError: If conversion fails
        """
        import sys

        # Get chip type from MCU
        chip = self.mcu  # e.g., "esp32c6", "esp32s3"

        # Get flash parameters from board config
        flash_mode = self.board_config.get("build", {}).get("flash_mode", "dio")
        flash_freq = self.board_config.get("build", {}).get("f_flash", "80m")
        flash_size = self.board_config.get("build", {}).get("flash_size", "4MB")

        # Convert frequency to esptool format if needed
        if isinstance(flash_freq, (int, float)):
            # Convert Hz to MHz format like "80m"
            flash_freq = f"{int(flash_freq // 1000000)}m"
        elif isinstance(flash_freq, str) and flash_freq.endswith('L'):
            # Handle string representation of long integers like "80000000L"
            freq_value = int(flash_freq.rstrip('L'))
            flash_freq = f"{freq_value // 1000000}m"

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
            result = subprocess.run(
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
                raise ConfigurableLinkerError(error_msg)

            if not output_bin.exists():
                raise ConfigurableLinkerError(f"firmware.bin was not created: {output_bin}")

            if self.show_progress:
                size = output_bin.stat().st_size
                print(f"✓ Created firmware.bin: {size:,} bytes ({size / 1024:.2f} KB)")

            return output_bin

        except subprocess.TimeoutExpired:
            raise ConfigurableLinkerError("Binary generation timeout")
        except Exception as e:
            raise ConfigurableLinkerError(f"Failed to generate binary: {e}")

    def _generate_bin_objcopy(self, elf_path: Path, output_bin: Path) -> Path:
        """Generate firmware.bin using objcopy (for non-ESP32 platforms).

        Args:
            elf_path: Path to firmware.elf
            output_bin: Path for output .bin file

        Returns:
            Path to generated firmware.bin

        Raises:
            ConfigurableLinkerError: If conversion fails
        """
        # Get objcopy tool
        objcopy_path = self.toolchain.get_objcopy_path()
        if objcopy_path is None or not objcopy_path.exists():
            raise ConfigurableLinkerError(
                f"objcopy not found: {objcopy_path}. "
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
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=30
            )

            if result.returncode != 0:
                error_msg = "Binary generation failed\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise ConfigurableLinkerError(error_msg)

            if not output_bin.exists():
                raise ConfigurableLinkerError(f"firmware.bin was not created: {output_bin}")

            if self.show_progress:
                size = output_bin.stat().st_size
                print(f"✓ Created firmware.bin: {size:,} bytes ({size / 1024 / 1024:.2f} MB)")

            return output_bin

        except subprocess.TimeoutExpired:
            raise ConfigurableLinkerError("Binary generation timeout")
        except Exception as e:
            raise ConfigurableLinkerError(f"Failed to generate binary: {e}")

    def generate_bootloader(self, output_bin: Optional[Path] = None) -> Path:
        """Generate bootloader.bin from bootloader ELF file.

        Args:
            output_bin: Optional path for output bootloader.bin

        Returns:
            Path to generated bootloader.bin

        Raises:
            ConfigurableLinkerError: If generation fails
        """
        if not self.mcu.startswith("esp32"):
            raise ConfigurableLinkerError(
                f"Bootloader generation only supported for ESP32 platforms, not {self.mcu}"
            )

        # Generate output path if not provided
        if output_bin is None:
            output_bin = self.build_dir / "bootloader.bin"

        # Get flash parameters from board config
        flash_mode = self.board_config.get("build", {}).get("flash_mode", "dio")
        flash_freq = self.board_config.get("build", {}).get("f_flash", "80m")
        flash_size = self.board_config.get("build", {}).get("flash_size", "4MB")

        # Convert frequency to esptool format if needed
        if isinstance(flash_freq, (int, float)):
            flash_freq = f"{int(flash_freq // 1000000)}m"
        elif isinstance(flash_freq, str) and flash_freq.endswith('L'):
            freq_value = int(flash_freq.rstrip('L'))
            flash_freq = f"{freq_value // 1000000}m"

        # Find bootloader ELF file in framework SDK
        bootloader_name = f"bootloader_{flash_mode}_{flash_freq.replace('m', 'm')}.elf"
        sdk_bin_dir = self.framework.get_sdk_dir() / self.mcu / "bin"
        bootloader_elf = sdk_bin_dir / bootloader_name

        if not bootloader_elf.exists():
            raise ConfigurableLinkerError(
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
        import sys
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
            result = subprocess.run(
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
                raise ConfigurableLinkerError(error_msg)

            if not output_bin.exists():
                raise ConfigurableLinkerError(f"bootloader.bin was not created: {output_bin}")

            if self.show_progress:
                size = output_bin.stat().st_size
                print(f"✓ Created bootloader.bin: {size:,} bytes ({size / 1024:.2f} KB)")

            return output_bin

        except subprocess.TimeoutExpired:
            raise ConfigurableLinkerError("Bootloader generation timeout")
        except Exception as e:
            raise ConfigurableLinkerError(f"Failed to generate bootloader: {e}")

    def generate_partition_table(self, output_bin: Optional[Path] = None) -> Path:
        """Generate partitions.bin from partition CSV file.

        Args:
            output_bin: Optional path for output partitions.bin

        Returns:
            Path to generated partitions.bin

        Raises:
            ConfigurableLinkerError: If generation fails
        """
        if not self.mcu.startswith("esp32"):
            raise ConfigurableLinkerError(
                f"Partition table generation only supported for ESP32 platforms, not {self.mcu}"
            )

        # Generate output path if not provided
        if output_bin is None:
            output_bin = self.build_dir / "partitions.bin"

        # Find partition CSV file - use default.csv from framework
        partitions_csv = self.framework.framework_path / "tools" / "partitions" / "default.csv"

        if not partitions_csv.exists():
            raise ConfigurableLinkerError(
                f"Partition CSV not found: {partitions_csv}"
            )

        # Find gen_esp32part.py tool - also in framework
        gen_tool = self.framework.framework_path / "tools" / "gen_esp32part.py"

        if not gen_tool.exists():
            raise ConfigurableLinkerError(
                f"Partition generation tool not found: {gen_tool}"
            )

        # Generate partition table using gen_esp32part.py
        import sys
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
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=30
            )

            if result.returncode != 0:
                error_msg = "Partition table generation failed\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise ConfigurableLinkerError(error_msg)

            if not output_bin.exists():
                raise ConfigurableLinkerError(f"partitions.bin was not created: {output_bin}")

            if self.show_progress:
                size = output_bin.stat().st_size
                print(f"✓ Created partitions.bin: {size:,} bytes")

            return output_bin

        except subprocess.TimeoutExpired:
            raise ConfigurableLinkerError("Partition table generation timeout")
        except Exception as e:
            raise ConfigurableLinkerError(f"Failed to generate partition table: {e}")

    def get_linker_info(self) -> Dict[str, Any]:
        """Get information about the linker configuration.

        Returns:
            Dictionary with linker information
        """
        info = {
            'board_id': self.board_id,
            'mcu': self.mcu,
            'build_dir': str(self.build_dir),
            'toolchain_type': self.toolchain.toolchain_type,
            'linker_path': str(self.toolchain.get_gxx_path()),
            'objcopy_path': str(self.toolchain.get_objcopy_path()),
        }

        # Add linker scripts
        try:
            scripts = self.get_linker_scripts()
            info['linker_scripts'] = [s.name for s in scripts]
            info['linker_script_count'] = len(scripts)
        except Exception as e:
            info['linker_scripts_error'] = str(e)

        # Add SDK libraries
        try:
            libs = self.get_sdk_libraries()
            info['sdk_library_count'] = len(libs)
            info['sdk_libraries_sample'] = [lib.name for lib in libs[:10]]
        except Exception as e:
            info['sdk_libraries_error'] = str(e)

        return info
