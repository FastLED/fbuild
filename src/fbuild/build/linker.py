"""
AVR linker wrapper for creating firmware binaries.

This module provides a wrapper around avr-gcc linker, avr-ar, avr-objcopy,
and avr-size for linking object files into firmware.
"""

import subprocess
import time
from pathlib import Path
from typing import List, Optional
from dataclasses import dataclass

from .compiler import ILinker, LinkerError
from ..subprocess_utils import safe_run


@dataclass
class SizeInfo:
    """Firmware size information."""

    text: int  # Program memory (flash) usage in bytes
    data: int  # Initialized data in RAM
    bss: int   # Uninitialized data in RAM
    total_flash: int  # Total flash used (text + data)
    total_ram: int    # Total RAM used (data + bss)
    max_flash: Optional[int] = None  # Maximum flash available
    max_ram: Optional[int] = None    # Maximum RAM available

    @property
    def flash_percent(self) -> Optional[float]:
        """Calculate flash usage percentage."""
        if self.max_flash:
            return (self.total_flash / self.max_flash) * 100
        return None

    @property
    def ram_percent(self) -> Optional[float]:
        """Calculate RAM usage percentage."""
        if self.max_ram:
            return (self.total_ram / self.max_ram) * 100
        return None

    @staticmethod
    def parse(size_output: str, max_flash: Optional[int] = None,
              max_ram: Optional[int] = None) -> 'SizeInfo':
        """
        Parse size tool output in either Berkeley or AVR format.

        Supports two output formats:
        1. Berkeley format (default for ESP32):
           text    data     bss     dec     hex filename
           123456   12345    4567  140368   2242c firmware.elf

        2. AVR section format (avr-size -A):
           .text            13558          0
           .data               50   8388864
           .bss               580   8388914

        Args:
            size_output: Output from size command (Berkeley or AVR format)
            max_flash: Maximum flash size for board
            max_ram: Maximum RAM size for board

        Returns:
            SizeInfo object with parsed size data
        """
        text = 0
        data = 0
        bss = 0

        lines = size_output.strip().split('\n')

        # Try Berkeley format first (ESP32, ARM, etc.)
        # Format: "   text    data     bss     dec     hex filename"
        # Values: " 123456   12345    4567  140368   2242c firmware.elf"
        for line in lines:
            # Skip header line that contains column names
            stripped = line.strip()
            if stripped.startswith('text') or not stripped:
                continue

            # Try to parse as Berkeley format (6 columns: text, data, bss, dec, hex, filename)
            parts = stripped.split()
            if len(parts) >= 6:
                try:
                    # Berkeley format has: text, data, bss, dec_total, hex_total, filename
                    maybe_text = int(parts[0])
                    maybe_data = int(parts[1])
                    maybe_bss = int(parts[2])
                    # Validate by checking if dec column matches
                    maybe_dec = int(parts[3])
                    if maybe_dec == maybe_text + maybe_data + maybe_bss:
                        # This is Berkeley format
                        text = maybe_text
                        data = maybe_data
                        bss = maybe_bss
                        break
                except (ValueError, IndexError):
                    pass

            # Try AVR section format (section name, size, address)
            if len(parts) >= 2:
                section = parts[0]
                try:
                    size = int(parts[1])
                    if section == '.text':
                        text = size
                    elif section == '.data':
                        data = size
                    elif section == '.bss':
                        bss = size
                except (ValueError, IndexError):
                    continue

        total_flash = text + data
        total_ram = data + bss

        return SizeInfo(
            text=text,
            data=data,
            bss=bss,
            total_flash=total_flash,
            total_ram=total_ram,
            max_flash=max_flash,
            max_ram=max_ram
        )


@dataclass
class LinkResult:
    """Result of linking operation."""

    success: bool
    elf_path: Optional[Path]
    hex_path: Optional[Path]
    size_info: Optional[SizeInfo]
    stdout: str
    stderr: str


class LinkerAVR(ILinker):
    """
    Wrapper for AVR linker tools.

    Links object files into firmware binaries using avr-gcc, avr-ar,
    avr-objcopy, and avr-size.
    """

    def __init__(
        self,
        avr_gcc: Path,
        avr_ar: Path,
        avr_objcopy: Path,
        avr_size: Path,
        mcu: str,
        max_flash: Optional[int] = None,
        max_ram: Optional[int] = None
    ):
        """
        Initialize linker.

        Args:
            avr_gcc: Path to avr-gcc executable (used for linking)
            avr_ar: Path to avr-ar executable (for creating archives)
            avr_objcopy: Path to avr-objcopy (for ELF to HEX conversion)
            avr_size: Path to avr-size (for size reporting)
            mcu: MCU type (e.g., atmega328p)
            max_flash: Maximum flash size for overflow detection
            max_ram: Maximum RAM size for overflow detection
        """
        self.avr_gcc = Path(avr_gcc)
        self.avr_ar = Path(avr_ar)
        self.avr_objcopy = Path(avr_objcopy)
        self.avr_size = Path(avr_size)
        self.mcu = mcu
        self.max_flash = max_flash
        self.max_ram = max_ram

        # Verify tools exist
        if not self.avr_gcc.exists():
            raise LinkerError(f"avr-gcc not found: {self.avr_gcc}")
        if not self.avr_ar.exists():
            raise LinkerError(f"avr-ar not found: {self.avr_ar}")
        if not self.avr_objcopy.exists():
            raise LinkerError(f"avr-objcopy not found: {self.avr_objcopy}")
        if not self.avr_size.exists():
            raise LinkerError(f"avr-size not found: {self.avr_size}")

    def link_legacy(
        self,
        sketch_objects: List[Path],
        core_objects: List[Path],
        output_elf: Path,
        output_hex: Path,
        lib_archives: Optional[List[Path]] = None,
        extra_flags: Optional[List[str]] = None,
        additional_objects: Optional[List[Path]] = None
    ) -> LinkResult:
        """
        Link object files into firmware (LEGACY method - use link() instead).

        Process:
        1. Link sketch objects + core objects + additional objects + library archives to create .elf
        2. Convert .elf to .hex using avr-objcopy
        3. Get size information using avr-size

        Note: Core objects are passed directly instead of being archived because
        LTO with -fno-fat-lto-objects produces bytecode-only objects that don't
        work well in archives (the archive won't have a proper symbol index).

        Args:
            sketch_objects: User sketch object files
            core_objects: Arduino core object files
            output_elf: Output .elf file path
            output_hex: Output .hex file path
            lib_archives: Optional list of library archive (.a) files
            extra_flags: Additional linker flags
            additional_objects: Optional additional object files (e.g., library objects for LTO)

        Returns:
            LinkResult with linking status and size info
        """
        try:
            # Create build directory if needed
            output_elf.parent.mkdir(parents=True, exist_ok=True)

            # Link to .elf - pass core objects directly instead of archiving
            link_result = self._link_elf(
                sketch_objects,
                core_objects,
                output_elf,
                lib_archives or [],
                extra_flags or [],
                additional_objects
            )

            if not link_result or link_result.returncode != 0:
                return LinkResult(
                    success=False,
                    elf_path=None,
                    hex_path=None,
                    size_info=None,
                    stdout=link_result.stdout if link_result else '',
                    stderr=link_result.stderr if link_result else 'Linking failed'
                )

            # Step 3: Convert to .hex
            if not self._objcopy_hex(output_elf, output_hex):
                return LinkResult(
                    success=False,
                    elf_path=output_elf if output_elf.exists() else None,
                    hex_path=None,
                    size_info=None,
                    stdout=link_result.stdout,
                    stderr='Failed to convert ELF to HEX'
                )

            # Step 4: Get size info
            size_info = self._get_size(output_elf)

            # Check for flash overflow
            if size_info and self.max_flash and size_info.total_flash > self.max_flash:
                return LinkResult(
                    success=False,
                    elf_path=output_elf,
                    hex_path=output_hex,
                    size_info=size_info,
                    stdout=link_result.stdout,
                    stderr=f'Sketch too large: {size_info.total_flash} bytes (maximum is {self.max_flash} bytes)'
                )

            return LinkResult(
                success=True,
                elf_path=output_elf,
                hex_path=output_hex,
                size_info=size_info,
                stdout=link_result.stdout,
                stderr=link_result.stderr
            )

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            return LinkResult(
                success=False,
                elf_path=None,
                hex_path=None,
                size_info=None,
                stdout='',
                stderr=f'Linking exception: {str(e)}'
            )

    def _create_core_archive(
        self,
        core_objects: List[Path],
        archive_path: Path
    ) -> bool:
        """
        Create core.a archive from core object files.

        Args:
            core_objects: List of core .o files
            archive_path: Output archive path

        Returns:
            True if successful, False otherwise
        """
        if not core_objects:
            return True  # No core objects, nothing to archive

        # Remove existing archive to avoid issues on Windows
        # Try multiple times with delays if deletion fails
        if archive_path.exists():
            for _ in range(5):
                try:
                    archive_path.unlink()
                    break
                except PermissionError:
                    # File might be locked, wait a bit
                    time.sleep(0.05)
                except KeyboardInterrupt as ke:
                    from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
                    handle_keyboard_interrupt_properly(ke)
                except Exception:
                    # Other error, just continue
                    break

        cmd = [
            str(self.avr_ar),
            'rcs',  # r=replace, c=create, s=index
            str(archive_path)
        ]
        cmd.extend(str(obj) for obj in core_objects)

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )

            # Check if command succeeded
            if result.returncode != 0:
                return False

            # On Windows, there might be a slight delay before file appears
            # Try checking existence with a small delay if needed
            for attempt in range(10):  # Try up to 10 times
                if archive_path.exists():
                    return True
                time.sleep(0.02 * (attempt + 1))  # Exponential backoff: 20ms, 40ms, 60ms...

            return False
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            return False

    def _link_elf(
        self,
        sketch_objects: List[Path],
        core_objects: List[Path],
        output_elf: Path,
        lib_archives: List[Path],
        extra_flags: List[str],
        additional_objects: Optional[List[Path]] = None
    ):
        """
        Link objects and archives to create .elf file.

        Args:
            sketch_objects: Sketch object files
            core_objects: Core object files (passed directly for LTO)
            output_elf: Output .elf file
            lib_archives: Library archives (.a files)
            extra_flags: Additional linker flags
            additional_objects: Additional object files (e.g., library objects for LTO)

        Returns:
            subprocess.CompletedProcess result
        """
        cmd = [
            str(self.avr_gcc),
            '-w',              # Suppress warnings
            '-Os',             # Optimize for size
            '-g',              # Include debug info
            '-flto',           # Link-time optimization
            '-fuse-linker-plugin',  # Use LTO plugin
            '-Wl,--gc-sections',    # Garbage collect unused sections
            '-Wl,--allow-multiple-definition',  # Allow multiple definitions (needed for some libraries like FastLED)
            f'-mmcu={self.mcu}',    # Target MCU
            '-o', str(output_elf)
        ]

        # Add sketch objects
        cmd.extend(str(obj) for obj in sketch_objects)

        # Add core objects (passed directly for LTO compatibility)
        cmd.extend(str(obj) for obj in core_objects)

        # Add additional objects (e.g., library objects for LTO)
        if additional_objects:
            cmd.extend(str(obj) for obj in additional_objects)

        # Start group for circular dependencies
        cmd.append('-Wl,--start-group')

        # Add library archives
        for lib_archive in lib_archives:
            if lib_archive.exists():
                cmd.append(str(lib_archive))

        # Add math library
        cmd.append('-lm')

        # End group
        cmd.append('-Wl,--end-group')

        # Add library path
        cmd.append(f'-L{output_elf.parent}')

        # Add extra flags
        cmd.extend(extra_flags)

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )
            return result
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            from types import SimpleNamespace
            return SimpleNamespace(
                returncode=-1,
                stdout='',
                stderr=str(e)
            )

    def _objcopy_hex(self, elf_path: Path, hex_path: Path) -> bool:
        """
        Convert .elf to .hex using avr-objcopy.

        Args:
            elf_path: Input .elf file
            hex_path: Output .hex file

        Returns:
            True if successful, False otherwise
        """
        cmd = [
            str(self.avr_objcopy),
            '-O', 'ihex',      # Intel HEX format
            '-R', '.eeprom',   # Remove EEPROM section
            str(elf_path),
            str(hex_path)
        ]

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )
            return result.returncode == 0 and hex_path.exists()
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            return False

    def _get_size(self, elf_path: Path) -> Optional[SizeInfo]:
        """
        Get firmware size information.

        Args:
            elf_path: Path to .elf file

        Returns:
            SizeInfo object or None if failed
        """
        cmd = [
            str(self.avr_size),
            '-A',  # Berkeley format with section details
            str(elf_path)
        ]

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )

            if result.returncode == 0:
                return SizeInfo.parse(
                    result.stdout,
                    self.max_flash,
                    self.max_ram
                )
            return None
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            return None

    def create_eep(self, elf_path: Path, eep_path: Path) -> bool:
        """
        Extract EEPROM data to .eep file (optional).

        Args:
            elf_path: Input .elf file
            eep_path: Output .eep file

        Returns:
            True if successful, False otherwise
        """
        cmd = [
            str(self.avr_objcopy),
            '-O', 'ihex',
            '-j', '.eeprom',
            '--set-section-flags=.eeprom=alloc,load',
            '--no-change-warnings',
            '--change-section-lma', '.eeprom=0',
            str(elf_path),
            str(eep_path)
        ]

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )
            return result.returncode == 0
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            return False

    # BaseLinker interface implementation
    def link(
        self,
        sketch_objects: List[Path],
        core_archive: Path,
        output_elf: Optional[Path] = None,
        library_archives: Optional[List[Path]] = None
    ) -> Path:
        """Link object files into firmware ELF (BaseLinker interface).

        This is the BaseLinker interface method. For the legacy method with more
        options, see link_with_options().

        Args:
            sketch_objects: List of sketch object files
            core_archive: Core archive file (core.a)
            output_elf: Optional path for output .elf file
            library_archives: Optional list of library archives

        Returns:
            Path to generated .elf file

        Raises:
            LinkerError: If linking fails
        """
        # Generate default paths
        if output_elf is None:
            output_elf = Path.cwd() / "build" / "firmware.elf"

        output_hex = output_elf.with_suffix('.hex')

        # For AVR with LTO, we don't use core.a directly.
        # Instead, extract object files from the archive if it exists
        core_objects = []
        if core_archive.exists() and core_archive.suffix == '.a':
            # For now, we'll assume core objects are in the same directory
            # This is a simplification; in practice, the orchestrator should
            # pass core objects directly
            core_obj_dir = core_archive.parent / "core"
            if core_obj_dir.exists():
                core_objects = list(core_obj_dir.glob("*.o"))

        # Use the legacy link method
        result = self.link_with_options(
            sketch_objects=sketch_objects,
            core_objects=core_objects,
            output_elf=output_elf,
            output_hex=output_hex,
            lib_archives=library_archives,
            extra_flags=None,
            additional_objects=None
        )

        if not result.success:
            raise LinkerError(f"Linking failed: {result.stderr}")

        if result.elf_path is None:
            raise LinkerError("Linking succeeded but no ELF file was generated")

        return result.elf_path

    def generate_bin(self, elf_path: Path) -> Path:
        """Generate binary from ELF file (BaseLinker interface).

        For AVR: Generates .hex (Intel HEX format)

        Args:
            elf_path: Path to firmware.elf file

        Returns:
            Path to generated binary file (.hex for AVR)

        Raises:
            LinkerError: If binary generation fails
        """
        hex_path = elf_path.with_suffix('.hex')

        if not self._objcopy_hex(elf_path, hex_path):
            raise LinkerError(f"Failed to generate HEX from {elf_path}")

        return hex_path

    def link_with_options(
        self,
        sketch_objects: List[Path],
        core_objects: List[Path],
        output_elf: Path,
        output_hex: Path,
        lib_archives: Optional[List[Path]] = None,
        extra_flags: Optional[List[str]] = None,
        additional_objects: Optional[List[Path]] = None
    ) -> LinkResult:
        """Link object files into firmware (legacy method with full options).

        This is the original link method with all the AVR-specific options.
        Process:
        1. Link sketch objects + core objects + additional objects + library archives to create .elf
        2. Convert .elf to .hex using avr-objcopy
        3. Get size information using avr-size

        Note: Core objects are passed directly instead of being archived because
        LTO with -fno-fat-lto-objects produces bytecode-only objects that don't
        work well in archives (the archive won't have a proper symbol index).

        Args:
            sketch_objects: User sketch object files
            core_objects: Arduino core object files
            output_elf: Output .elf file path
            output_hex: Output .hex file path
            lib_archives: Optional list of library archive (.a) files
            extra_flags: Additional linker flags
            additional_objects: Optional additional object files (e.g., library objects for LTO)

        Returns:
            LinkResult with linking status and size info
        """
        try:
            # Create build directory if needed
            output_elf.parent.mkdir(parents=True, exist_ok=True)

            # Link to .elf - pass core objects directly instead of archiving
            link_result = self._link_elf(
                sketch_objects,
                core_objects,
                output_elf,
                lib_archives or [],
                extra_flags or [],
                additional_objects
            )

            if not link_result or link_result.returncode != 0:
                return LinkResult(
                    success=False,
                    elf_path=None,
                    hex_path=None,
                    size_info=None,
                    stdout=link_result.stdout if link_result else '',
                    stderr=link_result.stderr if link_result else 'Linking failed'
                )

            # Step 3: Convert to .hex
            if not self._objcopy_hex(output_elf, output_hex):
                return LinkResult(
                    success=False,
                    elf_path=output_elf if output_elf.exists() else None,
                    hex_path=None,
                    size_info=None,
                    stdout=link_result.stdout,
                    stderr='Failed to convert ELF to HEX'
                )

            # Step 4: Get size info
            size_info = self._get_size(output_elf)

            # Check for flash overflow
            if size_info and self.max_flash and size_info.total_flash > self.max_flash:
                return LinkResult(
                    success=False,
                    elf_path=output_elf,
                    hex_path=output_hex,
                    size_info=size_info,
                    stdout=link_result.stdout,
                    stderr=f'Sketch too large: {size_info.total_flash} bytes (maximum is {self.max_flash} bytes)'
                )

            return LinkResult(
                success=True,
                elf_path=output_elf,
                hex_path=output_hex,
                size_info=size_info,
                stdout=link_result.stdout,
                stderr=link_result.stderr
            )

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly
            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception as e:
            return LinkResult(
                success=False,
                elf_path=None,
                hex_path=None,
                size_info=None,
                stdout='',
                stderr=f'Linking exception: {str(e)}'
            )


