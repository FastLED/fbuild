"""
Unit tests for daemon message serialization and deserialization.

This module verifies that all daemon messages can be correctly serialized
to dictionaries and deserialized back to objects, with special attention to:
- Enum field handling
- Optional enum fields
- Nested SerializableMessage objects
- Required vs optional fields
- Backwards compatibility
- Error handling for invalid data
"""

import time
from dataclasses import dataclass
from typing import Optional

import pytest

from fbuild.daemon.message_protocol import (
    EnumSerializationMixin,
)
from fbuild.daemon.messages import (
    BuildRequest,
    DaemonIdentity,
    DaemonIdentityResponse,
    DaemonState,
    DaemonStatus,
    DeployRequest,
    LockAcquireRequest,
    LockReleaseRequest,
    LockType,
    MonitorRequest,
    OperationType,
)


class TestBuildRequestSerialization:
    """Test BuildRequest message serialization/deserialization."""

    def test_build_request_roundtrip(self):
        """Verify BuildRequest can be serialized and deserialized preserving all fields."""
        # Create a BuildRequest with all fields populated
        original = BuildRequest(
            project_dir="/path/to/project",
            environment="esp32c6",
            clean_build=True,
            verbose=True,
            caller_pid=12345,
            caller_cwd="/path/to/cwd",
            jobs=4,
            timestamp=1234567890.0,
            request_id="build_test_123",
        )

        # Serialize to dict
        data = original.to_dict()

        # Verify it's a dictionary
        assert isinstance(data, dict)

        # Verify all fields are present
        assert data["project_dir"] == "/path/to/project"
        assert data["environment"] == "esp32c6"
        assert data["clean_build"] is True
        assert data["verbose"] is True
        assert data["caller_pid"] == 12345
        assert data["caller_cwd"] == "/path/to/cwd"
        assert data["jobs"] == 4
        assert data["timestamp"] == 1234567890.0
        assert data["request_id"] == "build_test_123"

        # Deserialize back to object
        restored = BuildRequest.from_dict(data)

        # Verify all fields match
        assert restored.project_dir == original.project_dir
        assert restored.environment == original.environment
        assert restored.clean_build == original.clean_build
        assert restored.verbose == original.verbose
        assert restored.caller_pid == original.caller_pid
        assert restored.caller_cwd == original.caller_cwd
        assert restored.jobs == original.jobs
        assert restored.timestamp == original.timestamp
        assert restored.request_id == original.request_id

    def test_build_request_jobs_field_serialization(self):
        """Specifically verify jobs field is serialized correctly."""
        # Test with jobs=None (default)
        msg_none = BuildRequest(
            project_dir="/path",
            environment="test",
            clean_build=False,
            verbose=False,
            caller_pid=123,
            caller_cwd="/cwd",
            jobs=None,
        )
        data_none = msg_none.to_dict()
        assert data_none["jobs"] is None
        restored_none = BuildRequest.from_dict(data_none)
        assert restored_none.jobs is None

        # Test with jobs=1 (serial)
        msg_serial = BuildRequest(
            project_dir="/path",
            environment="test",
            clean_build=False,
            verbose=False,
            caller_pid=123,
            caller_cwd="/cwd",
            jobs=1,
        )
        data_serial = msg_serial.to_dict()
        assert data_serial["jobs"] == 1
        restored_serial = BuildRequest.from_dict(data_serial)
        assert restored_serial.jobs == 1

        # Test with jobs=8 (custom worker count)
        msg_custom = BuildRequest(
            project_dir="/path",
            environment="test",
            clean_build=False,
            verbose=False,
            caller_pid=123,
            caller_cwd="/cwd",
            jobs=8,
        )
        data_custom = msg_custom.to_dict()
        assert data_custom["jobs"] == 8
        restored_custom = BuildRequest.from_dict(data_custom)
        assert restored_custom.jobs == 8

    def test_build_request_optional_fields_with_defaults(self):
        """Verify optional fields use defaults when not present in data."""
        # Minimal data with only required fields
        minimal_data = {
            "project_dir": "/path",
            "environment": "test",
            "clean_build": False,
            "verbose": False,
            "caller_pid": 123,
            "caller_cwd": "/cwd",
        }

        restored = BuildRequest.from_dict(minimal_data)

        # Verify defaults are applied
        assert restored.jobs is None  # Default value
        assert isinstance(restored.timestamp, float)  # Default factory generates timestamp
        assert isinstance(restored.request_id, str)  # Default factory generates ID
        assert restored.request_id.startswith("build_")


class TestDeployRequestSerialization:
    """Test DeployRequest message serialization/deserialization."""

    def test_deploy_request_roundtrip(self):
        """Verify DeployRequest can be serialized and deserialized."""
        original = DeployRequest(
            project_dir="/path/to/project",
            environment="esp32c6",
            port="/dev/ttyUSB0",
            clean_build=True,
            monitor_after=True,
            monitor_timeout=30.0,
            monitor_halt_on_error="ERROR",
            monitor_halt_on_success="SUCCESS",
            monitor_expect="READY",
            caller_pid=12345,
            caller_cwd="/path/to/cwd",
            monitor_show_timestamp=True,
            skip_build=False,
        )

        # Serialize and deserialize
        data = original.to_dict()
        restored = DeployRequest.from_dict(data)

        # Verify critical fields
        assert restored.project_dir == original.project_dir
        assert restored.environment == original.environment
        assert restored.port == original.port
        assert restored.monitor_after == original.monitor_after
        assert restored.monitor_timeout == original.monitor_timeout
        assert restored.monitor_show_timestamp == original.monitor_show_timestamp
        assert restored.skip_build == original.skip_build

    def test_deploy_request_with_none_values(self):
        """Verify DeployRequest handles None values correctly."""
        original = DeployRequest(
            project_dir="/path",
            environment="test",
            port=None,  # Auto-detect
            clean_build=False,
            monitor_after=False,
            monitor_timeout=None,
            monitor_halt_on_error=None,
            monitor_halt_on_success=None,
            monitor_expect=None,
            caller_pid=123,
            caller_cwd="/cwd",
        )

        data = original.to_dict()
        restored = DeployRequest.from_dict(data)

        # Verify None values are preserved
        assert restored.port is None
        assert restored.monitor_timeout is None
        assert restored.monitor_halt_on_error is None
        assert restored.monitor_halt_on_success is None
        assert restored.monitor_expect is None


class TestDaemonStatusSerialization:
    """Test DaemonStatus message with enum fields."""

    def test_daemon_status_enum_serialization(self):
        """Verify DaemonStatus correctly serializes enum fields."""
        original = DaemonStatus(
            state=DaemonState.BUILDING,
            message="Building project",
            updated_at=time.time(),
            operation_in_progress=True,
            operation_type=OperationType.BUILD,
        )

        # Serialize to dict
        data = original.to_dict()

        # Verify enums are converted to strings
        assert isinstance(data["state"], str)
        assert data["state"] == "building"
        assert isinstance(data["operation_type"], str)
        assert data["operation_type"] == "build"

        # Deserialize back
        restored = DaemonStatus.from_dict(data)

        # Verify enums are restored correctly
        assert isinstance(restored.state, DaemonState)
        assert restored.state == DaemonState.BUILDING
        assert isinstance(restored.operation_type, OperationType)
        assert restored.operation_type == OperationType.BUILD

    def test_daemon_status_optional_enum_fields(self):
        """Verify Optional[Enum] fields handle None correctly."""
        # Create status with operation_type=None
        original = DaemonStatus(
            state=DaemonState.IDLE,
            message="Idle",
            updated_at=time.time(),
            operation_in_progress=False,
            operation_type=None,  # Optional field
        )

        data = original.to_dict()
        assert data["operation_type"] is None

        restored = DaemonStatus.from_dict(data)
        assert restored.operation_type is None

    def test_daemon_status_all_enum_states(self):
        """Test serialization of all DaemonState enum values."""
        all_states = [
            DaemonState.IDLE,
            DaemonState.DEPLOYING,
            DaemonState.MONITORING,
            DaemonState.BUILDING,
            DaemonState.COMPLETED,
            DaemonState.FAILED,
            DaemonState.UNKNOWN,
        ]

        for state in all_states:
            original = DaemonStatus(state=state, message="Test", updated_at=time.time())
            data = original.to_dict()
            restored = DaemonStatus.from_dict(data)
            assert restored.state == state, f"Failed to roundtrip {state}"

    def test_daemon_status_all_operation_types(self):
        """Test serialization of all OperationType enum values."""
        all_types = [
            OperationType.BUILD,
            OperationType.DEPLOY,
            OperationType.MONITOR,
            OperationType.BUILD_AND_DEPLOY,
            OperationType.INSTALL_DEPENDENCIES,
        ]

        for op_type in all_types:
            original = DaemonStatus(
                state=DaemonState.BUILDING,
                message="Test",
                updated_at=time.time(),
                operation_type=op_type,
            )
            data = original.to_dict()
            restored = DaemonStatus.from_dict(data)
            assert restored.operation_type == op_type, f"Failed to roundtrip {op_type}"


class TestLockMessageSerialization:
    """Test lock-related message serialization."""

    def test_lock_acquire_request_with_enum(self):
        """Verify LockAcquireRequest serializes LockType enum."""
        original = LockAcquireRequest(
            client_id="client_123",
            project_dir="/path",
            environment="test",
            port="/dev/ttyUSB0",
            lock_type=LockType.EXCLUSIVE,
            description="Testing lock",
            timeout=60.0,
        )

        data = original.to_dict()

        # Verify enum is converted to string
        assert data["lock_type"] == "exclusive"

        restored = LockAcquireRequest.from_dict(data)

        # Verify enum is restored
        assert isinstance(restored.lock_type, LockType)
        assert restored.lock_type == LockType.EXCLUSIVE

    def test_lock_type_enum_values(self):
        """Test both LockType enum values."""
        for lock_type in [LockType.EXCLUSIVE, LockType.SHARED_READ]:
            original = LockAcquireRequest(
                client_id="test",
                project_dir="/path",
                environment="test",
                port="/dev/ttyUSB0",
                lock_type=lock_type,
            )
            data = original.to_dict()
            restored = LockAcquireRequest.from_dict(data)
            assert restored.lock_type == lock_type


class TestNestedMessageSerialization:
    """Test serialization of messages containing nested SerializableMessage objects."""

    def test_daemon_identity_response_with_nested_message(self):
        """Verify DaemonIdentityResponse correctly serializes nested DaemonIdentity."""
        identity = DaemonIdentity(
            name="fbuild_daemon_dev",
            version="1.3.5",
            started_at=1234567890.0,
            spawned_by_pid=999,
            spawned_by_cwd="/path",
            is_dev=True,
            pid=1000,
        )

        original = DaemonIdentityResponse(
            success=True,
            message="OK",
            identity=identity,
            timestamp=time.time(),
        )

        # Serialize
        data = original.to_dict()

        # Verify identity is serialized as nested dict
        assert isinstance(data["identity"], dict)
        assert data["identity"]["name"] == "fbuild_daemon_dev"
        assert data["identity"]["version"] == "1.3.5"
        assert data["identity"]["is_dev"] is True

        # Deserialize
        restored = DaemonIdentityResponse.from_dict(data)

        # Verify nested object is restored
        assert isinstance(restored.identity, DaemonIdentity)
        assert restored.identity.name == identity.name
        assert restored.identity.version == identity.version
        assert restored.identity.is_dev == identity.is_dev
        assert restored.identity.pid == identity.pid

    def test_daemon_identity_response_with_none_identity(self):
        """Verify DaemonIdentityResponse handles None identity field."""
        original = DaemonIdentityResponse(success=False, message="Not found", identity=None)

        data = original.to_dict()
        assert data["identity"] is None

        restored = DaemonIdentityResponse.from_dict(data)
        assert restored.identity is None


class TestErrorHandling:
    """Test error handling for invalid serialization/deserialization."""

    def test_missing_required_field_raises_error(self):
        """Verify deserialize raises KeyError for missing required fields."""
        # BuildRequest requires project_dir, environment, etc.
        incomplete_data = {
            "project_dir": "/path",
            # Missing 'environment' - required field
            "clean_build": False,
            "verbose": False,
            "caller_pid": 123,
            "caller_cwd": "/cwd",
        }

        with pytest.raises(KeyError, match="environment"):
            BuildRequest.from_dict(incomplete_data)

    def test_invalid_enum_value_raises_error(self):
        """Verify deserialize raises ValueError for invalid enum strings."""
        invalid_data = {
            "state": "INVALID_STATE",  # Not a valid DaemonState
            "message": "Test",
            "updated_at": time.time(),
        }

        with pytest.raises(ValueError, match="Invalid enum value"):
            DaemonStatus.from_dict(invalid_data)

    def test_invalid_lock_type_raises_error(self):
        """Verify invalid LockType value raises error."""
        invalid_data = {
            "client_id": "test",
            "project_dir": "/path",
            "environment": "test",
            "port": "/dev/ttyUSB0",
            "lock_type": "INVALID_LOCK",  # Not valid
        }

        with pytest.raises(ValueError):
            LockAcquireRequest.from_dict(invalid_data)


class TestBackwardsCompatibility:
    """Test backwards compatibility with old dict format."""

    def test_old_format_still_deserializes(self):
        """Verify messages from older versions can still be deserialized."""
        # Simulate an old BuildRequest dict that might be missing new fields
        old_format_data = {
            "project_dir": "/path/to/project",
            "environment": "esp32c6",
            "clean_build": False,
            "verbose": True,
            "caller_pid": 12345,
            "caller_cwd": "/path/to/cwd",
            # jobs field didn't exist in older versions
            # timestamp and request_id have defaults, so they're optional
        }

        # Should deserialize successfully, using defaults for missing fields
        restored = BuildRequest.from_dict(old_format_data)

        assert restored.project_dir == "/path/to/project"
        assert restored.environment == "esp32c6"
        assert restored.jobs is None  # Default value
        assert isinstance(restored.timestamp, float)
        assert isinstance(restored.request_id, str)

    def test_extra_fields_are_ignored(self):
        """Verify extra fields in dict don't cause errors."""
        # Data with extra fields that don't exist in the dataclass
        data_with_extras = {
            "project_dir": "/path",
            "environment": "test",
            "clean_build": False,
            "verbose": False,
            "caller_pid": 123,
            "caller_cwd": "/cwd",
            "extra_field_1": "ignored",
            "extra_field_2": 999,
        }

        # Should deserialize successfully, ignoring extra fields
        restored = BuildRequest.from_dict(data_with_extras)
        assert restored.project_dir == "/path"
        assert not hasattr(restored, "extra_field_1")


class TestEnumSerializationMixin:
    """Test the EnumSerializationMixin helper class."""

    def test_mixin_provides_serialization_methods(self):
        """Verify mixin provides to_dict and from_dict methods."""

        @dataclass
        class TestMessage(EnumSerializationMixin):
            status: DaemonState
            count: int

        msg = TestMessage(status=DaemonState.BUILDING, count=42)

        # Mixin provides to_dict
        data = msg.to_dict()
        assert isinstance(data, dict)
        assert data["status"] == "building"
        assert data["count"] == 42

        # Mixin provides from_dict
        restored = TestMessage.from_dict(data)
        assert restored.status == DaemonState.BUILDING
        assert restored.count == 42

    def test_mixin_handles_optional_enum(self):
        """Verify mixin handles Optional[Enum] correctly."""

        @dataclass
        class TestMessage(EnumSerializationMixin):
            required_state: DaemonState
            optional_state: Optional[DaemonState] = None

        # Test with None
        msg_none = TestMessage(required_state=DaemonState.IDLE, optional_state=None)
        data_none = msg_none.to_dict()
        assert data_none["optional_state"] is None
        restored_none = TestMessage.from_dict(data_none)
        assert restored_none.optional_state is None

        # Test with value
        msg_value = TestMessage(required_state=DaemonState.IDLE, optional_state=DaemonState.BUILDING)
        data_value = msg_value.to_dict()
        assert data_value["optional_state"] == "building"
        restored_value = TestMessage.from_dict(data_value)
        assert restored_value.optional_state == DaemonState.BUILDING


class TestComplexSerializationScenarios:
    """Test complex edge cases and scenarios."""

    def test_message_with_list_fields(self):
        """Verify messages with list fields serialize correctly."""
        original = DaemonStatus(
            state=DaemonState.BUILDING,
            message="Building",
            updated_at=time.time(),
            output_lines=["Line 1", "Line 2", "Line 3"],
        )

        data = original.to_dict()
        assert data["output_lines"] == ["Line 1", "Line 2", "Line 3"]

        restored = DaemonStatus.from_dict(data)
        assert restored.output_lines == ["Line 1", "Line 2", "Line 3"]

    def test_message_with_dict_fields(self):
        """Verify messages with dict fields serialize correctly."""
        original = DaemonStatus(
            state=DaemonState.IDLE,
            message="Idle",
            updated_at=time.time(),
            ports={"COM3": {"baud": 115200, "open": True}, "COM4": {"baud": 9600, "open": False}},
            locks={"port_locks": {}, "project_locks": {}},
        )

        data = original.to_dict()
        assert data["ports"]["COM3"]["baud"] == 115200
        assert data["locks"]["port_locks"] == {}

        restored = DaemonStatus.from_dict(data)
        assert restored.ports["COM3"]["baud"] == 115200
        assert restored.locks == {"port_locks": {}, "project_locks": {}}

    def test_timestamp_field_preservation(self):
        """Verify timestamp fields are preserved with full precision."""
        precise_timestamp = 1234567890.123456789
        original = BuildRequest(
            project_dir="/path",
            environment="test",
            clean_build=False,
            verbose=False,
            caller_pid=123,
            caller_cwd="/cwd",
            timestamp=precise_timestamp,
        )

        data = original.to_dict()
        restored = BuildRequest.from_dict(data)

        # Timestamp should be preserved exactly
        assert restored.timestamp == precise_timestamp


class TestMessageProtocolCompliance:
    """Test that messages comply with SerializableMessage protocol."""

    @pytest.mark.parametrize(
        "message_class",
        [
            BuildRequest,
            DeployRequest,
            MonitorRequest,
            DaemonStatus,
            LockAcquireRequest,
            LockReleaseRequest,
            DaemonIdentity,
            DaemonIdentityResponse,
        ],
    )
    def test_message_has_to_dict_method(self, message_class):
        """Verify all message classes have to_dict method."""
        assert hasattr(message_class, "to_dict"), f"{message_class.__name__} missing to_dict method"
        assert callable(getattr(message_class, "to_dict")), f"{message_class.__name__}.to_dict is not callable"

    @pytest.mark.parametrize(
        "message_class",
        [
            BuildRequest,
            DeployRequest,
            MonitorRequest,
            DaemonStatus,
            LockAcquireRequest,
            LockReleaseRequest,
            DaemonIdentity,
            DaemonIdentityResponse,
        ],
    )
    def test_message_has_from_dict_classmethod(self, message_class):
        """Verify all message classes have from_dict classmethod."""
        assert hasattr(message_class, "from_dict"), f"{message_class.__name__} missing from_dict method"
        assert callable(getattr(message_class, "from_dict")), f"{message_class.__name__}.from_dict is not callable"
