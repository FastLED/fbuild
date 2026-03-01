"""Unit tests for CrashDecoder — ESP32 crash stack trace decoder."""

import json
import sys
from pathlib import Path
from unittest.mock import patch

import pytest

from fbuild.daemon.crash_decoder import (
    CrashDecoder,
    _derive_addr2line_path,
    _load_build_info,
    create_crash_decoder,
)

_EXE = ".exe" if sys.platform == "win32" else ""


# ---------------------------------------------------------------------------
# Sample crash dumps
# ---------------------------------------------------------------------------

RISCV_CRASH = """\
abort() was called at PC 0x42002a3c on core 0

Stack dump detected

MEPC    : 0x40381d5a
RA      : 0x4038282e
SP      : 0x3fc96e10
GP      : 0x3fc8b400
TP      : 0x3fc8bbb4
T0      : 0x00000001
T1      : 0x00000002

Stack memory:
3fc96e50: 0x42002a3c 0x00000000 0x3fc96e70 0x42001234

ELF file SHA256: abc123
""".strip().splitlines()

XTENSA_CRASH = """\
Guru Meditation Error: Core  0 panic'ed (LoadProhibited). Exception was unhandled.
Core  0 register dump:
PC      : 0x400d1234  PS      : 0x00060430

Backtrace: 0x400d1234:0x3ffb1234 0x400d5678:0x3ffb5678 0x400d9abc:0x3ffb9abc

ELF file SHA256: def456
""".strip().splitlines()


# ---------------------------------------------------------------------------
# CrashDecoder — detection
# ---------------------------------------------------------------------------


class TestCrashDetection:
    """Test crash start/end pattern detection."""

    def test_detect_guru_meditation(self) -> None:
        decoder = CrashDecoder(None, None)
        assert decoder.detect_crash_start("Guru Meditation Error: Core  0 panic'ed (LoadProhibited)")

    def test_detect_abort(self) -> None:
        decoder = CrashDecoder(None, None)
        assert decoder.detect_crash_start("abort() was called at PC 0x42002a3c on core 0")

    def test_detect_watchdog(self) -> None:
        decoder = CrashDecoder(None, None)
        assert decoder.detect_crash_start("Task watchdog got triggered")

    def test_no_false_positive(self) -> None:
        decoder = CrashDecoder(None, None)
        assert not decoder.detect_crash_start("Hello from ESP32")
        assert not decoder.detect_crash_start("Temperature: 23.5")

    def test_detect_crash_end_elf_sha(self) -> None:
        decoder = CrashDecoder(None, None)
        decoder.accumulate("some crash line")
        assert decoder.detect_crash_end("ELF file SHA256: abc123def")

    def test_detect_crash_end_rebooting(self) -> None:
        decoder = CrashDecoder(None, None)
        decoder.accumulate("some crash line")
        assert decoder.detect_crash_end("Rebooting...")

    def test_detect_crash_end_double_blank(self) -> None:
        decoder = CrashDecoder(None, None)
        decoder.accumulate("some crash line")
        assert not decoder.detect_crash_end("")  # first blank
        assert decoder.detect_crash_end("")  # second blank

    def test_non_blank_resets_blank_counter(self) -> None:
        decoder = CrashDecoder(None, None)
        decoder.accumulate("some crash line")
        assert not decoder.detect_crash_end("")
        assert not decoder.detect_crash_end("more crash data")  # resets blank count
        assert not decoder.detect_crash_end("")  # first blank again
        assert decoder.detect_crash_end("")  # second blank


# ---------------------------------------------------------------------------
# CrashDecoder — accumulation
# ---------------------------------------------------------------------------


class TestAccumulation:
    """Test crash line buffering."""

    def test_accumulate_sets_flag(self) -> None:
        decoder = CrashDecoder(None, None)
        assert not decoder.is_accumulating
        decoder.accumulate("crash line 1")
        assert decoder.is_accumulating

    def test_reset_clears_state(self) -> None:
        decoder = CrashDecoder(None, None)
        decoder.accumulate("line 1")
        decoder.accumulate("line 2")
        decoder.reset()
        assert not decoder.is_accumulating


# ---------------------------------------------------------------------------
# CrashDecoder — address extraction
# ---------------------------------------------------------------------------


class TestAddressExtraction:
    """Test address extraction from crash dumps."""

    def test_riscv_registers(self) -> None:
        decoder = CrashDecoder(Path("/fake.elf"), Path("/fake/addr2line"))
        for line in RISCV_CRASH:
            decoder.accumulate(line)

        addresses = decoder._extract_addresses()
        # Should have: 0x42002a3c (abort PC), 0x40381d5a (MEPC), 0x4038282e (RA), 0x42001234 (stack)
        assert "0x42002a3c" in addresses
        assert "0x40381d5a" in addresses
        assert "0x4038282e" in addresses
        assert "0x42001234" in addresses
        # SP 0x3fc96e10 should NOT be included (data region, not code)
        lower_addrs = [a.lower() for a in addresses]
        assert "0x3fc96e10" not in lower_addrs

    def test_xtensa_backtrace(self) -> None:
        decoder = CrashDecoder(Path("/fake.elf"), Path("/fake/addr2line"))
        for line in XTENSA_CRASH:
            decoder.accumulate(line)

        addresses = decoder._extract_addresses()
        # Xtensa backtrace PCs
        assert "0x400d1234" in addresses
        assert "0x400d5678" in addresses
        assert "0x400d9abc" in addresses

    def test_no_duplicates(self) -> None:
        decoder = CrashDecoder(Path("/fake.elf"), Path("/fake/addr2line"))
        # Feed the same address multiple times
        decoder.accumulate("abort() was called at PC 0x42002a3c on core 0")
        decoder.accumulate("MEPC    : 0x42002a3c")

        addresses = decoder._extract_addresses()
        # Each unique address appears once
        lower_addrs = [a.lower() for a in addresses]
        assert lower_addrs.count("0x42002a3c") == 1

    def test_empty_buffer(self) -> None:
        decoder = CrashDecoder(Path("/fake.elf"), Path("/fake/addr2line"))
        assert decoder._extract_addresses() == []


# ---------------------------------------------------------------------------
# CrashDecoder — decode() with mocked addr2line
# ---------------------------------------------------------------------------


class TestDecode:
    """Test decode() method with mocked subprocess."""

    def test_decode_no_elf(self) -> None:
        decoder = CrashDecoder(None, Path("/fake/addr2line"))
        decoder.accumulate("abort() was called at PC 0x42002a3c")
        result = decoder.decode()
        assert len(result) == 1
        assert "no firmware.elf" in result[0]

    def test_decode_no_addr2line(self) -> None:
        decoder = CrashDecoder(Path("/fake.elf"), None)
        decoder.accumulate("abort() was called at PC 0x42002a3c")
        result = decoder.decode()
        assert len(result) == 1
        assert "addr2line not found" in result[0]

    def test_decode_warns_once(self) -> None:
        """Warning about missing ELF/addr2line should only appear once."""
        decoder = CrashDecoder(None, None)

        decoder.accumulate("abort() was called at PC 0x42002a3c")
        result1 = decoder.decode()
        decoder.reset()

        # Force a different crash hash to bypass debounce
        decoder.accumulate("abort() was called at PC 0x42003000")
        result2 = decoder.decode()
        decoder.reset()

        assert len(result1) == 1
        assert result2 == []  # Second time: no warning

    @patch("fbuild.daemon.crash_decoder.safe_run")
    def test_decode_success(self, mock_run: object) -> None:
        """Test successful addr2line decode."""
        from unittest.mock import MagicMock

        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stdout = "0x42002a3c: deliberate_crash at /src/main.cpp:17\n0x4038282e: esp_system_abort at /esp-idf/components/panic.c:93\n"
        mock_result.stderr = ""
        assert isinstance(mock_run, MagicMock)
        mock_run.return_value = mock_result

        decoder = CrashDecoder(Path("/firmware.elf"), Path("/bin/addr2line"))
        for line in RISCV_CRASH:
            decoder.accumulate(line)

        result = decoder.decode()
        assert any("Decoded Stack Trace" in line for line in result)
        assert any("deliberate_crash" in line for line in result)
        assert any("esp_system_abort" in line for line in result)

    @patch("fbuild.daemon.crash_decoder.safe_run")
    def test_decode_filters_unknown(self, mock_run: object) -> None:
        """addr2line lines with ??:0 should be filtered out."""
        from unittest.mock import MagicMock

        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stdout = "??:0\n?? ??:0\n0x42002a3c: real_func at /src/main.cpp:5\n"
        mock_result.stderr = ""
        assert isinstance(mock_run, MagicMock)
        mock_run.return_value = mock_result

        decoder = CrashDecoder(Path("/firmware.elf"), Path("/bin/addr2line"))
        decoder.accumulate("abort() was called at PC 0x42002a3c")

        result = decoder.decode()
        assert any("real_func" in line for line in result)
        assert not any("??" in line for line in result)

    @patch("fbuild.daemon.crash_decoder.safe_run")
    def test_decode_timeout(self, mock_run: object) -> None:
        """Test addr2line timeout handling."""
        import subprocess
        from unittest.mock import MagicMock

        assert isinstance(mock_run, MagicMock)
        mock_run.side_effect = subprocess.TimeoutExpired(cmd=["addr2line"], timeout=5)

        decoder = CrashDecoder(Path("/firmware.elf"), Path("/bin/addr2line"))
        decoder.accumulate("abort() was called at PC 0x42002a3c")

        result = decoder.decode()
        assert len(result) == 1
        assert "timed out" in result[0]

    def test_debounce(self) -> None:
        """Identical crash within debounce window should be skipped."""
        decoder = CrashDecoder(None, Path("/fake/addr2line"))

        decoder.accumulate("abort() was called at PC 0x42002a3c")
        result1 = decoder.decode()
        decoder.reset()

        # Same crash again immediately
        decoder.accumulate("abort() was called at PC 0x42002a3c")
        result2 = decoder.decode()
        decoder.reset()

        assert len(result1) == 1  # Warning (no ELF)
        assert len(result2) == 1
        assert "duplicate" in result2[0] or "debounce" in result2[0]


# ---------------------------------------------------------------------------
# CrashDecoder — state machine flow
# ---------------------------------------------------------------------------


class TestStateMachineFlow:
    """Test the full accumulate → detect_end → decode → reset cycle."""

    @patch("fbuild.daemon.crash_decoder.safe_run")
    def test_riscv_full_flow(self, mock_run: object) -> None:
        """Simulate a RISC-V crash flowing through the state machine."""
        from unittest.mock import MagicMock

        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stdout = "0x42002a3c: deliberate_crash at /src/main.cpp:17\n"
        mock_result.stderr = ""
        assert isinstance(mock_run, MagicMock)
        mock_run.return_value = mock_result

        decoder = CrashDecoder(Path("/firmware.elf"), Path("/bin/addr2line"))
        decoded_output: list[str] = []

        for line in RISCV_CRASH:
            if not decoder.is_accumulating:
                if decoder.detect_crash_start(line):
                    decoder.accumulate(line)
            else:
                if decoder.detect_crash_end(line):
                    decoded_output = decoder.decode()
                    decoder.reset()
                else:
                    decoder.accumulate(line)

        assert len(decoded_output) > 0
        assert any("deliberate_crash" in l for l in decoded_output)
        assert not decoder.is_accumulating

    @patch("fbuild.daemon.crash_decoder.safe_run")
    def test_xtensa_full_flow(self, mock_run: object) -> None:
        """Simulate an Xtensa crash flowing through the state machine."""
        from unittest.mock import MagicMock

        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stdout = "0x400d1234: app_main at /src/main.cpp:10\n0x400d5678: loop at /src/main.cpp:20\n0x400d9abc: setup at /src/main.cpp:30\n"
        mock_result.stderr = ""
        assert isinstance(mock_run, MagicMock)
        mock_run.return_value = mock_result

        decoder = CrashDecoder(Path("/firmware.elf"), Path("/bin/addr2line"))
        decoded_output: list[str] = []

        for line in XTENSA_CRASH:
            if not decoder.is_accumulating:
                if decoder.detect_crash_start(line):
                    decoder.accumulate(line)
            else:
                if decoder.detect_crash_end(line):
                    decoded_output = decoder.decode()
                    decoder.reset()
                else:
                    decoder.accumulate(line)

        assert len(decoded_output) > 0
        assert any("app_main" in l for l in decoded_output)


# ---------------------------------------------------------------------------
# _derive_addr2line_path
# ---------------------------------------------------------------------------


class TestDeriveAddr2line:
    """Test addr2line path derivation from gcc path."""

    def test_derive_riscv(self, tmp_path: Path) -> None:
        gcc = tmp_path / f"riscv32-esp-elf-gcc{_EXE}"
        gcc.touch()
        addr2line = tmp_path / f"riscv32-esp-elf-addr2line{_EXE}"
        addr2line.touch()

        result = _derive_addr2line_path(str(gcc))
        assert result is not None
        assert "riscv32-esp-elf-addr2line" in result.name

    def test_derive_xtensa(self, tmp_path: Path) -> None:
        gcc = tmp_path / f"xtensa-esp32s3-elf-gcc{_EXE}"
        gcc.touch()
        addr2line = tmp_path / f"xtensa-esp32s3-elf-addr2line{_EXE}"
        addr2line.touch()

        result = _derive_addr2line_path(str(gcc))
        assert result is not None
        assert "xtensa-esp32s3-elf-addr2line" in result.name

    def test_derive_missing(self, tmp_path: Path) -> None:
        gcc = tmp_path / f"riscv32-esp-elf-gcc{_EXE}"
        gcc.touch()
        # Don't create addr2line
        result = _derive_addr2line_path(str(gcc))
        assert result is None

    def test_derive_not_gcc(self, tmp_path: Path) -> None:
        not_gcc = tmp_path / f"riscv32-esp-elf-ar{_EXE}"
        not_gcc.touch()
        result = _derive_addr2line_path(str(not_gcc))
        assert result is None


# ---------------------------------------------------------------------------
# _load_build_info
# ---------------------------------------------------------------------------


class TestLoadBuildInfo:
    """Test loading ELF and addr2line paths from build_info.json."""

    def test_load_success(self, tmp_path: Path) -> None:
        # Create fake build_info.json
        build_dir = tmp_path / ".fbuild" / "build" / "esp32c6"
        build_dir.mkdir(parents=True)

        elf_file = tmp_path / ".fbuild" / "build" / "esp32c6" / "firmware.elf"
        elf_file.touch()

        gcc_file = tmp_path / "toolchain" / "bin" / f"riscv32-esp-elf-gcc{_EXE}"
        gcc_file.parent.mkdir(parents=True)
        gcc_file.touch()
        addr2line_file = tmp_path / "toolchain" / "bin" / f"riscv32-esp-elf-addr2line{_EXE}"
        addr2line_file.touch()

        build_info = {
            "firmware": {"elf_path": str(elf_file)},
            "toolchain": {"cc_path": str(gcc_file)},
        }
        (build_dir / "build_info.json").write_text(json.dumps(build_info), encoding="utf-8")

        elf_path, a2l_path = _load_build_info(tmp_path, "esp32c6")
        assert elf_path == elf_file
        assert a2l_path == addr2line_file

    def test_load_relative_elf(self, tmp_path: Path) -> None:
        build_dir = tmp_path / ".fbuild" / "build" / "esp32c6"
        build_dir.mkdir(parents=True)

        elf_file = build_dir / "firmware.elf"
        elf_file.touch()

        build_info = {
            "firmware": {"elf_path": ".fbuild/build/esp32c6/firmware.elf"},
            "toolchain": {},
        }
        (build_dir / "build_info.json").write_text(json.dumps(build_info), encoding="utf-8")

        elf_path, _ = _load_build_info(tmp_path, "esp32c6")
        assert elf_path is not None
        assert elf_path.exists()

    def test_load_explicit_addr2line(self, tmp_path: Path) -> None:
        build_dir = tmp_path / ".fbuild" / "build" / "esp32c6"
        build_dir.mkdir(parents=True)

        addr2line_file = tmp_path / "tools" / "addr2line"
        addr2line_file.parent.mkdir(parents=True)
        addr2line_file.touch()

        build_info = {
            "firmware": {},
            "toolchain": {"addr2line_path": str(addr2line_file)},
        }
        (build_dir / "build_info.json").write_text(json.dumps(build_info), encoding="utf-8")

        _, a2l_path = _load_build_info(tmp_path, "esp32c6")
        assert a2l_path == addr2line_file

    def test_load_missing_file(self, tmp_path: Path) -> None:
        elf_path, a2l_path = _load_build_info(tmp_path, "noexist")
        assert elf_path is None
        assert a2l_path is None

    def test_load_corrupt_json(self, tmp_path: Path) -> None:
        build_dir = tmp_path / ".fbuild" / "build" / "esp32c6"
        build_dir.mkdir(parents=True)
        (build_dir / "build_info.json").write_text("not json!", encoding="utf-8")

        elf_path, a2l_path = _load_build_info(tmp_path, "esp32c6")
        assert elf_path is None
        assert a2l_path is None


# ---------------------------------------------------------------------------
# create_crash_decoder factory
# ---------------------------------------------------------------------------


class TestFactory:
    """Test create_crash_decoder factory function."""

    def test_factory_no_project(self) -> None:
        decoder = create_crash_decoder(None, None)
        assert not decoder.can_decode

    def test_factory_with_project(self, tmp_path: Path) -> None:
        build_dir = tmp_path / ".fbuild" / "build" / "esp32c6"
        build_dir.mkdir(parents=True)

        elf_file = build_dir / "firmware.elf"
        elf_file.touch()

        gcc_file = tmp_path / "bin" / f"riscv32-esp-elf-gcc{_EXE}"
        gcc_file.parent.mkdir(parents=True)
        gcc_file.touch()
        addr2line_file = tmp_path / "bin" / f"riscv32-esp-elf-addr2line{_EXE}"
        addr2line_file.touch()

        build_info = {
            "firmware": {"elf_path": str(elf_file)},
            "toolchain": {"cc_path": str(gcc_file)},
        }
        (build_dir / "build_info.json").write_text(json.dumps(build_info), encoding="utf-8")

        decoder = create_crash_decoder(tmp_path, "esp32c6")
        assert decoder.can_decode


# ---------------------------------------------------------------------------
# can_decode property
# ---------------------------------------------------------------------------


class TestCanDecode:
    """Test can_decode property."""

    def test_both_present(self) -> None:
        decoder = CrashDecoder(Path("/a.elf"), Path("/b/addr2line"))
        assert decoder.can_decode

    def test_no_elf(self) -> None:
        decoder = CrashDecoder(None, Path("/b/addr2line"))
        assert not decoder.can_decode

    def test_no_addr2line(self) -> None:
        decoder = CrashDecoder(Path("/a.elf"), None)
        assert not decoder.can_decode

    def test_neither(self) -> None:
        decoder = CrashDecoder(None, None)
        assert not decoder.can_decode
