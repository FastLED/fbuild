"""Build info generator for creating build_info.json after successful builds.

This module generates build metadata similar to PlatformIO's idedata.json,
providing information about the build configuration, firmware paths, memory
usage, and toolchain information.

Design:
    - Stores build info in .fbuild/build/{env_name}/build_info.json
    - Generated automatically after each successful build
    - Contains all information needed for IDE integration and debugging
"""

import json
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Optional


@dataclass
class BoardInfo:
    """Board configuration information."""

    id: str
    name: str
    mcu: str
    platform: str
    f_cpu: Optional[int] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "id": self.id,
            "name": self.name,
            "mcu": self.mcu,
            "platform": self.platform,
            "f_cpu": self.f_cpu,
        }


@dataclass
class FirmwareInfo:
    """Firmware file paths and sizes."""

    elf_path: Optional[str] = None
    hex_path: Optional[str] = None
    bin_path: Optional[str] = None
    elf_size_bytes: Optional[int] = None
    hex_size_bytes: Optional[int] = None
    bin_size_bytes: Optional[int] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "elf_path": self.elf_path,
            "hex_path": self.hex_path,
            "bin_path": self.bin_path,
            "elf_size_bytes": self.elf_size_bytes,
            "hex_size_bytes": self.hex_size_bytes,
            "bin_size_bytes": self.bin_size_bytes,
        }


@dataclass
class MemoryUsage:
    """Memory usage for a specific memory type (flash or RAM)."""

    used_bytes: int
    max_bytes: Optional[int] = None
    percent: Optional[float] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "used_bytes": self.used_bytes,
            "max_bytes": self.max_bytes,
            "percent": self.percent,
        }


@dataclass
class MemoryInfo:
    """Combined memory usage information."""

    flash: Optional[MemoryUsage] = None
    ram: Optional[MemoryUsage] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "flash": self.flash.to_dict() if self.flash else None,
            "ram": self.ram.to_dict() if self.ram else None,
        }


@dataclass
class ToolchainInfo:
    """Toolchain version and path information."""

    version: Optional[str] = None
    cc_path: Optional[str] = None
    cxx_path: Optional[str] = None
    ar_path: Optional[str] = None
    objcopy_path: Optional[str] = None
    size_path: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "version": self.version,
            "cc_path": self.cc_path,
            "cxx_path": self.cxx_path,
            "ar_path": self.ar_path,
            "objcopy_path": self.objcopy_path,
            "size_path": self.size_path,
        }


@dataclass
class FrameworkInfo:
    """Framework version and path information."""

    name: str
    version: Optional[str] = None
    path: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "name": self.name,
            "version": self.version,
            "path": self.path,
        }


@dataclass
class ESP32SpecificInfo:
    """ESP32-specific build information."""

    bootloader_path: Optional[str] = None
    partitions_path: Optional[str] = None
    application_offset: Optional[str] = None
    flash_mode: Optional[str] = None
    flash_size: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "bootloader_path": self.bootloader_path,
            "partitions_path": self.partitions_path,
            "application_offset": self.application_offset,
            "flash_mode": self.flash_mode,
            "flash_size": self.flash_size,
        }


@dataclass
class BuildInfo:
    """Complete build information."""

    version: str = "1.0"
    build_timestamp: str = ""
    build_time_seconds: float = 0.0
    environment: str = ""
    board: Optional[BoardInfo] = None
    firmware: Optional[FirmwareInfo] = None
    memory: Optional[MemoryInfo] = None
    build_flags: List[str] = field(default_factory=list)
    lib_deps: List[str] = field(default_factory=list)
    toolchain: Optional[ToolchainInfo] = None
    framework: Optional[FrameworkInfo] = None
    esp32_specific: Optional[ESP32SpecificInfo] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "version": self.version,
            "build_timestamp": self.build_timestamp,
            "build_time_seconds": self.build_time_seconds,
            "environment": self.environment,
            "board": self.board.to_dict() if self.board else None,
            "firmware": self.firmware.to_dict() if self.firmware else None,
            "memory": self.memory.to_dict() if self.memory else None,
            "build_flags": self.build_flags,
            "lib_deps": self.lib_deps,
            "toolchain": self.toolchain.to_dict() if self.toolchain else None,
            "framework": self.framework.to_dict() if self.framework else None,
            "esp32_specific": self.esp32_specific.to_dict() if self.esp32_specific else None,
        }


class BuildInfoGenerator:
    """Generates and saves build_info.json after successful builds."""

    BUILD_INFO_FILENAME = "build_info.json"
    SCHEMA_VERSION = "1.0"

    def __init__(self, build_dir: Path):
        """Initialize the build info generator.

        Args:
            build_dir: Build directory (.fbuild/build/{env_name})
        """
        self.build_dir = Path(build_dir)
        self.build_info_path = self.build_dir / self.BUILD_INFO_FILENAME

    def generate_avr(
        self,
        env_name: str,
        board_id: str,
        board_name: str,
        mcu: str,
        f_cpu: int,
        build_time: float,
        elf_path: Optional[Path],
        hex_path: Optional[Path],
        size_info: Optional[Any],
        build_flags: List[str],
        lib_deps: List[str],
        toolchain_version: str,
        toolchain_paths: Dict[str, Path],
        framework_version: str,
        core_path: Optional[Path] = None,
    ) -> BuildInfo:
        """Generate build info for AVR platform.

        Args:
            env_name: Environment name (e.g., 'uno')
            board_id: Board ID (e.g., 'uno')
            board_name: Human-readable board name (e.g., 'Arduino Uno')
            mcu: MCU type (e.g., 'atmega328p')
            f_cpu: CPU frequency in Hz
            build_time: Build duration in seconds
            elf_path: Path to generated .elf file
            hex_path: Path to generated .hex file
            size_info: SizeInfo object from linker
            build_flags: List of build flags
            lib_deps: List of library dependencies
            toolchain_version: Toolchain version string
            toolchain_paths: Dict of toolchain binary paths
            framework_version: Arduino core version
            core_path: Path to Arduino core

        Returns:
            BuildInfo object with all metadata
        """
        # Board info
        board = BoardInfo(
            id=board_id,
            name=board_name,
            mcu=mcu,
            platform="atmelavr",
            f_cpu=f_cpu,
        )

        # Firmware info
        firmware = FirmwareInfo()
        if elf_path and elf_path.exists():
            firmware.elf_path = str(elf_path.relative_to(self.build_dir.parent.parent.parent))
            firmware.elf_size_bytes = elf_path.stat().st_size
        if hex_path and hex_path.exists():
            firmware.hex_path = str(hex_path.relative_to(self.build_dir.parent.parent.parent))
            firmware.hex_size_bytes = hex_path.stat().st_size

        # Memory info from size_info
        memory = None
        if size_info:
            flash_usage = MemoryUsage(
                used_bytes=size_info.total_flash,
                max_bytes=size_info.max_flash,
                percent=size_info.flash_percent,
            )
            ram_usage = MemoryUsage(
                used_bytes=size_info.total_ram,
                max_bytes=size_info.max_ram,
                percent=size_info.ram_percent,
            )
            memory = MemoryInfo(flash=flash_usage, ram=ram_usage)

        # Toolchain info
        toolchain = ToolchainInfo(
            version=toolchain_version,
            cc_path=str(toolchain_paths.get("gcc")) if toolchain_paths.get("gcc") else None,
            cxx_path=str(toolchain_paths.get("gxx")) if toolchain_paths.get("gxx") else None,
            ar_path=str(toolchain_paths.get("ar")) if toolchain_paths.get("ar") else None,
            objcopy_path=str(toolchain_paths.get("objcopy")) if toolchain_paths.get("objcopy") else None,
            size_path=str(toolchain_paths.get("size")) if toolchain_paths.get("size") else None,
        )

        # Framework info
        framework = FrameworkInfo(
            name="arduino",
            version=framework_version,
            path=str(core_path) if core_path else None,
        )

        return BuildInfo(
            version=self.SCHEMA_VERSION,
            build_timestamp=datetime.now(timezone.utc).isoformat(),
            build_time_seconds=round(build_time, 3),
            environment=env_name,
            board=board,
            firmware=firmware,
            memory=memory,
            build_flags=build_flags or [],
            lib_deps=lib_deps or [],
            toolchain=toolchain,
            framework=framework,
            esp32_specific=None,
        )

    def generate_esp32(
        self,
        env_name: str,
        board_id: str,
        board_name: str,
        mcu: str,
        f_cpu: int,
        build_time: float,
        elf_path: Optional[Path],
        bin_path: Optional[Path],
        size_info: Optional[Any],
        build_flags: List[str],
        lib_deps: List[str],
        toolchain_version: str,
        toolchain_paths: Dict[str, Path],
        framework_version: str,
        core_path: Optional[Path] = None,
        bootloader_path: Optional[Path] = None,
        partitions_path: Optional[Path] = None,
        application_offset: Optional[str] = None,
        flash_mode: Optional[str] = None,
        flash_size: Optional[str] = None,
    ) -> BuildInfo:
        """Generate build info for ESP32 platform.

        Args:
            env_name: Environment name
            board_id: Board ID
            board_name: Human-readable board name
            mcu: MCU type (e.g., 'esp32c6')
            f_cpu: CPU frequency in Hz
            build_time: Build duration in seconds
            elf_path: Path to generated .elf file
            bin_path: Path to generated .bin file
            size_info: SizeInfo object from linker
            build_flags: List of build flags
            lib_deps: List of library dependencies
            toolchain_version: Toolchain version string
            toolchain_paths: Dict of toolchain binary paths
            framework_version: ESP32 Arduino framework version
            core_path: Path to framework core
            bootloader_path: Path to bootloader.bin
            partitions_path: Path to partitions.bin
            application_offset: Application offset (e.g., '0x10000')
            flash_mode: Flash mode (e.g., 'dio', 'qio')
            flash_size: Flash size (e.g., '4MB')

        Returns:
            BuildInfo object with all metadata
        """
        # Board info
        board = BoardInfo(
            id=board_id,
            name=board_name,
            mcu=mcu,
            platform="espressif32",
            f_cpu=f_cpu,
        )

        # Firmware info
        firmware = FirmwareInfo()
        try:
            if elf_path and elf_path.exists():
                firmware.elf_path = str(elf_path.relative_to(self.build_dir.parent.parent.parent))
                firmware.elf_size_bytes = elf_path.stat().st_size
            if bin_path and bin_path.exists():
                firmware.bin_path = str(bin_path.relative_to(self.build_dir.parent.parent.parent))
                firmware.bin_size_bytes = bin_path.stat().st_size
        except ValueError:
            # If relative_to fails, use absolute paths
            if elf_path and elf_path.exists():
                firmware.elf_path = str(elf_path)
                firmware.elf_size_bytes = elf_path.stat().st_size
            if bin_path and bin_path.exists():
                firmware.bin_path = str(bin_path)
                firmware.bin_size_bytes = bin_path.stat().st_size

        # Memory info from size_info
        memory = None
        if size_info:
            flash_usage = MemoryUsage(
                used_bytes=getattr(size_info, 'total_flash', 0),
                max_bytes=getattr(size_info, 'max_flash', None),
                percent=getattr(size_info, 'flash_percent', None),
            )
            ram_usage = MemoryUsage(
                used_bytes=getattr(size_info, 'total_ram', 0),
                max_bytes=getattr(size_info, 'max_ram', None),
                percent=getattr(size_info, 'ram_percent', None),
            )
            memory = MemoryInfo(flash=flash_usage, ram=ram_usage)

        # Toolchain info
        toolchain = ToolchainInfo(
            version=toolchain_version,
            cc_path=str(toolchain_paths.get("gcc")) if toolchain_paths.get("gcc") else None,
            cxx_path=str(toolchain_paths.get("gxx")) if toolchain_paths.get("gxx") else None,
            ar_path=str(toolchain_paths.get("ar")) if toolchain_paths.get("ar") else None,
            objcopy_path=str(toolchain_paths.get("objcopy")) if toolchain_paths.get("objcopy") else None,
            size_path=str(toolchain_paths.get("size")) if toolchain_paths.get("size") else None,
        )

        # Framework info
        framework = FrameworkInfo(
            name="arduino",
            version=framework_version,
            path=str(core_path) if core_path else None,
        )

        # ESP32-specific info
        esp32_specific = None
        if bootloader_path or partitions_path or application_offset:
            esp32_specific = ESP32SpecificInfo(
                bootloader_path=str(bootloader_path) if bootloader_path else None,
                partitions_path=str(partitions_path) if partitions_path else None,
                application_offset=application_offset,
                flash_mode=flash_mode,
                flash_size=flash_size,
            )

        return BuildInfo(
            version=self.SCHEMA_VERSION,
            build_timestamp=datetime.now(timezone.utc).isoformat(),
            build_time_seconds=round(build_time, 3),
            environment=env_name,
            board=board,
            firmware=firmware,
            memory=memory,
            build_flags=build_flags or [],
            lib_deps=lib_deps or [],
            toolchain=toolchain,
            framework=framework,
            esp32_specific=esp32_specific,
        )

    def generate_generic(
        self,
        env_name: str,
        board_id: str,
        board_name: str,
        mcu: str,
        platform: str,
        f_cpu: int,
        build_time: float,
        elf_path: Optional[Path],
        hex_path: Optional[Path] = None,
        bin_path: Optional[Path] = None,
        size_info: Optional[Any] = None,
        build_flags: Optional[List[str]] = None,
        lib_deps: Optional[List[str]] = None,
        toolchain_version: Optional[str] = None,
        toolchain_paths: Optional[Dict[str, Path]] = None,
        framework_name: str = "arduino",
        framework_version: Optional[str] = None,
        core_path: Optional[Path] = None,
    ) -> BuildInfo:
        """Generate build info for any platform (generic method).

        This is a generic method that can be used for platforms that don't
        have specific generate methods (Teensy, RP2040, STM32).

        Args:
            env_name: Environment name
            board_id: Board ID
            board_name: Human-readable board name
            mcu: MCU type
            platform: Platform name
            f_cpu: CPU frequency in Hz
            build_time: Build duration in seconds
            elf_path: Path to generated .elf file
            hex_path: Path to generated .hex file (optional)
            bin_path: Path to generated .bin file (optional)
            size_info: SizeInfo object from linker
            build_flags: List of build flags
            lib_deps: List of library dependencies
            toolchain_version: Toolchain version string
            toolchain_paths: Dict of toolchain binary paths
            framework_name: Framework name (default: 'arduino')
            framework_version: Framework version
            core_path: Path to framework core

        Returns:
            BuildInfo object with all metadata
        """
        # Board info
        board = BoardInfo(
            id=board_id,
            name=board_name,
            mcu=mcu,
            platform=platform,
            f_cpu=f_cpu,
        )

        # Firmware info
        firmware = FirmwareInfo()
        try:
            base_dir = self.build_dir.parent.parent.parent
            if elf_path and elf_path.exists():
                try:
                    firmware.elf_path = str(elf_path.relative_to(base_dir))
                except ValueError:
                    firmware.elf_path = str(elf_path)
                firmware.elf_size_bytes = elf_path.stat().st_size
            if hex_path and hex_path.exists():
                try:
                    firmware.hex_path = str(hex_path.relative_to(base_dir))
                except ValueError:
                    firmware.hex_path = str(hex_path)
                firmware.hex_size_bytes = hex_path.stat().st_size
            if bin_path and bin_path.exists():
                try:
                    firmware.bin_path = str(bin_path.relative_to(base_dir))
                except ValueError:
                    firmware.bin_path = str(bin_path)
                firmware.bin_size_bytes = bin_path.stat().st_size
        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
        except Exception:
            pass  # Silently ignore path resolution errors

        # Memory info from size_info
        memory = None
        if size_info:
            flash_usage = MemoryUsage(
                used_bytes=getattr(size_info, 'total_flash', 0),
                max_bytes=getattr(size_info, 'max_flash', None),
                percent=getattr(size_info, 'flash_percent', None),
            )
            ram_usage = MemoryUsage(
                used_bytes=getattr(size_info, 'total_ram', 0),
                max_bytes=getattr(size_info, 'max_ram', None),
                percent=getattr(size_info, 'ram_percent', None),
            )
            memory = MemoryInfo(flash=flash_usage, ram=ram_usage)

        # Toolchain info
        toolchain = None
        if toolchain_paths:
            toolchain = ToolchainInfo(
                version=toolchain_version,
                cc_path=str(toolchain_paths.get("gcc")) if toolchain_paths.get("gcc") else None,
                cxx_path=str(toolchain_paths.get("gxx")) if toolchain_paths.get("gxx") else None,
                ar_path=str(toolchain_paths.get("ar")) if toolchain_paths.get("ar") else None,
                objcopy_path=str(toolchain_paths.get("objcopy")) if toolchain_paths.get("objcopy") else None,
                size_path=str(toolchain_paths.get("size")) if toolchain_paths.get("size") else None,
            )

        # Framework info
        framework = FrameworkInfo(
            name=framework_name,
            version=framework_version,
            path=str(core_path) if core_path else None,
        )

        return BuildInfo(
            version=self.SCHEMA_VERSION,
            build_timestamp=datetime.now(timezone.utc).isoformat(),
            build_time_seconds=round(build_time, 3),
            environment=env_name,
            board=board,
            firmware=firmware,
            memory=memory,
            build_flags=build_flags or [],
            lib_deps=lib_deps or [],
            toolchain=toolchain,
            framework=framework,
            esp32_specific=None,
        )

    def save(self, build_info: BuildInfo) -> Path:
        """Save build info to JSON file.

        Args:
            build_info: BuildInfo object to save

        Returns:
            Path to the saved build_info.json file
        """
        self.build_dir.mkdir(parents=True, exist_ok=True)

        with open(self.build_info_path, "w", encoding="utf-8") as f:
            json.dump(build_info.to_dict(), f, indent=2)

        return self.build_info_path

    def load(self) -> Optional[BuildInfo]:
        """Load build info from JSON file.

        Returns:
            BuildInfo object or None if file doesn't exist or is corrupted
        """
        if not self.build_info_path.exists():
            return None

        try:
            with open(self.build_info_path, "r", encoding="utf-8") as f:
                data = json.load(f)

            # Reconstruct BuildInfo from dict
            # This is a simplified reconstruction - for full fidelity,
            # we'd need from_dict methods on all dataclasses
            return BuildInfo(
                version=data.get("version", "1.0"),
                build_timestamp=data.get("build_timestamp", ""),
                build_time_seconds=data.get("build_time_seconds", 0.0),
                environment=data.get("environment", ""),
                build_flags=data.get("build_flags", []),
                lib_deps=data.get("lib_deps", []),
            )
        except (json.JSONDecodeError, KeyError):
            return None
