"""
Crash stack trace decoder for ESP32 devices.

Intercepts crash output from the serial monitor, extracts memory addresses,
runs addr2line against the firmware ELF, and returns decoded function names
and source locations.

Supports:
- RISC-V (ESP32-C6, ESP32-C3, ESP32-H2): MEPC/RA register dumps
- Xtensa (ESP32, ESP32-S2, ESP32-S3): Backtrace lines
- Stack memory pointer dumps (both architectures)
"""

import json
import logging
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional

from fbuild.subprocess_utils import safe_run

# --- Crash detection patterns (text, not bytes) ---

# Lines that signal the start of a crash dump
CRASH_START_PATTERNS: list[str] = [
    "Guru Meditation Error",
    "panic'ed",
    "Core  0 register dump",
    "Core  1 register dump",
    "LoadProhibited",
    "StoreProhibited",
    "Unhandled exception",
    "abort() was called",
    "Task watchdog got triggered",
]

# --- Address extraction regexes ---

# RISC-V: MEPC, RA, and other named registers
_RISCV_REGISTER_RE = re.compile(r"(?:MEPC|RA|SP|GP|TP|T[0-6]|S[0-9]|S1[01]|A[0-7])\s*:\s*(0x[0-9a-fA-F]+)")

# Xtensa: Backtrace: 0xPC:0xSP 0xPC:0xSP ...
_XTENSA_BACKTRACE_RE = re.compile(r"Backtrace:\s*((?:0x[0-9a-fA-F]+:0x[0-9a-fA-F]+\s*)+)")
_XTENSA_ADDR_PAIR_RE = re.compile(r"(0x[0-9a-fA-F]+):0x[0-9a-fA-F]+")

# Stack memory pointers — addresses in ESP32 code/data regions
# 0x3C=Flash DROM, 0x3F=DRAM, 0x40=IRAM, 0x42=Flash IROM, 0x50=RTC
_STACK_POINTER_RE = re.compile(r"0x(?:3[CcFf]|4[02]|50)[0-9a-fA-F]{6}")

# "abort() was called at PC 0xADDR"
_ABORT_PC_RE = re.compile(r"abort\(\) was called at PC (0x[0-9a-fA-F]+)")

# Debounce: skip identical crash dumps within this window (seconds)
_DEBOUNCE_SECONDS = 10.0

# Timeout for addr2line subprocess
_ADDR2LINE_TIMEOUT_SECONDS = 5


def _derive_addr2line_path(cc_path: str) -> Optional[Path]:
    """Derive addr2line path from the compiler (gcc) path.

    The toolchain prefix is everything before 'gcc' in the binary name:
      riscv32-esp-elf-gcc    -> riscv32-esp-elf-addr2line
      xtensa-esp32s3-elf-gcc -> xtensa-esp32s3-elf-addr2line

    Args:
        cc_path: Path to the toolchain's gcc binary

    Returns:
        Path to addr2line, or None if derivation fails or file doesn't exist
    """
    cc = Path(cc_path)
    name = cc.name

    # Strip .exe suffix for matching
    stem = name.replace(".exe", "")
    if not stem.endswith("gcc"):
        return None

    prefix = stem[: -len("gcc")]  # e.g. "riscv32-esp-elf-"
    suffix = ".exe" if sys.platform == "win32" else ""
    addr2line = cc.parent / f"{prefix}addr2line{suffix}"

    if addr2line.exists():
        return addr2line
    return None


def _load_build_info(project_dir: Path, env_name: str) -> tuple[Optional[Path], Optional[Path]]:
    """Load ELF path and addr2line path from build_info.json.

    Args:
        project_dir: Project root directory
        env_name: Build environment name

    Returns:
        Tuple of (elf_path, addr2line_path), either may be None
    """
    # Build info may be directly under env_name or under a build profile subdir
    # (e.g., .fbuild/build/{env}/release/build_info.json)
    from fbuild.paths import get_project_build_root

    env_build_dir = get_project_build_root(project_dir) / env_name
    build_info_path = env_build_dir / "build_info.json"
    if not build_info_path.exists():
        # Search under build profile subdirectories (release, quick, etc.)
        for child in env_build_dir.iterdir() if env_build_dir.exists() else []:
            candidate = child / "build_info.json"
            if candidate.exists():
                build_info_path = candidate
                break
        else:
            return None, None

    try:
        with open(build_info_path, "r", encoding="utf-8") as f:
            data = json.load(f)
    except (json.JSONDecodeError, OSError):
        return None, None

    # ELF path — relative paths in build_info.json are relative to .fbuild/
    elf_path: Optional[Path] = None
    firmware = data.get("firmware", {})
    if firmware and firmware.get("elf_path"):
        candidate = Path(firmware["elf_path"])
        if not candidate.is_absolute():
            # Try relative to .fbuild/ first, then project root
            from fbuild.paths import get_project_fbuild_dir

            fbuild_relative = get_project_fbuild_dir(project_dir) / candidate
            if fbuild_relative.exists():
                candidate = fbuild_relative
            else:
                candidate = project_dir / candidate
        if candidate.exists():
            elf_path = candidate

    # addr2line path — prefer explicit, fall back to derivation from cc_path
    addr2line_path: Optional[Path] = None
    toolchain = data.get("toolchain", {})
    if toolchain:
        if toolchain.get("addr2line_path"):
            candidate = Path(toolchain["addr2line_path"])
            if candidate.exists():
                addr2line_path = candidate

        if addr2line_path is None and toolchain.get("cc_path"):
            addr2line_path = _derive_addr2line_path(toolchain["cc_path"])

    return elf_path, addr2line_path


class CrashDecoder:
    """Accumulates ESP32 crash dump lines and decodes them with addr2line.

    Usage::

        decoder = CrashDecoder(elf_path, addr2line_path)
        for line in serial_lines:
            if decoder.detect_crash_start(line):
                decoder.accumulate(line)
            elif decoder.is_accumulating:
                if decoder.detect_crash_end(line):
                    decoded = decoder.decode()
                    decoder.reset()
                else:
                    decoder.accumulate(line)
    """

    def __init__(self, elf_path: Optional[Path], addr2line_path: Optional[Path]) -> None:
        """Initialize the crash decoder.

        Args:
            elf_path: Path to the firmware .elf file (None disables decoding)
            addr2line_path: Path to addr2line binary (None disables decoding)
        """
        self._elf_path = elf_path
        self._addr2line_path = addr2line_path
        self._buffer: list[str] = []
        self._is_accumulating = False
        self._blank_line_count = 0
        self._last_crash_hash: Optional[int] = None
        self._last_crash_time: float = 0.0
        self._warned_no_elf = False
        self._warned_no_addr2line = False

    @property
    def is_accumulating(self) -> bool:
        """Whether the decoder is currently buffering crash lines."""
        return self._is_accumulating

    @property
    def can_decode(self) -> bool:
        """Whether both ELF and addr2line are available for decoding."""
        return self._elf_path is not None and self._addr2line_path is not None

    def detect_crash_start(self, line: str) -> bool:
        """Check if a serial line indicates the start of a crash dump.

        Args:
            line: A single line of serial output (already decoded to str)

        Returns:
            True if this line matches a crash start pattern
        """
        return any(pattern in line for pattern in CRASH_START_PATTERNS)

    def accumulate(self, line: str) -> None:
        """Buffer a line that is part of an active crash dump.

        Args:
            line: A line from the crash dump
        """
        self._is_accumulating = True
        self._buffer.append(line)
        # Reset blank line counter on non-blank content
        if line.strip():
            self._blank_line_count = 0

    def detect_crash_end(self, line: str) -> bool:
        """Check if a crash dump has ended.

        A crash dump ends when we see two consecutive blank lines,
        or an "ELF file SHA256" line (end of ESP-IDF crash dump),
        or a "Rebooting..." line.

        Args:
            line: The next serial line after the crash started

        Returns:
            True if the crash dump appears to have ended
        """
        stripped = line.strip()

        # Explicit end markers
        if "ELF file SHA256" in line or "Rebooting..." in line:
            # Include this final line in the buffer
            self._buffer.append(line)
            return True

        # Two consecutive blank lines signal end of dump
        if not stripped:
            self._blank_line_count += 1
            if self._blank_line_count >= 2:
                return True
            return False

        self._blank_line_count = 0
        return False

    def decode(self) -> list[str]:
        """Decode the buffered crash dump using addr2line.

        Extracts addresses from the buffer, runs addr2line, and returns
        formatted decoded lines.

        Returns:
            List of decoded output lines (may be empty if decoding fails)
        """
        if not self._buffer:
            return []

        # Debounce: skip if identical crash within the window
        crash_hash = hash(tuple(self._buffer))
        now = time.monotonic()
        if crash_hash == self._last_crash_hash and (now - self._last_crash_time) < _DEBOUNCE_SECONDS:
            return ["  [crash decode skipped — duplicate within debounce window]"]
        self._last_crash_hash = crash_hash
        self._last_crash_time = now

        # Check prerequisites
        if self._elf_path is None:
            if not self._warned_no_elf:
                self._warned_no_elf = True
                return ["  [crash decode disabled — no firmware.elf found]"]
            return []

        if self._addr2line_path is None:
            if not self._warned_no_addr2line:
                self._warned_no_addr2line = True
                return ["  [crash decode disabled — addr2line not found]"]
            return []

        # Extract addresses
        addresses = self._extract_addresses()
        if not addresses:
            return []

        # Run addr2line
        return self._run_addr2line(addresses)

    def reset(self) -> None:
        """Clear the accumulator for the next crash."""
        self._buffer.clear()
        self._is_accumulating = False
        self._blank_line_count = 0

    def process_line(self, line: str) -> Optional[str]:
        """Process a single serial line through the crash decoder state machine.

        This is a convenience method that wraps detect_crash_start /
        accumulate / detect_crash_end / decode / reset into a single call,
        suitable for use as a ``line_callback`` in SerialMonitor.

        Args:
            line: A single line of serial output (already decoded to str)

        Returns:
            Multi-line decoded stack trace string when a crash dump completes,
            or None if no output is ready yet.
        """
        if not self._is_accumulating:
            if self.detect_crash_start(line):
                self.accumulate(line)
            return None

        # Currently accumulating crash lines
        if self.detect_crash_end(line):
            decoded_lines = self.decode()
            self.reset()
            if decoded_lines:
                return "\n".join(decoded_lines)
            return None

        self.accumulate(line)
        return None

    def _extract_addresses(self) -> list[str]:
        """Extract unique code addresses from the buffered crash dump.

        Returns:
            Ordered list of unique hex address strings
        """
        seen: set[str] = set()
        addresses: list[str] = []
        full_text = "\n".join(self._buffer)

        def _add(addr: str) -> None:
            low = addr.lower()
            if low not in seen:
                seen.add(low)
                addresses.append(addr)

        # "abort() was called at PC 0x..."
        for m in _ABORT_PC_RE.finditer(full_text):
            _add(m.group(1))

        # Xtensa backtrace (PC:SP pairs — extract PC)
        for bt_match in _XTENSA_BACKTRACE_RE.finditer(full_text):
            for pair_match in _XTENSA_ADDR_PAIR_RE.finditer(bt_match.group(1)):
                _add(pair_match.group(1))

        # RISC-V named registers
        for m in _RISCV_REGISTER_RE.finditer(full_text):
            val = m.group(1)
            # Only keep addresses in code regions (0x40xxxxxx, 0x42xxxxxx)
            if val.lower().startswith(("0x40", "0x42")):
                _add(val)

        # Stack memory pointers (code-region addresses from stack dumps)
        for m in _STACK_POINTER_RE.finditer(full_text):
            val = m.group(0)
            # Only code regions for addr2line
            if val.lower().startswith(("0x40", "0x42")):
                _add(val)

        return addresses

    def _run_addr2line(self, addresses: list[str]) -> list[str]:
        """Run addr2line on the extracted addresses.

        Args:
            addresses: List of hex address strings

        Returns:
            Formatted decoded output lines
        """
        assert self._addr2line_path is not None
        assert self._elf_path is not None

        cmd = [
            str(self._addr2line_path),
            "-pfiaC",  # pretty-print, functions, inlines, addresses, demangle
            "-e",
            str(self._elf_path),
            *addresses,
        ]

        try:
            result = safe_run(
                cmd,
                capture_output=True,
                text=True,
                timeout=_ADDR2LINE_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired:
            logging.warning("addr2line timed out after %ds", _ADDR2LINE_TIMEOUT_SECONDS)
            return ["  [crash decode timed out]"]
        except KeyboardInterrupt:  # noqa: KBI002
            raise
        except Exception as e:
            logging.warning("addr2line failed: %s", e)
            return [f"  [crash decode error: {e}]"]

        if result.returncode != 0:
            stderr = result.stderr.strip() if result.stderr else "unknown error"
            logging.warning("addr2line returned %d: %s", result.returncode, stderr)
            return [f"  [addr2line error: {stderr}]"]

        # Format output
        output_lines: list[str] = ["", "=== Decoded Stack Trace ==="]
        for raw_line in result.stdout.strip().splitlines():
            stripped = raw_line.strip()
            if stripped and stripped != "??:0" and stripped != "?? ??:0":
                output_lines.append(f"  {stripped}")
        output_lines.append("===========================")
        output_lines.append("")

        # Only return if we got something useful
        if len(output_lines) <= 4:  # header + footer + 2 blanks
            return []

        return output_lines


def create_crash_decoder(project_dir: Optional[Path], env_name: Optional[str]) -> CrashDecoder:
    """Factory: create a CrashDecoder from project build info.

    Loads the ELF and addr2line paths from build_info.json.
    Returns a decoder that gracefully degrades if paths are unavailable.

    Args:
        project_dir: Project root directory (None = no decoding)
        env_name: Build environment name (None = no decoding)

    Returns:
        Configured CrashDecoder instance
    """
    elf_path: Optional[Path] = None
    addr2line_path: Optional[Path] = None

    if project_dir is not None and env_name is not None:
        elf_path, addr2line_path = _load_build_info(project_dir, env_name)

    return CrashDecoder(elf_path, addr2line_path)
