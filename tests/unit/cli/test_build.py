"""Tests for CLI build command."""

from unittest.mock import MagicMock, patch

import pytest
from click.testing import CliRunner

from zapio.build.linker import SizeInfo
from zapio.build.orchestrator import BuildResult
from zapio.cli import build, main


class TestCLIBuild:
    """Tests for the 'zap build' command."""

    @pytest.fixture
    def runner(self):
        """Create CLI runner."""
        return CliRunner()

    @pytest.fixture
    def mock_orchestrator(self, tmp_path):
        """Create mock BuildOrchestrator and setup test environment."""
        # Create a fake platformio.ini file so CLI validation passes
        platformio_ini = tmp_path / "platformio.ini"
        platformio_ini.write_text(
            "[platformio]\ndefault_envs = default\n\n[env:default]\nplatform = atmelavr\nboard = uno\n"
        )

        # Mock PlatformIOConfig to avoid file system checks
        mock_config = MagicMock()
        mock_config.get_default_environment.return_value = "default"

        # Mock BuildOrchestrator and PlatformIOConfig
        with (
            patch("zapio.cli.BuildOrchestrator") as mock_orch_class,
            patch("zapio.config.PlatformIOConfig", return_value=mock_config),
        ):
            mock_instance = MagicMock()
            mock_orch_class.return_value = mock_instance
            yield mock_instance

    @pytest.fixture
    def project_dir(self, tmp_path):
        """Return the temp project directory."""
        return tmp_path

    @pytest.fixture
    def success_result(self, tmp_path):
        """Create successful build result."""
        hex_path = tmp_path / ".zap" / "build" / "uno" / "firmware.hex"
        elf_path = tmp_path / ".zap" / "build" / "uno" / "firmware.elf"

        size_info = SizeInfo(
            text=1000,
            data=50,
            bss=100,
            total_flash=1050,
            total_ram=150,  # data + bss
            max_flash=32256,
            max_ram=2048,
        )

        return BuildResult(
            success=True,
            hex_path=hex_path,
            elf_path=elf_path,
            size_info=size_info,
            build_time=12.34,
            message="Build successful",
        )

    @pytest.fixture
    def failure_result(self):
        """Create failed build result."""
        return BuildResult(
            success=False,
            hex_path=None,
            elf_path=None,
            size_info=None,
            build_time=2.5,
            message="Compilation failed: syntax error in main.cpp",
        )

    def test_build_success(
        self, runner, mock_orchestrator, success_result, project_dir
    ):
        """Test successful build."""
        mock_orchestrator.build.return_value = success_result

        result = runner.invoke(main, ["build", "-d", str(project_dir)])

        assert result.exit_code == 0
        assert "Build successful" in result.output
        assert "firmware.hex" in result.output
        assert "1050" in result.output  # Flash size
        assert "150" in result.output  # RAM size (data + bss)

        # Verify orchestrator was called correctly
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["env_name"] == "default"
        assert call_kwargs["clean"] is False
        assert call_kwargs["verbose"] is False

    def test_build_with_environment(
        self, runner, mock_orchestrator, success_result, project_dir
    ):
        """Test build with specific environment."""
        mock_orchestrator.build.return_value = success_result

        result = runner.invoke(
            main, ["build", "--environment", "uno", "-d", str(project_dir)]
        )

        assert result.exit_code == 0
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["env_name"] == "uno"

    def test_build_with_environment_short_option(
        self, runner, mock_orchestrator, success_result, project_dir
    ):
        """Test build with environment short option."""
        mock_orchestrator.build.return_value = success_result

        result = runner.invoke(main, ["build", "-e", "mega", "-d", str(project_dir)])

        assert result.exit_code == 0
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["env_name"] == "mega"

    def test_build_with_clean(
        self, runner, mock_orchestrator, success_result, project_dir
    ):
        """Test build with clean flag."""
        mock_orchestrator.build.return_value = success_result

        result = runner.invoke(main, ["build", "--clean", "-d", str(project_dir)])

        assert result.exit_code == 0
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["clean"] is True

    def test_build_with_clean_short_option(
        self, runner, mock_orchestrator, success_result, project_dir
    ):
        """Test build with clean short option."""
        mock_orchestrator.build.return_value = success_result

        result = runner.invoke(main, ["build", "-c", "-d", str(project_dir)])

        assert result.exit_code == 0
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["clean"] is True

    def test_build_with_verbose(
        self, runner, mock_orchestrator, success_result, project_dir
    ):
        """Test build with verbose flag."""
        mock_orchestrator.build.return_value = success_result

        result = runner.invoke(main, ["build", "--verbose", "-d", str(project_dir)])

        assert result.exit_code == 0
        assert "Building project:" in result.output
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["verbose"] is True

    def test_build_with_verbose_short_option(
        self, runner, mock_orchestrator, success_result, project_dir
    ):
        """Test build with verbose short option."""
        mock_orchestrator.build.return_value = success_result

        result = runner.invoke(main, ["build", "-v", "-d", str(project_dir)])

        assert result.exit_code == 0
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["verbose"] is True

    def test_build_with_project_dir(
        self, runner, mock_orchestrator, success_result, tmp_path
    ):
        """Test build with custom project directory."""
        mock_orchestrator.build.return_value = success_result

        result = runner.invoke(main, ["build", "--project-dir", str(tmp_path)])

        assert result.exit_code == 0
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["project_dir"] == tmp_path

    def test_build_with_project_dir_short_option(
        self, runner, mock_orchestrator, success_result, tmp_path
    ):
        """Test build with project directory short option."""
        mock_orchestrator.build.return_value = success_result

        result = runner.invoke(main, ["build", "-d", str(tmp_path)])

        assert result.exit_code == 0
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["project_dir"] == tmp_path

    def test_build_combined_options(
        self, runner, mock_orchestrator, success_result, project_dir
    ):
        """Test build with multiple options combined."""
        mock_orchestrator.build.return_value = success_result

        result = runner.invoke(
            main, ["build", "-e", "uno", "-c", "-v", "-d", str(project_dir)]
        )

        assert result.exit_code == 0
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["env_name"] == "uno"
        assert call_kwargs["clean"] is True
        assert call_kwargs["verbose"] is True

    def test_build_failure(
        self, runner, mock_orchestrator, failure_result, project_dir
    ):
        """Test failed build."""
        mock_orchestrator.build.return_value = failure_result

        result = runner.invoke(main, ["build", "-d", str(project_dir)])

        assert result.exit_code == 1
        assert "Build failed" in result.output
        assert "syntax error" in result.output

    def test_build_file_not_found(self, runner, mock_orchestrator, project_dir):
        """Test build with missing file."""
        mock_orchestrator.build.side_effect = FileNotFoundError(
            "platformio.ini not found"
        )

        result = runner.invoke(main, ["build", "-d", str(project_dir)])

        assert result.exit_code == 1
        assert "File not found" in result.output
        assert "platformio.ini" in result.output
        assert "Zapio project directory" in result.output

    def test_build_permission_error(self, runner, mock_orchestrator, project_dir):
        """Test build with permission error."""
        mock_orchestrator.build.side_effect = PermissionError(
            "Cannot write to build directory"
        )

        result = runner.invoke(main, ["build", "-d", str(project_dir)])

        assert result.exit_code == 1
        assert "Permission denied" in result.output

    def test_build_keyboard_interrupt(self, runner, mock_orchestrator, project_dir):
        """Test build interrupted by user."""
        mock_orchestrator.build.side_effect = KeyboardInterrupt()

        result = runner.invoke(main, ["build", "-d", str(project_dir)])

        assert result.exit_code == 130  # Standard SIGINT exit code
        assert "interrupted" in result.output

    def test_build_unexpected_error(self, runner, mock_orchestrator, project_dir):
        """Test build with unexpected error."""
        mock_orchestrator.build.side_effect = RuntimeError("Unexpected error occurred")

        result = runner.invoke(main, ["build", "-d", str(project_dir)])

        assert result.exit_code == 1
        assert "Unexpected error" in result.output
        assert "RuntimeError" in result.output

    def test_build_unexpected_error_verbose(
        self, runner, mock_orchestrator, project_dir
    ):
        """Test build with unexpected error in verbose mode."""
        mock_orchestrator.build.side_effect = RuntimeError("Unexpected error occurred")

        result = runner.invoke(main, ["build", "-v", "-d", str(project_dir)])

        assert result.exit_code == 1
        assert "Unexpected error" in result.output
        assert "Traceback:" in result.output

    def test_build_success_no_size_info(
        self, runner, mock_orchestrator, tmp_path, project_dir
    ):
        """Test successful build without size information."""
        result_no_size = BuildResult(
            success=True,
            hex_path=tmp_path / "firmware.hex",
            elf_path=tmp_path / "firmware.elf",
            size_info=None,
            build_time=5.0,
            message="Build successful",
        )
        mock_orchestrator.build.return_value = result_no_size

        result = runner.invoke(main, ["build", "-d", str(project_dir)])

        assert result.exit_code == 0
        assert "Build successful" in result.output
        assert "firmware.hex" in result.output
        # Should not have size info section
        assert "Firmware Size:" not in result.output

    def test_build_with_nonexistent_project_dir(self, runner):
        """Test build with nonexistent project directory."""
        result = runner.invoke(main, ["build", "-d", "/nonexistent/path"])

        assert result.exit_code == 2  # Click validation error
        assert (
            "does not exist" in result.output.lower()
            or "invalid" in result.output.lower()
        )

    def test_main_help(self, runner):
        """Test main help output."""
        result = runner.invoke(main, ["--help"])

        assert result.exit_code == 0
        assert "Zapio" in result.output
        assert "build" in result.output

    def test_build_help(self, runner):
        """Test build command help output."""
        result = runner.invoke(main, ["build", "--help"])

        assert result.exit_code == 0
        assert "Build firmware" in result.output
        assert "--environment" in result.output
        assert "--clean" in result.output
        assert "--verbose" in result.output
        assert "--project-dir" in result.output

    def test_main_version(self, runner):
        """Test version flag."""
        result = runner.invoke(main, ["--version"])

        assert result.exit_code == 0
        assert "0.1.0" in result.output


class TestCLIIntegration:
    """Integration tests for CLI."""

    @pytest.fixture
    def runner(self):
        """Create CLI runner."""
        return CliRunner()

    def test_cli_import(self):
        """Test that CLI can be imported."""
        from zapio.cli import main

        assert callable(build)
        assert callable(main)

    def test_cli_is_click_group(self):
        """Test that main is a Click group."""
        import click

        assert isinstance(main, click.Group)

    def test_build_is_click_command(self):
        """Test that build is a Click command."""
        import click

        # build is a Command wrapped by the group
        assert "build" in main.commands
        assert isinstance(main.commands["build"], click.Command)
