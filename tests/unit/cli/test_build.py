"""Tests for CLI build command."""

import sys
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from zapio.build.linker import SizeInfo
from zapio.build.orchestrator import BuildResult
from zapio.cli import main


class TestCLIBuild:
    """Tests for the 'zap build' command."""

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
        self, mock_orchestrator, success_result, project_dir, monkeypatch, capsys
    ):
        """Test successful build."""
        mock_orchestrator.build.return_value = success_result

        monkeypatch.setattr(sys, "argv", ["zap", "build", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        captured = capsys.readouterr()
        assert "Build successful" in captured.out
        assert "firmware.hex" in captured.out
        assert "1050" in captured.out  # Flash size
        assert "150" in captured.out  # RAM size (data + bss)

        # Verify orchestrator was called correctly
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["env_name"] == "default"
        assert call_kwargs["clean"] is False
        assert call_kwargs["verbose"] is False

    def test_build_with_environment(
        self, mock_orchestrator, success_result, project_dir, monkeypatch
    ):
        """Test build with specific environment."""
        mock_orchestrator.build.return_value = success_result

        monkeypatch.setattr(
            sys, "argv", ["zap", "build", "--environment", "uno", str(project_dir)]
        )

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["env_name"] == "uno"

    def test_build_with_environment_short_option(
        self, mock_orchestrator, success_result, project_dir, monkeypatch
    ):
        """Test build with environment short option."""
        mock_orchestrator.build.return_value = success_result

        monkeypatch.setattr(
            sys, "argv", ["zap", "build", "-e", "mega", str(project_dir)]
        )

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["env_name"] == "mega"

    def test_build_with_clean(
        self, mock_orchestrator, success_result, project_dir, monkeypatch
    ):
        """Test build with clean flag."""
        mock_orchestrator.build.return_value = success_result

        monkeypatch.setattr(sys, "argv", ["zap", "build", "--clean", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["clean"] is True

    def test_build_with_clean_short_option(
        self, mock_orchestrator, success_result, project_dir, monkeypatch
    ):
        """Test build with clean short option."""
        mock_orchestrator.build.return_value = success_result

        monkeypatch.setattr(sys, "argv", ["zap", "build", "-c", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["clean"] is True

    def test_build_with_verbose(
        self, mock_orchestrator, success_result, project_dir, monkeypatch, capsys
    ):
        """Test build with verbose flag."""
        mock_orchestrator.build.return_value = success_result

        monkeypatch.setattr(
            sys, "argv", ["zap", "build", "--verbose", str(project_dir)]
        )

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        captured = capsys.readouterr()
        assert "Building project:" in captured.out
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["verbose"] is True

    def test_build_with_verbose_short_option(
        self, mock_orchestrator, success_result, project_dir, monkeypatch
    ):
        """Test build with verbose short option."""
        mock_orchestrator.build.return_value = success_result

        monkeypatch.setattr(sys, "argv", ["zap", "build", "-v", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["verbose"] is True

    def test_build_with_project_dir(
        self, mock_orchestrator, success_result, tmp_path, monkeypatch
    ):
        """Test build with custom project directory as positional argument."""
        mock_orchestrator.build.return_value = success_result

        monkeypatch.setattr(sys, "argv", ["zap", "build", str(tmp_path)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["project_dir"] == tmp_path

    def test_build_combined_options(
        self, mock_orchestrator, success_result, project_dir, monkeypatch
    ):
        """Test build with multiple options combined."""
        mock_orchestrator.build.return_value = success_result

        monkeypatch.setattr(
            sys, "argv", ["zap", "build", "-e", "uno", "-c", "-v", str(project_dir)]
        )

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        mock_orchestrator.build.assert_called_once()
        call_kwargs = mock_orchestrator.build.call_args.kwargs
        assert call_kwargs["env_name"] == "uno"
        assert call_kwargs["clean"] is True
        assert call_kwargs["verbose"] is True

    def test_build_failure(
        self, mock_orchestrator, failure_result, project_dir, monkeypatch, capsys
    ):
        """Test failed build."""
        mock_orchestrator.build.return_value = failure_result

        monkeypatch.setattr(sys, "argv", ["zap", "build", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 1
        captured = capsys.readouterr()
        assert "Build failed" in captured.out
        assert "syntax error" in captured.out

    def test_build_file_not_found(
        self, mock_orchestrator, project_dir, monkeypatch, capsys
    ):
        """Test build with missing file."""
        mock_orchestrator.build.side_effect = FileNotFoundError(
            "platformio.ini not found"
        )

        monkeypatch.setattr(sys, "argv", ["zap", "build", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 1
        captured = capsys.readouterr()
        assert "File not found" in captured.out
        assert "platformio.ini" in captured.out
        assert "Zapio project directory" in captured.out

    def test_build_permission_error(
        self, mock_orchestrator, project_dir, monkeypatch, capsys
    ):
        """Test build with permission error."""
        mock_orchestrator.build.side_effect = PermissionError(
            "Cannot write to build directory"
        )

        monkeypatch.setattr(sys, "argv", ["zap", "build", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 1
        captured = capsys.readouterr()
        assert "Permission denied" in captured.out

    def test_build_keyboard_interrupt(
        self, mock_orchestrator, project_dir, monkeypatch, capsys
    ):
        """Test build interrupted by user."""
        mock_orchestrator.build.side_effect = KeyboardInterrupt()

        monkeypatch.setattr(sys, "argv", ["zap", "build", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 130  # Standard SIGINT exit code
        captured = capsys.readouterr()
        assert "interrupted" in captured.out

    def test_build_unexpected_error(
        self, mock_orchestrator, project_dir, monkeypatch, capsys
    ):
        """Test build with unexpected error."""
        mock_orchestrator.build.side_effect = RuntimeError("Unexpected error occurred")

        monkeypatch.setattr(sys, "argv", ["zap", "build", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 1
        captured = capsys.readouterr()
        assert "Unexpected error" in captured.out
        assert "RuntimeError" in captured.out

    def test_build_unexpected_error_verbose(
        self, mock_orchestrator, project_dir, monkeypatch, capsys
    ):
        """Test build with unexpected error in verbose mode."""
        mock_orchestrator.build.side_effect = RuntimeError("Unexpected error occurred")

        monkeypatch.setattr(sys, "argv", ["zap", "build", "-v", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 1
        captured = capsys.readouterr()
        assert "Unexpected error" in captured.out
        assert "Traceback:" in captured.out

    def test_build_success_no_size_info(
        self, mock_orchestrator, tmp_path, project_dir, monkeypatch, capsys
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

        monkeypatch.setattr(sys, "argv", ["zap", "build", str(project_dir)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        captured = capsys.readouterr()
        assert "Build successful" in captured.out
        assert "firmware.hex" in captured.out
        # Should not have size info section
        assert "Firmware Size:" not in captured.out

    def test_build_with_nonexistent_project_dir(self, monkeypatch, capsys):
        """Test build with nonexistent project directory."""
        nonexistent_path = Path("/nonexistent/path")

        monkeypatch.setattr(sys, "argv", ["zap", "build", str(nonexistent_path)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 2
        captured = capsys.readouterr()
        assert "does not exist" in captured.out.lower()

    def test_main_help(self, monkeypatch, capsys):
        """Test main help output."""
        monkeypatch.setattr(sys, "argv", ["zap", "--help"])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        captured = capsys.readouterr()
        assert "Zapio" in captured.out
        assert "build" in captured.out

    def test_build_help(self, monkeypatch, capsys):
        """Test build command help output."""
        monkeypatch.setattr(sys, "argv", ["zap", "build", "--help"])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        captured = capsys.readouterr()
        assert "--environment" in captured.out
        assert "--clean" in captured.out
        assert "--verbose" in captured.out
        assert "project_dir" in captured.out

    def test_main_version(self, monkeypatch, capsys):
        """Test version flag."""
        monkeypatch.setattr(sys, "argv", ["zap", "--version"])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        captured = capsys.readouterr()
        assert "0.1.0" in captured.out


class TestCLIIntegration:
    """Integration tests for CLI."""

    def test_cli_import(self):
        """Test that CLI can be imported."""
        from zapio.cli import build_command, main

        assert callable(build_command)
        assert callable(main)

    def test_main_is_function(self):
        """Test that main is a callable function."""
        from zapio.cli import main

        assert callable(main)
