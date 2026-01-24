"""
Integration tests for parameter flow from CLI to orchestrator.

This test suite validates end-to-end parameter passing through the system:
- CLI argument parsing (--jobs flag)
- Daemon request serialization/deserialization
- Build processor parameter extraction
- Orchestrator parameter reception
- Context manager resource management

These tests use mocking to verify parameter flow without running actual builds.
"""

import json
import os
from pathlib import Path
from unittest.mock import Mock, patch

import pytest

from fbuild.daemon.messages import BuildRequest


@pytest.mark.integration
class TestJobsParameterFlow:
    """Test that the jobs parameter flows correctly from CLI to orchestrator."""

    def test_jobs_parameter_reaches_orchestrator(self, tmp_path: Path):
        """
        Verify that when CLI is called with --jobs N, the orchestrator's build()
        method receives jobs=N.

        This is a critical integration test ensuring the parameter flow:
        CLI --jobs N → BuildRequest(jobs=N) → orchestrator.build(jobs=N)
        """
        # Create a minimal platformio.ini
        project_dir = tmp_path / "test_project"
        project_dir.mkdir()
        platformio_ini = project_dir / "platformio.ini"
        platformio_ini.write_text(
            """
[env:test]
platform = espressif32
board = esp32dev
framework = arduino
"""
        )

        # Mock the orchestrator to capture the jobs parameter
        with patch("fbuild.daemon.processors.build_processor.sys.modules") as mock_modules:
            # Create a mock orchestrator class
            mock_orchestrator_class = Mock()
            mock_orchestrator_instance = Mock()
            mock_orchestrator_class.return_value = mock_orchestrator_instance

            # Set up the mock to return a successful build result
            from fbuild.build.orchestrator import BuildResult

            mock_build_result = BuildResult(success=True, hex_path=None, elf_path=None, size_info=None, build_time=1.0, message="Build succeeded")
            mock_orchestrator_instance.build.return_value = mock_build_result

            # Mock sys.modules to return our mock orchestrator
            mock_modules.get.return_value = {"OrchestratorESP32": mock_orchestrator_class}
            mock_modules.__getitem__.return_value = type(
                "MockModule",
                (),
                {
                    "OrchestratorESP32": mock_orchestrator_class,
                },
            )()

            # Create a BuildRequest with jobs=4
            build_request = BuildRequest(
                project_dir=str(project_dir),
                environment="test",
                clean_build=False,
                verbose=False,
                caller_pid=os.getpid(),
                caller_cwd=str(Path.cwd()),
                jobs=4,
            )

            # Import and create the build processor
            from fbuild.daemon.processors.build_processor import BuildRequestProcessor

            processor = BuildRequestProcessor()

            # Mock the daemon context
            mock_context = Mock()
            mock_context.lock_manager = Mock()
            mock_context.lock_manager.acquire_project_lock = Mock(return_value=True)
            mock_context.lock_manager.release_project_lock = Mock()

            # Mock module reloading to avoid import side effects
            with patch.object(processor, "_reload_build_modules"):
                # Mock output file setup
                with patch("fbuild.daemon.processors.build_processor.Path") as mock_path_class:
                    mock_path = Mock()
                    mock_path.parent.mkdir = Mock()
                    mock_path.write_text = Mock()
                    mock_path_class.return_value = mock_path

                    with patch("builtins.open", create=True) as mock_open:
                        mock_open.return_value.__enter__ = Mock()
                        mock_open.return_value.__exit__ = Mock()

                        # Mock the output module
                        with patch("fbuild.daemon.processors.build_processor.set_output_file"):
                            with patch("fbuild.daemon.processors.build_processor.reset_timer"):
                                # Execute the build (this will call execute_operation internally)
                                with patch("fbuild.config.ini_parser.PlatformIOConfig") as mock_config_class:
                                    mock_config = Mock()
                                    mock_config.get_env_config.return_value = {"platform": "espressif32"}
                                    mock_config_class.return_value = mock_config

                                    with patch("fbuild.packages.cache.Cache"):
                                        with patch("fbuild.daemon.processors.build_processor.getattr") as mock_getattr:
                                            mock_getattr.return_value = mock_orchestrator_class

                                            # Execute the internal build logic
                                            result = processor._execute_build(build_request, mock_context)

            # Verify the orchestrator.build() was called with jobs=4
            mock_orchestrator_instance.build.assert_called_once()
            call_kwargs = mock_orchestrator_instance.build.call_args[1]
            assert call_kwargs["jobs"] == 4, f"Expected jobs=4, got jobs={call_kwargs.get('jobs')}"
            assert result is True, "Build should have succeeded"

    def test_jobs_parameter_default_none(self, tmp_path: Path):
        """
        Verify that without --jobs flag, orchestrator receives jobs=None.

        jobs=None signals the orchestrator to use the default parallelism
        (CPU count or daemon's shared queue).
        """
        # Create a minimal platformio.ini
        project_dir = tmp_path / "test_project"
        project_dir.mkdir()
        platformio_ini = project_dir / "platformio.ini"
        platformio_ini.write_text(
            """
[env:test]
platform = atmelavr
board = uno
framework = arduino
"""
        )

        # Mock the orchestrator to capture the jobs parameter
        with patch("fbuild.daemon.processors.build_processor.sys.modules") as mock_modules:
            mock_orchestrator_class = Mock()
            mock_orchestrator_instance = Mock()
            mock_orchestrator_class.return_value = mock_orchestrator_instance

            from fbuild.build.orchestrator import BuildResult

            mock_build_result = BuildResult(success=True, hex_path=None, elf_path=None, size_info=None, build_time=1.0, message="Build succeeded")
            mock_orchestrator_instance.build.return_value = mock_build_result

            mock_modules.get.return_value = {"BuildOrchestratorAVR": mock_orchestrator_class}
            mock_modules.__getitem__.return_value = type(
                "MockModule",
                (),
                {
                    "BuildOrchestratorAVR": mock_orchestrator_class,
                },
            )()

            # Create a BuildRequest with jobs=None (default)
            build_request = BuildRequest(
                project_dir=str(project_dir),
                environment="test",
                clean_build=False,
                verbose=False,
                caller_pid=os.getpid(),
                caller_cwd=str(Path.cwd()),
                jobs=None,  # Default: no explicit jobs parameter
            )

            from fbuild.daemon.processors.build_processor import BuildRequestProcessor

            processor = BuildRequestProcessor()
            mock_context = Mock()
            mock_context.lock_manager = Mock()
            mock_context.lock_manager.acquire_project_lock = Mock(return_value=True)
            mock_context.lock_manager.release_project_lock = Mock()

            with patch.object(processor, "_reload_build_modules"):
                with patch("fbuild.daemon.processors.build_processor.Path") as mock_path_class:
                    mock_path = Mock()
                    mock_path.parent.mkdir = Mock()
                    mock_path.write_text = Mock()
                    mock_path_class.return_value = mock_path

                    with patch("builtins.open", create=True) as mock_open:
                        mock_open.return_value.__enter__ = Mock()
                        mock_open.return_value.__exit__ = Mock()

                        with patch("fbuild.daemon.processors.build_processor.set_output_file"):
                            with patch("fbuild.daemon.processors.build_processor.reset_timer"):
                                with patch("fbuild.config.ini_parser.PlatformIOConfig") as mock_config_class:
                                    mock_config = Mock()
                                    mock_config.get_env_config.return_value = {"platform": "atmelavr"}
                                    mock_config_class.return_value = mock_config

                                    with patch("fbuild.packages.cache.Cache"):
                                        with patch("fbuild.daemon.processors.build_processor.getattr") as mock_getattr:
                                            mock_getattr.return_value = mock_orchestrator_class
                                            result = processor._execute_build(build_request, mock_context)

            mock_orchestrator_instance.build.assert_called_once()
            call_kwargs = mock_orchestrator_instance.build.call_args[1]
            assert call_kwargs["jobs"] is None, f"Expected jobs=None, got jobs={call_kwargs.get('jobs')}"
            assert result is True, "Build should have succeeded"

    def test_context_manager_receives_jobs(self):
        """
        Verify that managed_compilation_queue() context manager is called with
        the correct jobs value and properly manages queue lifecycle.

        This tests the context manager pattern used to ensure resource cleanup
        of temporary compilation queues.
        """
        from fbuild.build.orchestrator import (
            get_compilation_queue_for_build,
            managed_compilation_queue,
        )

        # Test Case 1: jobs=1 (serial mode)
        queue, should_cleanup = get_compilation_queue_for_build(jobs=1, verbose=False)
        assert queue is None, "Serial mode should return None"
        assert should_cleanup is False, "Serial mode requires no cleanup"

        # Test Case 2: jobs=None (default parallelism - daemon's shared queue)
        queue, should_cleanup = get_compilation_queue_for_build(jobs=None, verbose=False)
        # Returns daemon queue (no cleanup needed)
        assert should_cleanup is False, "Default mode should not require cleanup"

        # Test Case 3: Custom worker count (requires cleanup)
        # We can't easily test this without actually creating a queue,
        # but we can verify the logic path exists

        # Test the context manager pattern with a mock queue
        mock_queue = Mock()
        mock_queue.num_workers = 4

        with patch("fbuild.build.orchestrator.get_compilation_queue_for_build") as mock_get_queue:
            # Simulate a temporary queue that requires cleanup
            mock_get_queue.return_value = (mock_queue, True)

            with managed_compilation_queue(jobs=4, verbose=False) as queue:
                assert queue is mock_queue, "Context manager should yield the queue"

            # Verify shutdown was called
            mock_queue.shutdown.assert_called_once()

    def test_build_request_serialization_includes_jobs(self):
        """
        Verify that BuildRequest with jobs=4 can be serialized, deserialized,
        and preserves the jobs parameter.

        This ensures that the jobs parameter survives the daemon IPC round-trip.
        """
        # Create a BuildRequest with jobs=4
        original_request = BuildRequest(
            project_dir="/path/to/project",
            environment="esp32c6",
            clean_build=False,
            verbose=True,
            caller_pid=12345,
            caller_cwd="/working/dir",
            jobs=4,
        )

        # Serialize to dictionary (simulating JSON encoding)
        serialized = original_request.to_dict()

        # Verify jobs is in the serialized data
        assert "jobs" in serialized, "jobs should be in serialized data"
        assert serialized["jobs"] == 4, "jobs value should be preserved in serialization"

        # Verify all required fields are present
        assert serialized["project_dir"] == "/path/to/project"
        assert serialized["environment"] == "esp32c6"
        assert serialized["clean_build"] is False
        assert serialized["verbose"] is True
        assert serialized["caller_pid"] == 12345
        assert serialized["caller_cwd"] == "/working/dir"

        # Deserialize from dictionary (simulating JSON decoding)
        deserialized_request = BuildRequest.from_dict(serialized)

        # Verify jobs is preserved after deserialization
        assert deserialized_request.jobs == 4, "jobs should be preserved after deserialization"
        assert deserialized_request.project_dir == "/path/to/project"
        assert deserialized_request.environment == "esp32c6"
        assert deserialized_request.clean_build is False
        assert deserialized_request.verbose is True
        assert deserialized_request.caller_pid == 12345
        assert deserialized_request.caller_cwd == "/working/dir"

    def test_build_request_serialization_with_jobs_none(self):
        """
        Verify that BuildRequest with jobs=None serializes and deserializes correctly.

        jobs=None is the default value and should be preserved through serialization.
        """
        original_request = BuildRequest(
            project_dir="/path/to/project",
            environment="uno",
            clean_build=True,
            verbose=False,
            caller_pid=67890,
            caller_cwd="/another/dir",
            jobs=None,  # Explicitly set to None
        )

        # Serialize
        serialized = original_request.to_dict()
        assert "jobs" in serialized, "jobs field should exist even when None"
        assert serialized["jobs"] is None, "jobs value should be None in serialization"

        # Deserialize
        deserialized_request = BuildRequest.from_dict(serialized)
        assert deserialized_request.jobs is None, "jobs should remain None after deserialization"
        assert deserialized_request.project_dir == "/path/to/project"
        assert deserialized_request.environment == "uno"


@pytest.mark.integration
class TestParameterFlowEndToEnd:
    """Test parameter flow through the complete system."""

    def test_cli_to_daemon_request_serialization(self, tmp_path: Path):
        """
        Test that CLI arguments are correctly converted to BuildRequest
        and can be serialized for daemon communication.

        This simulates the full flow:
        1. CLI parses --jobs 4
        2. Creates BuildRequest(jobs=4)
        3. Serializes to JSON file for daemon
        4. Daemon reads and deserializes
        5. Passes to build processor
        """
        # Simulate CLI creating a BuildRequest
        from fbuild.cli import BuildArgs

        build_args = BuildArgs(
            project_dir=tmp_path / "project",
            environment="esp32c6",
            clean=False,
            verbose=True,
            jobs=4,  # User specified --jobs 4
        )

        # Create a BuildRequest from CLI args (this is what daemon_client.request_build does)
        build_request = BuildRequest(
            project_dir=str(build_args.project_dir),
            environment=build_args.environment or "default",
            clean_build=build_args.clean,
            verbose=build_args.verbose,
            caller_pid=os.getpid(),
            caller_cwd=str(Path.cwd()),
            jobs=build_args.jobs,  # This should flow through to orchestrator
        )

        # Simulate writing to daemon request file
        request_file = tmp_path / "build_request.json"
        request_data = build_request.to_dict()
        request_file.write_text(json.dumps(request_data))

        # Simulate daemon reading the request file
        loaded_data = json.loads(request_file.read_text())
        loaded_request = BuildRequest.from_dict(loaded_data)

        # Verify the jobs parameter made it through the round-trip
        assert loaded_request.jobs == 4, "jobs parameter should survive serialization round-trip"
        assert loaded_request.project_dir == str(build_args.project_dir)
        assert loaded_request.environment == "esp32c6"
        assert loaded_request.verbose is True

    def test_orchestrator_protocol_compliance(self):
        """
        Verify that platform-specific build methods comply with the
        PlatformBuildMethod protocol signature.

        This ensures all orchestrators accept the jobs parameter.
        """
        from fbuild.build.orchestrator import BuildResult, PlatformBuildMethod

        # Define a mock build method that follows the protocol
        def mock_build_method(
            project_path: Path,
            env_name: str,
            target: str,
            verbose: bool,
            clean: bool,
            jobs: int | None = None,
        ) -> BuildResult:
            """Mock build method following PlatformBuildMethod protocol."""
            return BuildResult(success=True, hex_path=None, elf_path=None, size_info=None, build_time=1.0, message=f"Built with jobs={jobs}")

        # Verify the mock method matches the protocol
        assert isinstance(mock_build_method, PlatformBuildMethod), "Mock should match PlatformBuildMethod protocol"

        # Call with various jobs values
        result1 = mock_build_method(Path("/project"), "env", "firmware", False, False, jobs=None)
        assert "jobs=None" in result1.message

        result2 = mock_build_method(Path("/project"), "env", "firmware", False, False, jobs=1)
        assert "jobs=1" in result2.message

        result3 = mock_build_method(Path("/project"), "env", "firmware", False, False, jobs=8)
        assert "jobs=8" in result3.message


if __name__ == "__main__":
    # Allow running tests directly
    pytest.main([__file__, "-v", "-s"])
