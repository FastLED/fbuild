"""
Unit tests for MCP tools, resources, and prompts.

Tests mock get_daemon_context() and call MCP tool functions directly.
"""

import json
import threading
import time
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from fbuild.daemon.daemon_context import DaemonContext

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def mock_context():
    """Create a mock DaemonContext with all subsystems."""
    context = MagicMock(spec=DaemonContext)

    # Core identity
    context.daemon_pid = 12345
    context.daemon_started_at = time.time() - 60.0

    # Status manager
    mock_status = MagicMock()
    mock_status.state = MagicMock()
    mock_status.state.value = "idle"
    mock_status.message = "Ready"
    context.status_manager = MagicMock()
    context.status_manager.read_status.return_value = mock_status
    context.status_manager.get_operation_in_progress.return_value = False

    # Operation tracking
    context.operation_in_progress = False
    context.operation_lock = threading.Lock()

    # Device manager
    context.device_manager = MagicMock()
    context.device_manager.get_all_devices.return_value = {}

    # Lock manager
    context.lock_manager = MagicMock()
    context.lock_manager.get_lock_status.return_value = {"port_locks": {}, "project_locks": {}}
    stale_summary = MagicMock()
    stale_summary.stale_port_locks = []
    stale_summary.stale_project_locks = []
    stale_summary.has_stale_locks.return_value = False
    context.lock_manager.get_stale_locks.return_value = stale_summary
    context.lock_manager.force_release_stale_locks.return_value = 0

    # Compilation queue
    context.compilation_queue = MagicMock()
    context.compilation_queue.get_statistics.return_value = {
        "pending": 0,
        "running": 0,
        "completed": 5,
        "failed": 1,
        "cancelled": 0,
        "total_jobs": 6,
    }

    # Operation registry
    context.operation_registry = MagicMock()
    context.operation_registry.operations = {}
    context.operation_registry.get_operation.return_value = None
    context.operation_registry.get_operations_by_project.return_value = []

    # Error collector
    context.error_collector = MagicMock()
    context.error_collector.get_errors.return_value = []
    context.error_collector.get_errors_by_phase.return_value = []
    context.error_collector.get_error_count.return_value = {"warnings": 0, "errors": 0, "fatal": 0, "total": 0}
    context.error_collector.format_summary.return_value = "No errors"
    context.error_collector.has_errors.return_value = False
    context.error_collector.has_warnings.return_value = False
    context.error_collector.format_errors.return_value = "No errors"

    # Firmware ledger
    context.firmware_ledger = MagicMock()
    context.firmware_ledger.get_deployment.return_value = None

    # Client manager
    context.client_manager = MagicMock()
    context.client_manager.get_all_clients.return_value = {}

    return context


@pytest.fixture(autouse=True)
def _patch_context(mock_context):
    """Patch get_daemon_context to return our mock for all tests."""
    with patch("fbuild.daemon.fastapi_app.get_daemon_context", return_value=mock_context):
        yield


# ===========================================================================
# Query tool tests
# ===========================================================================


class TestGetDaemonStatus:
    def test_returns_expected_fields(self, mock_context):
        from fbuild.daemon.mcp.tools_query import get_daemon_status

        with patch("fbuild.daemon.client.http_utils.get_daemon_port", return_value=8865):
            result = get_daemon_status()

        assert result["pid"] == 12345
        assert result["version"]
        assert result["port"] == 8865
        assert result["state"] == "idle"
        assert result["operation_in_progress"] is False


class TestListDevices:
    def test_empty_devices(self):
        from fbuild.daemon.mcp.tools_query import list_devices

        result = list_devices()
        assert result["device_count"] == 0
        assert result["devices"] == []

    def test_with_devices(self, mock_context):
        from fbuild.daemon.mcp.tools_query import list_devices

        mock_state = MagicMock()
        mock_state.__dict__ = {"port": "COM3", "is_connected": True}
        mock_context.device_manager.get_all_devices.return_value = {"dev-1": mock_state}

        result = list_devices()
        assert result["device_count"] == 1
        assert result["devices"][0]["device_id"] == "dev-1"


class TestGetLockStatus:
    def test_empty_locks(self):
        from fbuild.daemon.mcp.tools_query import get_lock_status

        result = get_lock_status()
        assert result["active_port_lock_count"] == 0
        assert result["active_project_lock_count"] == 0


class TestGetBuildQueueStatus:
    def test_returns_queue_stats(self):
        from fbuild.daemon.mcp.tools_query import get_build_queue_status

        result = get_build_queue_status()
        assert result["completed"] == 5
        assert result["failed"] == 1
        assert result["total_jobs"] == 6


class TestGetOperationStatus:
    def test_not_found(self):
        from fbuild.daemon.mcp.tools_query import get_operation_status

        result = get_operation_status("nonexistent-id")
        assert result["found"] is False

    def test_found(self, mock_context):
        from fbuild.daemon.mcp.tools_query import get_operation_status

        mock_op = MagicMock()
        mock_op.operation_id = "op-123"
        mock_op.operation_type.value = "build"
        mock_op.state.value = "completed"
        mock_op.project_dir = "/project"
        mock_op.environment = "uno"
        mock_op.duration.return_value = 12.3
        mock_op.error_message = None
        mock_context.operation_registry.get_operation.return_value = mock_op

        result = get_operation_status("op-123")
        assert result["found"] is True
        assert result["type"] == "build"
        assert result["state"] == "completed"


class TestGetOperationHistory:
    def test_empty_history(self):
        from fbuild.daemon.mcp.tools_query import get_operation_history

        result = get_operation_history()
        assert result["count"] == 0
        assert result["operations"] == []

    def test_with_project_filter(self, mock_context):
        from fbuild.daemon.mcp.tools_query import get_operation_history

        mock_op = MagicMock()
        mock_op.operation_id = "op-1"
        mock_op.operation_type.value = "build"
        mock_op.state.value = "completed"
        mock_op.project_dir = "/proj"
        mock_op.environment = "uno"
        mock_op.created_at = time.time()
        mock_op.duration.return_value = 5.0
        mock_op.error_message = None
        mock_context.operation_registry.get_operations_by_project.return_value = [mock_op]

        result = get_operation_history(project_dir="/proj")
        assert result["count"] == 1
        assert result["operations"][0]["operation_id"] == "op-1"

    def test_limit(self, mock_context):
        from fbuild.daemon.mcp.tools_query import get_operation_history

        ops = []
        for i in range(5):
            mock_op = MagicMock()
            mock_op.operation_id = f"op-{i}"
            mock_op.operation_type.value = "build"
            mock_op.state.value = "completed"
            mock_op.project_dir = "/proj"
            mock_op.environment = "uno"
            mock_op.created_at = time.time() - i
            mock_op.duration.return_value = 1.0
            mock_op.error_message = None
            ops.append(mock_op)

        mock_context.operation_registry.operations = {f"op-{i}": ops[i] for i in range(5)}

        result = get_operation_history(limit=2)
        assert result["count"] == 2


class TestGetBuildErrors:
    def test_no_errors(self):
        from fbuild.daemon.mcp.tools_query import get_build_errors

        result = get_build_errors()
        assert result["errors"] == []
        assert result["summary"] == "No errors"

    def test_with_phase_filter(self, mock_context):
        from fbuild.daemon.mcp.tools_query import get_build_errors

        mock_err = MagicMock()
        mock_err.severity.value = "error"
        mock_err.phase = "compile"
        mock_err.file_path = "main.cpp"
        mock_err.error_message = "syntax error"
        mock_err.stderr = None
        mock_err.timestamp = time.time()
        mock_context.error_collector.get_errors_by_phase.return_value = [mock_err]

        result = get_build_errors(phase="compile")
        assert len(result["errors"]) == 1
        assert result["errors"][0]["phase"] == "compile"
        mock_context.error_collector.get_errors_by_phase.assert_called_with("compile")


class TestGetFirmwareStatus:
    def test_no_firmware(self):
        from fbuild.daemon.mcp.tools_query import get_firmware_status

        result = get_firmware_status("COM3")
        assert result["found"] is False
        assert result["port"] == "COM3"

    def test_with_firmware(self, mock_context):
        from fbuild.daemon.mcp.tools_query import get_firmware_status

        entry = MagicMock()
        entry.firmware_hash = "abc123"
        entry.source_hash = "def456"
        entry.project_dir = "/proj"
        entry.environment = "uno"
        entry.upload_timestamp = time.time()
        entry.is_stale.return_value = False
        mock_context.firmware_ledger.get_deployment.return_value = entry

        result = get_firmware_status("COM3")
        assert result["found"] is True
        assert result["firmware_hash"] == "abc123"
        assert result["is_stale"] is False


class TestGetConnectedClients:
    def test_no_clients(self):
        from fbuild.daemon.mcp.tools_query import get_connected_clients

        result = get_connected_clients()
        assert result["client_count"] == 0

    def test_with_clients(self, mock_context):
        from fbuild.daemon.mcp.tools_query import get_connected_clients

        client_info = MagicMock()
        client_info.client_id = "client-1"
        client_info.pid = 9999
        client_info.connect_time = time.time()
        client_info.last_heartbeat = time.time()
        client_info.metadata = {}
        client_info.is_alive.return_value = True
        mock_context.client_manager.get_all_clients.return_value = {"client-1": client_info}

        result = get_connected_clients()
        assert result["client_count"] == 1
        assert result["clients"][0]["client_id"] == "client-1"
        assert result["clients"][0]["is_alive"] is True


# ===========================================================================
# Action tool tests
# ===========================================================================


class TestTriggerBuild:
    def test_rejects_when_busy(self, mock_context):
        from mcp.server.fastmcp.exceptions import ToolError

        from fbuild.daemon.mcp.tools_action import trigger_build

        mock_context.operation_in_progress = True

        with pytest.raises(ToolError, match="already in progress"):
            trigger_build(
                project_dir="/proj",
                environment="uno",
                clean=False,
                verbose=False,
            )

    def test_success(self, mock_context):
        from fbuild.daemon.mcp.tools_action import trigger_build

        with patch("fbuild.daemon.processors.build_processor.BuildRequestProcessor.process_request", return_value=True):
            result = trigger_build(
                project_dir="/proj",
                environment="uno",
                clean=False,
                verbose=False,
            )

        assert result["success"] is True
        assert result["exit_code"] == 0

    def test_failure(self, mock_context):
        from fbuild.daemon.mcp.tools_action import trigger_build

        with patch("fbuild.daemon.processors.build_processor.BuildRequestProcessor.process_request", return_value=False):
            result = trigger_build(
                project_dir="/proj",
                environment="uno",
                clean=True,
                verbose=True,
                jobs=4,
            )

        assert result["success"] is False
        assert result["exit_code"] == 1


class TestTriggerDeploy:
    def test_rejects_when_busy(self, mock_context):
        from mcp.server.fastmcp.exceptions import ToolError

        from fbuild.daemon.mcp.tools_action import trigger_deploy

        mock_context.operation_in_progress = True

        with pytest.raises(ToolError, match="already in progress"):
            trigger_deploy(
                project_dir="/proj",
                environment="uno",
            )

    def test_success(self, mock_context):
        from fbuild.daemon.mcp.tools_action import trigger_deploy

        with patch("fbuild.daemon.processors.deploy_processor.DeployRequestProcessor.process_request", return_value=True):
            result = trigger_deploy(
                project_dir="/proj",
                environment="esp32c6",
                port="COM3",
            )

        assert result["success"] is True


class TestRefreshDevices:
    def test_returns_devices(self, mock_context):
        from fbuild.daemon.mcp.tools_action import refresh_devices

        mock_dev = MagicMock()
        mock_dev.device_id = "dev-1"
        mock_dev.port = "COM3"
        mock_dev.description = "USB Serial"
        mock_dev.hwid = "USB VID:PID=1234:5678"
        mock_context.device_manager.refresh_devices.return_value = [mock_dev]

        result = refresh_devices()
        assert result["device_count"] == 1
        assert result["devices"][0]["port"] == "COM3"


class TestClearStaleLocks:
    def test_no_stale_locks(self):
        from fbuild.daemon.mcp.tools_action import clear_stale_locks

        result = clear_stale_locks()
        assert result["released_count"] == 0
        assert result["stale_port_locks"] == []
        assert result["stale_project_locks"] == []

    def test_with_stale_locks(self, mock_context):
        from fbuild.daemon.mcp.tools_action import clear_stale_locks

        port_lock = MagicMock()
        port_lock.resource_id = "COM3"
        proj_lock = MagicMock()
        proj_lock.resource_id = "/old/project"

        stale = MagicMock()
        stale.stale_port_locks = [port_lock]
        stale.stale_project_locks = [proj_lock]
        mock_context.lock_manager.get_stale_locks.return_value = stale
        mock_context.lock_manager.force_release_stale_locks.return_value = 2

        result = clear_stale_locks()
        assert result["released_count"] == 2
        assert result["stale_port_locks"] == ["COM3"]
        assert result["stale_project_locks"] == ["/old/project"]


# ===========================================================================
# Resource tests
# ===========================================================================


class TestDaemonLogResource:
    def test_reads_log_file(self, tmp_path):
        from fbuild.daemon.mcp.resources import daemon_log

        log_content = "\n".join([f"line {i}" for i in range(300)])
        log_file = tmp_path / "daemon.log"
        log_file.write_text(log_content)

        with patch("fbuild.daemon.paths.LOG_FILE", log_file):
            result = daemon_log()

        lines = result.strip().split("\n")
        # Should return last 200 lines
        assert len(lines) == 200
        assert lines[-1] == "line 299"
        assert lines[0] == "line 100"

    def test_missing_log_file(self):
        from fbuild.daemon.mcp.resources import daemon_log

        with patch("fbuild.daemon.paths.LOG_FILE", Path("/nonexistent/daemon.log")):
            result = daemon_log()

        assert "not available" in result


class TestProjectConfigResource:
    def test_valid_config(self, tmp_path):
        from fbuild.daemon.mcp.resources import project_config

        ini_content = "[env:uno]\nboard = uno\nplatform = atmelavr\n"
        (tmp_path / "platformio.ini").write_text(ini_content)

        result = project_config(str(tmp_path))
        data = json.loads(result)
        assert data["project_dir"] == str(tmp_path)
        assert "uno" in data["environments"]

    def test_invalid_config(self, tmp_path):
        from fbuild.daemon.mcp.resources import project_config

        result = project_config(str(tmp_path / "nonexistent"))
        data = json.loads(result)
        assert "error" in data


class TestFirmwareResource:
    def test_no_firmware(self):
        from fbuild.daemon.mcp.resources import firmware_info

        result = firmware_info("COM3")
        data = json.loads(result)
        assert data["found"] is False

    def test_with_firmware(self, mock_context):
        from fbuild.daemon.mcp.resources import firmware_info

        entry = MagicMock()
        entry.firmware_hash = "abc"
        entry.source_hash = "def"
        entry.project_dir = "/proj"
        entry.environment = "uno"
        entry.upload_timestamp = 1234567890.0
        entry.is_stale.return_value = True
        mock_context.firmware_ledger.get_deployment.return_value = entry

        result = firmware_info("COM3")
        data = json.loads(result)
        assert data["found"] is True
        assert data["is_stale"] is True


# ===========================================================================
# Prompt tests
# ===========================================================================


class TestDiagnoseBuildFailure:
    def test_no_errors(self):
        from fbuild.daemon.mcp.prompts import diagnose_build_failure

        result = diagnose_build_failure()
        assert "Build Failure Diagnostic Report" in result
        assert "No errors recorded" in result

    def test_with_errors_and_stale_locks(self, mock_context):
        from fbuild.daemon.mcp.prompts import diagnose_build_failure

        mock_context.error_collector.has_errors.return_value = True
        mock_context.error_collector.format_errors.return_value = "error: foo.cpp:10: undefined reference"

        stale = MagicMock()
        stale.has_stale_locks.return_value = True
        stale_lock = MagicMock()
        stale_lock.resource_id = "/stuck/project"
        stale.stale_port_locks = []
        stale.stale_project_locks = [stale_lock]
        mock_context.lock_manager.get_stale_locks.return_value = stale

        result = diagnose_build_failure()
        assert "undefined reference" in result
        assert "Stale Lock Warning" in result
        assert "/stuck/project" in result


class TestRecommendDeployTarget:
    def test_no_devices(self):
        from fbuild.daemon.mcp.prompts import recommend_deploy_target

        result = recommend_deploy_target()
        assert "No devices connected" in result

    def test_with_available_device(self, mock_context):
        from fbuild.daemon.mcp.prompts import recommend_deploy_target

        mock_state = MagicMock()
        mock_state.is_connected = True
        mock_state.is_available_for_exclusive.return_value = True
        mock_state.exclusive_lease = None
        mock_state.monitor_leases = {}
        mock_info = MagicMock()
        mock_info.port = "COM3"
        mock_info.description = "USB Serial"
        mock_state.device_info = mock_info

        mock_context.device_manager.get_all_devices.return_value = {"dev-1": mock_state}
        mock_context.firmware_ledger.get_deployment.return_value = None

        result = recommend_deploy_target()
        assert "COM3" in result
        assert "Recommendation" in result


# ===========================================================================
# Backward-compatibility shim test
# ===========================================================================


class TestShim:
    def test_shim_exports_mcp(self):
        """Verify the shim re-exports the mcp instance."""
        from fbuild.daemon.mcp import mcp as mcp_direct
        from fbuild.daemon.mcp_server import mcp as mcp_shim

        assert mcp_shim is mcp_direct
