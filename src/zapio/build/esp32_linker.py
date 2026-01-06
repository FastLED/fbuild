"""ESP32 Linker.

This module handles linking of ESP32 Arduino sketches, core, and libraries.

Linking Process:
    1. Collect all object files (sketch, core, libraries)
    2. Collect linker scripts from SDK
    3. Collect ESP-IDF precompiled libraries
    4. Link everything into firmware.elf
    5. Generate firmware.bin from .elf

Linking Strategy:
    - Use GCC/G++ from toolchain (riscv32-esp-elf-g++ or xtensa-esp32-elf-g++)
    - Use linker scripts from SDK (memory.ld, sections.ld, rom .ld files)
    - Link with core.a, ESP-IDF .a libraries
    - Generate .elf firmware
    - Convert .elf to .bin using objcopy
"""

import subprocess
from pathlib import Path
from typing import List, Dict, Any, Optional

from ..packages.esp32_platform import ESP32Platform
from ..packages.esp32_toolchain import ESP32Toolchain
from ..packages.esp32_framework import ESP32Framework


class ESP32LinkerError(Exception):
    """Raised when ESP32 linking operations fail."""
    pass


class ESP32Linker:
    """Manages ESP32 linking process.

    This class handles:
    - Linker script management
    - ESP-IDF library collection
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
        show_progress: bool = True
    ):
        """Initialize ESP32 linker.

        Args:
            platform: ESP32 platform instance
            toolchain: ESP32 toolchain instance
            framework: ESP32 framework instance
            board_id: Board identifier (e.g., "esp32-c6-devkitm-1")
            build_dir: Directory for build artifacts
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
            raise ESP32LinkerError(f"Linker script directory not found: {sdk_ld_dir}")

        # Main linker scripts (in order)
        # These are the primary linker scripts that must be included
        scripts = []

        # Memory layout (mandatory for all)
        memory_ld = sdk_ld_dir / "memory.ld"
        if not memory_ld.exists():
            raise ESP32LinkerError(f"Required linker script not found: {memory_ld}")
        scripts.append(memory_ld)

        # Section definitions (varies by MCU and flash mode)
        # ESP32-S3 stores sections.ld in flash mode subdirectories
        sections_ld = sdk_ld_dir / "sections.ld"
        if not sections_ld.exists() and self.mcu == "esp32s3":
            # ESP32-S3 has flash mode-specific sections.ld files
            flash_mode = self.board_config.get("build", {}).get("flash_mode", "qio")
            psram_mode = self.board_config.get("build", {}).get("psram_mode", "qspi")

            # ESP32-S3 flash mode directories: qio_qspi, dio_qspi, qio_opi, dio_opi, opi_opi
            flash_dir = f"{flash_mode}_{psram_mode}"
            sections_ld = sdk_ld_dir.parent / flash_dir / "sections.ld"

        if sections_ld.exists():
            scripts.append(sections_ld)

        # ROM symbols (mandatory for all)
        rom_ld = sdk_ld_dir / f"{self.mcu}.rom.ld"
        if not rom_ld.exists():
            raise ESP32LinkerError(f"Required linker script not found: {rom_ld}")
        scripts.append(rom_ld)

        # Additional ROM linker scripts (optional but recommended)
        # These provide symbols for ROM functions that are commonly used
        rom_scripts = [
            f"{self.mcu}.rom.api.ld",              # ROM API functions (MCU-specific)
            f"{self.mcu}.rom.libgcc.ld",           # GCC built-in functions in ROM
            f"{self.mcu}.rom.newlib.ld",           # Newlib C library functions in ROM
            f"{self.mcu}.rom.newlib-data.ld",      # Newlib data (_global_impure_ptr, etc.)
            f"{self.mcu}.rom.newlib-funcs.ld",     # Newlib functions
            f"{self.mcu}.rom.newlib-reent-funcs.ld",  # Newlib reentrant functions
            f"{self.mcu}.rom.newlib-time.ld",      # Newlib time functions
            f"{self.mcu}.rom.newlib-locale.ld",    # Newlib locale functions
            f"{self.mcu}.rom.newlib-nano.ld",      # Newlib nano version
            f"{self.mcu}.rom.newlib-normal.ld",    # Newlib normal version
            f"{self.mcu}.rom.libc.ld",             # C library (_global_impure_ptr, syscall_table_ptr)
            f"{self.mcu}.rom.libc-funcs.ld",       # C library functions
            f"{self.mcu}.rom.spiflash.ld",         # SPI flash functions (spi_flash_*, esp_flash_*)
            f"{self.mcu}.rom.spiflash_legacy.ld",  # Legacy SPI flash functions
            f"{self.mcu}.rom.coexist.ld",          # WiFi/BT coexistence (g_coa_funcs_p)
            f"{self.mcu}.rom.heap.ld",             # Heap management (heap_tlsf_table_ptr)
            f"{self.mcu}.rom.wdt.ld",              # Watchdog timer HAL (wdt_hal_*)
            f"{self.mcu}.rom.systimer.ld",         # System timer HAL (systimer_hal_*)
            f"{self.mcu}.rom.syscalls.ld",         # System calls
            f"{self.mcu}.rom.eco3.ld",             # ECO3 chip revision fixes
            f"{self.mcu}.rom.eco7.ld",             # ECO7 chip revision fixes
            f"{self.mcu}.rom.redefined.ld",        # Redefined symbols
            f"{self.mcu}.rom.version.ld",          # ROM version
            f"{self.mcu}.rom.phy.ld",              # PHY functions
            f"{self.mcu}.rom.pp.ld",               # PP functions
            f"{self.mcu}.rom.net80211.ld",         # Net80211 functions
            f"{self.mcu}.rom.rvfp.ld",             # RISC-V floating point
            f"{self.mcu}.rom.ble_50.ld",           # BLE 5.0 functions
            f"{self.mcu}.rom.ble_cca.ld",          # BLE CCA functions
            f"{self.mcu}.rom.ble_dtm.ld",          # BLE DTM functions
            f"{self.mcu}.rom.ble_master.ld",       # BLE master functions
            f"{self.mcu}.rom.ble_scan.ld",         # BLE scan functions
            f"{self.mcu}.rom.ble_smp.ld",          # BLE SMP functions
            f"{self.mcu}.rom.ble_test.ld",         # BLE test functions
            f"{self.mcu}.rom.bt_funcs.ld",         # Bluetooth functions
            f"{self.mcu}.rom.eco3_bt_funcs.ld",    # ECO3 BT functions
            f"{self.mcu}.rom.eco7_bt_funcs.ld",    # ECO7 BT functions
            f"{self.mcu}.peripherals.ld",          # Peripheral register addresses
            "rom.api.ld",                          # Generic ROM API aliases (some MCUs have this)
        ]

        for script_name in rom_scripts:
            script_path = sdk_ld_dir / script_name
            if script_path.exists():
                scripts.append(script_path)

        self._linker_scripts_cache = scripts
        return scripts

    def get_sdk_libraries(self) -> List[Path]:
        """Get list of ESP-IDF precompiled libraries.

        Returns:
            List of .a library file paths
        """
        if self._sdk_libs_cache is not None:
            return self._sdk_libs_cache

        # Get flash mode from board configuration
        flash_mode = self.board_config.get("build", {}).get("flash_mode", "qio")

        # Get SDK libraries (including flash mode-specific library)
        self._sdk_libs_cache = self.framework.get_sdk_libs(self.mcu, flash_mode)
        return self._sdk_libs_cache

    def get_linker_flags(self) -> List[str]:
        """Get linker flags from board and platform configuration.

        Returns:
            List of linker flags
        """
        flags = []

        # Get SDK flags directory
        sdk_flags_dir = self.framework.get_sdk_flags_dir(self.mcu)

        # Read linker flags from SDK
        ld_flags_file = sdk_flags_dir / "ld_flags"
        if ld_flags_file.exists():
            with open(ld_flags_file, 'r') as f:
                ld_flags_content = f.read().strip()
                # Parse flags carefully
                import shlex
                try:
                    flags.extend(shlex.split(ld_flags_content))
                except Exception:
                    flags.extend(ld_flags_content.split())

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
            ESP32LinkerError: If linking fails
        """
        if not object_files:
            raise ESP32LinkerError("No object files provided for linking")

        if not core_archive.exists():
            raise ESP32LinkerError(f"Core archive not found: {core_archive}")

        # Initialize library archives list
        if library_archives is None:
            library_archives = []

        # Get linker tool (use g++ for C++ support)
        linker_path = self.toolchain.get_gxx_path()
        if linker_path is None or not linker_path.exists():
            raise ESP32LinkerError(
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
            # Use just the name if it's in the ld_dir, otherwise use full path
            if script.parent == ld_dir or (self.mcu == "esp32s3" and script.parent.name.endswith(("_qspi", "_opi"))):
                cmd.append(f"-T{script.name}")  # Use just the name since we added -L
            else:
                cmd.append(f"-T{script}")  # Use full path

        # Add object files
        cmd.extend([str(obj) for obj in object_files])

        # Add core archive
        cmd.append(str(core_archive))

        # Add SDK library directory to search path
        sdk_lib_dir = self.framework.get_sdk_dir() / self.mcu / "lib"
        if sdk_lib_dir.exists():
            cmd.append(f"-L{sdk_lib_dir}")

        # Add SDK libraries
        # Group libraries to resolve circular dependencies
        cmd.append("-Wl,--start-group")

        # Add user library archives first (so they can reference SDK libs)
        for lib_archive in library_archives:
            if lib_archive.exists():
                cmd.append(str(lib_archive))

        # Add SDK libraries
        for lib in sdk_libs:
            cmd.append(str(lib))

        # Add standard libraries (must be in the group for circular deps)
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
                raise ESP32LinkerError(error_msg)

            if not output_elf.exists():
                raise ESP32LinkerError(f"firmware.elf was not created: {output_elf}")

            if self.show_progress:
                size = output_elf.stat().st_size
                print(f"✓ Created firmware.elf: {size:,} bytes ({size / 1024 / 1024:.2f} MB)")

            return output_elf

        except subprocess.TimeoutExpired:
            raise ESP32LinkerError("Linking timeout")
        except Exception as e:
            raise ESP32LinkerError(f"Failed to link: {e}")

    def generate_bin(self, elf_path: Path, output_bin: Optional[Path] = None) -> Path:
        """Generate firmware.bin from firmware.elf.

        Args:
            elf_path: Path to firmware.elf
            output_bin: Optional path for output .bin file

        Returns:
            Path to generated firmware.bin

        Raises:
            ESP32LinkerError: If conversion fails
        """
        if not elf_path.exists():
            raise ESP32LinkerError(f"ELF file not found: {elf_path}")

        # Get objcopy tool
        objcopy_path = self.toolchain.get_objcopy_path()
        if objcopy_path is None or not objcopy_path.exists():
            raise ESP32LinkerError(
                f"objcopy not found: {objcopy_path}. "
                "Ensure toolchain is installed."
            )

        # Generate output path if not provided
        if output_bin is None:
            output_bin = self.build_dir / "firmware.bin"

        # Build objcopy command
        # objcopy -O binary firmware.elf firmware.bin
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
                error_msg += f"Command: {' '.join(cmd)}\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise ESP32LinkerError(error_msg)

            if not output_bin.exists():
                raise ESP32LinkerError(f"firmware.bin was not created: {output_bin}")

            if self.show_progress:
                size = output_bin.stat().st_size
                print(f"✓ Created firmware.bin: {size:,} bytes ({size / 1024 / 1024:.2f} MB)")

            return output_bin

        except subprocess.TimeoutExpired:
            raise ESP32LinkerError("Binary generation timeout")
        except Exception as e:
            raise ESP32LinkerError(f"Failed to generate binary: {e}")

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
