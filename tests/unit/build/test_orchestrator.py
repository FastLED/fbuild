"""
Unit tests for BuildOrchestrator.

Tests the complete build orchestration process including:
- Configuration parsing
- Board configuration loading
- Toolchain and core management
- Source scanning
- Compiler and linker integration
- Build result generation
"""

import pytest
from pathlib import Path
from unittest.mock import Mock, patch

from zapio.build.orchestrator import (
    BuildOrchestrator,
    BuildOrchestratorError
)
from zapio.config import BoardConfig
from zapio.packages import Cache
from zapio.build import SourceCollection


# Test fixtures

@pytest.fixture
def mock_cache(tmp_path):
    """Create a mock cache instance."""
    cache = Mock(spec=Cache)
    cache.project_dir = tmp_path
    cache.cache_root = tmp_path / '.zap' / 'cache'
    cache.build_root = tmp_path / '.zap' / 'build'
    cache.get_build_dir = Mock(return_value=tmp_path / '.zap' / 'build' / 'uno')
    cache.get_core_build_dir = Mock(return_value=tmp_path / '.zap' / 'build' / 'uno' / 'core')
    cache.get_src_build_dir = Mock(return_value=tmp_path / '.zap' / 'build' / 'uno' / 'src')
    cache.ensure_build_directories = Mock()
    cache.clean_build = Mock()
    return cache


@pytest.fixture
def mock_platformio_config():
    """Create mock PlatformIO config."""
    config = Mock()
    config.get_default_environment = Mock(return_value='uno')
    config.get_env_config = Mock(return_value={
        'platform': 'atmelavr',
        'board': 'uno',
        'framework': 'arduino'
    })
    return config


@pytest.fixture
def mock_board_config():
    """Create mock board config."""
    return BoardConfig.from_board_id('uno')


@pytest.fixture
def mock_toolchain():
    """Create mock toolchain."""
    toolchain = Mock()
    toolchain.ensure_toolchain = Mock(return_value=Path('/toolchain'))
    toolchain.get_all_tools = Mock(return_value={
        'avr-gcc': Path('/toolchain/bin/avr-gcc'),
        'avr-g++': Path('/toolchain/bin/avr-g++'),
        'avr-ar': Path('/toolchain/bin/avr-ar'),
        'avr-objcopy': Path('/toolchain/bin/avr-objcopy'),
        'avr-size': Path('/toolchain/bin/avr-size'),
    })
    return toolchain


@pytest.fixture
def mock_arduino_core():
    """Create mock Arduino core."""
    core = Mock()
    core.AVR_VERSION = '1.8.6'
    core._core_path = Path('/arduino/avr')
    core.ensure_avr_core = Mock(return_value=Path('/arduino/avr'))
    return core


@pytest.fixture
def mock_source_collection():
    """Create mock source collection."""
    return SourceCollection(
        sketch_sources=[Path('src/main.cpp')],
        core_sources=[Path('core/wiring.c'), Path('core/main.cpp')],
        variant_sources=[Path('variant/pins.c')],
        headers=[]
    )


@pytest.fixture
def mock_compiler():
    """Create mock compiler."""
    compiler = Mock()
    compiler.needs_rebuild = Mock(return_value=True)

    # Mock compile to return success result
    result = Mock()
    result.success = True
    result.object_file = Path('build/output.o')
    compiler.compile = Mock(return_value=result)

    return compiler


@pytest.fixture
def mock_linker():
    """Create mock linker."""
    linker = Mock()

    # Mock link result
    from zapio.build.linker import LinkResult, SizeInfo

    size_info = SizeInfo(
        text=1000,
        data=50,
        bss=100,
        total_flash=1050,
        total_ram=150,
        max_flash=32256,
        max_ram=2048
    )

    link_result = LinkResult(
        success=True,
        elf_path=Path('build/firmware.elf'),
        hex_path=Path('build/firmware.hex'),
        size_info=size_info,
        stdout='',
        stderr=''
    )

    linker.link = Mock(return_value=link_result)
    return linker


# Tests

class TestBuildOrchestratorInit:
    """Test BuildOrchestrator initialization."""

    def test_init_default(self):
        """Test initialization with defaults."""
        orchestrator = BuildOrchestrator()

        assert orchestrator.cache is None
        assert orchestrator.verbose is False

    def test_init_with_cache(self, mock_cache):
        """Test initialization with cache."""
        orchestrator = BuildOrchestrator(cache=mock_cache)

        assert orchestrator.cache == mock_cache

    def test_init_verbose(self):
        """Test initialization with verbose mode."""
        orchestrator = BuildOrchestrator(verbose=True)

        assert orchestrator.verbose is True


class TestParseConfig:
    """Test configuration parsing."""

    def test_parse_config_success(self, tmp_path):
        """Test successful config parsing."""
        # Create platformio.ini
        ini_content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
"""
        ini_path = tmp_path / 'platformio.ini'
        ini_path.write_text(ini_content)

        orchestrator = BuildOrchestrator()
        config = orchestrator._parse_config(tmp_path)

        assert config is not None
        assert 'uno' in config.get_environments()

    def test_parse_config_not_found(self, tmp_path):
        """Test error when platformio.ini not found."""
        orchestrator = BuildOrchestrator()

        with pytest.raises(BuildOrchestratorError) as exc_info:
            orchestrator._parse_config(tmp_path)

        assert 'platformio.ini not found' in str(exc_info.value)

    def test_parse_config_invalid(self, tmp_path):
        """Test error with invalid config."""
        # Create invalid ini file
        ini_path = tmp_path / 'platformio.ini'
        ini_path.write_text('[invalid syntax')

        orchestrator = BuildOrchestrator()

        with pytest.raises(BuildOrchestratorError) as exc_info:
            orchestrator._parse_config(tmp_path)

        assert 'Failed to parse' in str(exc_info.value)


class TestLoadBoardConfig:
    """Test board configuration loading."""

    def test_load_board_config_uno(self):
        """Test loading Uno board config."""
        orchestrator = BuildOrchestrator()
        env_config = {
            'platform': 'atmelavr',
            'board': 'uno',
            'framework': 'arduino'
        }

        board_config = orchestrator._load_board_config('uno', env_config)

        assert board_config.name == 'Arduino Uno'
        assert board_config.mcu == 'atmega328p'
        assert board_config.f_cpu == '16000000L'

    def test_load_board_config_with_overrides(self):
        """Test loading board config with overrides."""
        orchestrator = BuildOrchestrator()
        env_config = {
            'platform': 'atmelavr',
            'board': 'uno',
            'framework': 'arduino',
            'board_build.f_cpu': '8000000L'
        }

        board_config = orchestrator._load_board_config('uno', env_config)

        # Override should be applied
        assert board_config.f_cpu == '8000000L'

    def test_load_board_config_unknown(self):
        """Test error with unknown board."""
        orchestrator = BuildOrchestrator()
        env_config = {
            'platform': 'atmelavr',
            'board': 'unknown_board',
            'framework': 'arduino'
        }

        with pytest.raises(BuildOrchestratorError) as exc_info:
            orchestrator._load_board_config('unknown_board', env_config)

        assert 'Unknown board' in str(exc_info.value)


class TestEnsureToolchain:
    """Test toolchain management."""

    @patch('zapio.build.orchestrator.Toolchain')
    def test_ensure_toolchain_success(self, mock_toolchain_class, mock_cache):
        """Test successful toolchain setup."""
        mock_toolchain = Mock()
        mock_toolchain.ensure_toolchain = Mock(return_value=Path('/toolchain'))
        mock_toolchain_class.return_value = mock_toolchain

        orchestrator = BuildOrchestrator(cache=mock_cache)
        toolchain = orchestrator._ensure_toolchain()

        assert toolchain == mock_toolchain
        mock_toolchain.ensure_toolchain.assert_called_once()

    @patch('zapio.build.orchestrator.Toolchain')
    def test_ensure_toolchain_failure(self, mock_toolchain_class, mock_cache):
        """Test toolchain setup failure."""
        mock_toolchain = Mock()
        mock_toolchain.ensure_toolchain = Mock(side_effect=Exception('Download failed'))
        mock_toolchain_class.return_value = mock_toolchain

        orchestrator = BuildOrchestrator(cache=mock_cache)

        with pytest.raises(BuildOrchestratorError) as exc_info:
            orchestrator._ensure_toolchain()

        assert 'Failed to setup toolchain' in str(exc_info.value)


class TestEnsureArduinoCore:
    """Test Arduino core management."""

    @patch('zapio.build.orchestrator.ArduinoCore')
    def test_ensure_arduino_core_success(self, mock_core_class, mock_cache):
        """Test successful core setup."""
        mock_core = Mock()
        mock_core.AVR_VERSION = '1.8.6'
        mock_core._core_path = Path('/arduino/avr')
        mock_core.ensure_avr_core = Mock(return_value=Path('/arduino/avr'))
        mock_core_class.return_value = mock_core

        orchestrator = BuildOrchestrator(cache=mock_cache)
        core = orchestrator._ensure_arduino_core()

        assert core == mock_core
        mock_core.ensure_avr_core.assert_called_once()

    @patch('zapio.build.orchestrator.ArduinoCore')
    def test_ensure_arduino_core_failure(self, mock_core_class, mock_cache):
        """Test core setup failure."""
        mock_core = Mock()
        mock_core.ensure_avr_core = Mock(side_effect=Exception('Download failed'))
        mock_core_class.return_value = mock_core

        orchestrator = BuildOrchestrator(cache=mock_cache)

        with pytest.raises(BuildOrchestratorError) as exc_info:
            orchestrator._ensure_arduino_core()

        assert 'Failed to setup Arduino core' in str(exc_info.value)


class TestScanSources:
    """Test source scanning."""

    @patch('zapio.build.orchestrator.SourceScanner')
    def test_scan_sources(self, mock_scanner_class, tmp_path, mock_board_config):
        """Test source scanning."""
        mock_scanner = Mock()
        mock_collection = SourceCollection(
            sketch_sources=[Path('src/main.cpp')],
            core_sources=[Path('core/wiring.c')],
            variant_sources=[Path('variant/pins.c')],
            headers=[]
        )
        mock_scanner.scan = Mock(return_value=mock_collection)
        mock_scanner_class.return_value = mock_scanner

        orchestrator = BuildOrchestrator()
        core_path = Path('/arduino/avr')

        sources = orchestrator._scan_sources(
            tmp_path,
            tmp_path / 'build',
            mock_board_config,
            core_path
        )

        assert len(sources.sketch_sources) == 1
        assert len(sources.core_sources) == 1
        assert len(sources.variant_sources) == 1


class TestCreateCompiler:
    """Test compiler creation."""

    def test_create_compiler(self, mock_toolchain, mock_board_config):
        """Test compiler creation with correct parameters."""
        orchestrator = BuildOrchestrator()
        core_path = Path('/arduino/avr')

        with patch('zapio.build.orchestrator.Compiler') as mock_compiler_class:
            mock_compiler = Mock()
            mock_compiler_class.return_value = mock_compiler

            _compiler = orchestrator._create_compiler(
                mock_toolchain,
                mock_board_config,
                core_path
            )

            # Verify Compiler was called with correct arguments
            mock_compiler_class.assert_called_once()
            call_args = mock_compiler_class.call_args

            assert call_args.kwargs['mcu'] == 'atmega328p'
            assert call_args.kwargs['f_cpu'] == '16000000L'


class TestCreateLinker:
    """Test linker creation."""

    def test_create_linker(self, mock_toolchain, mock_board_config):
        """Test linker creation with correct parameters."""
        orchestrator = BuildOrchestrator()

        with patch('zapio.build.orchestrator.Linker') as mock_linker_class:
            mock_linker = Mock()
            mock_linker_class.return_value = mock_linker

            _linker = orchestrator._create_linker(
                mock_toolchain,
                mock_board_config
            )

            # Verify Linker was called with correct arguments
            mock_linker_class.assert_called_once()
            call_args = mock_linker_class.call_args

            assert call_args.kwargs['mcu'] == 'atmega328p'
            assert call_args.kwargs['max_flash'] == 32256
            assert call_args.kwargs['max_ram'] == 2048


class TestCompileSources:
    """Test source compilation."""

    def test_compile_sources_success(self, tmp_path, mock_compiler):
        """Test successful source compilation."""
        orchestrator = BuildOrchestrator()
        sources = [Path('src/main.cpp'), Path('src/helper.cpp')]
        output_dir = tmp_path / 'build'

        objects = orchestrator._compile_sources(
            mock_compiler,
            sources,
            output_dir,
            verbose=False,
            source_type='sketch'
        )

        assert len(objects) == 2
        assert mock_compiler.compile.call_count == 2

    def test_compile_sources_with_cache(self, tmp_path, mock_compiler):
        """Test compilation with cached objects."""
        # Mock needs_rebuild to return False (cached)
        mock_compiler.needs_rebuild = Mock(return_value=False)

        orchestrator = BuildOrchestrator()
        sources = [Path('src/main.cpp')]
        output_dir = tmp_path / 'build'

        objects = orchestrator._compile_sources(
            mock_compiler,
            sources,
            output_dir,
            verbose=False,
            source_type='sketch'
        )

        # Should not compile (using cache)
        assert len(objects) == 1
        mock_compiler.compile.assert_not_called()

    def test_compile_sources_failure(self, tmp_path, mock_compiler):
        """Test compilation failure."""
        # Mock compile to return failure
        result = Mock()
        result.success = False
        result.stderr = 'Compilation error'
        mock_compiler.compile = Mock(return_value=result)

        orchestrator = BuildOrchestrator()
        sources = [Path('src/main.cpp')]
        output_dir = tmp_path / 'build'

        with pytest.raises(BuildOrchestratorError) as exc_info:
            orchestrator._compile_sources(
                mock_compiler,
                sources,
                output_dir,
                verbose=False,
                source_type='sketch'
            )

        assert 'Compilation failed' in str(exc_info.value)


class TestBuildFullProcess:
    """Test complete build process."""

    @patch('zapio.build.orchestrator.PlatformIOConfig')
    @patch('zapio.build.orchestrator.Toolchain')
    @patch('zapio.build.orchestrator.ArduinoCore')
    @patch('zapio.build.orchestrator.SourceScanner')
    @patch('zapio.build.orchestrator.Compiler')
    @patch('zapio.build.orchestrator.Linker')
    def test_build_success(
        self,
        mock_linker_class,
        mock_compiler_class,
        mock_scanner_class,
        mock_core_class,
        mock_toolchain_class,
        mock_config_class,
        tmp_path
    ):
        """Test successful complete build."""
        # Setup mocks
        from zapio.build.linker import LinkResult, SizeInfo

        # PlatformIO config
        mock_config = Mock()
        mock_config.get_default_environment = Mock(return_value='uno')
        mock_config.get_env_config = Mock(return_value={
            'platform': 'atmelavr',
            'board': 'uno',
            'framework': 'arduino'
        })
        mock_config_class.return_value = mock_config

        # Toolchain
        mock_toolchain = Mock()
        mock_toolchain.ensure_toolchain = Mock()
        mock_toolchain.get_all_tools = Mock(return_value={
            'avr-gcc': Path('/bin/avr-gcc'),
            'avr-g++': Path('/bin/avr-g++'),
            'avr-ar': Path('/bin/avr-ar'),
            'avr-objcopy': Path('/bin/avr-objcopy'),
            'avr-size': Path('/bin/avr-size'),
        })
        mock_toolchain_class.return_value = mock_toolchain

        # Arduino core
        mock_core = Mock()
        mock_core.AVR_VERSION = '1.8.6'
        mock_core._core_path = Path('/arduino/avr')
        mock_core.ensure_avr_core = Mock()
        mock_core_class.return_value = mock_core

        # Source scanner
        mock_scanner = Mock()
        mock_sources = SourceCollection(
            sketch_sources=[Path('src/main.cpp')],
            core_sources=[Path('core/wiring.c')],
            variant_sources=[],
            headers=[]
        )
        mock_scanner.scan = Mock(return_value=mock_sources)
        mock_scanner_class.return_value = mock_scanner

        # Compiler
        mock_compiler = Mock()
        mock_compiler.needs_rebuild = Mock(return_value=True)
        compile_result = Mock()
        compile_result.success = True
        compile_result.object_file = Path('build/main.o')
        mock_compiler.compile = Mock(return_value=compile_result)
        mock_compiler_class.return_value = mock_compiler

        # Linker
        mock_linker = Mock()
        size_info = SizeInfo(
            text=1000,
            data=50,
            bss=100,
            total_flash=1050,
            total_ram=150,
            max_flash=32256,
            max_ram=2048
        )
        link_result = LinkResult(
            success=True,
            elf_path=Path('build/firmware.elf'),
            hex_path=Path('build/firmware.hex'),
            size_info=size_info,
            stdout='',
            stderr=''
        )
        mock_linker.link = Mock(return_value=link_result)
        mock_linker_class.return_value = mock_linker

        # Create platformio.ini
        ini_content = """
[env:uno]
platform = atmelavr
board = uno
framework = arduino
"""
        ini_path = tmp_path / 'platformio.ini'
        ini_path.write_text(ini_content)

        # Run build
        orchestrator = BuildOrchestrator()
        result = orchestrator.build(tmp_path, env_name='uno', verbose=False)

        # Verify result
        assert result.success is True
        assert result.hex_path is not None
        assert result.size_info is not None
        assert result.size_info.total_flash == 1050

    @patch('zapio.build.orchestrator.PlatformIOConfig')
    def test_build_no_platformio_ini(self, mock_config_class, tmp_path):
        """Test build with missing platformio.ini."""
        mock_config_class.side_effect = Exception('File not found')

        orchestrator = BuildOrchestrator()
        result = orchestrator.build(tmp_path)

        assert result.success is False
        assert 'platformio.ini not found' in result.message

    @patch('zapio.build.orchestrator.PlatformIOConfig')
    def test_build_no_default_env(self, mock_config_class, tmp_path):
        """Test build with no default environment."""
        mock_config = Mock()
        mock_config.get_default_environment = Mock(return_value=None)
        mock_config_class.return_value = mock_config

        # Create platformio.ini
        ini_path = tmp_path / 'platformio.ini'
        ini_path.write_text('[env:uno]\nboard=uno')

        orchestrator = BuildOrchestrator()
        result = orchestrator.build(tmp_path)

        assert result.success is False
        assert 'No environment specified' in result.message


class TestPrintSizeInfo:
    """Test size info printing."""

    def test_print_size_info(self, capsys):
        """Test size info output."""
        from zapio.build.linker import SizeInfo

        size_info = SizeInfo(
            text=1000,
            data=50,
            bss=100,
            total_flash=1050,
            total_ram=150,
            max_flash=32256,
            max_ram=2048
        )

        orchestrator = BuildOrchestrator()
        orchestrator._print_size_info(size_info)

        captured = capsys.readouterr()
        assert 'Firmware Size:' in captured.out
        assert '1050 bytes' in captured.out
        assert '3.3%' in captured.out  # Flash percentage

    def test_print_size_info_none(self, capsys):
        """Test size info with None."""
        orchestrator = BuildOrchestrator()
        orchestrator._print_size_info(None)

        captured = capsys.readouterr()
        assert captured.out == ''
