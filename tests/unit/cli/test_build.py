"""Tests for CLI build command."""

import sys
from pathlib import Path
from unittest.mock import patch

import pytest

from fbuild.cli import main


@pytest.fixture(autouse=True)
def reset_output_module():
    """Reset the output module state after each test to avoid stream closure issues."""
    yield
    # Reset the output stream to sys.stdout after each test
    from fbuild import output

    output._output_stream = sys.stdout


class TestCLIBuild:
    """Tests for the 'fbuild build' command."""

    @pytest.fixture
    def project_dir(self, tmp_path):
        """Return the temp project directory with platformio.ini."""
        # Create a fake platformio.ini file so CLI validation passes
        platformio_ini = tmp_path / "platformio.ini"
        platformio_ini.write_text("[platformio]\ndefault_envs = default\n\n[env:default]\nplatform = atmelavr\nboard = uno\n")
        return tmp_path

    def test_build_success(self, project_dir, monkeypatch):
        """Test successful build."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = True
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 0

            # Verify daemon client was called correctly
            mock_client.request_build.assert_called_once()
            call_kwargs = mock_client.request_build.call_args.kwargs
            assert call_kwargs["environment"] == "default"
            assert call_kwargs["clean_build"] is False
            assert call_kwargs["verbose"] is False

    def test_build_with_environment(self, project_dir, monkeypatch):
        """Test build with specific environment."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", "--environment", "uno", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = True
            mock_env_detector.detect_environment.return_value = "uno"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 0
            mock_client.request_build.assert_called_once()
            call_kwargs = mock_client.request_build.call_args.kwargs
            assert call_kwargs["environment"] == "uno"

    def test_build_with_environment_short_option(self, project_dir, monkeypatch):
        """Test build with environment short option."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", "-e", "mega", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = True
            mock_env_detector.detect_environment.return_value = "mega"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 0
            mock_client.request_build.assert_called_once()
            call_kwargs = mock_client.request_build.call_args.kwargs
            assert call_kwargs["environment"] == "mega"

    def test_build_with_clean(self, project_dir, monkeypatch):
        """Test build with clean flag."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", "--clean", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = True
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 0
            mock_client.request_build.assert_called_once()
            call_kwargs = mock_client.request_build.call_args.kwargs
            assert call_kwargs["clean_build"] is True

    def test_build_with_clean_short_option(self, project_dir, monkeypatch):
        """Test build with clean short option."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", "-c", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = True
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 0
            call_kwargs = mock_client.request_build.call_args.kwargs
            assert call_kwargs["clean_build"] is True

    def test_build_with_verbose(self, project_dir, monkeypatch, capsys):
        """Test build with verbose flag."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", "--verbose", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = True
            mock_env_detector.detect_environment.return_value = "default"

            # Reset output module to use current sys.stdout (captured by capsys)
            from fbuild import output

            output.init_timer(sys.stdout)

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 0
            captured = capsys.readouterr()
            assert "Building project:" in captured.out
            mock_client.request_build.assert_called_once()
            call_kwargs = mock_client.request_build.call_args.kwargs
            assert call_kwargs["verbose"] is True

    def test_build_with_verbose_short_option(self, project_dir, monkeypatch):
        """Test build with verbose short option."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", "-v", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = True
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 0
            call_kwargs = mock_client.request_build.call_args.kwargs
            assert call_kwargs["verbose"] is True

    def test_build_with_project_dir(self, tmp_path, monkeypatch):
        """Test build with custom project directory as positional argument."""
        # Create platformio.ini in tmp_path
        platformio_ini = tmp_path / "platformio.ini"
        platformio_ini.write_text("[platformio]\ndefault_envs = default\n\n[env:default]\nplatform = atmelavr\nboard = uno\n")

        monkeypatch.setattr(sys, "argv", ["fbuild", "build", str(tmp_path)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = True
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 0
            mock_client.request_build.assert_called_once()
            call_kwargs = mock_client.request_build.call_args.kwargs
            assert call_kwargs["project_dir"] == tmp_path

    def test_build_combined_options(self, project_dir, monkeypatch):
        """Test build with multiple options combined."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", "-e", "uno", "-c", "-v", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = True
            mock_env_detector.detect_environment.return_value = "uno"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 0
            mock_client.request_build.assert_called_once()
            call_kwargs = mock_client.request_build.call_args.kwargs
            assert call_kwargs["environment"] == "uno"
            assert call_kwargs["clean_build"] is True
            assert call_kwargs["verbose"] is True

    def test_build_failure(self, project_dir, monkeypatch):
        """Test failed build."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = False
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 1

    def test_build_file_not_found(self, project_dir, monkeypatch, capsys):
        """Test build with missing file."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.side_effect = FileNotFoundError("platformio.ini not found")
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 1
            captured = capsys.readouterr()
            assert "File not found" in captured.out
            assert "platformio.ini" in captured.out
            assert "fbuild project directory" in captured.out

    def test_build_permission_error(self, project_dir, monkeypatch, capsys):
        """Test build with permission error."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.side_effect = PermissionError("Cannot write to build directory")
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 1
            captured = capsys.readouterr()
            assert "Permission denied" in captured.out

    def test_build_keyboard_interrupt(self, project_dir, monkeypatch, capsys):
        """Test build interrupted by user."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.side_effect = KeyboardInterrupt()
            mock_env_detector.detect_environment.return_value = "default"

            # The handler now re-raises KeyboardInterrupt after calling _thread.interrupt_main()
            with pytest.raises(KeyboardInterrupt):
                main()

    def test_build_unexpected_error(self, project_dir, monkeypatch, capsys):
        """Test build with unexpected error."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.side_effect = RuntimeError("Unexpected error occurred")
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 1
            captured = capsys.readouterr()
            assert "Unexpected error" in captured.out
            assert "RuntimeError" in captured.out

    def test_build_unexpected_error_verbose(self, project_dir, monkeypatch, capsys):
        """Test build with unexpected error in verbose mode."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", "-v", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.side_effect = RuntimeError("Unexpected error occurred")
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 1
            captured = capsys.readouterr()
            assert "Unexpected error" in captured.out
            assert "Traceback:" in captured.out

    def test_build_success_no_size_info(self, project_dir, monkeypatch):
        """Test successful build without size information."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", str(project_dir)])

        with (
            patch("fbuild.cli.daemon_client") as mock_client,
            patch("fbuild.cli.EnvironmentDetector") as mock_env_detector,
        ):
            mock_client.request_build.return_value = True
            mock_env_detector.detect_environment.return_value = "default"

            with pytest.raises(SystemExit) as exc_info:
                main()

            assert exc_info.value.code == 0

    def test_build_with_nonexistent_project_dir(self, monkeypatch, capsys):
        """Test build with nonexistent project directory."""
        nonexistent_path = Path("/nonexistent/path")

        monkeypatch.setattr(sys, "argv", ["fbuild", "build", str(nonexistent_path)])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 2
        captured = capsys.readouterr()
        assert "does not exist" in captured.out.lower()

    def test_main_help(self, monkeypatch, capsys):
        """Test main help output."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "--help"])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        captured = capsys.readouterr()
        assert "fbuild" in captured.out
        assert "build" in captured.out

    def test_build_help(self, monkeypatch, capsys):
        """Test build command help output."""
        monkeypatch.setattr(sys, "argv", ["fbuild", "build", "--help"])

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
        monkeypatch.setattr(sys, "argv", ["fbuild", "--version"])

        with pytest.raises(SystemExit) as exc_info:
            main()

        assert exc_info.value.code == 0
        captured = capsys.readouterr()
        from fbuild import __version__

        assert __version__ in captured.out


class TestCLIIntegration:
    """Integration tests for CLI."""

    def test_cli_import(self):
        """Test that CLI can be imported."""
        from fbuild.cli import build_command, main

        assert callable(build_command)
        assert callable(main)

    def test_main_is_function(self):
        """Test that main is a callable function."""
        from fbuild.cli import main

        assert callable(main)
