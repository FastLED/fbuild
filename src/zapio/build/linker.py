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
    def parse(avr_size_output: str, max_flash: Optional[int] = None,
              max_ram: Optional[int] = None) -> 'SizeInfo':
        """
        Parse avr-size output.

        Args:
            avr_size_output: Output from `avr-size -A` command
            max_flash: Maximum flash size for board
            max_ram: Maximum RAM size for board

        Returns:
            SizeInfo object with parsed size data
        """
        text = 0
        data = 0
        bss = 0

        for line in avr_size_output.split('\n'):
            parts = line.split()
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


class Linker:
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

    def link(
        self,
        sketch_objects: List[Path],
        core_objects: List[Path],
        output_elf: Path,
        output_hex: Path,
        extra_flags: Optional[List[str]] = None
    ) -> LinkResult:
        """
        Link object files into firmware.

        Process:
        1. Create core.a archive from core objects
        2. Link sketch objects + core.a to create .elf
        3. Convert .elf to .hex using avr-objcopy
        4. Get size information using avr-size

        Args:
            sketch_objects: User sketch object files
            core_objects: Arduino core object files
            output_elf: Output .elf file path
            output_hex: Output .hex file path
            extra_flags: Additional linker flags

        Returns:
            LinkResult with linking status and size info
        """
        try:
            # Create build directory if needed
            output_elf.parent.mkdir(parents=True, exist_ok=True)

            # Step 1: Create core.a archive
            core_archive = output_elf.parent / 'core.a'
            if not self._create_core_archive(core_objects, core_archive):
                return LinkResult(
                    success=False,
                    elf_path=None,
                    hex_path=None,
                    size_info=None,
                    stdout='',
                    stderr='Failed to create core.a archive'
                )

            # Step 2: Link to .elf
            link_result = self._link_elf(
                sketch_objects,
                core_archive,
                output_elf,
                extra_flags or []
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
        except Exception:
            return False

    def _link_elf(
        self,
        sketch_objects: List[Path],
        core_archive: Path,
        output_elf: Path,
        extra_flags: List[str]
    ):
        """
        Link objects and archive to create .elf file.

        Args:
            sketch_objects: Sketch object files
            core_archive: Core archive (core.a)
            output_elf: Output .elf file
            extra_flags: Additional linker flags

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
            f'-mmcu={self.mcu}',    # Target MCU
            '-o', str(output_elf)
        ]

        # Add sketch objects
        cmd.extend(str(obj) for obj in sketch_objects)

        # Add core archive
        if core_archive.exists():
            cmd.append(str(core_archive))

        # Add library path
        cmd.append(f'-L{output_elf.parent}')

        # Add math library
        cmd.append('-lm')

        # Add extra flags
        cmd.extend(extra_flags)

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )
            return result
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
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )
            return result.returncode == 0 and hex_path.exists()
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
            result = subprocess.run(
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
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=False
            )
            return result.returncode == 0
        except Exception:
            return False


class LinkerError(Exception):
    """Raised when linking fails."""
    pass
