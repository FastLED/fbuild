"""
Unit tests for Compiler class.

Tests the AVR-GCC compiler wrapper functionality.
"""

import pytest
from pathlib import Path
from unittest.mock import Mock, patch
from zapio.build.compiler import Compiler, CompileResult, CompilerError


class TestCompiler:
    """Test suite for Compiler class."""

    @pytest.fixture
    def mock_gcc(self, tmp_path):
        """Create mock avr-gcc executable."""
        gcc = tmp_path / 'bin' / 'avr-gcc'
        gcc.parent.mkdir(parents=True, exist_ok=True)
        gcc.write_text('#!/bin/sh\necho "gcc"')
        return gcc

    @pytest.fixture
    def mock_gpp(self, tmp_path):
        """Create mock avr-g++ executable."""
        gpp = tmp_path / 'bin' / 'avr-g++'
        gpp.parent.mkdir(parents=True, exist_ok=True)
        gpp.write_text('#!/bin/sh\necho "g++"')
        return gpp

    @pytest.fixture
    def includes(self, tmp_path):
        """Create mock include directories."""
        core = tmp_path / 'core'
        variant = tmp_path / 'variant'
        core.mkdir()
        variant.mkdir()
        return [core, variant]

    @pytest.fixture
    def defines(self):
        """Standard Arduino defines."""
        return {
            'ARDUINO': '10819',
            'ARDUINO_AVR_UNO': '',
            'ARDUINO_ARCH_AVR': ''
        }

    @pytest.fixture
    def compiler(self, mock_gcc, mock_gpp, includes, defines):
        """Create Compiler instance."""
        return Compiler(
            avr_gcc=mock_gcc,
            avr_gpp=mock_gpp,
            mcu='atmega328p',
            f_cpu='16000000L',
            includes=includes,
            defines=defines
        )

    def test_init_success(self, mock_gcc, mock_gpp, includes, defines):
        """Test successful compiler initialization."""
        compiler = Compiler(
            avr_gcc=mock_gcc,
            avr_gpp=mock_gpp,
            mcu='atmega328p',
            f_cpu='16000000L',
            includes=includes,
            defines=defines
        )

        assert compiler.avr_gcc == mock_gcc
        assert compiler.avr_gpp == mock_gpp
        assert compiler.mcu == 'atmega328p'
        assert compiler.f_cpu == '16000000L'
        assert len(compiler.includes) == 2
        assert compiler.defines == defines

    def test_init_gcc_not_found(self, tmp_path, mock_gpp, includes, defines):
        """Test initialization with missing avr-gcc."""
        fake_gcc = tmp_path / 'nonexistent' / 'avr-gcc'

        with pytest.raises(CompilerError, match='avr-gcc not found'):
            Compiler(
                avr_gcc=fake_gcc,
                avr_gpp=mock_gpp,
                mcu='atmega328p',
                f_cpu='16000000L',
                includes=includes,
                defines=defines
            )

    def test_init_gpp_not_found(self, mock_gcc, tmp_path, includes, defines):
        """Test initialization with missing avr-g++."""
        fake_gpp = tmp_path / 'nonexistent' / 'avr-g++'

        with pytest.raises(CompilerError, match='avr-g\\+\\+ not found'):
            Compiler(
                avr_gcc=mock_gcc,
                avr_gpp=fake_gpp,
                mcu='atmega328p',
                f_cpu='16000000L',
                includes=includes,
                defines=defines
            )

    def test_build_c_command(self, compiler, tmp_path):
        """Test building C compilation command."""
        source = tmp_path / 'test.c'
        output = tmp_path / 'test.o'

        cmd = compiler._build_c_command(source, output, [])

        # Verify command structure
        assert str(compiler.avr_gcc) in cmd
        assert '-c' in cmd
        assert '-g' in cmd
        assert '-Os' in cmd
        assert '-std=gnu11' in cmd
        assert '-ffunction-sections' in cmd
        assert '-fdata-sections' in cmd
        assert f'-mmcu={compiler.mcu}' in cmd
        assert str(source) in cmd
        assert '-o' in cmd
        assert str(output) in cmd

        # Verify defines
        assert any('-DARDUINO=10819' in arg for arg in cmd)
        assert any('-DF_CPU=16000000L' in arg for arg in cmd)

        # Verify includes
        for include in compiler.includes:
            assert any(f'-I{include}' in arg for arg in cmd)

    def test_build_cpp_command(self, compiler, tmp_path):
        """Test building C++ compilation command."""
        source = tmp_path / 'test.cpp'
        output = tmp_path / 'test.o'

        cmd = compiler._build_cpp_command(source, output, [])

        # Verify command structure
        assert str(compiler.avr_gpp) in cmd
        assert '-c' in cmd
        assert '-g' in cmd
        assert '-Os' in cmd
        assert '-std=gnu++11' in cmd
        assert '-fpermissive' in cmd
        assert '-fno-exceptions' in cmd
        assert '-fno-threadsafe-statics' in cmd
        assert '-ffunction-sections' in cmd
        assert '-fdata-sections' in cmd
        assert f'-mmcu={compiler.mcu}' in cmd
        assert str(source) in cmd
        assert '-o' in cmd
        assert str(output) in cmd

    def test_build_c_command_extra_flags(self, compiler, tmp_path):
        """Test C command with extra flags."""
        source = tmp_path / 'test.c'
        output = tmp_path / 'test.o'
        extra_flags = ['-Wall', '-Wextra']

        cmd = compiler._build_c_command(source, output, extra_flags)

        assert '-Wall' in cmd
        assert '-Wextra' in cmd

    def test_build_cpp_command_extra_flags(self, compiler, tmp_path):
        """Test C++ command with extra flags."""
        source = tmp_path / 'test.cpp'
        output = tmp_path / 'test.o'
        extra_flags = ['-Wall', '-Wextra']

        cmd = compiler._build_cpp_command(source, output, extra_flags)

        assert '-Wall' in cmd
        assert '-Wextra' in cmd

    @patch('subprocess.run')
    def test_compile_c_success(self, mock_run, compiler, tmp_path):
        """Test successful C compilation."""
        source = tmp_path / 'test.c'
        source.write_text('int main() { return 0; }')
        output = tmp_path / 'test.o'

        # Mock successful compilation
        mock_result = Mock()
        mock_result.returncode = 0
        mock_result.stdout = ''
        mock_result.stderr = ''
        mock_run.return_value = mock_result

        # Create output file (simulating compiler)
        output.write_bytes(b'\x7fELF')

        result = compiler.compile_c(source, output)

        assert result.success is True
        assert result.returncode == 0
        assert result.object_file == output
        mock_run.assert_called_once()

    @patch('subprocess.run')
    def test_compile_cpp_success(self, mock_run, compiler, tmp_path):
        """Test successful C++ compilation."""
        source = tmp_path / 'test.cpp'
        source.write_text('int main() { return 0; }')
        output = tmp_path / 'test.o'

        # Mock successful compilation
        mock_result = Mock()
        mock_result.returncode = 0
        mock_result.stdout = ''
        mock_result.stderr = ''
        mock_run.return_value = mock_result

        # Create output file
        output.write_bytes(b'\x7fELF')

        result = compiler.compile_cpp(source, output)

        assert result.success is True
        assert result.returncode == 0
        assert result.object_file == output

    @patch('subprocess.run')
    def test_compile_c_failure(self, mock_run, compiler, tmp_path):
        """Test C compilation failure."""
        source = tmp_path / 'test.c'
        source.write_text('int main() { return 0; }')
        output = tmp_path / 'test.o'

        # Mock failed compilation
        mock_result = Mock()
        mock_result.returncode = 1
        mock_result.stdout = ''
        mock_result.stderr = 'test.c:1:1: error: expected declaration'
        mock_run.return_value = mock_result

        result = compiler.compile_c(source, output)

        assert result.success is False
        assert result.returncode == 1
        assert result.object_file is None
        assert 'error' in result.stderr

    @patch('subprocess.run')
    def test_compile_auto_detect_c(self, mock_run, compiler, tmp_path):
        """Test compile() auto-detects C files."""
        source = tmp_path / 'test.c'
        source.write_text('int main() { return 0; }')
        output = tmp_path / 'test.o'

        mock_result = Mock()
        mock_result.returncode = 0
        mock_result.stdout = ''
        mock_result.stderr = ''
        mock_run.return_value = mock_result
        output.write_bytes(b'\x7fELF')

        result = compiler.compile(source, output)

        assert result.success is True
        # Should have called avr-gcc (not avr-g++)
        call_args = mock_run.call_args[0][0]
        assert str(compiler.avr_gcc) in call_args

    @patch('subprocess.run')
    def test_compile_auto_detect_cpp(self, mock_run, compiler, tmp_path):
        """Test compile() auto-detects C++ files."""
        source = tmp_path / 'test.cpp'
        source.write_text('int main() { return 0; }')
        output = tmp_path / 'test.o'

        mock_result = Mock()
        mock_result.returncode = 0
        mock_result.stdout = ''
        mock_result.stderr = ''
        mock_run.return_value = mock_result
        output.write_bytes(b'\x7fELF')

        result = compiler.compile(source, output)

        assert result.success is True
        # Should have called avr-g++ (not avr-gcc)
        call_args = mock_run.call_args[0][0]
        assert str(compiler.avr_gpp) in call_args

    def test_compile_unknown_extension(self, compiler, tmp_path):
        """Test compile() with unknown file extension."""
        source = tmp_path / 'test.xyz'
        output = tmp_path / 'test.o'

        with pytest.raises(CompilerError, match='Unknown source file type'):
            compiler.compile(source, output)

    @patch('subprocess.run')
    def test_compile_sources_success(self, mock_run, compiler, tmp_path):
        """Test compiling multiple source files."""
        sources = [
            tmp_path / 'test1.c',
            tmp_path / 'test2.cpp',
            tmp_path / 'test3.c'
        ]

        for source in sources:
            source.write_text('int main() { return 0; }')

        output_dir = tmp_path / 'build'

        # Mock successful compilations
        def mock_compile(*args, **kwargs):
            result = Mock()
            result.returncode = 0
            result.stdout = ''
            result.stderr = ''
            # Create output file
            cmd = args[0]
            output_idx = cmd.index('-o') + 1
            output_path = Path(cmd[output_idx])
            output_path.parent.mkdir(parents=True, exist_ok=True)
            output_path.write_bytes(b'\x7fELF')
            return result

        mock_run.side_effect = mock_compile

        object_files = compiler.compile_sources(sources, output_dir)

        assert len(object_files) == 3
        assert all(obj.exists() for obj in object_files)
        assert all(obj.suffix == '.o' for obj in object_files)
        assert mock_run.call_count == 3

    @patch('subprocess.run')
    def test_compile_sources_failure(self, mock_run, compiler, tmp_path):
        """Test compile_sources stops on first failure."""
        sources = [
            tmp_path / 'test1.c',
            tmp_path / 'test2.c'
        ]

        for source in sources:
            source.write_text('int main() { return 0; }')

        output_dir = tmp_path / 'build'

        # Mock first compilation fails
        mock_result = Mock()
        mock_result.returncode = 1
        mock_result.stdout = ''
        mock_result.stderr = 'compilation error'
        mock_run.return_value = mock_result

        with pytest.raises(CompilerError, match='Failed to compile'):
            compiler.compile_sources(sources, output_dir)

        # Should only call once (stop on first error)
        assert mock_run.call_count == 1

    def test_needs_rebuild_no_object(self, compiler, tmp_path):
        """Test needs_rebuild when object doesn't exist."""
        source = tmp_path / 'test.c'
        source.write_text('int main() { return 0; }')
        obj = tmp_path / 'test.o'

        assert compiler.needs_rebuild(source, obj) is True

    def test_needs_rebuild_source_newer(self, compiler, tmp_path):
        """Test needs_rebuild when source is newer."""
        source = tmp_path / 'test.c'
        obj = tmp_path / 'test.o'

        # Create old object file
        obj.write_bytes(b'\x7fELF')

        # Create newer source file
        import time
        time.sleep(0.01)
        source.write_text('int main() { return 0; }')

        assert compiler.needs_rebuild(source, obj) is True

    def test_needs_rebuild_object_newer(self, compiler, tmp_path):
        """Test needs_rebuild when object is newer."""
        source = tmp_path / 'test.c'
        obj = tmp_path / 'test.o'

        # Create source first
        source.write_text('int main() { return 0; }')

        # Create newer object file
        import time
        time.sleep(0.01)
        obj.write_bytes(b'\x7fELF')

        assert compiler.needs_rebuild(source, obj) is False

    @patch('subprocess.run')
    def test_execute_compiler_exception(self, mock_run, compiler, tmp_path):
        """Test _execute_compiler handles exceptions."""
        source = tmp_path / 'test.c'
        output = tmp_path / 'test.o'

        # Mock subprocess raising exception
        mock_run.side_effect = Exception('Test exception')

        result = compiler.compile_c(source, output)

        assert result.success is False
        assert result.returncode == -1
        assert 'Test exception' in result.stderr

    def test_includes_converted_to_paths(self, mock_gcc, mock_gpp, defines):
        """Test include directories converted to Path objects."""
        includes = [Path('path/to/core'), Path('path/to/variant')]

        compiler = Compiler(
            avr_gcc=mock_gcc,
            avr_gpp=mock_gpp,
            mcu='atmega328p',
            f_cpu='16000000L',
            includes=includes,
            defines=defines
        )

        assert all(isinstance(inc, Path) for inc in compiler.includes)

    def test_f_cpu_in_defines_not_duplicated(self, compiler, tmp_path):
        """Test F_CPU not duplicated if already in defines."""
        # Create compiler with F_CPU in defines
        compiler.defines['F_CPU'] = '8000000L'

        source = tmp_path / 'test.c'
        output = tmp_path / 'test.o'

        cmd = compiler._build_c_command(source, output, [])

        # Count F_CPU occurrences
        f_cpu_count = sum(1 for arg in cmd if 'F_CPU' in arg)
        assert f_cpu_count == 1  # Should only appear once

    def test_compile_result_dataclass(self):
        """Test CompileResult dataclass."""
        result = CompileResult(
            success=True,
            object_file=Path('test.o'),
            stdout='output',
            stderr='',
            returncode=0
        )

        assert result.success is True
        assert result.object_file == Path('test.o')
        assert result.stdout == 'output'
        assert result.stderr == ''
        assert result.returncode == 0

    @patch('subprocess.run')
    def test_compile_with_spaces_in_path(self, mock_run, compiler, tmp_path):
        """Test compilation with spaces in file paths."""
        # Create directory with spaces
        space_dir = tmp_path / 'my project'
        space_dir.mkdir()

        source = space_dir / 'test file.c'
        source.write_text('int main() { return 0; }')
        output = space_dir / 'test file.o'

        mock_result = Mock()
        mock_result.returncode = 0
        mock_result.stdout = ''
        mock_result.stderr = ''
        mock_run.return_value = mock_result
        output.write_bytes(b'\x7fELF')

        result = compiler.compile_c(source, output)

        assert result.success is True
        # Command should handle spaces correctly
        call_args = mock_run.call_args[0][0]
        assert any('test file.c' in str(arg) for arg in call_args)


class TestCompilerError:
    """Test CompilerError exception."""

    def test_compiler_error_raised(self):
        """Test CompilerError can be raised."""
        with pytest.raises(CompilerError):
            raise CompilerError('Test error')

    def test_compiler_error_message(self):
        """Test CompilerError message."""
        try:
            raise CompilerError('Test message')
        except CompilerError as e:
            assert str(e) == 'Test message'
