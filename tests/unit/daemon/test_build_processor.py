"""
Unit tests for BuildRequestProcessor.

Tests the build request processor's operation including:
- Module reloading
- Lock requirements
- Build execution
- Error handling
"""

import sys
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from fbuild.daemon.daemon_context import DaemonContext
from fbuild.daemon.messages import BuildRequest, OperationType
from fbuild.daemon.processors.build_processor import BuildRequestProcessor


@pytest.fixture
def mock_context():
    """Create a mock daemon context."""
    context = MagicMock(spec=DaemonContext)
    context.status_manager = MagicMock()
    context.lock_manager = MagicMock()
    context.operation_registry = MagicMock()
    context.compilation_queue = MagicMock()
    return context


@pytest.fixture
def build_request():
    """Create a test build request."""
    return BuildRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        clean_build=False,
        verbose=False,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )


@pytest.fixture
def processor():
    """Create a BuildRequestProcessor instance."""
    return BuildRequestProcessor()


def test_get_operation_type(processor):
    """Test that processor returns BUILD operation type."""
    assert processor.get_operation_type() == OperationType.BUILD


def test_get_required_locks(processor, build_request, mock_context):
    """Test that processor requires only project lock."""
    locks = processor.get_required_locks(build_request, mock_context)

    assert locks == {"project": "/path/to/project"}
    assert "port" not in locks


def test_execute_operation_success(processor, build_request, mock_context):
    """Test successful build execution."""
    # Mock the orchestrator
    mock_orchestrator = MagicMock()
    mock_build_result = MagicMock()
    mock_build_result.success = True
    mock_orchestrator.build.return_value = mock_build_result

    # Mock the orchestrator class in sys.modules
    mock_orchestrator_class = MagicMock(return_value=mock_orchestrator)

    # Mock platformio.ini existence and config
    mock_config = MagicMock()
    mock_config.get_env_config.return_value = {"platform": "espressif32"}

    with patch.object(sys, "modules", {"fbuild.build.orchestrator_esp32": MagicMock(OrchestratorESP32=mock_orchestrator_class), **sys.modules}):
        with patch.object(processor, "_reload_build_modules"):
            with patch("pathlib.Path.exists", return_value=True):
                with patch("fbuild.config.ini_parser.PlatformIOConfig", return_value=mock_config):
                    result = processor.execute_operation(build_request, mock_context)

    assert result is True
    mock_orchestrator.build.assert_called_once_with(
        project_dir=Path("/path/to/project"),
        env_name="esp32dev",
        clean=False,
        verbose=False,
        jobs=None,
        queue=mock_context.compilation_queue,
    )


def test_execute_operation_build_failure(processor, build_request, mock_context):
    """Test build execution with build failure."""
    # Mock the orchestrator
    mock_orchestrator = MagicMock()
    mock_build_result = MagicMock()
    mock_build_result.success = False
    mock_build_result.message = "Compilation error"
    mock_orchestrator.build.return_value = mock_build_result

    # Mock the orchestrator class in sys.modules
    mock_orchestrator_class = MagicMock(return_value=mock_orchestrator)

    with patch.object(sys, "modules", {"fbuild.build.orchestrator_avr": MagicMock(BuildOrchestratorAVR=mock_orchestrator_class), **sys.modules}):
        with patch.object(processor, "_reload_build_modules"):
            result = processor.execute_operation(build_request, mock_context)

    assert result is False


def test_execute_operation_orchestrator_import_error(processor, build_request, mock_context):
    """Test build execution when orchestrator import fails."""
    with patch.object(sys, "modules", {}):
        with patch.object(processor, "_reload_build_modules"):
            result = processor.execute_operation(build_request, mock_context)

    assert result is False


def test_execute_operation_orchestrator_attribute_error(processor, build_request, mock_context):
    """Test build execution when orchestrator class is missing."""
    with patch.object(sys, "modules", {"fbuild.build.orchestrator_avr": MagicMock(spec=[]), **sys.modules}):  # No BuildOrchestratorAVR attribute
        with patch.object(processor, "_reload_build_modules"):
            result = processor.execute_operation(build_request, mock_context)

    assert result is False


def test_execute_operation_with_clean_build(processor, mock_context):
    """Test build execution with clean build flag."""
    request = BuildRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        clean_build=True,
        verbose=True,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )

    # Mock the orchestrator
    mock_orchestrator = MagicMock()
    mock_build_result = MagicMock()
    mock_build_result.success = True
    mock_orchestrator.build.return_value = mock_build_result

    # Mock the orchestrator class in sys.modules
    mock_orchestrator_class = MagicMock(return_value=mock_orchestrator)

    # Mock platformio.ini existence and config
    mock_config = MagicMock()
    mock_config.get_env_config.return_value = {"platform": "espressif32"}

    with patch.object(sys, "modules", {"fbuild.build.orchestrator_esp32": MagicMock(OrchestratorESP32=mock_orchestrator_class), **sys.modules}):
        with patch.object(processor, "_reload_build_modules"):
            with patch("pathlib.Path.exists", return_value=True):
                with patch("fbuild.config.ini_parser.PlatformIOConfig", return_value=mock_config):
                    result = processor.execute_operation(request, mock_context)

    assert result is True
    mock_orchestrator.build.assert_called_once_with(
        project_dir=Path("/path/to/project"),
        env_name="esp32dev",
        clean=True,
        verbose=True,
        jobs=None,
        queue=mock_context.compilation_queue,
    )


def test_reload_build_modules(processor):
    """Test module reloading logic."""
    # Create actual module objects (not MagicMock) for importlib.reload
    import types

    mock_module1 = types.ModuleType("fbuild.packages.downloader")
    mock_module2 = types.ModuleType("fbuild.build.compiler")

    # Save original sys.modules
    original_modules = sys.modules.copy()

    # Add our test modules to sys.modules
    sys.modules["fbuild.packages.downloader"] = mock_module1
    sys.modules["fbuild.build.compiler"] = mock_module2

    try:
        with patch("importlib.reload") as mock_reload:
            # Configure reload to return the module
            mock_reload.side_effect = lambda m: m

            processor._reload_build_modules()

            # Should attempt to reload existing modules (at least the ones we provided)
            assert mock_reload.call_count >= 2
    finally:
        # Restore original sys.modules
        sys.modules.clear()
        sys.modules.update(original_modules)


def test_reload_build_modules_handles_errors(processor):
    """Test that module reloading handles errors gracefully."""
    # Create a mock module that raises an error when reloaded
    mock_module = MagicMock()

    with patch.object(sys, "modules", {"fbuild.packages.downloader": mock_module}):
        with patch("importlib.reload", side_effect=Exception("Import error")):
            # Should not raise exception
            processor._reload_build_modules()


def test_reload_build_modules_handles_keyboard_interrupt(processor):
    """Test that module reloading properly handles KeyboardInterrupt."""
    import types

    mock_module = types.ModuleType("fbuild.packages.downloader")

    # Use a side_effect that raises KeyboardInterrupt only once, then returns module
    call_count = [0]

    def raise_once(module):
        call_count[0] += 1
        if call_count[0] == 1:
            raise KeyboardInterrupt()
        return module

    # Save original sys.modules
    original_modules = sys.modules.copy()
    sys.modules["fbuild.packages.downloader"] = mock_module

    try:
        with patch("importlib.reload", side_effect=raise_once):
            with patch("fbuild.interrupt_utils.handle_keyboard_interrupt_properly") as mock_handler:
                processor._reload_build_modules()
                # Handler should be called at least once
                assert mock_handler.call_count >= 1
    finally:
        # Restore original sys.modules
        sys.modules.clear()
        sys.modules.update(original_modules)
