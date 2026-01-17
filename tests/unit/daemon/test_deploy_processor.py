"""
Unit tests for DeployRequestProcessor.

Tests the deploy request processor's operation including:
- Lock requirements (project + port)
- Build + deploy coordination
- Monitor after deploy
- Multi-phase status updates
"""

import sys
from unittest.mock import MagicMock, patch

import pytest

from fbuild.daemon.daemon_context import DaemonContext
from fbuild.daemon.messages import DaemonState, DeployRequest, OperationType
from fbuild.daemon.processors.deploy_processor import DeployRequestProcessor


@pytest.fixture
def mock_context():
    """Create a mock daemon context."""
    context = MagicMock(spec=DaemonContext)
    context.status_manager = MagicMock()
    context.lock_manager = MagicMock()
    context.operation_registry = MagicMock()
    context.port_state_manager = MagicMock()
    return context


@pytest.fixture
def deploy_request():
    """Create a test deploy request."""
    return DeployRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        clean_build=False,
        port="/dev/ttyUSB0",
        monitor_after=False,
        monitor_halt_on_error=None,
        monitor_halt_on_success=None,
        monitor_expect=None,
        monitor_timeout=None,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )


@pytest.fixture
def processor():
    """Create a DeployRequestProcessor instance."""
    return DeployRequestProcessor()


def test_get_operation_type(processor):
    """Test that processor returns DEPLOY operation type."""
    assert processor.get_operation_type() == OperationType.DEPLOY


def test_get_required_locks_with_port(processor, deploy_request, mock_context):
    """Test that processor requires project and port locks."""
    locks = processor.get_required_locks(deploy_request, mock_context)

    assert locks == {"project": "/path/to/project", "port": "/dev/ttyUSB0"}


def test_get_required_locks_without_port(processor, mock_context):
    """Test lock requirements when port is not specified."""
    request = DeployRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        clean_build=False,
        port=None,
        monitor_after=False,
        monitor_halt_on_error=None,
        monitor_halt_on_success=None,
        monitor_expect=None,
        monitor_timeout=None,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )

    locks = processor.get_required_locks(request, mock_context)

    assert locks == {"project": "/path/to/project"}
    assert "port" not in locks


def test_get_starting_state(processor):
    """Test that deploy starts in DEPLOYING state."""
    assert processor.get_starting_state() == DaemonState.DEPLOYING


def test_get_status_messages(processor, deploy_request):
    """Test status message generation."""
    assert "Deploying esp32dev" in processor.get_starting_message(deploy_request)
    assert "successful" in processor.get_success_message(deploy_request).lower()
    assert "failed" in processor.get_failure_message(deploy_request).lower()


def test_execute_operation_success(processor, deploy_request, mock_context):
    """Test successful deploy execution."""
    # Mock successful build
    mock_build_result = MagicMock()
    mock_build_result.success = True

    # Mock successful deploy
    mock_deploy_result = MagicMock()
    mock_deploy_result.success = True
    mock_deploy_result.port = "/dev/ttyUSB0"

    with patch.object(processor, "_build_firmware", return_value=True):
        with patch.object(processor, "_deploy_firmware", return_value="/dev/ttyUSB0"):
            result = processor.execute_operation(deploy_request, mock_context)

    assert result is True


def test_execute_operation_build_failure(processor, deploy_request, mock_context):
    """Test deploy execution when build fails."""
    with patch.object(processor, "_build_firmware", return_value=False):
        result = processor.execute_operation(deploy_request, mock_context)

    assert result is False


def test_execute_operation_deploy_failure(processor, deploy_request, mock_context):
    """Test deploy execution when deploy fails."""
    with patch.object(processor, "_build_firmware", return_value=True):
        with patch.object(processor, "_deploy_firmware", return_value=None):
            result = processor.execute_operation(deploy_request, mock_context)

    assert result is False


def test_execute_operation_with_monitor(processor, mock_context):
    """Test deploy execution with monitor after deploy."""
    request = DeployRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        clean_build=False,
        port="/dev/ttyUSB0",
        monitor_after=True,
        monitor_halt_on_error=None,
        monitor_halt_on_success=None,
        monitor_expect=None,
        monitor_timeout=None,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )

    with patch.object(processor, "_build_firmware", return_value=True):
        with patch.object(processor, "_deploy_firmware", return_value="/dev/ttyUSB0"):
            with patch.object(processor, "_start_monitoring") as mock_monitor:
                result = processor.execute_operation(request, mock_context)

    assert result is True
    mock_monitor.assert_called_once()


def test_build_firmware_success(processor, deploy_request, mock_context, tmp_path):
    """Test successful firmware build."""
    # Create a temporary platformio.ini file
    platformio_ini = tmp_path / "platformio.ini"
    platformio_ini.write_text("[env:esp32dev]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n")

    # Update deploy_request to use the temporary directory
    deploy_request.project_dir = str(tmp_path)

    # Mock the orchestrator
    mock_orchestrator = MagicMock()
    mock_build_result = MagicMock()
    mock_build_result.success = True
    mock_orchestrator.build.return_value = mock_build_result

    mock_orchestrator_class = MagicMock(return_value=mock_orchestrator)

    # Mock sys.modules to contain the orchestrator class
    mock_module = MagicMock()
    mock_module.OrchestratorESP32 = mock_orchestrator_class

    with patch.dict(sys.modules, {"fbuild.build.orchestrator_esp32": mock_module}):
        with patch.object(processor, "_reload_build_modules"):
            with patch.object(processor, "_update_status"):
                with patch("fbuild.packages.cache.Cache"):
                    result = processor._build_firmware(deploy_request, mock_context)

    assert result is True


def test_build_firmware_failure(processor, deploy_request, mock_context, tmp_path):
    """Test firmware build failure."""
    # Create a temporary platformio.ini file
    platformio_ini = tmp_path / "platformio.ini"
    platformio_ini.write_text("[env:esp32dev]\nplatform = espressif32\nboard = esp32dev\nframework = arduino\n")

    # Update deploy_request to use the temporary directory
    deploy_request.project_dir = str(tmp_path)

    # Mock the orchestrator
    mock_orchestrator = MagicMock()
    mock_build_result = MagicMock()
    mock_build_result.success = False
    mock_build_result.message = "Compilation error"
    mock_orchestrator.build.return_value = mock_build_result

    mock_orchestrator_class = MagicMock(return_value=mock_orchestrator)

    # Mock sys.modules to contain the orchestrator class
    mock_module = MagicMock()
    mock_module.OrchestratorESP32 = mock_orchestrator_class

    with patch.dict(sys.modules, {"fbuild.build.orchestrator_esp32": mock_module}):
        with patch.object(processor, "_reload_build_modules"):
            with patch.object(processor, "_update_status"):
                with patch("fbuild.packages.cache.Cache"):
                    result = processor._build_firmware(deploy_request, mock_context)

    assert result is False


def test_deploy_firmware_success(processor, deploy_request, mock_context):
    """Test successful firmware deployment."""
    # Mock the deployer
    mock_deployer = MagicMock()
    mock_deploy_result = MagicMock()
    mock_deploy_result.success = True
    mock_deploy_result.port = "/dev/ttyUSB0"
    mock_deployer.deploy.return_value = mock_deploy_result

    mock_deployer_class = MagicMock(return_value=mock_deployer)

    with patch.object(sys, "modules", {"fbuild.deploy.deployer_esp32": MagicMock(ESP32Deployer=mock_deployer_class), **sys.modules}):
        with patch.object(processor, "_update_status"):
            result = processor._deploy_firmware(deploy_request, mock_context)

    assert result == "/dev/ttyUSB0"


def test_deploy_firmware_failure(processor, deploy_request, mock_context):
    """Test firmware deployment failure."""
    # Mock the deployer
    mock_deployer = MagicMock()
    mock_deploy_result = MagicMock()
    mock_deploy_result.success = False
    mock_deploy_result.message = "Upload failed"
    mock_deployer.deploy.return_value = mock_deploy_result

    mock_deployer_class = MagicMock(return_value=mock_deployer)

    with patch.object(sys, "modules", {"fbuild.deploy.deployer_esp32": MagicMock(ESP32Deployer=mock_deployer_class), **sys.modules}):
        with patch.object(processor, "_update_status"):
            result = processor._deploy_firmware(deploy_request, mock_context)

    assert result is None


def test_start_monitoring(processor, deploy_request, mock_context):
    """Test starting monitor after deploy."""
    # Mock MonitorRequestProcessor at its actual import location (inside the function)
    with patch("fbuild.daemon.processors.monitor_processor.MonitorRequestProcessor") as mock_processor_class:
        mock_processor = MagicMock()
        mock_processor_class.return_value = mock_processor
        mock_processor.process_request = MagicMock()

        with patch.object(processor, "_update_status"):
            processor._start_monitoring(deploy_request, "/dev/ttyUSB0", mock_context)

        # Should create processor and process monitor request
        mock_processor_class.assert_called_once()
        mock_processor.process_request.assert_called_once()


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
