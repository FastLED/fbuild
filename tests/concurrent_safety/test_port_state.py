"""
COM port state tracking tests for PortStateManager.

These tests verify that:
1. Port state tracks upload/monitor transitions
2. Port state cleared on completion or failure
3. Port info includes client PID and project info
4. Multiple ports tracked independently

No hardware required - tests PortStateManager directly.
"""

import threading
import time

import pytest

from fbuild.daemon.port_state_manager import PortInfo, PortState, PortStateManager

pytestmark = pytest.mark.concurrent


class TestPortStateAcquisition:
    """Tests for acquiring and tracking port state."""

    def test_acquire_port_sets_state(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Acquiring a port should set its state correctly."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/test/project",
            environment="esp32c6",
            operation_id="deploy_123",
        )

        info = port_state_manager.get_port_info("COM3")
        assert info is not None
        assert info.port == "COM3"
        assert info.state == PortState.UPLOADING
        assert info.client_pid == 12345
        assert info.project_dir == "/test/project"
        assert info.environment == "esp32c6"
        assert info.operation_id == "deploy_123"
        assert info.acquired_at is not None
        assert info.last_activity is not None

    def test_acquire_port_marks_unavailable(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Acquired port should show as unavailable."""
        assert port_state_manager.is_port_available("COM3") is True

        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/test/project",
            environment="esp32c6",
            operation_id="deploy_123",
        )

        assert port_state_manager.is_port_available("COM3") is False


class TestPortStateTransitions:
    """Tests for port state transitions."""

    def test_state_transition_uploading_to_monitoring(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Port state should transition from UPLOADING to MONITORING."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/test/project",
            environment="esp32c6",
            operation_id="deploy_123",
        )

        info = port_state_manager.get_port_info("COM3")
        assert info is not None
        assert info.state == PortState.UPLOADING

        port_state_manager.update_state("COM3", PortState.MONITORING)

        info = port_state_manager.get_port_info("COM3")
        assert info is not None
        assert info.state == PortState.MONITORING

    def test_state_update_preserves_other_info(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """State update should preserve other port info."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/test/project",
            environment="esp32c6",
            operation_id="deploy_123",
        )

        port_state_manager.update_state("COM3", PortState.MONITORING)

        info = port_state_manager.get_port_info("COM3")
        assert info is not None
        assert info.client_pid == 12345
        assert info.project_dir == "/test/project"
        assert info.environment == "esp32c6"
        assert info.operation_id == "deploy_123"

    def test_state_update_updates_last_activity(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """State update should update last_activity timestamp."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/test/project",
            environment="esp32c6",
            operation_id="deploy_123",
        )

        info1 = port_state_manager.get_port_info("COM3")
        assert info1 is not None
        initial_activity = info1.last_activity

        time.sleep(0.1)
        port_state_manager.update_state("COM3", PortState.MONITORING)

        info2 = port_state_manager.get_port_info("COM3")
        assert info2 is not None
        assert info2.last_activity is not None
        assert initial_activity is not None
        assert info2.last_activity > initial_activity


class TestPortStateRelease:
    """Tests for releasing port state."""

    def test_release_port_removes_state(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Releasing a port should remove its state."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/test/project",
            environment="esp32c6",
            operation_id="deploy_123",
        )

        assert port_state_manager.get_port_info("COM3") is not None

        port_state_manager.release_port("COM3")

        assert port_state_manager.get_port_info("COM3") is None

    def test_release_port_marks_available(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Released port should show as available."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/test/project",
            environment="esp32c6",
            operation_id="deploy_123",
        )

        assert port_state_manager.is_port_available("COM3") is False

        port_state_manager.release_port("COM3")

        assert port_state_manager.is_port_available("COM3") is True

    def test_release_unknown_port_no_error(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Releasing an unknown port should not raise error."""
        port_state_manager.release_port("COM99")  # Should not raise


class TestMultiplePortTracking:
    """Tests for tracking multiple ports independently."""

    def test_multiple_ports_tracked_independently(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Each port should have independent state."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/project1",
            environment="esp32c6",
            operation_id="deploy_1",
        )

        port_state_manager.acquire_port(
            port="COM4",
            state=PortState.MONITORING,
            client_pid=67890,
            project_dir="/project2",
            environment="esp32dev",
            operation_id="monitor_1",
        )

        info_com3 = port_state_manager.get_port_info("COM3")
        info_com4 = port_state_manager.get_port_info("COM4")

        assert info_com3 is not None
        assert info_com3.state == PortState.UPLOADING
        assert info_com3.project_dir == "/project1"

        assert info_com4 is not None
        assert info_com4.state == PortState.MONITORING
        assert info_com4.project_dir == "/project2"

    def test_get_all_ports_returns_snapshot(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """get_all_ports should return snapshot of all tracked ports."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/project1",
            environment="esp32c6",
            operation_id="deploy_1",
        )

        port_state_manager.acquire_port(
            port="COM4",
            state=PortState.MONITORING,
            client_pid=67890,
            project_dir="/project2",
            environment="esp32dev",
            operation_id="monitor_1",
        )

        all_ports = port_state_manager.get_all_ports()

        assert len(all_ports) == 2
        assert "COM3" in all_ports
        assert "COM4" in all_ports
        assert all_ports["COM3"].state == PortState.UPLOADING
        assert all_ports["COM4"].state == PortState.MONITORING

    def test_release_one_port_keeps_others(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Releasing one port should not affect others."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/project1",
            environment="esp32c6",
            operation_id="deploy_1",
        )

        port_state_manager.acquire_port(
            port="COM4",
            state=PortState.MONITORING,
            client_pid=67890,
            project_dir="/project2",
            environment="esp32dev",
            operation_id="monitor_1",
        )

        port_state_manager.release_port("COM3")

        assert port_state_manager.get_port_info("COM3") is None
        assert port_state_manager.get_port_info("COM4") is not None

    def test_port_count_tracking(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Port count should track number of active ports."""
        assert port_state_manager.get_port_count() == 0

        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/project1",
            environment="esp32c6",
            operation_id="deploy_1",
        )
        assert port_state_manager.get_port_count() == 1

        port_state_manager.acquire_port(
            port="COM4",
            state=PortState.MONITORING,
            client_pid=67890,
            project_dir="/project2",
            environment="esp32dev",
            operation_id="monitor_1",
        )
        assert port_state_manager.get_port_count() == 2

        port_state_manager.release_port("COM3")
        assert port_state_manager.get_port_count() == 1

        port_state_manager.release_port("COM4")
        assert port_state_manager.get_port_count() == 0


class TestPortStateConcurrency:
    """Tests for thread-safe port state operations."""

    def test_concurrent_acquire_release(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Concurrent acquire/release operations should be thread-safe."""
        errors: list[Exception] = []

        def acquire_release_port(port_num: int) -> None:
            try:
                port = f"COM{port_num}"
                for _ in range(10):
                    port_state_manager.acquire_port(
                        port=port,
                        state=PortState.UPLOADING,
                        client_pid=port_num,
                        project_dir=f"/project{port_num}",
                        environment="esp32c6",
                        operation_id=f"op_{port_num}",
                    )
                    time.sleep(0.01)
                    port_state_manager.release_port(port)
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=acquire_release_port, args=(i,)) for i in range(5)]

        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)

        assert len(errors) == 0
        assert port_state_manager.get_port_count() == 0

    def test_concurrent_state_updates(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """Concurrent state updates should be thread-safe."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.UPLOADING,
            client_pid=12345,
            project_dir="/project",
            environment="esp32c6",
            operation_id="deploy_1",
        )

        errors: list[Exception] = []
        states = [PortState.UPLOADING, PortState.MONITORING, PortState.RESERVED]

        def toggle_state() -> None:
            try:
                for state in states * 10:
                    port_state_manager.update_state("COM3", state)
                    time.sleep(0.01)
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=toggle_state) for _ in range(5)]

        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)

        assert len(errors) == 0

        # Port should still be tracked
        info = port_state_manager.get_port_info("COM3")
        assert info is not None


class TestPortInfoSerialization:
    """Tests for PortInfo serialization."""

    def test_port_info_to_dict(self) -> None:
        """PortInfo.to_dict should serialize correctly."""
        info = PortInfo(
            port="COM3",
            state=PortState.MONITORING,
            client_pid=12345,
            project_dir="/test/project",
            environment="esp32c6",
            operation_id="deploy_123",
            acquired_at=1000.0,
            last_activity=1001.0,
        )

        data = info.to_dict()

        assert data["port"] == "COM3"
        assert data["state"] == "monitoring"
        assert data["client_pid"] == 12345
        assert data["project_dir"] == "/test/project"
        assert data["environment"] == "esp32c6"
        assert data["operation_id"] == "deploy_123"
        assert data["acquired_at"] == 1000.0
        assert data["last_activity"] == 1001.0

    def test_port_info_from_dict(self) -> None:
        """PortInfo.from_dict should deserialize correctly."""
        data = {
            "port": "COM3",
            "state": "monitoring",
            "client_pid": 12345,
            "project_dir": "/test/project",
            "environment": "esp32c6",
            "operation_id": "deploy_123",
            "acquired_at": 1000.0,
            "last_activity": 1001.0,
        }

        info = PortInfo.from_dict(data)

        assert info.port == "COM3"
        assert info.state == PortState.MONITORING
        assert info.client_pid == 12345
        assert info.project_dir == "/test/project"
        assert info.environment == "esp32c6"
        assert info.operation_id == "deploy_123"
        assert info.acquired_at == 1000.0
        assert info.last_activity == 1001.0

    def test_port_info_from_dict_invalid_state(self) -> None:
        """PortInfo.from_dict should handle invalid state gracefully."""
        data = {
            "port": "COM3",
            "state": "invalid_state",
            "client_pid": 12345,
        }

        info = PortInfo.from_dict(data)

        assert info.state == PortState.AVAILABLE  # Default on invalid

    def test_get_ports_summary(
        self,
        port_state_manager: PortStateManager,
    ) -> None:
        """get_ports_summary should return dict of all port info."""
        port_state_manager.acquire_port(
            port="COM3",
            state=PortState.MONITORING,
            client_pid=12345,
            project_dir="/project",
            environment="esp32c6",
            operation_id="deploy_1",
        )

        summary = port_state_manager.get_ports_summary()

        assert "COM3" in summary
        assert summary["COM3"]["state"] == "monitoring"
        assert summary["COM3"]["client_pid"] == 12345
