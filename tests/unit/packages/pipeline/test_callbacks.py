"""Unit tests for the pipeline progress callback protocol and implementations."""

from fbuild.packages.pipeline.callbacks import NullCallback, ProgressCallback
from fbuild.packages.pipeline.models import TaskPhase


class TestProgressCallbackProtocol:
    """Tests for the ProgressCallback protocol."""

    def test_null_callback_is_progress_callback(self) -> None:
        """NullCallback should implement the ProgressCallback protocol."""
        cb = NullCallback()
        assert isinstance(cb, ProgressCallback)

    def test_null_callback_on_progress_does_nothing(self) -> None:
        """NullCallback.on_progress should silently discard updates."""
        cb = NullCallback()
        # Should not raise
        cb.on_progress("test-package", TaskPhase.DOWNLOADING, 50.0, 100.0, "50%")
        cb.on_progress("test-package", TaskPhase.UNPACKING, 10, 20, "Extracting...")
        cb.on_progress("test-package", TaskPhase.INSTALLING, 0, 0, "Verifying...")
        cb.on_progress("test-package", TaskPhase.DONE, 1, 1, "Complete")

    def test_null_callback_all_phases(self) -> None:
        """NullCallback should accept all TaskPhase values."""
        cb = NullCallback()
        for phase in TaskPhase:
            cb.on_progress("pkg", phase, 0.0, 0.0, "")

    def test_custom_callback_implements_protocol(self) -> None:
        """A custom class with on_progress should satisfy ProgressCallback."""

        class MyCallback:
            def __init__(self) -> None:
                self.calls: list[tuple[str, TaskPhase, float, float, str]] = []

            def on_progress(self, task_name: str, phase: TaskPhase, progress: float, total: float, detail: str) -> None:
                self.calls.append((task_name, phase, progress, total, detail))

        cb = MyCallback()
        assert isinstance(cb, ProgressCallback)

    def test_custom_callback_receives_args(self) -> None:
        """Custom callback should receive correct arguments."""

        class RecordingCallback:
            def __init__(self) -> None:
                self.calls: list[tuple[str, TaskPhase, float, float, str]] = []

            def on_progress(self, task_name: str, phase: TaskPhase, progress: float, total: float, detail: str) -> None:
                self.calls.append((task_name, phase, progress, total, detail))

        cb = RecordingCallback()
        cb.on_progress("my-pkg", TaskPhase.DOWNLOADING, 512.0, 1024.0, "1.2 MB/s")
        assert len(cb.calls) == 1
        assert cb.calls[0] == ("my-pkg", TaskPhase.DOWNLOADING, 512.0, 1024.0, "1.2 MB/s")

    def test_class_without_on_progress_is_not_protocol(self) -> None:
        """A class without on_progress should NOT satisfy ProgressCallback."""

        class NotACallback:
            pass

        obj = NotACallback()
        assert not isinstance(obj, ProgressCallback)

    def test_class_with_wrong_method_name_is_not_protocol(self) -> None:
        """A class with a different method name should NOT satisfy ProgressCallback."""

        class WrongMethod:
            def on_update(self, task_name: str, phase: TaskPhase, progress: float, total: float, detail: str) -> None:
                pass

        obj = WrongMethod()
        assert not isinstance(obj, ProgressCallback)

    def test_null_callback_zero_values(self) -> None:
        """NullCallback should handle zero progress/total gracefully."""
        cb = NullCallback()
        cb.on_progress("pkg", TaskPhase.WAITING, 0.0, 0.0, "")

    def test_null_callback_empty_strings(self) -> None:
        """NullCallback should handle empty task_name and detail."""
        cb = NullCallback()
        cb.on_progress("", TaskPhase.DOWNLOADING, 0.0, 0.0, "")

    def test_null_callback_large_values(self) -> None:
        """NullCallback should handle very large progress values."""
        cb = NullCallback()
        cb.on_progress("big-pkg", TaskPhase.DOWNLOADING, 1e12, 2e12, "Fast download")

    def test_multiple_callbacks_independent(self) -> None:
        """Multiple NullCallback instances should be independent."""
        cb1 = NullCallback()
        cb2 = NullCallback()
        cb1.on_progress("pkg1", TaskPhase.DOWNLOADING, 1.0, 2.0, "a")
        cb2.on_progress("pkg2", TaskPhase.UNPACKING, 3.0, 4.0, "b")
        # No shared state to verify, just checking no exceptions
