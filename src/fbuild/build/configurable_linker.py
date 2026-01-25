"""Configurable Linker.

This module provides a generic, configuration-driven linker that can link
for any platform (ESP32, AVR, etc.) based on platform configuration files.

Design:
    - Loads linker flags, scripts, libraries from JSON/Python config
    - Generic implementation replaces platform-specific linker classes
    - Same interface as ESP32Linker for drop-in replacement
"""

import gc
import json
import platform
import subprocess
import time
from pathlib import Path
from typing import List, Dict, Any, Optional, Union

from ..packages.package import IPackage, IToolchain, IFramework
from ..output import log_detail, format_size
from .binary_generator import BinaryGenerator
from .compiler import ILinker, LinkerError
from ..subprocess_utils import safe_run


class ConfigurableLinkerError(LinkerError):
    """Raised when configurable linking operations fail."""
    pass


def _path_to_string(path: Path, relative_to: Optional[Path] = None) -> str:
    """Convert a Path to a string suitable for Unix-based toolchains on Windows.

    The ESP32 toolchain binaries (ld.exe, g++.exe) are native Windows executables
    that understand Windows paths. When relative_to is provided, converts to a
    relative path with forward slashes (Unix-style) which the toolchain prefers.

    Args:
        path: Path object to convert
        relative_to: Optional base directory to make path relative to

    Returns:
        String representation (relative with forward slashes if relative_to provided, else absolute)
    """
    if relative_to is not None:
        try:
            # Try to make path relative to base directory
            rel_path = path.resolve().relative_to(relative_to.resolve())
            # Use forward slashes for relative paths (toolchain expects Unix-style)
            return rel_path.as_posix()
        except ValueError:
            # If path is not relative to base (e.g., toolchain paths), use absolute
            return str(path)
    return str(path)


class ConfigurableLinker(ILinker):
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
        platform: IPackage,
        toolchain: IToolchain,
        framework: IFramework,
        board_id: str,
        build_dir: Path,
        platform_config: Optional[Union[Dict, Path]] = None,
        show_progress: bool = True,
        env_config: Optional[Dict[str, Any]] = None
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
            env_config: Optional platformio.ini environment configuration
        """
        self.platform = platform
        self.toolchain = toolchain
        self.framework = framework
        self.board_id = board_id
        self.build_dir = build_dir
        self.show_progress = show_progress
        self.env_config = env_config or {}

        # Load board configuration
        self.board_config = platform.get_board_json(board_id)  # type: ignore[attr-defined]

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
                    f"No platform configuration found for {self.mcu}. " +
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

        # Initialize binary generator
        self.binary_generator = BinaryGenerator(
            mcu=self.mcu,
            board_config=self.board_config,
            build_dir=build_dir,
            toolchain=toolchain,
            framework=framework,
            show_progress=show_progress,
            env_config=self.env_config
        )

    def get_linker_scripts(self) -> List[Path]:
        """Get list of linker script paths for the MCU.

        Returns:
            List of .ld file paths in linking order
        """
        if self._linker_scripts_cache is not None:
            return self._linker_scripts_cache

        scripts = []

        # Check if framework has a get_linker_script method (Teensy-style)
        if hasattr(self.framework, 'get_linker_script'):
            linker_script = self.framework.get_linker_script(self.board_id)  # type: ignore[attr-defined]
            if linker_script and linker_script.exists():
                scripts.append(linker_script)

        # Otherwise use ESP32-style SDK directory approach
        elif hasattr(self.framework, 'get_sdk_dir'):
            # Apply SDK fallback for MCUs not fully supported in the platform
            # (e.g., esp32c2 can use esp32c3 SDK)
            from ..packages.sdk_utils import SDKPathResolver
            sdk_dir = self.framework.get_sdk_dir()  # type: ignore[attr-defined]
            resolver = SDKPathResolver(sdk_dir, show_progress=False)
            resolved_mcu = resolver._resolve_mcu(self.mcu)

            # Get linker script directory
            sdk_ld_dir = sdk_dir / resolved_mcu / "ld"

            if not sdk_ld_dir.exists():
                raise ConfigurableLinkerError(f"Linker script directory not found: {sdk_ld_dir}")

            # Get linker scripts from config
            config_scripts = self.config.get('linker_scripts', [])

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
                f"No linker scripts found for {self.mcu}"
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

        # Only ESP32 frameworks have SDK libraries
        if hasattr(self.framework, 'get_sdk_libs'):
            # Get flash mode from board configuration
            flash_mode = self.board_config.get("build", {}).get("flash_mode", "qio")

            # Get SDK libraries
            self._sdk_libs_cache = self.framework.get_sdk_libs(self.mcu, flash_mode)  # type: ignore[attr-defined]
        else:
            # No SDK libraries for this framework (e.g., Teensy)
            self._sdk_libs_cache = []

        return self._sdk_libs_cache or []

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
                f"Linker not found: {linker_path}. " +
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
        # Linker executable must remain absolute path
        cmd = [_path_to_string(linker_path)]
        cmd.extend(linker_flags)

        # Add linker script directory to library search path (ESP32-specific)
        if hasattr(self.framework, 'get_sdk_dir'):
            ld_dir = self.framework.get_sdk_dir() / self.mcu / "ld"  # type: ignore[attr-defined]
            # SDK directories stay absolute (outside build dir)
            cmd.append(f"-L{_path_to_string(ld_dir)}")

            # For ESP32-S3, also add flash mode directory to search path
            if self.mcu == "esp32s3":
                flash_mode = self.board_config.get("build", {}).get("flash_mode", "qio")
                psram_mode = self.board_config.get("build", {}).get("psram_mode", "qspi")
                flash_dir = self.framework.get_sdk_dir() / self.mcu / f"{flash_mode}_{psram_mode}"  # type: ignore[attr-defined]
                if flash_dir.exists():
                    cmd.append(f"-L{_path_to_string(flash_dir)}")

            # Add linker scripts with ESP32-specific path handling
            for script in linker_scripts:
                if script.parent == ld_dir or (self.mcu == "esp32s3" and script.parent.name.endswith(("_qspi", "_opi"))):
                    cmd.append(f"-T{script.name}")
                else:
                    # SDK scripts stay absolute (outside build dir)
                    cmd.append(f"-T{_path_to_string(script)}")
        else:
            # For non-ESP32 platforms (e.g., Teensy), use absolute paths
            for script in linker_scripts:
                cmd.append(f"-T{_path_to_string(script)}")

        # Add object files using relative paths (they're in build_dir)
        cmd.extend([_path_to_string(obj, relative_to=self.build_dir) for obj in object_files])

        # Add core archive using relative path (it's in build_dir)
        cmd.append(_path_to_string(core_archive, relative_to=self.build_dir))

        # Add SDK library directory to search path (ESP32-specific)
        if hasattr(self.framework, 'get_sdk_dir'):
            sdk_lib_dir = self.framework.get_sdk_dir() / self.mcu / "lib"  # type: ignore[attr-defined]
            if sdk_lib_dir.exists():
                # SDK directories stay absolute (outside build dir)
                cmd.append(f"-L{_path_to_string(sdk_lib_dir)}")

        # Group libraries to resolve circular dependencies
        cmd.append("-Wl,--start-group")

        # Add user library archives first
        for lib_archive in library_archives:
            if lib_archive.exists():
                # Library archives may be in build_dir or outside, try relative first
                cmd.append(_path_to_string(lib_archive, relative_to=self.build_dir))

        # Add SDK libraries (these stay absolute - outside build dir)
        for lib in sdk_libs:
            cmd.append(_path_to_string(lib))

        # Add standard libraries
        cmd.extend([
            "-lgcc",
            "-lstdc++",
            "-lm",
            "-lc",
        ])

        cmd.append("-Wl,--end-group")

        # Add output using relative path (it's in build_dir)
        cmd.extend(["-o", _path_to_string(output_elf, relative_to=self.build_dir)])

        # Execute linker
        if self.show_progress:
            log_detail("Linking firmware.elf...")
            log_detail(f"Object files: {len(object_files)}")
            log_detail(f"Core archive: {core_archive.name}")
            log_detail(f"SDK libraries: {len(sdk_libs)}")
            log_detail(f"Linker scripts: {len(linker_scripts)}")
            core_size = core_archive.stat().st_size if core_archive.exists() else 0
            lib_size = sum(a.stat().st_size for a in library_archives if a.exists())
            obj_size = sum(o.stat().st_size for o in object_files if o.exists())
            log_detail(f"Inputs: core ({format_size(core_size)}) + {len(library_archives)} libs ({format_size(lib_size)}) + {len(object_files)} objects ({format_size(obj_size)})")
            # Log the actual linker command for debugging
            log_detail(f"Linker command: {cmd[0]} ... ({len(cmd)} args total)")
            # Debug: show object file paths and conversion to relative
            if object_files:
                log_detail("Object file paths (first 3, converted to relative):")
                for i, obj in enumerate(object_files[:3]):
                    converted_path = _path_to_string(obj, relative_to=self.build_dir)
                    log_detail(f"  [{i}] {obj} -> {converted_path}")

        # Add retry logic for Windows file locking issues
        is_windows = platform.system() == "Windows"
        max_retries = 5 if is_windows else 1
        delay = 0.1

        try:
            for attempt in range(max_retries):
                # On Windows, force garbage collection and add delay before retry
                if is_windows and attempt > 0:
                    gc.collect()
                    time.sleep(delay)
                    if self.show_progress:
                        log_detail(f"Retrying linking (attempt {attempt + 1}/{max_retries})...")

                try:
                    # Debug: log full linker command on first attempt
                    if attempt == 0:
                        debug_cmd_file = self.build_dir / "linker_command.txt"
                        with open(debug_cmd_file, "w") as f:
                            f.write("LINKER COMMAND:\n")
                            f.write(f"CWD: {self.build_dir}\n\n")
                            for i, arg in enumerate(cmd):
                                f.write(f"  [{i}] {arg}\n")
                        if self.show_progress:
                            log_detail(f"Linker command saved to: {debug_cmd_file}")
                            log_detail(f"Working directory: {self.build_dir}")

                        # Debug: Test if the file is accessible with a simple subprocess call
                        test_file = self.build_dir / "obj" / "Blink.ino.o"
                        if test_file.exists():
                            log_detail(f"DEBUG: Object file exists: {test_file}")
                            log_detail(f"DEBUG: File size: {test_file.stat().st_size} bytes")
                            # Try to access it with a simple command
                            try:
                                test_result = safe_run(
                                    ["cmd.exe", "/c", "dir", "obj\\Blink.ino.o"],
                                    cwd=str(self.build_dir),
                                    capture_output=True,
                                    text=True,
                                    timeout=5
                                )
                                if test_result.returncode == 0:
                                    log_detail("DEBUG: File accessible via subprocess dir command")
                                else:
                                    log_detail(f"DEBUG: dir command failed: {test_result.stderr}")
                            except KeyboardInterrupt:
                                raise
                            except Exception as e:
                                log_detail(f"DEBUG: Test access failed: {e}")
                        else:
                            log_detail(f"DEBUG: Object file DOES NOT EXIST: {test_file}")

                    result = safe_run(
                        cmd,
                        capture_output=True,
                        text=True,
                        timeout=120,
                        cwd=str(self.build_dir)  # Run from build directory (must be string)
                    )
                except subprocess.TimeoutExpired:
                    if attempt < max_retries - 1:
                        if self.show_progress:
                            log_detail(f"[Timeout] Linker timeout (attempt {attempt + 1}/{max_retries}), retrying...")
                        delay = min(delay * 2, 2.0)  # Exponential backoff
                        continue
                    else:
                        raise ConfigurableLinkerError(f"Linking timeout after {max_retries} attempts (120s each)")

                if result.returncode != 0:
                    # Check if error is due to file truncation/locking (Windows-specific)
                    # Windows file locking manifests as: "file truncated", "error reading", "No such file", or "no more archived files"
                    stderr_lower = result.stderr.lower()
                    is_file_locking_error = (
                        "file truncated" in stderr_lower or
                        "error reading" in stderr_lower or
                        "no such file" in stderr_lower or
                        "no more archived files" in stderr_lower
                    )
                    if is_windows and is_file_locking_error:
                        if attempt < max_retries - 1:
                            if self.show_progress:
                                log_detail("[Windows] Detected file locking error, retrying...")
                            delay = min(delay * 2, 1.0)  # Exponential backoff, max 1s
                            continue
                        else:
                            # Last attempt failed
                            error_msg = f"Linking failed after {max_retries} attempts (file locking)\n"
                            error_msg += f"stderr: {result.stderr}\n"
                            error_msg += f"stdout: {result.stdout}"
                            raise ConfigurableLinkerError(error_msg)
                    else:
                        # Non-file-locking error, fail immediately
                        error_msg = "Linking failed\n"
                        error_msg += f"stderr: {result.stderr}\n"
                        error_msg += f"stdout: {result.stdout}"
                        # Log stderr to output file so clients can see it
                        if result.stderr:
                            log_detail(f"LINKER ERROR: {result.stderr[:1000]}")
                        raise ConfigurableLinkerError(error_msg)

                # Success - linker returned 0
                break

            if not output_elf.exists():
                raise ConfigurableLinkerError(f"firmware.elf was not created: {output_elf}")

            if self.show_progress:
                size = output_elf.stat().st_size
                log_detail(f"Created firmware.elf: {format_size(size)}")

            return output_elf

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
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
        try:
            return self.binary_generator.generate_bin(elf_path, output_bin)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ConfigurableLinkerError(f"Binary generation failed: {e}")

    def generate_hex(self, elf_path: Path, output_hex: Optional[Path] = None) -> Path:
        """Generate firmware.hex from firmware.elf using objcopy.

        Args:
            elf_path: Path to firmware.elf
            output_hex: Optional path for output .hex file

        Returns:
            Path to generated firmware.hex

        Raises:
            ConfigurableLinkerError: If conversion fails
        """
        if not elf_path.exists():
            raise ConfigurableLinkerError(f"ELF file not found: {elf_path}")

        # Generate output path if not provided
        if output_hex is None:
            output_hex = self.build_dir / "firmware.hex"

        # Get objcopy tool from toolchain
        objcopy_path = self.toolchain.get_objcopy_path()
        if objcopy_path is None or not objcopy_path.exists():
            raise ConfigurableLinkerError(
                f"objcopy not found: {objcopy_path}. " +
                "Ensure toolchain is installed."
            )

        # Build objcopy command: convert ELF to Intel HEX format
        cmd = [
            str(objcopy_path),
            "-O", "ihex",
            "-R", ".eeprom",
            str(elf_path),
            str(output_hex)
        ]

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=True,
                timeout=30
            )

            if result.returncode != 0:
                error_msg = "HEX generation failed\n"
                error_msg += f"stderr: {result.stderr}\n"
                error_msg += f"stdout: {result.stdout}"
                raise ConfigurableLinkerError(error_msg)

            if not output_hex.exists():
                raise ConfigurableLinkerError(f"firmware.hex was not created: {output_hex}")

            if self.show_progress:
                size = output_hex.stat().st_size
                log_detail(f"Created firmware.hex: {format_size(size)}")

            return output_hex

        except subprocess.TimeoutExpired:
            raise ConfigurableLinkerError("HEX generation timeout")
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ConfigurableLinkerError(f"Failed to generate HEX: {e}")

    def get_size_info(self, elf_path: Path):
        """Get firmware size information from ELF file.

        Args:
            elf_path: Path to firmware.elf

        Returns:
            SizeInfo object with size data or None if failed

        Raises:
            ConfigurableLinkerError: If size calculation fails
        """
        from .linker import SizeInfo

        if not elf_path.exists():
            raise ConfigurableLinkerError(f"ELF file not found: {elf_path}")

        # Get arm-none-eabi-size or appropriate size tool from toolchain
        # Check if toolchain has a get_size_path method
        if hasattr(self.toolchain, 'get_size_path'):
            size_tool = self.toolchain.get_size_path()
        else:
            # Fall back to looking for size tool in toolchain bin directory
            gcc_path = self.toolchain.get_gcc_path()
            if gcc_path is None:
                return None
            toolchain_bin = gcc_path.parent
            size_tool = toolchain_bin / "arm-none-eabi-size"
            if not size_tool.exists():
                size_tool = toolchain_bin / "arm-none-eabi-size.exe"

        if size_tool and not size_tool.exists():
            # If we can't find the size tool, return None (non-fatal)
            return None

        try:
            result = safe_run(
                [str(size_tool), str(elf_path)],
                capture_output=True,
                text=True,
                timeout=10
            )

            if result.returncode == 0:
                # Get max flash and RAM from board config
                max_flash = self.board_config.get("upload", {}).get("maximum_size")
                max_ram = self.board_config.get("upload", {}).get("maximum_ram_size")

                return SizeInfo.parse(
                    result.stdout,
                    max_flash=max_flash,
                    max_ram=max_ram
                )
            else:
                return None

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            return None

    def generate_bootloader(self, output_bin: Optional[Path] = None) -> Path:
        """Generate bootloader.bin from bootloader ELF file.

        Args:
            output_bin: Optional path for output bootloader.bin

        Returns:
            Path to generated bootloader.bin

        Raises:
            ConfigurableLinkerError: If generation fails
        """
        try:
            return self.binary_generator.generate_bootloader(output_bin)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ConfigurableLinkerError(f"Bootloader generation failed: {e}")

    def generate_partition_table(self, output_bin: Optional[Path] = None) -> Path:
        """Generate partitions.bin from partition CSV file.

        Args:
            output_bin: Optional path for output partitions.bin

        Returns:
            Path to generated partitions.bin

        Raises:
            ConfigurableLinkerError: If generation fails
        """
        try:
            return self.binary_generator.generate_partition_table(output_bin)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ConfigurableLinkerError(f"Partition table generation failed: {e}")

    def generate_merged_bin(self, output_bin: Optional[Path] = None) -> Path:
        """Generate merged.bin combining bootloader, partition table, and firmware.

        Args:
            output_bin: Optional path for output merged.bin

        Returns:
            Path to generated merged.bin

        Raises:
            ConfigurableLinkerError: If generation fails
        """
        try:
            return self.binary_generator.generate_merged_bin(output_bin)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            raise ConfigurableLinkerError(f"Merged bin generation failed: {e}")

    def get_linker_info(self) -> Dict[str, Any]:
        """Get information about the linker configuration.

        Returns:
            Dictionary with linker information
        """
        info = {
            'board_id': self.board_id,
            'mcu': self.mcu,
            'build_dir': str(self.build_dir),
            'toolchain_type': self.toolchain.toolchain_type,  # type: ignore[attr-defined]
            'linker_path': str(self.toolchain.get_gxx_path()),
            'objcopy_path': str(self.toolchain.get_objcopy_path()),
        }

        # Add linker scripts
        try:
            scripts = self.get_linker_scripts()
            info['linker_scripts'] = [s.name for s in scripts]
            info['linker_script_count'] = len(scripts)
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            info['linker_scripts_error'] = str(e)

        # Add SDK libraries
        try:
            libs = self.get_sdk_libraries()
            info['sdk_library_count'] = len(libs)
            info['sdk_libraries_sample'] = [lib.name for lib in libs[:10]]
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            info['sdk_libraries_error'] = str(e)

        return info
