"""
Unit tests for Linker class.

Tests the AVR linker wrapper functionality.
"""

import pytest
from pathlib import Path
from unittest.mock import Mock, patch
from zapio.build.linker import Linker, LinkResult, SizeInfo, LinkerError


class TestSizeInfo:
    """Test suite for SizeInfo class."""

    def test_size_info_creation(self):
        """Test SizeInfo dataclass creation."""
        size = SizeInfo(
            text=1000,
            data=50,
            bss=100,
            total_flash=1050,
            total_ram=150,
            max_flash=32256,
            max_ram=2048
        )

        assert size.text == 1000
        assert size.data == 50
        assert size.bss == 100
        assert size.total_flash == 1050
        assert size.total_ram == 150
        assert size.max_flash == 32256
        assert size.max_ram == 2048

    def test_flash_percent(self):
        """Test flash percentage calculation."""
        size = SizeInfo(
            text=16128,
            data=0,
            bss=0,
            total_flash=16128,
            total_ram=0,
            max_flash=32256,
            max_ram=2048
        )

        assert size.flash_percent == pytest.approx(50.0, rel=0.01)

    def test_ram_percent(self):
        """Test RAM percentage calculation."""
        size = SizeInfo(
            text=0,
            data=512,
            bss=512,
            total_flash=0,
            total_ram=1024,
            max_flash=32256,
            max_ram=2048
        )

        assert size.ram_percent == pytest.approx(50.0, rel=0.01)

    def test_percent_without_max(self):
        """Test percentage returns None without max values."""
        size = SizeInfo(
            text=1000,
            data=50,
            bss=100,
            total_flash=1050,
            total_ram=150
        )

        assert size.flash_percent is None
        assert size.ram_percent is None

    def test_parse_avr_size_output(self):
        """Test parsing avr-size output."""
        output = """
avr-size -A firmware.elf
section           size
.text             1030
.data               9
.bss              150
.comment           30
Total            1219
"""

        size = SizeInfo.parse(output, max_flash=32256, max_ram=2048)

        assert size.text == 1030
        assert size.data == 9
        assert size.bss == 150
        assert size.total_flash == 1039  # text + data
        assert size.total_ram == 159     # data + bss
        assert size.max_flash == 32256
        assert size.max_ram == 2048

    def test_parse_empty_output(self):
        """Test parsing empty avr-size output."""
        size = SizeInfo.parse("")

        assert size.text == 0
        assert size.data == 0
        assert size.bss == 0
        assert size.total_flash == 0
        assert size.total_ram == 0

    def test_parse_malformed_output(self):
        """Test parsing malformed avr-size output."""
        output = "garbage\nmore garbage\n"

        size = SizeInfo.parse(output)

        assert size.text == 0
        assert size.data == 0
        assert size.bss == 0


class TestLinker:
    """Test suite for Linker class."""

    @pytest.fixture
    def mock_tools(self, tmp_path):
        """Create mock linker tools."""
        tools_dir = tmp_path / 'tools'
        tools_dir.mkdir()

        avr_gcc = tools_dir / 'avr-gcc'
        avr_ar = tools_dir / 'avr-ar'
        avr_objcopy = tools_dir / 'avr-objcopy'
        avr_size = tools_dir / 'avr-size'

        for tool in [avr_gcc, avr_ar, avr_objcopy, avr_size]:
            tool.write_text('#!/bin/sh\necho "tool"')

        return {
            'gcc': avr_gcc,
            'ar': avr_ar,
            'objcopy': avr_objcopy,
            'size': avr_size
        }

    @pytest.fixture
    def linker(self, mock_tools):
        """Create Linker instance."""
        return Linker(
            avr_gcc=mock_tools['gcc'],
            avr_ar=mock_tools['ar'],
            avr_objcopy=mock_tools['objcopy'],
            avr_size=mock_tools['size'],
            mcu='atmega328p',
            max_flash=32256,
            max_ram=2048
        )

    def test_init_success(self, mock_tools):
        """Test successful linker initialization."""
        linker = Linker(
            avr_gcc=mock_tools['gcc'],
            avr_ar=mock_tools['ar'],
            avr_objcopy=mock_tools['objcopy'],
            avr_size=mock_tools['size'],
            mcu='atmega328p',
            max_flash=32256,
            max_ram=2048
        )

        assert linker.avr_gcc == mock_tools['gcc']
        assert linker.avr_ar == mock_tools['ar']
        assert linker.avr_objcopy == mock_tools['objcopy']
        assert linker.avr_size == mock_tools['size']
        assert linker.mcu == 'atmega328p'
        assert linker.max_flash == 32256
        assert linker.max_ram == 2048

    def test_init_gcc_not_found(self, mock_tools, tmp_path):
        """Test initialization with missing avr-gcc."""
        fake_gcc = tmp_path / 'nonexistent' / 'avr-gcc'

        with pytest.raises(LinkerError, match='avr-gcc not found'):
            Linker(
                avr_gcc=fake_gcc,
                avr_ar=mock_tools['ar'],
                avr_objcopy=mock_tools['objcopy'],
                avr_size=mock_tools['size'],
                mcu='atmega328p'
            )

    def test_init_ar_not_found(self, mock_tools, tmp_path):
        """Test initialization with missing avr-ar."""
        fake_ar = tmp_path / 'nonexistent' / 'avr-ar'

        with pytest.raises(LinkerError, match='avr-ar not found'):
            Linker(
                avr_gcc=mock_tools['gcc'],
                avr_ar=fake_ar,
                avr_objcopy=mock_tools['objcopy'],
                avr_size=mock_tools['size'],
                mcu='atmega328p'
            )

    def test_init_objcopy_not_found(self, mock_tools, tmp_path):
        """Test initialization with missing avr-objcopy."""
        fake_objcopy = tmp_path / 'nonexistent' / 'avr-objcopy'

        with pytest.raises(LinkerError, match='avr-objcopy not found'):
            Linker(
                avr_gcc=mock_tools['gcc'],
                avr_ar=mock_tools['ar'],
                avr_objcopy=fake_objcopy,
                avr_size=mock_tools['size'],
                mcu='atmega328p'
            )

    def test_init_size_not_found(self, mock_tools, tmp_path):
        """Test initialization with missing avr-size."""
        fake_size = tmp_path / 'nonexistent' / 'avr-size'

        with pytest.raises(LinkerError, match='avr-size not found'):
            Linker(
                avr_gcc=mock_tools['gcc'],
                avr_ar=mock_tools['ar'],
                avr_objcopy=mock_tools['objcopy'],
                avr_size=fake_size,
                mcu='atmega328p'
            )

    @patch('subprocess.run')
    def test_create_core_archive_success(self, mock_run, linker, tmp_path):
        """Test successful core archive creation."""
        core_objects = [
            tmp_path / 'core1.o',
            tmp_path / 'core2.o'
        ]
        for obj in core_objects:
            obj.write_bytes(b'\x7fELF')

        archive_path = tmp_path / 'core.a'

        # Mock successful archive creation
        def side_effect(*args, **kwargs):
            archive_path.write_bytes(b'archive')
            result = Mock()
            result.returncode = 0
            return result

        mock_run.side_effect = side_effect

        success = linker._create_core_archive(core_objects, archive_path)

        assert success is True
        assert archive_path.exists()
        mock_run.assert_called_once()

        # Verify command
        call_args = mock_run.call_args[0][0]
        assert str(linker.avr_ar) in call_args
        assert 'rcs' in call_args
        assert str(archive_path) in call_args

    @patch('subprocess.run')
    def test_create_core_archive_empty_list(self, mock_run, linker, tmp_path):
        """Test archive creation with empty object list."""
        archive_path = tmp_path / 'core.a'

        success = linker._create_core_archive([], archive_path)

        assert success is True
        mock_run.assert_not_called()

    @patch('subprocess.run')
    def test_create_core_archive_failure(self, mock_run, linker, tmp_path):
        """Test archive creation failure."""
        core_objects = [tmp_path / 'core1.o']
        core_objects[0].write_bytes(b'\x7fELF')
        archive_path = tmp_path / 'core.a'

        # Mock failed archive creation
        mock_result = Mock()
        mock_result.returncode = 1
        mock_run.return_value = mock_result

        success = linker._create_core_archive(core_objects, archive_path)

        assert success is False

    @patch('subprocess.run')
    def test_link_elf_success(self, mock_run, linker, tmp_path):
        """Test successful ELF linking."""
        sketch_objects = [tmp_path / 'sketch.o']
        sketch_objects[0].write_bytes(b'\x7fELF')

        core_archive = tmp_path / 'core.a'
        core_archive.write_bytes(b'archive')

        output_elf = tmp_path / 'firmware.elf'

        # Mock successful link
        mock_result = Mock()
        mock_result.returncode = 0
        mock_result.stdout = 'link output'
        mock_result.stderr = ''
        mock_run.return_value = mock_result

        result = linker._link_elf(sketch_objects, core_archive, output_elf, [])

        assert result.returncode == 0
        mock_run.assert_called_once()

        # Verify command
        call_args = mock_run.call_args[0][0]
        assert str(linker.avr_gcc) in call_args
        assert '-flto' in call_args
        assert f'-mmcu={linker.mcu}' in call_args
        assert str(output_elf) in call_args

    @patch('subprocess.run')
    def test_objcopy_hex_success(self, mock_run, linker, tmp_path):
        """Test successful ELF to HEX conversion."""
        elf_path = tmp_path / 'firmware.elf'
        elf_path.write_bytes(b'\x7fELF')

        hex_path = tmp_path / 'firmware.hex'

        # Mock successful conversion
        def side_effect(*args, **kwargs):
            hex_path.write_text(':00000001FF\n')  # EOF marker
            result = Mock()
            result.returncode = 0
            return result

        mock_run.side_effect = side_effect

        success = linker._objcopy_hex(elf_path, hex_path)

        assert success is True
        assert hex_path.exists()
        mock_run.assert_called_once()

        # Verify command
        call_args = mock_run.call_args[0][0]
        assert str(linker.avr_objcopy) in call_args
        assert '-O' in call_args
        assert 'ihex' in call_args
        assert '-R' in call_args
        assert '.eeprom' in call_args

    @patch('subprocess.run')
    def test_objcopy_hex_failure(self, mock_run, linker, tmp_path):
        """Test ELF to HEX conversion failure."""
        elf_path = tmp_path / 'firmware.elf'
        elf_path.write_bytes(b'\x7fELF')

        hex_path = tmp_path / 'firmware.hex'

        # Mock failed conversion
        mock_result = Mock()
        mock_result.returncode = 1
        mock_run.return_value = mock_result

        success = linker._objcopy_hex(elf_path, hex_path)

        assert success is False

    @patch('subprocess.run')
    def test_get_size_success(self, mock_run, linker, tmp_path):
        """Test successful size information retrieval."""
        elf_path = tmp_path / 'firmware.elf'
        elf_path.write_bytes(b'\x7fELF')

        size_output = """
firmware.elf:
section     size
.text       1030
.data          9
.bss         150
"""

        mock_result = Mock()
        mock_result.returncode = 0
        mock_result.stdout = size_output
        mock_result.stderr = ''
        mock_run.return_value = mock_result

        size_info = linker._get_size(elf_path)

        assert size_info is not None
        assert size_info.text == 1030
        assert size_info.data == 9
        assert size_info.bss == 150
        assert size_info.max_flash == 32256
        assert size_info.max_ram == 2048

    @patch('subprocess.run')
    def test_get_size_failure(self, mock_run, linker, tmp_path):
        """Test size information retrieval failure."""
        elf_path = tmp_path / 'firmware.elf'
        elf_path.write_bytes(b'\x7fELF')

        mock_result = Mock()
        mock_result.returncode = 1
        mock_run.return_value = mock_result

        size_info = linker._get_size(elf_path)

        assert size_info is None

    @patch('subprocess.run')
    def test_link_full_success(self, mock_run, linker, tmp_path):
        """Test full linking process."""
        sketch_objects = [tmp_path / 'sketch.o']
        sketch_objects[0].write_bytes(b'\x7fELF')

        core_objects = [tmp_path / 'core1.o', tmp_path / 'core2.o']
        for obj in core_objects:
            obj.write_bytes(b'\x7fELF')

        output_elf = tmp_path / 'build' / 'firmware.elf'
        output_hex = tmp_path / 'build' / 'firmware.hex'

        # Mock all subprocess calls
        call_count = [0]

        def mock_subprocess(*args, **kwargs):
            call_count[0] += 1
            result = Mock()
            result.returncode = 0
            result.stdout = '.text 1030\n.data 9\n.bss 150'
            result.stderr = ''

            # Create expected output files
            if call_count[0] == 1:  # avr-ar (create core.a)
                (output_elf.parent / 'core.a').write_bytes(b'archive')
            elif call_count[0] == 2:  # avr-gcc (link)
                output_elf.parent.mkdir(parents=True, exist_ok=True)
                output_elf.write_bytes(b'\x7fELF')
            elif call_count[0] == 3:  # avr-objcopy (hex)
                output_hex.write_text(':00000001FF\n')

            return result

        mock_run.side_effect = mock_subprocess

        result = linker.link(
            sketch_objects,
            core_objects,
            output_elf,
            output_hex
        )

        assert result.success is True
        assert result.elf_path == output_elf
        assert result.hex_path == output_hex
        assert result.size_info is not None
        assert output_elf.exists()
        assert output_hex.exists()

    @patch('subprocess.run')
    def test_link_archive_failure(self, mock_run, linker, tmp_path):
        """Test linking with archive creation failure."""
        sketch_objects = [tmp_path / 'sketch.o']
        sketch_objects[0].write_bytes(b'\x7fELF')

        core_objects = [tmp_path / 'core1.o']
        core_objects[0].write_bytes(b'\x7fELF')

        output_elf = tmp_path / 'firmware.elf'
        output_hex = tmp_path / 'firmware.hex'

        # Mock failed archive creation
        mock_result = Mock()
        mock_result.returncode = 1
        mock_run.return_value = mock_result

        result = linker.link(
            sketch_objects,
            core_objects,
            output_elf,
            output_hex
        )

        assert result.success is False
        assert 'Failed to create core.a' in result.stderr

    @patch('subprocess.run')
    def test_link_flash_overflow(self, mock_run, linker, tmp_path):
        """Test linking detects flash overflow."""
        sketch_objects = [tmp_path / 'sketch.o']
        sketch_objects[0].write_bytes(b'\x7fELF')

        core_objects = []
        output_elf = tmp_path / 'build' / 'firmware.elf'
        output_hex = tmp_path / 'build' / 'firmware.hex'

        call_count = [0]

        def mock_subprocess(*args, **kwargs):
            call_count[0] += 1
            result = Mock()
            result.returncode = 0

            # Return size larger than max_flash (32256)
            result.stdout = '.text 40000\n.data 100\n.bss 100'
            result.stderr = ''

            if call_count[0] == 1:  # link
                output_elf.parent.mkdir(parents=True, exist_ok=True)
                output_elf.write_bytes(b'\x7fELF')
            elif call_count[0] == 2:  # objcopy
                output_hex.write_text(':00000001FF\n')

            return result

        mock_run.side_effect = mock_subprocess

        result = linker.link(
            sketch_objects,
            core_objects,
            output_elf,
            output_hex
        )

        assert result.success is False
        assert 'Sketch too large' in result.stderr
        assert result.size_info is not None

    @patch('subprocess.run')
    def test_create_eep_success(self, mock_run, linker, tmp_path):
        """Test EEPROM file creation."""
        elf_path = tmp_path / 'firmware.elf'
        elf_path.write_bytes(b'\x7fELF')

        eep_path = tmp_path / 'firmware.eep'

        # Mock successful EEPROM extraction
        def side_effect(*args, **kwargs):
            eep_path.write_text(':00000001FF\n')
            result = Mock()
            result.returncode = 0
            return result

        mock_run.side_effect = side_effect

        success = linker.create_eep(elf_path, eep_path)

        assert success is True
        mock_run.assert_called_once()

        # Verify command includes EEPROM-specific flags
        call_args = mock_run.call_args[0][0]
        assert '-j' in call_args
        assert '.eeprom' in call_args

    def test_link_result_dataclass(self):
        """Test LinkResult dataclass."""
        size_info = SizeInfo(
            text=1000,
            data=50,
            bss=100,
            total_flash=1050,
            total_ram=150
        )

        result = LinkResult(
            success=True,
            elf_path=Path('firmware.elf'),
            hex_path=Path('firmware.hex'),
            size_info=size_info,
            stdout='output',
            stderr=''
        )

        assert result.success is True
        assert result.elf_path == Path('firmware.elf')
        assert result.hex_path == Path('firmware.hex')
        assert result.size_info == size_info
        assert result.stdout == 'output'
        assert result.stderr == ''


class TestLinkerError:
    """Test LinkerError exception."""

    def test_linker_error_raised(self):
        """Test LinkerError can be raised."""
        with pytest.raises(LinkerError):
            raise LinkerError('Test error')

    def test_linker_error_message(self):
        """Test LinkerError message."""
        try:
            raise LinkerError('Test message')
        except LinkerError as e:
            assert str(e) == 'Test message'
