"""Unit tests for the pipeline adapters module.

Tests verify:
- AVR task graph construction from platformio.ini
- Library dependency spec parsing (URLs, GitHub shorthand, registry specs, plain names)
- Platform auto-detection and delegation
- Cache filtering (cached vs uncached tasks)
- Error handling for invalid configs and unsupported platforms
- Task dependency graph structure (correct DAG edges)
"""

import os
import textwrap
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from fbuild.packages.cache import Cache
from fbuild.packages.pipeline.adapters import (
    TaskGraphError,
    _detect_platform,
    _parse_lib_spec,
    build_avr_task_graph,
    build_task_graph,
    filter_uncached_tasks,
    is_task_cached,
)
from fbuild.packages.pipeline.models import PackageTask, TaskPhase

# ─── Fixtures ─────────────────────────────────────────────────────────────────


@pytest.fixture
def tmp_project(tmp_path: Path) -> Path:
    """Create a temporary project directory with a minimal AVR platformio.ini."""
    ini_content = textwrap.dedent(
        """\
        [env:uno]
        platform = atmelavr
        board = uno
        framework = arduino
    """
    )
    ini_path = tmp_path / "platformio.ini"
    ini_path.write_text(ini_content)
    return tmp_path


@pytest.fixture
def tmp_project_with_libs(tmp_path: Path) -> Path:
    """Create a temporary project directory with AVR config and library deps."""
    ini_content = textwrap.dedent(
        """\
        [env:uno]
        platform = atmelavr
        board = uno
        framework = arduino
        lib_deps =
            SPI
            Wire
            https://github.com/FastLED/FastLED
    """
    )
    ini_path = tmp_path / "platformio.ini"
    ini_path.write_text(ini_content)
    return tmp_path


@pytest.fixture
def tmp_project_esp32(tmp_path: Path) -> Path:
    """Create a temporary project with ESP32 config (unsupported by adapter)."""
    ini_content = textwrap.dedent(
        """\
        [env:esp32dev]
        platform = espressif32
        board = esp32dev
        framework = arduino
    """
    )
    ini_path = tmp_path / "platformio.ini"
    ini_path.write_text(ini_content)
    return tmp_path


@pytest.fixture
def cache(tmp_path: Path) -> Cache:
    """Create a Cache instance pointing to a temp directory."""
    cache_dir = tmp_path / "cache"
    cache_dir.mkdir()
    with patch.dict(os.environ, {"FBUILD_CACHE_DIR": str(cache_dir)}):
        return Cache(tmp_path)


# ─── AVR Task Graph Tests ────────────────────────────────────────────────────


class TestBuildAvrTaskGraph:
    """Tests for build_avr_task_graph()."""

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_basic_avr_project_produces_two_tasks(self, mock_detect: MagicMock, tmp_project: Path, cache: Cache) -> None:
        """A basic AVR project (no libs) should produce toolchain + framework tasks."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project, "uno", cache)

        assert len(tasks) == 2
        names = [t.name for t in tasks]
        assert "toolchain-atmelavr" in names
        assert "framework-arduino-avr" in names

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_toolchain_task_has_no_dependencies(self, mock_detect: MagicMock, tmp_project: Path, cache: Cache) -> None:
        """Toolchain task should have no dependencies."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project, "uno", cache)
        toolchain = next(t for t in tasks if t.name == "toolchain-atmelavr")

        assert toolchain.dependencies == []

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_framework_task_has_no_dependencies(self, mock_detect: MagicMock, tmp_project: Path, cache: Cache) -> None:
        """Framework task should have no dependencies."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project, "uno", cache)
        framework = next(t for t in tasks if t.name == "framework-arduino-avr")

        assert framework.dependencies == []

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_toolchain_task_has_correct_url(self, mock_detect: MagicMock, tmp_project: Path, cache: Cache) -> None:
        """Toolchain task should have the detected download URL."""
        expected_url = "https://downloads.arduino.cc/tools/avr-gcc-test.zip"
        mock_detect.return_value = (expected_url, "abc123")

        tasks = build_avr_task_graph(tmp_project, "uno", cache)
        toolchain = next(t for t in tasks if t.name == "toolchain-atmelavr")

        assert toolchain.url == expected_url

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_framework_task_has_correct_url(self, mock_detect: MagicMock, tmp_project: Path, cache: Cache) -> None:
        """Framework task URL should match ArduinoCore.AVR_URL."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project, "uno", cache)
        framework = next(t for t in tasks if t.name == "framework-arduino-avr")

        from fbuild.packages.arduino_core import ArduinoCore

        assert framework.url == ArduinoCore.AVR_URL

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_project_with_libs_produces_lib_tasks(self, mock_detect: MagicMock, tmp_project_with_libs: Path, cache: Cache) -> None:
        """A project with lib_deps should produce library tasks."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project_with_libs, "uno", cache)

        # toolchain + framework + 3 libs
        assert len(tasks) == 5
        lib_names = [t.name for t in tasks if t.name not in ("toolchain-atmelavr", "framework-arduino-avr")]
        assert "SPI" in lib_names
        assert "Wire" in lib_names
        assert "FastLED" in lib_names

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_lib_tasks_depend_on_framework(self, mock_detect: MagicMock, tmp_project_with_libs: Path, cache: Cache) -> None:
        """Library tasks should depend on framework-arduino-avr."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project_with_libs, "uno", cache)
        lib_tasks = [t for t in tasks if t.name not in ("toolchain-atmelavr", "framework-arduino-avr")]

        for lib_task in lib_tasks:
            assert "framework-arduino-avr" in lib_task.dependencies

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_all_tasks_have_dest_paths(self, mock_detect: MagicMock, tmp_project: Path, cache: Cache) -> None:
        """All tasks should have non-empty dest_path."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project, "uno", cache)

        for task in tasks:
            assert task.dest_path, f"Task {task.name} has empty dest_path"

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_all_tasks_start_in_waiting_phase(self, mock_detect: MagicMock, tmp_project: Path, cache: Cache) -> None:
        """All tasks should start in WAITING phase."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project, "uno", cache)

        for task in tasks:
            assert task.phase == TaskPhase.WAITING

    def test_wrong_platform_raises_error(self, tmp_project_esp32: Path, cache: Cache) -> None:
        """build_avr_task_graph should reject non-AVR platforms."""
        with pytest.raises(TaskGraphError, match="Expected platform 'atmelavr'"):
            build_avr_task_graph(tmp_project_esp32, "esp32dev", cache)

    def test_missing_platformio_ini_raises_error(self, tmp_path: Path, cache: Cache) -> None:
        """Missing platformio.ini should raise TaskGraphError."""
        with pytest.raises(TaskGraphError, match="Failed to parse"):
            build_avr_task_graph(tmp_path, "uno", cache)

    def test_invalid_env_name_raises_error(self, tmp_project: Path, cache: Cache) -> None:
        """Non-existent environment should raise TaskGraphError."""
        with pytest.raises(TaskGraphError, match="Failed to parse"):
            build_avr_task_graph(tmp_project, "nonexistent", cache)


# ─── Library Spec Parsing Tests ──────────────────────────────────────────────


class TestParseLibSpec:
    """Tests for _parse_lib_spec()."""

    def test_full_github_url(self) -> None:
        """Full GitHub URL should extract name from path."""
        name, url, version = _parse_lib_spec("https://github.com/FastLED/FastLED")
        assert name == "FastLED"
        assert url == "https://github.com/FastLED/FastLED"
        assert version == "latest"

    def test_github_url_with_git_suffix(self) -> None:
        """GitHub URL with .git suffix should strip it from name."""
        name, url, version = _parse_lib_spec("https://github.com/user/MyLib.git")
        assert name == "MyLib"

    def test_github_archive_url_extracts_version(self) -> None:
        """GitHub archive URL should extract version from path."""
        name, url, version = _parse_lib_spec("https://github.com/user/repo/archive/refs/tags/v1.2.3.tar.gz")
        assert name == "v1.2.3.tar.gz" or name == "repo"  # URL parsing varies
        assert version != "latest"  # Should extract version

    def test_owner_name_at_version(self) -> None:
        """owner/name@version should resolve to GitHub URL."""
        name, url, version = _parse_lib_spec("fastled/FastLED@3.7.8")
        assert name == "FastLED"
        assert version == "3.7.8"
        assert "github.com" in url

    def test_owner_name_at_caret_version(self) -> None:
        """owner/name@^version should strip caret."""
        name, url, version = _parse_lib_spec("fastled/FastLED@^3.7.8")
        assert name == "FastLED"
        assert version == "3.7.8"

    def test_simple_name(self) -> None:
        """Plain library name should return registry-style URL."""
        name, url, version = _parse_lib_spec("SPI")
        assert name == "SPI"
        assert version == "latest"

    def test_simple_name_wire(self) -> None:
        """Plain library name Wire."""
        name, url, version = _parse_lib_spec("Wire")
        assert name == "Wire"
        assert version == "latest"

    def test_symlink_protocol(self) -> None:
        """symlink:// should return local version."""
        name, url, version = _parse_lib_spec("symlink://./libs/mylib")
        assert name == "mylib"
        assert version == "local"

    def test_file_protocol(self) -> None:
        """file:// should return local version."""
        name, url, version = _parse_lib_spec("file:///path/to/lib")
        assert name == "lib"
        assert version == "local"

    def test_relative_path(self) -> None:
        """Relative path should return local version."""
        name, url, version = _parse_lib_spec("../../my_lib")
        assert name == "my_lib"
        assert version == "local"

    def test_owner_name_no_version(self) -> None:
        """owner/name without version should return GitHub URL."""
        name, url, version = _parse_lib_spec("arduino/ArduinoCore-avr")
        assert name == "ArduinoCore-avr"
        assert "github.com" in url
        assert version == "latest"

    def test_whitespace_stripped(self) -> None:
        """Leading/trailing whitespace should be stripped."""
        name, url, version = _parse_lib_spec("  SPI  ")
        assert name == "SPI"


# ─── Platform Detection Tests ────────────────────────────────────────────────


class TestDetectPlatform:
    """Tests for _detect_platform()."""

    def test_atmelavr(self) -> None:
        assert _detect_platform({"platform": "atmelavr"}) == "atmelavr"

    def test_espressif32(self) -> None:
        assert _detect_platform({"platform": "espressif32"}) == "espressif32"

    def test_platformio_shorthand(self) -> None:
        """platformio/espressif32 -> espressif32."""
        assert _detect_platform({"platform": "platformio/espressif32"}) == "espressif32"

    def test_url_based_esp32(self) -> None:
        """URL containing espressif32 should be detected."""
        assert _detect_platform({"platform": "https://github.com/platformio/platform-espressif32.git"}) == "espressif32"

    def test_url_based_atmelavr(self) -> None:
        """URL containing atmelavr should be detected."""
        assert _detect_platform({"platform": "https://github.com/platformio/platform-atmelavr.git"}) == "atmelavr"

    def test_missing_platform_raises_error(self) -> None:
        """Missing platform key should raise TaskGraphError."""
        with pytest.raises(TaskGraphError, match="No 'platform' specified"):
            _detect_platform({"board": "uno"})

    def test_empty_platform_raises_error(self) -> None:
        """Empty platform string should raise TaskGraphError."""
        with pytest.raises(TaskGraphError, match="No 'platform' specified"):
            _detect_platform({"platform": ""})

    def test_case_insensitive(self) -> None:
        """Platform detection should be case-insensitive."""
        assert _detect_platform({"platform": "AtmelAVR"}) == "atmelavr"


# ─── Auto-Detect Platform Tests ──────────────────────────────────────────────


class TestBuildTaskGraph:
    """Tests for build_task_graph() auto-detection."""

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_avr_project_delegates_to_avr_adapter(self, mock_detect: MagicMock, tmp_project: Path, cache: Cache) -> None:
        """build_task_graph should delegate AVR projects to build_avr_task_graph."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_task_graph(tmp_project, "uno", cache)

        assert len(tasks) == 2
        assert any(t.name == "toolchain-atmelavr" for t in tasks)
        assert any(t.name == "framework-arduino-avr" for t in tasks)

    def test_unsupported_platform_raises_error(self, tmp_project_esp32: Path, cache: Cache) -> None:
        """Unsupported platforms should raise TaskGraphError with helpful message."""
        with pytest.raises(TaskGraphError, match="not yet supported"):
            build_task_graph(tmp_project_esp32, "esp32dev", cache)

    def test_missing_ini_raises_error(self, tmp_path: Path, cache: Cache) -> None:
        """Missing platformio.ini should raise TaskGraphError."""
        with pytest.raises(TaskGraphError, match="Failed to parse"):
            build_task_graph(tmp_path, "uno", cache)


# ─── Cache Filtering Tests ───────────────────────────────────────────────────


class TestIsTaskCached:
    """Tests for is_task_cached()."""

    def test_nonexistent_path_is_not_cached(self, tmp_path: Path, cache: Cache) -> None:
        """A task pointing to a nonexistent path is not cached."""
        task = PackageTask(
            name="test-pkg",
            url="https://example.com/pkg.tar.gz",
            version="1.0.0",
            dest_path=str(tmp_path / "nonexistent"),
        )
        assert is_task_cached(task, cache) is False

    def test_empty_directory_is_not_cached(self, tmp_path: Path, cache: Cache) -> None:
        """A task pointing to an empty directory is not cached."""
        empty_dir = tmp_path / "empty"
        empty_dir.mkdir()

        task = PackageTask(
            name="test-pkg",
            url="https://example.com/pkg.tar.gz",
            version="1.0.0",
            dest_path=str(empty_dir),
        )
        assert is_task_cached(task, cache) is False

    def test_populated_directory_is_cached(self, tmp_path: Path, cache: Cache) -> None:
        """A task pointing to a non-empty directory is cached."""
        pkg_dir = tmp_path / "cached_pkg"
        pkg_dir.mkdir()
        (pkg_dir / "some_file.txt").write_text("content")

        task = PackageTask(
            name="test-pkg",
            url="https://example.com/pkg.tar.gz",
            version="1.0.0",
            dest_path=str(pkg_dir),
        )
        assert is_task_cached(task, cache) is True


class TestFilterUncachedTasks:
    """Tests for filter_uncached_tasks()."""

    def test_all_uncached(self, tmp_path: Path, cache: Cache) -> None:
        """All tasks pointing to nonexistent paths are uncached."""
        tasks = [
            PackageTask(name="a", url="https://a.com/a.tar.gz", version="1.0", dest_path=str(tmp_path / "a")),
            PackageTask(name="b", url="https://b.com/b.tar.gz", version="1.0", dest_path=str(tmp_path / "b")),
        ]

        cached, uncached = filter_uncached_tasks(tasks, cache)

        assert len(cached) == 0
        assert len(uncached) == 2

    def test_all_cached(self, tmp_path: Path, cache: Cache) -> None:
        """All tasks pointing to populated dirs are cached."""
        for name in ("a", "b"):
            d = tmp_path / name
            d.mkdir()
            (d / "file.txt").write_text("x")

        tasks = [
            PackageTask(name="a", url="https://a.com/a.tar.gz", version="1.0", dest_path=str(tmp_path / "a")),
            PackageTask(name="b", url="https://b.com/b.tar.gz", version="1.0", dest_path=str(tmp_path / "b")),
        ]

        cached, uncached = filter_uncached_tasks(tasks, cache)

        assert len(cached) == 2
        assert len(uncached) == 0
        # Cached tasks should be marked as DONE
        for task in cached:
            assert task.phase == TaskPhase.DONE
            assert task.status_text == "Cached"

    def test_mixed_cached_and_uncached(self, tmp_path: Path, cache: Cache) -> None:
        """Mixture of cached and uncached tasks."""
        cached_dir = tmp_path / "cached"
        cached_dir.mkdir()
        (cached_dir / "file.txt").write_text("x")

        tasks = [
            PackageTask(name="cached_pkg", url="https://a.com/a.tar.gz", version="1.0", dest_path=str(cached_dir)),
            PackageTask(name="new_pkg", url="https://b.com/b.tar.gz", version="1.0", dest_path=str(tmp_path / "new")),
        ]

        cached, uncached = filter_uncached_tasks(tasks, cache)

        assert len(cached) == 1
        assert cached[0].name == "cached_pkg"
        assert len(uncached) == 1
        assert uncached[0].name == "new_pkg"


# ─── DAG Structure Tests ─────────────────────────────────────────────────────


class TestDagStructure:
    """Tests verifying the DAG structure of task graphs."""

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_avr_dag_has_valid_dependencies(self, mock_detect: MagicMock, tmp_project_with_libs: Path, cache: Cache) -> None:
        """All dependency references in the AVR DAG should point to valid tasks."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project_with_libs, "uno", cache)
        task_names = {t.name for t in tasks}

        for task in tasks:
            for dep in task.dependencies:
                assert dep in task_names, f"Task '{task.name}' depends on unknown task '{dep}'"

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_avr_dag_has_no_cycles(self, mock_detect: MagicMock, tmp_project_with_libs: Path, cache: Cache) -> None:
        """The AVR DAG should pass scheduler validation (no cycles)."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project_with_libs, "uno", cache)

        # Use the scheduler to validate the DAG
        from fbuild.packages.pipeline.scheduler import DependencyScheduler

        scheduler = DependencyScheduler()
        for task in tasks:
            scheduler.add_task(task)
        scheduler.validate()  # Should not raise

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    def test_avr_dag_toolchain_and_framework_are_independent(self, mock_detect: MagicMock, tmp_project: Path, cache: Cache) -> None:
        """Toolchain and framework should be downloadable in parallel (no mutual deps)."""
        mock_detect.return_value = ("https://downloads.arduino.cc/tools/avr-gcc-test.zip", None)

        tasks = build_avr_task_graph(tmp_project, "uno", cache)
        toolchain = next(t for t in tasks if t.name == "toolchain-atmelavr")
        framework = next(t for t in tasks if t.name == "framework-arduino-avr")

        assert "framework-arduino-avr" not in toolchain.dependencies
        assert "toolchain-atmelavr" not in framework.dependencies


# ─── ParallelInstaller Tests ─────────────────────────────────────────────────


class TestParallelInstaller:
    """Tests for the ParallelInstaller public API."""

    def test_instantiation(self) -> None:
        """ParallelInstaller should be instantiable with worker counts."""
        from fbuild.packages.pipeline import ParallelInstaller

        installer = ParallelInstaller(
            download_workers=4,
            unpack_workers=2,
            install_workers=2,
        )
        assert installer._download_workers == 4
        assert installer._unpack_workers == 2
        assert installer._install_workers == 2

    @patch("fbuild.packages.pipeline.adapters._detect_avr_package_filename")
    @patch("fbuild.packages.pipeline.filter_uncached_tasks")
    @patch("fbuild.packages.pipeline.build_task_graph")
    def test_all_cached_returns_immediately(
        self,
        mock_build: MagicMock,
        mock_filter: MagicMock,
        mock_detect: MagicMock,
        tmp_project: Path,
    ) -> None:
        """When all tasks are cached, install_dependencies should return without running pipeline."""
        from fbuild.packages.pipeline import ParallelInstaller

        mock_detect.return_value = ("https://downloads.arduino.cc/tools/test.zip", None)

        cached_task = PackageTask(
            name="toolchain-atmelavr",
            url="https://downloads.arduino.cc/tools/test.zip",
            version="1.0",
            dest_path="/tmp/toolchain",
        )
        cached_task.phase = TaskPhase.DONE
        cached_task.status_text = "Cached"

        mock_build.return_value = [cached_task]
        mock_filter.return_value = ([cached_task], [])

        installer = ParallelInstaller(download_workers=4, unpack_workers=2, install_workers=2)
        result = installer.install_dependencies(
            project_path=tmp_project,
            env_name="uno",
            verbose=False,
            use_tui=False,
        )

        assert result.success
        assert result.total_elapsed == 0.0
        assert len(result.tasks) == 1

    def test_empty_task_graph_succeeds(self, tmp_path: Path) -> None:
        """Empty task graph should return success immediately."""
        from fbuild.packages.pipeline import ParallelInstaller

        with patch("fbuild.packages.pipeline.build_task_graph", return_value=[]):
            installer = ParallelInstaller(download_workers=4, unpack_workers=2, install_workers=2)
            result = installer.install_dependencies(
                project_path=tmp_path,
                env_name="uno",
                verbose=False,
                use_tui=False,
            )

        assert result.success
        assert len(result.tasks) == 0
