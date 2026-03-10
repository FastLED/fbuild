"""Unit tests for zccache integration in CompilationExecutor."""

import os
from pathlib import Path
from unittest.mock import MagicMock, patch

from fbuild.build.compilation_executor import CompilationExecutor


class TestZccacheDetection:
    """Tests for zccache binary detection."""

    def test_zccache_found_via_shutil_which(self) -> None:
        with patch("shutil.which", return_value="/usr/bin/zccache"):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))
            assert executor.zccache_path == "/usr/bin/zccache"

    def test_zccache_not_found(self) -> None:
        with patch("shutil.which", return_value=None):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))
            assert executor.zccache_path is None

    def test_zccache_disabled(self) -> None:
        with patch("shutil.which", return_value="/usr/bin/zccache"):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"), use_zccache=False)
            assert executor.zccache_path is None


class TestZccacheCommandBuilding:
    """Tests for compiler command construction with zccache."""

    def test_command_with_zccache_no_response_file(self) -> None:
        """zccache should be prepended even without response files."""
        with patch("shutil.which", return_value="/usr/bin/zccache"):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))
            cmd = executor._build_compile_command(
                compiler_path=Path("/usr/bin/gcc"),
                source_path=Path("/src/main.c"),
                output_path=Path("/out/main.o"),
                compile_flags=["-O2"],
                include_paths=["-I/usr/include"],
            )
            assert cmd[0] == "/usr/bin/zccache"

    def test_command_with_zccache_and_response_file(self) -> None:
        """Key improvement: zccache works WITH response files (sccache could not)."""
        with patch("shutil.which", return_value="/usr/bin/zccache"):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))
            cmd = executor._build_compile_command(
                compiler_path=Path("/usr/bin/gcc"),
                source_path=Path("/src/main.c"),
                output_path=Path("/out/main.o"),
                compile_flags=["-O2"],
                include_paths=["@/tmp/includes.rsp"],
            )
            # zccache should STILL be prepended (unlike sccache which was bypassed)
            assert cmd[0] == "/usr/bin/zccache"
            assert "@/tmp/includes.rsp" in cmd

    def test_command_without_zccache(self) -> None:
        """Without zccache, compiler path is used directly."""
        with patch("shutil.which", return_value=None):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))
            cmd = executor._build_compile_command(
                compiler_path=Path("/usr/bin/gcc"),
                source_path=Path("/src/main.c"),
                output_path=Path("/out/main.o"),
                compile_flags=["-O2"],
                include_paths=["-I/usr/include"],
            )
            assert cmd[0] == str(Path("/usr/bin/gcc"))
            assert "/usr/bin/zccache" not in cmd


class TestZccacheSessionManagement:
    """Tests for zccache session lifecycle."""

    def test_start_session_sets_env(self) -> None:
        """start_zccache_session should set ZCCACHE_SESSION_ID in environment."""
        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stdout = "session-abc123\n"
        mock_result.stderr = ""

        with patch("shutil.which", return_value="/usr/bin/zccache"):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))

            with patch("fbuild.build.compilation_executor.safe_run", return_value=mock_result):
                executor.start_zccache_session(Path("/usr/bin/g++"))

            assert executor._zccache_session_id == "session-abc123"
            assert os.environ.get("ZCCACHE_SESSION_ID") == "session-abc123"

            # Cleanup
            executor.end_zccache_session()

    def test_end_session_clears_env(self) -> None:
        """end_zccache_session should remove ZCCACHE_SESSION_ID from environment."""
        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stdout = "session-abc123\n"
        mock_result.stderr = ""

        with patch("shutil.which", return_value="/usr/bin/zccache"):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))

            with patch("fbuild.build.compilation_executor.safe_run", return_value=mock_result):
                executor.start_zccache_session(Path("/usr/bin/g++"))

            with patch("fbuild.build.compilation_executor.safe_run", return_value=mock_result):
                executor.end_zccache_session()

            assert executor._zccache_session_id is None
            assert "ZCCACHE_SESSION_ID" not in os.environ

    def test_start_session_failure_does_not_crash(self) -> None:
        """If session-start fails, compilation should still work (without caching)."""
        mock_result = MagicMock()
        mock_result.returncode = 1
        mock_result.stdout = ""
        mock_result.stderr = "daemon not running"

        with patch("shutil.which", return_value="/usr/bin/zccache"):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))

            with patch("fbuild.build.compilation_executor.safe_run", return_value=mock_result):
                executor.start_zccache_session(Path("/usr/bin/g++"))

            assert executor._zccache_session_id is None
            assert "ZCCACHE_SESSION_ID" not in os.environ

    def test_start_session_exception_does_not_crash(self) -> None:
        """If session-start throws, it should be caught gracefully."""
        with patch("shutil.which", return_value="/usr/bin/zccache"):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))

            with patch("fbuild.build.compilation_executor.safe_run", side_effect=OSError("not found")):
                executor.start_zccache_session(Path("/usr/bin/g++"))

            assert executor._zccache_session_id is None

    def test_end_session_without_start_is_noop(self) -> None:
        """end_zccache_session without start should be a no-op."""
        with patch("shutil.which", return_value="/usr/bin/zccache"):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))
            executor.end_zccache_session()  # Should not raise

    def test_no_session_when_zccache_disabled(self) -> None:
        """Session methods should be no-ops when zccache is not available."""
        with patch("shutil.which", return_value=None):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))
            executor.start_zccache_session(Path("/usr/bin/g++"))  # Should not raise
            assert executor._zccache_session_id is None
            executor.end_zccache_session()  # Should not raise

    def test_session_start_calls_zccache_with_compiler_path(self) -> None:
        """Verify session-start is called with the correct compiler path."""
        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stdout = "session-xyz\n"
        mock_result.stderr = ""

        with patch("shutil.which", return_value="/usr/bin/zccache"):
            executor = CompilationExecutor(build_dir=Path("/tmp/build"))

            with patch("fbuild.build.compilation_executor.safe_run", return_value=mock_result) as mock_run:
                compiler = Path("/opt/toolchain/xtensa-esp32-elf-g++")
                executor.start_zccache_session(compiler)

            mock_run.assert_called_once()
            call_args = mock_run.call_args[0][0]
            assert call_args[0] == "/usr/bin/zccache"
            assert call_args[1] == "session-start"
            assert call_args[2] == "--compiler"
            # The path is resolved to absolute — just verify it contains the compiler name
            assert "xtensa-esp32-elf-g++" in call_args[3]

            # Cleanup
            executor.end_zccache_session()
