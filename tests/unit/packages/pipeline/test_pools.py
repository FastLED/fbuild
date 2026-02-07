"""Unit tests for the pipeline thread pools (DownloadPool, UnpackPool, InstallPool).

Tests use mocking to avoid real network and filesystem operations.
"""

import json
import os
import tarfile
import tempfile
import threading
import time
import zipfile
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from fbuild.packages.pipeline.callbacks import NullCallback
from fbuild.packages.pipeline.models import PackageTask, TaskPhase
from fbuild.packages.pipeline.pools import (
    DownloadPool,
    InstallPool,
    UnpackPool,
    _format_size,
    _format_transfer_speed,
)

# ─── Helpers ──────────────────────────────────────────────────────────────────


class RecordingCallback:
    """Callback that records all on_progress calls for assertions."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, TaskPhase, float, float, str]] = []
        self._lock = threading.Lock()

    def on_progress(self, task_name: str, phase: TaskPhase, progress: float, total: float, detail: str) -> None:
        with self._lock:
            self.calls.append((task_name, phase, progress, total, detail))

    def get_calls(self) -> list[tuple[str, TaskPhase, float, float, str]]:
        with self._lock:
            return list(self.calls)

    def get_phases(self) -> list[TaskPhase]:
        with self._lock:
            return [c[1] for c in self.calls]


def make_task(name: str, url: str, dest_path: str, version: str = "1.0.0") -> PackageTask:
    """Create a PackageTask with minimal required fields."""
    return PackageTask(
        name=name,
        url=url,
        version=version,
        dest_path=dest_path,
    )


def create_tar_gz(dest: Path, files: dict[str, bytes]) -> Path:
    """Create a .tar.gz archive at dest containing the given files.

    Args:
        dest: Path for the archive file.
        files: Mapping of filename -> contents.

    Returns:
        Path to the created archive.
    """
    import io

    with tarfile.open(dest, "w:gz") as tar:
        for name, content in files.items():
            info = tarfile.TarInfo(name=name)
            info.size = len(content)
            tar.addfile(info, io.BytesIO(content))
    return dest


def create_zip(dest: Path, files: dict[str, bytes]) -> Path:
    """Create a .zip archive at dest containing the given files.

    Args:
        dest: Path for the archive file.
        files: Mapping of filename -> contents.

    Returns:
        Path to the created archive.
    """
    with zipfile.ZipFile(dest, "w") as zf:
        for name, content in files.items():
            zf.writestr(name, content)
    return dest


# ─── DownloadPool Tests ──────────────────────────────────────────────────────


class TestDownloadPool:
    """Tests for the DownloadPool class."""

    def test_creation(self) -> None:
        """DownloadPool should initialize with specified worker count."""
        with DownloadPool(max_workers=3) as pool:
            assert pool.max_workers == 3

    def test_context_manager(self) -> None:
        """DownloadPool should support context manager protocol."""
        pool = DownloadPool(max_workers=2)
        with pool:
            assert pool.max_workers == 2
        # After exit, pool should be shut down
        with pytest.raises(RuntimeError, match="shut down"):
            pool.submit_download(make_task("x", "http://x", "/tmp/x"), NullCallback())

    def test_shutdown_prevents_new_submissions(self) -> None:
        """After shutdown, submit_download should raise RuntimeError."""
        pool = DownloadPool(max_workers=1)
        pool.shutdown()
        with pytest.raises(RuntimeError, match="shut down"):
            pool.submit_download(make_task("x", "http://x", "/tmp/x"), NullCallback())

    @patch("fbuild.packages.pipeline.pools.platform")
    def test_submit_download_mocked(self, mock_platform: MagicMock) -> None:
        """submit_download should download file and report progress."""
        mock_platform.system.return_value = "Linux"

        # Create a mock HTTP response
        chunk_data = b"hello world" * 100
        mock_response = MagicMock()
        mock_response.headers = {"content-length": str(len(chunk_data))}
        mock_response.iter_content.return_value = [chunk_data]
        mock_response.raise_for_status.return_value = None

        with tempfile.TemporaryDirectory() as tmpdir:
            dest = os.path.join(tmpdir, "output")
            task = make_task("test-pkg", "http://example.com/archive.tar.gz", dest)
            task.mark_started()

            cb = RecordingCallback()

            with patch("requests.get", return_value=mock_response):
                with DownloadPool(max_workers=1) as pool:
                    future = pool.submit_download(task, cb)
                    result = future.result(timeout=10)

            # Verify file was created
            assert result.exists()
            assert result.name == "archive.tar.gz"

            # Verify callback received progress updates
            calls = cb.get_calls()
            assert len(calls) >= 2  # At least start and completion
            assert calls[0][1] == TaskPhase.DOWNLOADING
            assert calls[-1][1] == TaskPhase.DOWNLOADING
            assert "complete" in calls[-1][4].lower()

    @patch("fbuild.packages.pipeline.pools.platform")
    def test_submit_download_multiple_chunks(self, mock_platform: MagicMock) -> None:
        """Progress should be reported for each chunk."""
        mock_platform.system.return_value = "Linux"

        chunks = [b"a" * 1024, b"b" * 1024, b"c" * 1024]
        total = sum(len(c) for c in chunks)

        mock_response = MagicMock()
        mock_response.headers = {"content-length": str(total)}
        mock_response.iter_content.return_value = chunks
        mock_response.raise_for_status.return_value = None

        with tempfile.TemporaryDirectory() as tmpdir:
            dest = os.path.join(tmpdir, "output")
            task = make_task("chunked", "http://example.com/file.tar.gz", dest)
            task.mark_started()

            cb = RecordingCallback()

            with patch("requests.get", return_value=mock_response):
                with DownloadPool(max_workers=1) as pool:
                    future = pool.submit_download(task, cb)
                    future.result(timeout=10)

            # Should have: 1 start + 3 chunks + 1 completion = 5 callbacks
            calls = cb.get_calls()
            assert len(calls) >= 4  # start + chunks + completion

    @patch("fbuild.packages.pipeline.pools.platform")
    def test_concurrent_downloads(self, mock_platform: MagicMock) -> None:
        """Multiple downloads should run concurrently."""
        mock_platform.system.return_value = "Linux"

        active_count = threading.Semaphore(0)
        barrier = threading.Event()

        def slow_get(url: str, stream: bool, timeout: int) -> MagicMock:
            active_count.release()
            barrier.wait(timeout=5)
            mock_resp = MagicMock()
            mock_resp.headers = {"content-length": "5"}
            mock_resp.iter_content.return_value = [b"hello"]
            mock_resp.raise_for_status.return_value = None
            return mock_resp

        with tempfile.TemporaryDirectory() as tmpdir:
            tasks = [make_task(f"pkg-{i}", f"http://example.com/file{i}.tar.gz", os.path.join(tmpdir, f"out{i}")) for i in range(3)]
            for t in tasks:
                t.mark_started()

            with patch("requests.get", side_effect=slow_get):
                with DownloadPool(max_workers=3) as pool:
                    futures = [pool.submit_download(t, NullCallback()) for t in tasks]

                    # Wait for all 3 to be active concurrently
                    for _ in range(3):
                        assert active_count.acquire(timeout=5)

                    # Release all
                    barrier.set()

                    for f in futures:
                        f.result(timeout=10)

    def test_download_network_error_propagates(self) -> None:
        """Network errors should propagate through the future."""
        mock_response = MagicMock()
        mock_response.raise_for_status.side_effect = Exception("Connection refused")

        with tempfile.TemporaryDirectory() as tmpdir:
            task = make_task("fail-pkg", "http://example.com/bad.tar.gz", os.path.join(tmpdir, "out"))

            with patch("requests.get", return_value=mock_response):
                with DownloadPool(max_workers=1) as pool:
                    future = pool.submit_download(task, NullCallback())
                    with pytest.raises(Exception, match="Connection refused"):
                        future.result(timeout=10)


# ─── UnpackPool Tests ─────────────────────────────────────────────────────────


class TestUnpackPool:
    """Tests for the UnpackPool class."""

    def test_creation(self) -> None:
        """UnpackPool should initialize with specified worker count."""
        with UnpackPool(max_workers=2) as pool:
            assert pool.max_workers == 2

    def test_context_manager(self) -> None:
        """UnpackPool should support context manager protocol."""
        pool = UnpackPool(max_workers=1)
        with pool:
            pass
        with pytest.raises(RuntimeError, match="shut down"):
            pool.submit_unpack(
                make_task("x", "http://x", "/tmp/x"),
                Path("/tmp/x.tar.gz"),
                NullCallback(),
            )

    def test_shutdown_prevents_new_submissions(self) -> None:
        """After shutdown, submit_unpack should raise RuntimeError."""
        pool = UnpackPool(max_workers=1)
        pool.shutdown()
        with pytest.raises(RuntimeError, match="shut down"):
            pool.submit_unpack(
                make_task("x", "http://x", "/tmp/x"),
                Path("/tmp/x.tar.gz"),
                NullCallback(),
            )

    @patch("fbuild.packages.pipeline.pools.platform")
    def test_unpack_tar_gz(self, mock_platform: MagicMock) -> None:
        """Should extract .tar.gz archives and report progress."""
        mock_platform.system.return_value = "Linux"

        with tempfile.TemporaryDirectory() as tmpdir:
            # Create a .tar.gz archive
            archive_path = Path(tmpdir) / "test.tar.gz"
            files = {
                "file1.txt": b"content1",
                "file2.txt": b"content2",
                "dir/file3.txt": b"content3",
            }
            create_tar_gz(archive_path, files)

            dest = Path(tmpdir) / "extracted"
            task = make_task("tar-pkg", "http://example.com/test.tar.gz", str(dest))

            cb = RecordingCallback()

            with UnpackPool(max_workers=1) as pool:
                future = pool.submit_unpack(task, archive_path, cb)
                result = future.result(timeout=30)

            # Verify extraction
            assert result.exists()
            assert (result / "file1.txt").exists()
            assert (result / "file2.txt").exists()

            # Verify progress callbacks
            calls = cb.get_calls()
            assert len(calls) >= 2
            assert all(c[1] == TaskPhase.UNPACKING for c in calls)

    @patch("fbuild.packages.pipeline.pools.platform")
    def test_unpack_zip(self, mock_platform: MagicMock) -> None:
        """Should extract .zip archives and report progress."""
        mock_platform.system.return_value = "Linux"

        with tempfile.TemporaryDirectory() as tmpdir:
            archive_path = Path(tmpdir) / "test.zip"
            files = {
                "readme.txt": b"hello",
                "src/main.cpp": b"int main() {}",
            }
            create_zip(archive_path, files)

            dest = Path(tmpdir) / "extracted"
            task = make_task("zip-pkg", "http://example.com/test.zip", str(dest))

            cb = RecordingCallback()

            with UnpackPool(max_workers=1) as pool:
                future = pool.submit_unpack(task, archive_path, cb)
                result = future.result(timeout=30)

            assert result.exists()
            assert (result / "readme.txt").exists()

    @patch("fbuild.packages.pipeline.pools.platform")
    def test_unpack_single_subdir(self, mock_platform: MagicMock) -> None:
        """Archives with a single subdirectory should unwrap it."""
        mock_platform.system.return_value = "Linux"

        with tempfile.TemporaryDirectory() as tmpdir:
            archive_path = Path(tmpdir) / "test.tar.gz"
            # Files inside a single subdirectory (GitHub-style)
            files = {
                "project-1.0/src/main.cpp": b"int main() {}",
                "project-1.0/README.md": b"# Project",
            }
            create_tar_gz(archive_path, files)

            dest = Path(tmpdir) / "extracted"
            task = make_task("github-pkg", "http://example.com/test.tar.gz", str(dest))

            with UnpackPool(max_workers=1) as pool:
                future = pool.submit_unpack(task, archive_path, NullCallback())
                result = future.result(timeout=30)

            # The single subdir should be unwrapped
            assert result.exists()
            assert (result / "src" / "main.cpp").exists()

    def test_unpack_unsupported_format(self) -> None:
        """Should raise ValueError for unsupported archive formats."""
        with tempfile.TemporaryDirectory() as tmpdir:
            archive_path = Path(tmpdir) / "test.rar"
            archive_path.write_bytes(b"not an archive")

            dest = Path(tmpdir) / "extracted"
            task = make_task("bad-pkg", "http://example.com/test.rar", str(dest))

            with UnpackPool(max_workers=1) as pool:
                future = pool.submit_unpack(task, archive_path, NullCallback())
                with pytest.raises(ValueError, match="Unsupported archive format"):
                    future.result(timeout=10)

    @patch("fbuild.packages.pipeline.pools.platform")
    def test_concurrent_unpacks(self, mock_platform: MagicMock) -> None:
        """Multiple unpack operations should run concurrently."""
        mock_platform.system.return_value = "Linux"

        with tempfile.TemporaryDirectory() as tmpdir:
            archives = []
            tasks = []
            for i in range(3):
                archive = Path(tmpdir) / f"archive{i}.tar.gz"
                files = {f"file{j}.txt": f"content{i}{j}".encode() for j in range(5)}
                create_tar_gz(archive, files)
                archives.append(archive)

                dest = Path(tmpdir) / f"extracted{i}"
                tasks.append(make_task(f"pkg-{i}", f"http://example.com/archive{i}.tar.gz", str(dest)))

            with UnpackPool(max_workers=3) as pool:
                futures = [pool.submit_unpack(t, a, NullCallback()) for t, a in zip(tasks, archives)]
                results = [f.result(timeout=30) for f in futures]

            for result in results:
                assert result.exists()


# ─── InstallPool Tests ────────────────────────────────────────────────────────


class TestInstallPool:
    """Tests for the InstallPool class."""

    def test_creation(self) -> None:
        """InstallPool should initialize with specified worker count."""
        with InstallPool(max_workers=2) as pool:
            assert pool.max_workers == 2

    def test_context_manager(self) -> None:
        """InstallPool should support context manager protocol."""
        pool = InstallPool(max_workers=1)
        with pool:
            pass
        with pytest.raises(RuntimeError, match="shut down"):
            pool.submit_install(make_task("x", "http://x", "/tmp/x"), Path("/tmp/x"), NullCallback())

    def test_shutdown_prevents_new_submissions(self) -> None:
        """After shutdown, submit_install should raise RuntimeError."""
        pool = InstallPool(max_workers=1)
        pool.shutdown()
        with pytest.raises(RuntimeError, match="shut down"):
            pool.submit_install(make_task("x", "http://x", "/tmp/x"), Path("/tmp/x"), NullCallback())

    def test_install_verifies_files(self) -> None:
        """Install should verify extracted directory contains files."""
        with tempfile.TemporaryDirectory() as tmpdir:
            extracted = Path(tmpdir) / "pkg"
            extracted.mkdir()
            (extracted / "file1.txt").write_text("hello")
            (extracted / "file2.txt").write_text("world")

            task = make_task("verify-pkg", "http://example.com/pkg.tar.gz", str(extracted), version="2.0")

            cb = RecordingCallback()

            with InstallPool(max_workers=1) as pool:
                future = pool.submit_install(task, extracted, cb)
                result = future.result(timeout=10)

            assert result == extracted

            # Verify fingerprint was written
            fingerprint_path = extracted / ".pipeline_fingerprint.json"
            assert fingerprint_path.exists()
            data = json.loads(fingerprint_path.read_text())
            assert data["name"] == "verify-pkg"
            assert data["version"] == "2.0"
            assert data["file_count"] == 2  # file1.txt + file2.txt

    def test_install_reports_progress(self) -> None:
        """Install should report progress through callback."""
        with tempfile.TemporaryDirectory() as tmpdir:
            extracted = Path(tmpdir) / "pkg"
            extracted.mkdir()
            (extracted / "file.txt").write_text("content")

            task = make_task("progress-pkg", "http://example.com/pkg.tar.gz", str(extracted))

            cb = RecordingCallback()

            with InstallPool(max_workers=1) as pool:
                future = pool.submit_install(task, extracted, cb)
                future.result(timeout=10)

            calls = cb.get_calls()
            assert len(calls) >= 3  # verify, count, fingerprint
            assert all(c[1] == TaskPhase.INSTALLING for c in calls)
            assert "Verifying" in calls[0][4]
            assert "complete" in calls[-1][4].lower()

    def test_install_empty_directory_raises(self) -> None:
        """Install should raise if extracted directory has no files."""
        with tempfile.TemporaryDirectory() as tmpdir:
            extracted = Path(tmpdir) / "empty_pkg"
            extracted.mkdir()

            task = make_task("empty-pkg", "http://example.com/pkg.tar.gz", str(extracted))

            with InstallPool(max_workers=1) as pool:
                future = pool.submit_install(task, extracted, NullCallback())
                with pytest.raises(FileNotFoundError, match="No files found"):
                    future.result(timeout=10)

    def test_install_nonexistent_path_raises(self) -> None:
        """Install should raise if extracted path doesn't exist."""
        task = make_task("missing-pkg", "http://example.com/pkg.tar.gz", "/tmp/nonexistent")

        with InstallPool(max_workers=1) as pool:
            future = pool.submit_install(task, Path("/tmp/nonexistent_dir_xyz123"), NullCallback())
            with pytest.raises(FileNotFoundError, match="does not exist"):
                future.result(timeout=10)

    def test_install_nested_files(self) -> None:
        """Install should count files recursively in subdirectories."""
        with tempfile.TemporaryDirectory() as tmpdir:
            extracted = Path(tmpdir) / "nested_pkg"
            extracted.mkdir()
            (extracted / "src").mkdir()
            (extracted / "src" / "main.cpp").write_text("int main() {}")
            (extracted / "include").mkdir()
            (extracted / "include" / "header.h").write_text("#pragma once")
            (extracted / "README.md").write_text("# Docs")

            task = make_task("nested-pkg", "http://example.com/pkg.tar.gz", str(extracted))

            with InstallPool(max_workers=1) as pool:
                future = pool.submit_install(task, extracted, NullCallback())
                result = future.result(timeout=10)

            fingerprint = json.loads((result / ".pipeline_fingerprint.json").read_text())
            assert fingerprint["file_count"] == 3  # main.cpp, header.h, README.md

    def test_concurrent_installs(self) -> None:
        """Multiple install operations should run concurrently."""
        with tempfile.TemporaryDirectory() as tmpdir:
            tasks_and_paths = []
            for i in range(4):
                extracted = Path(tmpdir) / f"pkg{i}"
                extracted.mkdir()
                for j in range(3):
                    (extracted / f"file{j}.txt").write_text(f"content{i}{j}")
                task = make_task(f"pkg-{i}", f"http://example.com/pkg{i}.tar.gz", str(extracted))
                tasks_and_paths.append((task, extracted))

            with InstallPool(max_workers=4) as pool:
                futures = [pool.submit_install(t, p, NullCallback()) for t, p in tasks_and_paths]
                results = [f.result(timeout=10) for f in futures]

            for result in results:
                assert (result / ".pipeline_fingerprint.json").exists()

    def test_install_fingerprint_includes_url(self) -> None:
        """Fingerprint should include the original URL."""
        with tempfile.TemporaryDirectory() as tmpdir:
            extracted = Path(tmpdir) / "pkg"
            extracted.mkdir()
            (extracted / "f.txt").write_text("x")

            url = "http://example.com/special-pkg.tar.gz"
            task = make_task("url-pkg", url, str(extracted))

            with InstallPool(max_workers=1) as pool:
                future = pool.submit_install(task, extracted, NullCallback())
                result = future.result(timeout=10)

            data = json.loads((result / ".pipeline_fingerprint.json").read_text())
            assert data["url"] == url

    def test_install_fingerprint_includes_timestamp(self) -> None:
        """Fingerprint should include an installed_at timestamp."""
        with tempfile.TemporaryDirectory() as tmpdir:
            extracted = Path(tmpdir) / "pkg"
            extracted.mkdir()
            (extracted / "f.txt").write_text("x")

            before = time.time()
            task = make_task("ts-pkg", "http://example.com/pkg.tar.gz", str(extracted))

            with InstallPool(max_workers=1) as pool:
                future = pool.submit_install(task, extracted, NullCallback())
                future.result(timeout=10)
            after = time.time()

            data = json.loads((extracted / ".pipeline_fingerprint.json").read_text())
            assert before <= data["installed_at"] <= after


# ─── Helper Function Tests ───────────────────────────────────────────────────


class TestFormatSize:
    """Tests for the _format_size helper."""

    def test_bytes(self) -> None:
        assert _format_size(0) == "0 B"
        assert _format_size(100) == "100 B"
        assert _format_size(1023) == "1023 B"

    def test_kilobytes(self) -> None:
        assert _format_size(1024) == "1.0 KB"
        assert _format_size(2048) == "2.0 KB"
        assert _format_size(1536) == "1.5 KB"

    def test_megabytes(self) -> None:
        assert _format_size(1048576) == "1.0 MB"
        assert _format_size(5 * 1024 * 1024) == "5.0 MB"

    def test_gigabytes(self) -> None:
        assert _format_size(1073741824) == "1.0 GB"
        assert _format_size(2 * 1024 * 1024 * 1024) == "2.0 GB"


class TestFormatTransferSpeed:
    """Tests for the _format_transfer_speed helper."""

    def test_with_start_time(self) -> None:
        task = make_task("speed-test", "http://x", "/tmp/x")
        task.start_time = time.monotonic() - 1.0  # 1 second ago
        speed_str = _format_transfer_speed(1024 * 1024, task)
        assert "MB/s" in speed_str or "KB/s" in speed_str

    def test_without_start_time(self) -> None:
        task = make_task("no-start", "http://x", "/tmp/x")
        result = _format_transfer_speed(2048, task)
        assert "KB" in result or "B" in result

    def test_zero_elapsed(self) -> None:
        task = make_task("instant", "http://x", "/tmp/x")
        task.start_time = time.monotonic()  # just now
        result = _format_transfer_speed(1024, task)
        # Should not crash, may show very high speed
        assert isinstance(result, str)


# ─── Cross-Pool Integration Tests ────────────────────────────────────────────


class TestPoolInteraction:
    """Tests verifying pools work together correctly."""

    def test_all_pools_support_context_manager(self) -> None:
        """All three pool types should support context manager protocol."""
        with DownloadPool(max_workers=1):
            pass
        with UnpackPool(max_workers=1):
            pass
        with InstallPool(max_workers=1):
            pass

    def test_all_pools_expose_max_workers(self) -> None:
        """All pools should expose their max_workers property."""
        with DownloadPool(max_workers=4) as dp:
            assert dp.max_workers == 4
        with UnpackPool(max_workers=2) as up:
            assert up.max_workers == 2
        with InstallPool(max_workers=3) as ip:
            assert ip.max_workers == 3

    @patch("fbuild.packages.pipeline.pools.platform")
    def test_unpack_then_install_pipeline(self, mock_platform: MagicMock) -> None:
        """Unpack result should feed into install as a mini-pipeline."""
        mock_platform.system.return_value = "Linux"

        with tempfile.TemporaryDirectory() as tmpdir:
            # Create archive
            archive_path = Path(tmpdir) / "pkg.tar.gz"
            files = {
                "src/main.cpp": b"int main() { return 0; }",
                "include/header.h": b"#pragma once",
            }
            create_tar_gz(archive_path, files)

            dest = Path(tmpdir) / "extracted"
            task = make_task("pipeline-pkg", "http://example.com/pkg.tar.gz", str(dest))

            cb = RecordingCallback()

            with UnpackPool(max_workers=1) as up:
                unpack_future = up.submit_unpack(task, archive_path, cb)
                extracted = unpack_future.result(timeout=30)

            with InstallPool(max_workers=1) as ip:
                install_future = ip.submit_install(task, extracted, cb)
                installed = install_future.result(timeout=10)

            assert installed.exists()
            assert (installed / ".pipeline_fingerprint.json").exists()

            # Verify both phases were reported
            phases = cb.get_phases()
            assert TaskPhase.UNPACKING in phases
            assert TaskPhase.INSTALLING in phases

    def test_thread_safety_of_recording_callback(self) -> None:
        """RecordingCallback should be thread-safe under concurrent writes."""
        cb = RecordingCallback()
        barrier = threading.Barrier(5)

        def write_progress(thread_id: int) -> None:
            barrier.wait()
            for i in range(100):
                cb.on_progress(f"thread-{thread_id}", TaskPhase.DOWNLOADING, float(i), 100.0, f"step {i}")

        threads = [threading.Thread(target=write_progress, args=(i,)) for i in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        calls = cb.get_calls()
        assert len(calls) == 500  # 5 threads * 100 calls
