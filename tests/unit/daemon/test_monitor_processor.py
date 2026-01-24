"""
Unit tests for MonitorRequestProcessor.

Tests the monitor request processor's operation including:
- Port lock requirements
- Request validation
- Monitor execution
- Timeout handling
"""

import sys
from unittest.mock import MagicMock, patch

import pytest

from fbuild.daemon.daemon_context import DaemonContext
from fbuild.daemon.messages import DaemonState, MonitorRequest, OperationType
from fbuild.daemon.processors.monitor_processor import MonitorRequestProcessor


@pytest.fixture
def mock_context():
    """Create a mock daemon context."""
    context = MagicMock(spec=DaemonContext)
    context.status_manager = MagicMock()
    context.lock_manager = MagicMock()
    context.operation_registry = MagicMock()
    context.port_state_manager = MagicMock()
    context.shared_serial_manager = MagicMock()
    # Return None from get_session_info to indicate port is not already managed
    context.shared_serial_manager.get_session_info.return_value = None
    return context


@pytest.fixture
def monitor_request():
    """Create a test monitor request."""
    return MonitorRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        port="/dev/ttyUSB0",
        baud_rate=115200,
        halt_on_error=None,
        halt_on_success=None,
        expect=None,
        timeout=None,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )


@pytest.fixture
def processor():
    """Create a MonitorRequestProcessor instance."""
    return MonitorRequestProcessor()


def test_get_operation_type(processor):
    """Test that processor returns MONITOR operation type."""
    assert processor.get_operation_type() == OperationType.MONITOR


def test_get_required_locks_with_port(processor, monitor_request, mock_context):
    """Test that processor requires port lock."""
    locks = processor.get_required_locks(monitor_request, mock_context)

    assert locks == {"port": "/dev/ttyUSB0"}


def test_get_required_locks_without_port(processor, mock_context):
    """Test lock requirements when port is not specified."""
    request = MonitorRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        port=None,
        baud_rate=115200,
        halt_on_error=None,
        halt_on_success=None,
        expect=None,
        timeout=None,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )

    locks = processor.get_required_locks(request, mock_context)

    assert locks == {}


def test_validate_request_with_port(processor, monitor_request, mock_context):
    """Test request validation with valid port."""
    assert processor.validate_request(monitor_request, mock_context) is True


def test_validate_request_without_port(processor, mock_context):
    """Test request validation fails without port."""
    request = MonitorRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        port=None,
        baud_rate=115200,
        halt_on_error=None,
        halt_on_success=None,
        expect=None,
        timeout=None,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )

    assert processor.validate_request(request, mock_context) is False


def test_get_starting_state(processor):
    """Test that monitor starts in MONITORING state."""
    assert processor.get_starting_state() == DaemonState.MONITORING


def test_get_status_messages(processor, monitor_request):
    """Test status message generation."""
    starting = processor.get_starting_message(monitor_request)
    assert "Monitoring" in starting
    assert "esp32dev" in starting
    assert "/dev/ttyUSB0" in starting

    assert "completed" in processor.get_success_message(monitor_request).lower()
    assert "failed" in processor.get_failure_message(monitor_request).lower()


def test_execute_operation_success(processor, monitor_request, mock_context):
    """Test successful monitor execution."""
    # Mock the monitor
    mock_monitor = MagicMock()
    mock_monitor.monitor.return_value = 0  # Success exit code

    mock_monitor_class = MagicMock(return_value=mock_monitor)

    with patch.dict(sys.modules, {"fbuild.deploy.monitor": MagicMock(SerialMonitor=mock_monitor_class)}):
        with patch("pathlib.Path.mkdir"):
            with patch("pathlib.Path.write_text"):
                with patch("pathlib.Path.exists", return_value=False):
                    result = processor.execute_operation(monitor_request, mock_context)

    assert result is True
    mock_monitor.monitor.assert_called_once()


def test_execute_operation_failure(processor, monitor_request, mock_context):
    """Test monitor execution with failure."""
    # Mock the monitor
    mock_monitor = MagicMock()
    mock_monitor.monitor.return_value = 1  # Failure exit code

    mock_monitor_class = MagicMock(return_value=mock_monitor)

    with patch.dict(sys.modules, {"fbuild.deploy.monitor": MagicMock(SerialMonitor=mock_monitor_class)}):
        with patch("pathlib.Path.mkdir"):
            with patch("pathlib.Path.write_text"):
                with patch("pathlib.Path.exists", return_value=False):
                    result = processor.execute_operation(monitor_request, mock_context)

    assert result is False


def test_execute_operation_monitor_import_error(processor, monitor_request, mock_context):
    """Test monitor execution when SerialMonitor import fails."""
    # Remove the monitor module from sys.modules to simulate import failure
    # We need to patch sys.modules.get to raise KeyError for the specific module
    original_modules = sys.modules.copy()

    # Create a patched modules dict that raises KeyError for fbuild.deploy.monitor
    class ModulesDict(dict):
        def __getitem__(self, key):
            if key == "fbuild.deploy.monitor":
                raise KeyError(key)
            return super().__getitem__(key)

    patched_modules = ModulesDict(original_modules)
    # Remove the actual module if it exists
    patched_modules.pop("fbuild.deploy.monitor", None)

    with patch.dict(sys.modules, patched_modules, clear=True):
        with patch("pathlib.Path.mkdir"):
            with patch("pathlib.Path.write_text"):
                with patch("pathlib.Path.exists", return_value=False):
                    result = processor.execute_operation(monitor_request, mock_context)

    assert result is False


def test_execute_operation_with_timeout(processor, mock_context):
    """Test monitor execution with timeout."""
    request = MonitorRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        port="/dev/ttyUSB0",
        baud_rate=115200,
        halt_on_error=None,
        halt_on_success=None,
        expect=None,
        timeout=30.0,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )

    # Mock the monitor
    mock_monitor = MagicMock()
    mock_monitor.monitor.return_value = 0

    mock_monitor_class = MagicMock(return_value=mock_monitor)

    with patch.dict(sys.modules, {"fbuild.deploy.monitor": MagicMock(SerialMonitor=mock_monitor_class)}):
        with patch("pathlib.Path.mkdir"):
            with patch("pathlib.Path.write_text"):
                with patch("pathlib.Path.exists", return_value=False):
                    result = processor.execute_operation(request, mock_context)

    assert result is True
    # Verify timeout was passed as integer
    call_args = mock_monitor.monitor.call_args
    assert call_args[1]["timeout"] == 30


def test_execute_operation_with_halt_patterns(processor, mock_context):
    """Test monitor execution with halt patterns."""
    request = MonitorRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        port="/dev/ttyUSB0",
        baud_rate=115200,
        halt_on_error="ERROR:",
        halt_on_success="SUCCESS:",
        expect=["pattern1", "pattern2"],
        timeout=None,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )

    # Mock the monitor
    mock_monitor = MagicMock()
    mock_monitor.monitor.return_value = 0

    mock_monitor_class = MagicMock(return_value=mock_monitor)

    with patch.dict(sys.modules, {"fbuild.deploy.monitor": MagicMock(SerialMonitor=mock_monitor_class)}):
        with patch("pathlib.Path.mkdir"):
            with patch("pathlib.Path.write_text"):
                with patch("pathlib.Path.exists", return_value=False):
                    result = processor.execute_operation(request, mock_context)

    assert result is True
    # Verify halt patterns were passed
    call_args = mock_monitor.monitor.call_args
    assert call_args[1]["halt_on_error"] == "ERROR:"
    assert call_args[1]["halt_on_success"] == "SUCCESS:"
    assert call_args[1]["expect"] == ["pattern1", "pattern2"]


def test_execute_operation_creates_output_files(processor, monitor_request, mock_context):
    """Test that monitor execution creates output and summary files."""
    mock_monitor = MagicMock()
    mock_monitor.monitor.return_value = 0

    mock_monitor_class = MagicMock(return_value=mock_monitor)

    with patch.dict(sys.modules, {"fbuild.deploy.monitor": MagicMock(SerialMonitor=mock_monitor_class)}):
        with patch("pathlib.Path.mkdir") as mock_mkdir:
            with patch("pathlib.Path.write_text") as mock_write:
                with patch("pathlib.Path.exists", return_value=True):
                    with patch("pathlib.Path.unlink") as mock_unlink:
                        result = processor.execute_operation(monitor_request, mock_context)

    assert result is True
    # Should create parent directory
    mock_mkdir.assert_called()
    # Should write empty output file
    mock_write.assert_called_once_with("", encoding="utf-8")
    # Should delete old summary file if it exists
    mock_unlink.assert_called_once()


def test_execute_operation_uses_default_baud_rate(processor, mock_context):
    """Test that monitor uses default baud rate when not specified."""
    request = MonitorRequest(
        project_dir="/path/to/project",
        environment="esp32dev",
        port="/dev/ttyUSB0",
        baud_rate=None,  # No baud rate specified
        halt_on_error=None,
        halt_on_success=None,
        expect=None,
        timeout=None,
        caller_pid=12345,
        caller_cwd="/home/user",
        request_id="test-request-123",
    )

    mock_monitor = MagicMock()
    mock_monitor.monitor.return_value = 0

    mock_monitor_class = MagicMock(return_value=mock_monitor)

    with patch.dict(sys.modules, {"fbuild.deploy.monitor": MagicMock(SerialMonitor=mock_monitor_class)}):
        with patch("pathlib.Path.mkdir"):
            with patch("pathlib.Path.write_text"):
                with patch("pathlib.Path.exists", return_value=False):
                    result = processor.execute_operation(request, mock_context)

    assert result is True
    # Verify default baud rate was used (115200)
    call_args = mock_monitor.monitor.call_args
    assert call_args[1]["baud"] == 115200
