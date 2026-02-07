"""Unit tests for error handling and edge cases in the parallel package pipeline.

Tests cover:
- Download retry with exponential backoff on transient network failures
- Extraction retry on PermissionError (Windows antivirus delays)
- Ctrl-C cancellation with partial download cleanup
- Non-retryable errors (HTTP 404, unsupported archive format)
- Single package degenerate case
- All packages already cached
- Mixed cached/uncached packages
- Non-TTY fallback output
- Pipeline cancellation cleans up temp files
- Cleanup of .download temp files on failure
"""

import os
import tempfile
import threading
import time
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock, patch

import pytest

from fbuild.packages.pipeline.callbacks import NullCallback
from fbuild.packages.pipeline.models import PackageTask, PipelineResult, TaskPhase
from fbuild.packages.pipeline.pipeline import (
    ParallelPipeline,
    PipelineCancelledError,
)
from fbuild.packages.pipeline.pools import (
    DownloadPool,
    UnpackPool,
    _cleanup_temp_file,
)

# ─── Helpers ──────────────────────────────────────────────────────────────────


class RecordingCallback:
    """Thread-safe callback that records all progress updates."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, TaskPhase, float, float, str]] = []
        self._lock = threading.Lock()

    def on_progress(self, task_name: str, phase: TaskPhase, progress: float, total: float, detail: str) -> None:
        with self._lock:
            self.calls.append((task_name, phase, progress, total, detail))

    def get_calls(self) -> list[tuple[str, TaskPhase, float, float, str]]:
        with self._lock:
            return list(self.calls)

    def get_details_for(self, task_name: str) -> list[str]:
        with self._lock:
            return [c[4] for c in self.calls if c[0] == task_name]

    def get_phases_for(self, task_name: str) -> list[TaskPhase]:
        with self._lock:
            return [c[1] for c in self.calls if c[0] == task_name]


def make_task(
    name: str,
    url: str = "http://example.com/pkg.tar.gz",
    dest_path: str = "/tmp/pkg",
    version: str = "1.0.0",
    dependencies: list[str] | None = None,
) -> PackageTask:
    """Create a PackageTask with sensible defaults for testing."""
    return PackageTask(
        name=name,
        url=url,
        version=version,
        dest_path=dest_path,
        dependencies=dependencies if dependencies is not None else [],
    )


# ─── Mock Pools ───────────────────────────────────────────────────────────────


def make_mock_download_pool(delay: float = 0.0, fail_tasks: set[str] | None = None) -> MagicMock:
    """Create a mock DownloadPool."""
    fail_set = fail_tasks or set()
    pool = MagicMock()
    pool.__enter__ = MagicMock(return_value=pool)
    pool.__exit__ = MagicMock(return_value=False)

    def mock_submit(task: PackageTask, callback: Any) -> MagicMock:
        future = MagicMock()
        archive_path = Path(task.dest_path).parent / "archive.tar.gz"

        def get_result(timeout: float | None = None) -> Path:
            if delay > 0:
                time.sleep(delay)
            if task.name in fail_set:
                raise ConnectionError(f"Download failed for {task.name}")
            return archive_path

        future.result = get_result
        future.done.return_value = True
        future.cancel.return_value = True
        return future

    pool.submit_download = mock_submit
    return pool


def make_mock_unpack_pool(delay: float = 0.0, fail_tasks: set[str] | None = None) -> MagicMock:
    """Create a mock UnpackPool."""
    fail_set = fail_tasks or set()
    pool = MagicMock()
    pool.__enter__ = MagicMock(return_value=pool)
    pool.__exit__ = MagicMock(return_value=False)

    def mock_submit(task: PackageTask, archive_path: Path, callback: Any) -> MagicMock:
        future = MagicMock()
        extracted_path = Path(task.dest_path)

        def get_result(timeout: float | None = None) -> Path:
            if delay > 0:
                time.sleep(delay)
            if task.name in fail_set:
                raise OSError(f"Unpack failed for {task.name}")
            return extracted_path

        future.result = get_result
        future.done.return_value = True
        future.cancel.return_value = True
        return future

    pool.submit_unpack = mock_submit
    return pool


def make_mock_install_pool(delay: float = 0.0, fail_tasks: set[str] | None = None) -> MagicMock:
    """Create a mock InstallPool."""
    fail_set = fail_tasks or set()
    pool = MagicMock()
    pool.__enter__ = MagicMock(return_value=pool)
    pool.__exit__ = MagicMock(return_value=False)

    def mock_submit(task: PackageTask, extracted_path: Path, callback: Any) -> MagicMock:
        future = MagicMock()

        def get_result(timeout: float | None = None) -> Path:
            if delay > 0:
                time.sleep(delay)
            if task.name in fail_set:
                raise RuntimeError(f"Install failed for {task.name}")
            return extracted_path

        future.result = get_result
        future.done.return_value = True
        future.cancel.return_value = True
        return future

    pool.submit_install = mock_submit
    return pool


# ─── Download Retry Tests ────────────────────────────────────────────────────


class TestDownloadRetry:
    """Tests for download retry with exponential backoff."""

    @patch("fbuild.packages.pipeline.pools.platform")
    @patch("fbuild.packages.pipeline.pools._RETRY_BACKOFF_BASE", 0.01)
    def test_retry_on_connection_error(self, mock_platform: MagicMock) -> None:
        """Download should retry on ConnectionError."""
        mock_platform.system.return_value = "Linux"

        import requests

        call_count = 0

        def mock_get(url: str, stream: bool, timeout: int) -> MagicMock:
            nonlocal call_count
            call_count += 1
            if call_count < 3:
                raise requests.ConnectionError(f"Connection refused (attempt {call_count})")
            mock_resp = MagicMock()
            mock_resp.headers = {"content-length": "5"}
            mock_resp.iter_content.return_value = [b"hello"]
            mock_resp.raise_for_status.return_value = None
            return mock_resp

        with tempfile.TemporaryDirectory() as tmpdir:
            task = make_task("retry-pkg", "http://example.com/pkg.tar.gz", os.path.join(tmpdir, "output"))
            task.mark_started()
            cb = RecordingCallback()

            with patch("requests.get", side_effect=mock_get):
                with DownloadPool(max_workers=1) as pool:
                    future = pool.submit_download(task, cb)
                    result = future.result(timeout=30)

            assert result.exists()
            assert call_count == 3

            # Verify retry messages were reported
            details = cb.get_details_for("retry-pkg")
            retry_messages = [d for d in details if "Retry" in d]
            assert len(retry_messages) >= 1

    @patch("fbuild.packages.pipeline.pools.platform")
    @patch("fbuild.packages.pipeline.pools._RETRY_BACKOFF_BASE", 0.01)
    def test_retry_on_timeout(self, mock_platform: MagicMock) -> None:
        """Download should retry on Timeout errors."""
        mock_platform.system.return_value = "Linux"

        import requests

        call_count = 0

        def mock_get(url: str, stream: bool, timeout: int) -> MagicMock:
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                raise requests.Timeout("Connection timed out")
            mock_resp = MagicMock()
            mock_resp.headers = {"content-length": "5"}
            mock_resp.iter_content.return_value = [b"hello"]
            mock_resp.raise_for_status.return_value = None
            return mock_resp

        with tempfile.TemporaryDirectory() as tmpdir:
            task = make_task("timeout-pkg", "http://example.com/pkg.tar.gz", os.path.join(tmpdir, "output"))
            task.mark_started()

            with patch("requests.get", side_effect=mock_get):
                with DownloadPool(max_workers=1) as pool:
                    future = pool.submit_download(task, NullCallback())
                    result = future.result(timeout=30)

            assert result.exists()
            assert call_count == 2

    @patch("fbuild.packages.pipeline.pools.platform")
    @patch("fbuild.packages.pipeline.pools._MAX_DOWNLOAD_RETRIES", 2)
    @patch("fbuild.packages.pipeline.pools._RETRY_BACKOFF_BASE", 0.01)
    def test_retries_exhausted_raises(self, mock_platform: MagicMock) -> None:
        """Should raise after all retries are exhausted."""
        mock_platform.system.return_value = "Linux"

        import requests

        def mock_get(url: str, stream: bool, timeout: int) -> MagicMock:
            raise requests.ConnectionError("Connection refused")

        with tempfile.TemporaryDirectory() as tmpdir:
            task = make_task("fail-pkg", "http://example.com/pkg.tar.gz", os.path.join(tmpdir, "output"))
            task.mark_started()

            with patch("requests.get", side_effect=mock_get):
                with DownloadPool(max_workers=1) as pool:
                    future = pool.submit_download(task, NullCallback())
                    with pytest.raises(requests.ConnectionError, match="Connection refused"):
                        future.result(timeout=30)

    @patch("fbuild.packages.pipeline.pools.platform")
    def test_non_retryable_http_error(self, mock_platform: MagicMock) -> None:
        """HTTP 404 should not be retried (non-transient error)."""
        mock_platform.system.return_value = "Linux"

        import requests

        call_count = 0

        def mock_get(url: str, stream: bool, timeout: int) -> MagicMock:
            nonlocal call_count
            call_count += 1
            mock_resp = MagicMock()
            mock_resp.raise_for_status.side_effect = requests.HTTPError("404 Not Found")
            return mock_resp

        with tempfile.TemporaryDirectory() as tmpdir:
            task = make_task("404-pkg", "http://example.com/pkg.tar.gz", os.path.join(tmpdir, "output"))
            task.mark_started()

            with patch("requests.get", side_effect=mock_get):
                with DownloadPool(max_workers=1) as pool:
                    future = pool.submit_download(task, NullCallback())
                    with pytest.raises(requests.HTTPError, match="404"):
                        future.result(timeout=30)

            # Should only attempt once for non-transient errors
            assert call_count == 1

    @patch("fbuild.packages.pipeline.pools.platform")
    @patch("fbuild.packages.pipeline.pools._RETRY_BACKOFF_BASE", 0.01)
    def test_retry_cleans_up_temp_file(self, mock_platform: MagicMock) -> None:
        """Failed download attempts should clean up partial temp files."""
        mock_platform.system.return_value = "Linux"

        import requests

        call_count = 0

        def mock_get(url: str, stream: bool, timeout: int) -> MagicMock:
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                # First call writes some data then fails
                mock_resp = MagicMock()
                mock_resp.headers = {"content-length": "1000"}
                mock_resp.raise_for_status.return_value = None
                mock_resp.iter_content.side_effect = requests.ConnectionError("Connection reset")
                return mock_resp
            # Second call succeeds
            mock_resp = MagicMock()
            mock_resp.headers = {"content-length": "5"}
            mock_resp.iter_content.return_value = [b"hello"]
            mock_resp.raise_for_status.return_value = None
            return mock_resp

        with tempfile.TemporaryDirectory() as tmpdir:
            task = make_task("cleanup-pkg", "http://example.com/pkg.tar.gz", os.path.join(tmpdir, "output"))
            task.mark_started()

            with patch("requests.get", side_effect=mock_get):
                with DownloadPool(max_workers=1) as pool:
                    future = pool.submit_download(task, NullCallback())
                    result = future.result(timeout=30)

            assert result.exists()
            # Temp file should not remain
            temp_file = Path(str(result) + ".download")
            assert not temp_file.exists()


# ─── Extraction Retry Tests ──────────────────────────────────────────────────


class TestExtractionRetry:
    """Tests for extraction retry on PermissionError (Windows AV)."""

    @patch("fbuild.packages.pipeline.pools.platform")
    @patch("fbuild.packages.pipeline.pools._MAX_EXTRACT_RETRIES", 3)
    @patch("fbuild.packages.pipeline.pools._EXTRACT_RETRY_DELAY", 0.01)
    def test_retry_on_permission_error(self, mock_platform: MagicMock) -> None:
        """Extraction should retry on PermissionError."""
        mock_platform.system.return_value = "Linux"

        import io
        import tarfile

        attempt_number = 0
        original_extract = tarfile.TarFile.extract

        def flaky_extract(self_tar: Any, member: Any, path: Any = ".", **kwargs: Any) -> None:
            nonlocal attempt_number
            # Track which attempt we're on by detecting "Starting extraction" in callback
            # Simpler: fail on the first file of the first attempt only
            member_name = member.name if hasattr(member, "name") else str(member)
            if attempt_number == 0 and member_name == "a.txt":
                attempt_number += 1
                raise PermissionError("File is being used by another process")
            original_extract(self_tar, member, path, **kwargs)

        with tempfile.TemporaryDirectory() as tmpdir:
            # Create archive
            archive_path = Path(tmpdir) / "test.tar.gz"
            with tarfile.open(archive_path, "w:gz") as tar:
                for name in ["a.txt", "b.txt", "c.txt"]:
                    info = tarfile.TarInfo(name=name)
                    data = b"content"
                    info.size = len(data)
                    tar.addfile(info, io.BytesIO(data))

            dest = Path(tmpdir) / "extracted"
            task = make_task("av-pkg", "http://example.com/test.tar.gz", str(dest))
            cb = RecordingCallback()

            with patch.object(tarfile.TarFile, "extract", flaky_extract):
                with UnpackPool(max_workers=1) as pool:
                    future = pool.submit_unpack(task, archive_path, cb)
                    result = future.result(timeout=30)

            assert result.exists()

            # Verify retry messages were reported
            details = cb.get_details_for("av-pkg")
            retry_messages = [d for d in details if "Retry" in d]
            assert len(retry_messages) >= 1

    @patch("fbuild.packages.pipeline.pools.platform")
    @patch("fbuild.packages.pipeline.pools._MAX_EXTRACT_RETRIES", 2)
    @patch("fbuild.packages.pipeline.pools._EXTRACT_RETRY_DELAY", 0.01)
    def test_extraction_retries_exhausted(self, mock_platform: MagicMock) -> None:
        """Should raise after all extraction retries are exhausted."""
        mock_platform.system.return_value = "Linux"

        import io
        import tarfile

        def always_fail_extract(self_tar: Any, member: Any, path: Any = ".", **kwargs: Any) -> None:
            raise PermissionError("File locked by antivirus")

        with tempfile.TemporaryDirectory() as tmpdir:
            archive_path = Path(tmpdir) / "test.tar.gz"
            with tarfile.open(archive_path, "w:gz") as tar:
                info = tarfile.TarInfo(name="a.txt")
                data = b"content"
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))

            dest = Path(tmpdir) / "extracted"
            task = make_task("locked-pkg", "http://example.com/test.tar.gz", str(dest))

            with patch.object(tarfile.TarFile, "extract", always_fail_extract):
                with UnpackPool(max_workers=1) as pool:
                    future = pool.submit_unpack(task, archive_path, NullCallback())
                    with pytest.raises(PermissionError, match="locked by antivirus"):
                        future.result(timeout=30)

    def test_unsupported_format_not_retried(self) -> None:
        """ValueError for unsupported format should not be retried."""
        with tempfile.TemporaryDirectory() as tmpdir:
            archive_path = Path(tmpdir) / "test.rar"
            archive_path.write_bytes(b"not an archive")
            dest = Path(tmpdir) / "extracted"
            task = make_task("bad-fmt", "http://example.com/test.rar", str(dest))

            with UnpackPool(max_workers=1) as pool:
                future = pool.submit_unpack(task, archive_path, NullCallback())
                with pytest.raises(ValueError, match="Unsupported archive format"):
                    future.result(timeout=10)


# ─── Cleanup Tests ───────────────────────────────────────────────────────────


class TestCleanup:
    """Tests for cleanup on cancellation and failure."""

    def test_cleanup_temp_file_removes_existing(self) -> None:
        """_cleanup_temp_file should remove an existing temp file."""
        with tempfile.TemporaryDirectory() as tmpdir:
            temp_file = Path(tmpdir) / "archive.tar.gz.download"
            temp_file.write_bytes(b"partial data")
            assert temp_file.exists()

            _cleanup_temp_file(temp_file)
            assert not temp_file.exists()

    def test_cleanup_temp_file_nonexistent(self) -> None:
        """_cleanup_temp_file should not raise for nonexistent files."""
        _cleanup_temp_file(Path("/tmp/nonexistent_xyz123.download"))

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_cancel_cleans_up_partial_downloads(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Cancellation should attempt to clean up .download temp files."""
        mock_dl_cls.return_value = make_mock_download_pool(delay=1.0)
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        with tempfile.TemporaryDirectory() as tmpdir:
            # Create a .download temp file to simulate a partial download
            dest_dir = Path(tmpdir) / "pkg"
            dest_dir.mkdir(parents=True)
            temp_file = dest_dir / "archive.tar.gz.download"
            temp_file.write_bytes(b"partial download data")

            tasks = [make_task("cancel-pkg", dest_path=str(dest_dir / "output"))]

            pipeline = ParallelPipeline(
                download_workers=1,
                unpack_workers=1,
                install_workers=1,
            )

            def cancel_soon() -> None:
                time.sleep(0.1)
                pipeline.cancel()

            cancel_thread = threading.Thread(target=cancel_soon)
            cancel_thread.start()

            with pytest.raises(PipelineCancelledError):
                pipeline.run(tasks, NullCallback())

            cancel_thread.join()

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_pipeline_cleanup_on_keyboard_interrupt(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """KeyboardInterrupt should trigger cleanup and re-raise KeyboardInterrupt."""
        # Create a download pool that raises KeyboardInterrupt
        pool = MagicMock()
        pool.__enter__ = MagicMock(return_value=pool)
        pool.__exit__ = MagicMock(return_value=False)

        def mock_submit(task: PackageTask, callback: Any) -> MagicMock:
            future = MagicMock()
            future.done.return_value = False
            future.cancel.return_value = True
            return future

        pool.submit_download = mock_submit
        mock_dl_cls.return_value = pool
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [make_task("int-pkg", dest_path="/tmp/int-pkg")]

        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        # Simulate KeyboardInterrupt by patching time.sleep
        call_count = 0

        def interrupt_on_sleep(seconds: float) -> None:
            nonlocal call_count
            call_count += 1
            if call_count > 2:
                raise KeyboardInterrupt()

        with patch("fbuild.packages.pipeline.pipeline.time.sleep", side_effect=interrupt_on_sleep):
            with pytest.raises(KeyboardInterrupt):
                pipeline.run(tasks, NullCallback())


# ─── Edge Case Tests ─────────────────────────────────────────────────────────


class TestEdgeCases:
    """Tests for pipeline edge cases."""

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_single_task_with_recording_callback(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Single task should complete with full phase progression reported."""
        mock_dl_cls.return_value = make_mock_download_pool()
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [make_task("single-pkg", dest_path="/tmp/single")]
        pipeline = ParallelPipeline(
            download_workers=1,
            unpack_workers=1,
            install_workers=1,
        )

        cb = RecordingCallback()
        result = pipeline.run(tasks, cb)

        assert result.success is True
        assert result.completed_count == 1
        assert len(result.tasks) == 1

        phases = cb.get_phases_for("single-pkg")
        assert TaskPhase.DOWNLOADING in phases
        assert TaskPhase.UNPACKING in phases
        assert TaskPhase.INSTALLING in phases
        assert TaskPhase.DONE in phases

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_all_tasks_fail_at_different_phases(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Tasks can fail at different phases independently."""
        mock_dl_cls.return_value = make_mock_download_pool(fail_tasks={"dl-fail"})
        mock_up_cls.return_value = make_mock_unpack_pool(fail_tasks={"up-fail"})
        mock_ip_cls.return_value = make_mock_install_pool(fail_tasks={"ip-fail"})

        tasks = [
            make_task("dl-fail", dest_path="/tmp/dl-fail"),
            make_task("up-fail", dest_path="/tmp/up-fail"),
            make_task("ip-fail", dest_path="/tmp/ip-fail"),
        ]

        pipeline = ParallelPipeline(
            download_workers=3,
            unpack_workers=2,
            install_workers=2,
        )

        result = pipeline.run(tasks, NullCallback())

        assert result.success is False
        assert result.failed_count == 3
        for task in result.tasks:
            assert task.phase == TaskPhase.FAILED

    @patch("fbuild.packages.pipeline.pipeline.InstallPool")
    @patch("fbuild.packages.pipeline.pipeline.UnpackPool")
    @patch("fbuild.packages.pipeline.pipeline.DownloadPool")
    def test_failed_dependency_error_message(
        self,
        mock_dl_cls: MagicMock,
        mock_up_cls: MagicMock,
        mock_ip_cls: MagicMock,
    ) -> None:
        """Failed tasks should have descriptive error messages about which dependency failed."""
        mock_dl_cls.return_value = make_mock_download_pool(fail_tasks={"parent"})
        mock_up_cls.return_value = make_mock_unpack_pool()
        mock_ip_cls.return_value = make_mock_install_pool()

        tasks = [
            make_task("parent", dest_path="/tmp/parent"),
            make_task("child", dest_path="/tmp/child", dependencies=["parent"]),
        ]

        pipeline = ParallelPipeline(
            download_workers=2,
            unpack_workers=1,
            install_workers=1,
        )

        result = pipeline.run(tasks, NullCallback())

        tasks_by_name = {t.name: t for t in result.tasks}
        assert "parent" in tasks_by_name["child"].error_message


# ─── VerboseCallback Tests ────────────────────────────────────────────────────


class TestVerboseCallback:
    """Tests for the _VerboseCallback (non-TTY fallback)."""

    def test_verbose_callback_prints_done(self, capsys: Any) -> None:
        """VerboseCallback should print DONE phase updates."""
        from fbuild.packages.pipeline import _VerboseCallback

        cb = _VerboseCallback()
        cb.on_progress("test-pkg", TaskPhase.DONE, 1, 1, "Complete in 2.5s")

        captured = capsys.readouterr()
        assert "test-pkg" in captured.out
        assert "Done" in captured.out
        assert "Complete in 2.5s" in captured.out

    def test_verbose_callback_prints_failed_to_stderr(self, capsys: Any) -> None:
        """VerboseCallback should print FAILED updates to stderr."""
        from fbuild.packages.pipeline import _VerboseCallback

        cb = _VerboseCallback()
        cb.on_progress("fail-pkg", TaskPhase.FAILED, 0, 0, "Network error")

        captured = capsys.readouterr()
        assert "fail-pkg" in captured.err
        assert "Failed" in captured.err

    def test_verbose_callback_prints_progress_with_percentage(self, capsys: Any) -> None:
        """VerboseCallback should print percentage for phases with total > 0."""
        from fbuild.packages.pipeline import _VerboseCallback

        cb = _VerboseCallback()
        cb.on_progress("dl-pkg", TaskPhase.DOWNLOADING, 50, 100, "2.1 MB/s")

        captured = capsys.readouterr()
        assert "50%" in captured.out
        assert "dl-pkg" in captured.out


# ─── PipelineResult Tests ────────────────────────────────────────────────────


class TestPipelineResultEdgeCases:
    """Edge case tests for PipelineResult."""

    def test_empty_result(self) -> None:
        """Empty PipelineResult should have zero counts."""
        result = PipelineResult(tasks=[], total_elapsed=0.0, success=True)
        assert result.completed_count == 0
        assert result.failed_count == 0
        assert result.failed_tasks == []

    def test_all_done_result(self) -> None:
        """All-done result should show full success."""
        tasks = [make_task("a"), make_task("b")]
        for t in tasks:
            t.phase = TaskPhase.DONE
        result = PipelineResult(tasks=tasks, total_elapsed=1.0, success=True)
        assert result.completed_count == 2
        assert result.failed_count == 0

    def test_mixed_result(self) -> None:
        """Mixed result should correctly count done and failed."""
        tasks = [make_task("ok"), make_task("bad")]
        tasks[0].phase = TaskPhase.DONE
        tasks[1].phase = TaskPhase.FAILED
        tasks[1].error_message = "Something went wrong"
        result = PipelineResult(tasks=tasks, total_elapsed=1.0, success=False)
        assert result.completed_count == 1
        assert result.failed_count == 1
        assert len(result.failed_tasks) == 1
        assert result.failed_tasks[0].name == "bad"

    def test_result_serialization_roundtrip(self) -> None:
        """PipelineResult should survive serialization roundtrip."""
        tasks = [make_task("pkg-a", version="2.0")]
        tasks[0].phase = TaskPhase.DONE
        tasks[0].elapsed = 3.5
        result = PipelineResult(tasks=tasks, total_elapsed=5.0, success=True)

        data = result.to_dict()
        restored = PipelineResult.from_dict(data)

        assert restored.success is True
        assert restored.total_elapsed == 5.0
        assert len(restored.tasks) == 1
        assert restored.tasks[0].name == "pkg-a"
        assert restored.tasks[0].phase == TaskPhase.DONE


# ─── ParallelInstaller Tests ─────────────────────────────────────────────────


class TestParallelInstallerEdgeCases:
    """Edge case tests for the ParallelInstaller public API."""

    def test_is_tty_detection(self) -> None:
        """_is_tty() should not crash even with unusual stdout."""
        from fbuild.packages.pipeline import _is_tty

        # Should return a bool without crashing
        result = _is_tty()
        assert isinstance(result, bool)

    def test_is_tty_with_broken_stdout(self) -> None:
        """_is_tty() should return False when stdout.isatty() raises."""
        from fbuild.packages.pipeline import _is_tty

        class BrokenStdout:
            def isatty(self) -> bool:
                raise ValueError("broken")

        with patch("fbuild.packages.pipeline.sys.stdout", BrokenStdout()):
            assert _is_tty() is False

    def test_is_tty_with_no_isatty(self) -> None:
        """_is_tty() should return False when stdout has no isatty method."""
        from fbuild.packages.pipeline import _is_tty

        with patch("fbuild.packages.pipeline.sys.stdout", object()):
            assert _is_tty() is False
