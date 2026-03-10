"""
Unit tests for the compile-all-framework-libraries approach in OrchestratorESP32.

Tests the logic in _compile_all_framework_libraries and _setup_framework_library:
- Enumeration of all framework libraries (not just detected ones)
- Deduplication against already-compiled lib_deps archives
- Header-only library handling
- Inter-library include path injection
- _setup_framework_library source copying and metadata creation
"""

import json
from pathlib import Path
from typing import List, Optional
from unittest.mock import MagicMock, patch

from fbuild.build.orchestrator_esp32 import FrameworkLibraryResult, OrchestratorESP32
from fbuild.packages.library_manager_esp32 import LibraryESP32

# =============================================================================
# Helpers
# =============================================================================


def _create_framework_libraries(tmp_path: Path, libs: dict[str, list[str]]) -> Path:
    """Create a mock framework libraries directory.

    Args:
        tmp_path: pytest tmp_path fixture
        libs: Mapping of library_name -> list of source filenames in src/
              Use ".h" extension for header-only libs, ".cpp" for compilable ones.

    Returns:
        Path to the mock libraries/ directory
    """
    libs_dir = tmp_path / "framework" / "libraries"
    for lib_name, files in libs.items():
        src_dir = libs_dir / lib_name / "src"
        src_dir.mkdir(parents=True, exist_ok=True)
        for filename in files:
            filepath = src_dir / filename
            filepath.parent.mkdir(parents=True, exist_ok=True)
            filepath.write_text(f"// {filename}\n")
    return libs_dir


def _make_orchestrator() -> OrchestratorESP32:
    """Create an OrchestratorESP32 with a mock cache."""
    cache = MagicMock()
    return OrchestratorESP32(cache, verbose=False)


def _make_framework_mock(libraries_dir: Path) -> MagicMock:
    """Create a mock FrameworkESP32 that returns the given libraries dir."""
    fw = MagicMock()
    fw.get_libraries_dir.return_value = libraries_dir
    return fw


def _make_toolchain_mock(bin_path: Optional[Path] = None) -> MagicMock:
    """Create a mock ToolchainESP32."""
    tc = MagicMock()
    if bin_path is None:
        bin_path = Path("/mock/toolchain/bin")
    tc.get_bin_path.return_value = bin_path
    return tc


def _make_compiler_mock(base_flags: Optional[List[str]] = None, include_paths: Optional[List[Path]] = None) -> MagicMock:
    """Create a mock ConfigurableCompiler."""
    comp = MagicMock()
    comp.get_base_flags.return_value = base_flags if base_flags is not None else ["-Os"]
    comp.get_include_paths.return_value = include_paths if include_paths is not None else []
    # No compilation_executor by default
    comp.compilation_executor = None
    return comp


# =============================================================================
# Test: _setup_framework_library
# =============================================================================


class TestSetupFrameworkLibrary:
    """Tests for _setup_framework_library static method."""

    def test_copies_src_dir(self, tmp_path: Path) -> None:
        """Source files from framework library src/ are copied to build lib dir."""
        # Create framework library with src/
        fw_root = tmp_path / "framework_lib"
        fw_src = fw_root / "src"
        fw_src.mkdir(parents=True)
        (fw_src / "MyLib.h").write_text("#pragma once\nvoid foo();\n")
        (fw_src / "MyLib.cpp").write_text("void foo() {}\n")

        # Create target library
        lib_dir = tmp_path / "build" / "libs" / "mylib"
        library = LibraryESP32(lib_dir, "mylib")

        OrchestratorESP32._setup_framework_library(library, fw_root)

        assert library.src_dir.exists()
        assert (library.src_dir / "MyLib.h").exists()
        assert (library.src_dir / "MyLib.cpp").exists()
        assert (library.src_dir / "MyLib.h").read_text() == "#pragma once\nvoid foo();\n"

    def test_creates_library_json_metadata(self, tmp_path: Path) -> None:
        """library.json metadata file is created with correct fields."""
        fw_root = tmp_path / "framework_lib"
        fw_src = fw_root / "src"
        fw_src.mkdir(parents=True)
        (fw_src / "Lib.h").write_text("// header\n")

        lib_dir = tmp_path / "build" / "libs" / "mylib"
        library = LibraryESP32(lib_dir, "mylib")

        OrchestratorESP32._setup_framework_library(library, fw_root)

        assert library.info_file.exists()
        metadata = json.loads(library.info_file.read_text())
        assert metadata["name"] == "mylib"
        assert metadata["source"] == "framework-builtin"
        assert "arduino" in metadata["frameworks"]

    def test_overwrites_existing_src_dir(self, tmp_path: Path) -> None:
        """If src/ already exists in build dir, it is replaced with fresh copy."""
        fw_root = tmp_path / "framework_lib"
        fw_src = fw_root / "src"
        fw_src.mkdir(parents=True)
        (fw_src / "New.h").write_text("// new\n")

        lib_dir = tmp_path / "build" / "libs" / "mylib"
        library = LibraryESP32(lib_dir, "mylib")

        # Pre-populate with stale content
        library.src_dir.mkdir(parents=True)
        (library.src_dir / "Old.h").write_text("// stale\n")

        OrchestratorESP32._setup_framework_library(library, fw_root)

        assert (library.src_dir / "New.h").exists()
        assert not (library.src_dir / "Old.h").exists()

    def test_copies_subdirectories_recursively(self, tmp_path: Path) -> None:
        """Subdirectories within src/ are copied recursively."""
        fw_root = tmp_path / "framework_lib"
        fw_src = fw_root / "src"
        sub = fw_src / "hal"
        sub.mkdir(parents=True)
        (fw_src / "Lib.h").write_text("// top\n")
        (sub / "hal_impl.c").write_text("// hal\n")

        lib_dir = tmp_path / "build" / "libs" / "mylib"
        library = LibraryESP32(lib_dir, "mylib")

        OrchestratorESP32._setup_framework_library(library, fw_root)

        assert (library.src_dir / "hal" / "hal_impl.c").exists()

    def test_falls_back_to_root_when_no_src_dir(self, tmp_path: Path) -> None:
        """If framework lib has no src/ subdir, copies from root instead."""
        fw_root = tmp_path / "framework_lib"
        fw_root.mkdir(parents=True)
        # Files directly in root, no src/ subdirectory
        (fw_root / "Lib.h").write_text("// root header\n")
        (fw_root / "Lib.cpp").write_text("// root source\n")

        lib_dir = tmp_path / "build" / "libs" / "mylib"
        library = LibraryESP32(lib_dir, "mylib")

        OrchestratorESP32._setup_framework_library(library, fw_root)

        assert library.src_dir.exists()
        assert (library.src_dir / "Lib.h").exists()
        assert (library.src_dir / "Lib.cpp").exists()

    def test_library_exists_after_setup(self, tmp_path: Path) -> None:
        """After setup, LibraryESP32.exists returns True (lib_dir, src_dir, info_file all exist)."""
        fw_root = tmp_path / "framework_lib"
        fw_src = fw_root / "src"
        fw_src.mkdir(parents=True)
        (fw_src / "Lib.h").write_text("// h\n")

        lib_dir = tmp_path / "build" / "libs" / "mylib"
        library = LibraryESP32(lib_dir, "mylib")

        OrchestratorESP32._setup_framework_library(library, fw_root)

        assert library.exists


# =============================================================================
# Test: Library enumeration logic
# =============================================================================


class TestFrameworkLibraryEnumeration:
    """Tests for the enumeration/filtering logic inside _compile_all_framework_libraries.

    These test the actual method with mocked compilation dependencies.
    """

    def _call_compile_all(
        self,
        orchestrator: OrchestratorESP32,
        libraries_dir: Path,
        build_dir: Path,
        existing_archives: Optional[List[Path]] = None,
    ) -> FrameworkLibraryResult:
        """Call _compile_all_framework_libraries with mocked compilation layer.

        Mocks LibraryManagerESP32 and toolchain so no real compilation happens.
        Returns a FrameworkLibraryResult.
        """
        framework = _make_framework_mock(libraries_dir)
        toolchain = _make_toolchain_mock()
        compiler = _make_compiler_mock()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            # Always say rebuild needed so we exercise the full path
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            # prepare_compile_jobs returns a fake job list (non-empty = has sources)
            mock_mgr.prepare_compile_jobs.return_value = []

            return orchestrator._compile_all_framework_libraries(
                framework,
                build_dir,
                existing_archives or [],
                toolchain,
                compiler,
                False,
            )

    def test_enumerates_all_libraries_with_src(self, tmp_path: Path) -> None:
        """All library directories containing src/ are enumerated."""
        libs_dir = _create_framework_libraries(
            tmp_path,
            {
                "BLE": ["BLEDevice.cpp", "BLEDevice.h"],
                "WiFi": ["WiFi.cpp", "WiFi.h"],
                "SPI": ["SPI.cpp", "SPI.h"],
            },
        )
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        orchestrator = _make_orchestrator()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            # Return empty list (no actual compilation needed for enumeration test)
            mock_mgr.prepare_compile_jobs.return_value = []

            orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                [],
                _make_toolchain_mock(),
                _make_compiler_mock(),
                False,
            )

            # All 3 libraries should have prepare_compile_jobs called
            assert mock_mgr.prepare_compile_jobs.call_count == 3

    def test_skips_libraries_without_src(self, tmp_path: Path) -> None:
        """Library directories without src/ subdirectory are skipped."""
        libs_dir = tmp_path / "framework" / "libraries"

        # Library WITH src/
        (libs_dir / "Good" / "src").mkdir(parents=True)
        (libs_dir / "Good" / "src" / "Good.cpp").write_text("// good\n")

        # Library WITHOUT src/ (files directly in root)
        (libs_dir / "NoSrc").mkdir(parents=True)
        (libs_dir / "NoSrc" / "NoSrc.cpp").write_text("// no src\n")

        build_dir = tmp_path / "build"
        build_dir.mkdir()

        orchestrator = _make_orchestrator()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            mock_mgr.prepare_compile_jobs.return_value = []

            orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                [],
                _make_toolchain_mock(),
                _make_compiler_mock(),
                False,
            )

            # Only "Good" should have prepare_compile_jobs called
            assert mock_mgr.prepare_compile_jobs.call_count == 1

    def test_skips_dot_prefixed_directories(self, tmp_path: Path) -> None:
        """Directories starting with '.' are skipped."""
        libs_dir = tmp_path / "framework" / "libraries"

        (libs_dir / "Real" / "src").mkdir(parents=True)
        (libs_dir / "Real" / "src" / "Real.cpp").write_text("// real\n")

        (libs_dir / ".git" / "src").mkdir(parents=True)
        (libs_dir / ".git" / "src" / "Git.cpp").write_text("// git\n")

        build_dir = tmp_path / "build"
        build_dir.mkdir()

        orchestrator = _make_orchestrator()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            mock_mgr.prepare_compile_jobs.return_value = []

            orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                [],
                _make_toolchain_mock(),
                _make_compiler_mock(),
                False,
            )

            assert mock_mgr.prepare_compile_jobs.call_count == 1

    def test_nonexistent_libraries_dir_returns_empty(self, tmp_path: Path) -> None:
        """If framework libraries dir doesn't exist, returns empty lists."""
        orchestrator = _make_orchestrator()
        framework = _make_framework_mock(tmp_path / "nonexistent")

        result = orchestrator._compile_all_framework_libraries(
            framework,
            tmp_path / "build",
            [],
            _make_toolchain_mock(),
            _make_compiler_mock(),
            False,
        )

        assert result.archives == []
        assert result.include_paths == []

    def test_toolchain_bin_none_returns_empty(self, tmp_path: Path) -> None:
        """If toolchain.get_bin_path() returns None, returns empty lists."""
        libs_dir = _create_framework_libraries(tmp_path, {"BLE": ["BLE.cpp"]})
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        orchestrator = _make_orchestrator()
        toolchain = MagicMock()
        toolchain.get_bin_path.return_value = None

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32"):
            result = orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                [],
                toolchain,
                _make_compiler_mock(),
                False,
            )

        assert result.archives == []
        assert result.include_paths == []


# =============================================================================
# Test: Deduplication against existing archives
# =============================================================================


class TestDeduplicationAgainstExistingArchives:
    """Tests that libraries already compiled from lib_deps are not re-compiled."""

    def test_skips_library_matching_existing_archive(self, tmp_path: Path) -> None:
        """A framework library whose sanitized name matches an existing archive is skipped."""
        libs_dir = _create_framework_libraries(
            tmp_path,
            {
                "WiFi": ["WiFi.cpp"],
                "BLE": ["BLE.cpp"],
            },
        )
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        # Simulate "libwifi.a" already compiled from lib_deps
        existing = [Path("/some/build/libs/wifi/libwifi.a")]

        orchestrator = _make_orchestrator()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            mock_mgr.prepare_compile_jobs.return_value = []

            orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                existing,
                _make_toolchain_mock(),
                _make_compiler_mock(),
                False,
            )

            # Only BLE should have prepare_compile_jobs called (WiFi matches existing "libwifi.a")
            assert mock_mgr.prepare_compile_jobs.call_count == 1

    def test_deduplication_is_case_insensitive(self, tmp_path: Path) -> None:
        """Deduplication normalizes to lowercase: 'BLE' matches 'libble.a'."""
        libs_dir = _create_framework_libraries(tmp_path, {"BLE": ["BLE.cpp"]})
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        existing = [Path("/build/libs/ble/libble.a")]

        orchestrator = _make_orchestrator()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")

            orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                existing,
                _make_toolchain_mock(),
                _make_compiler_mock(),
                False,
            )

            # BLE should be skipped (matches "libble.a")
            assert mock_mgr.prepare_compile_jobs.call_count == 0

    def test_no_dedup_when_archive_name_doesnt_start_with_lib(self, tmp_path: Path) -> None:
        """Archives not starting with 'lib' prefix are ignored for dedup."""
        libs_dir = _create_framework_libraries(tmp_path, {"WiFi": ["WiFi.cpp"]})
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        # Archive without "lib" prefix — shouldn't match anything
        existing = [Path("/build/wifi.a")]

        orchestrator = _make_orchestrator()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            mock_mgr.prepare_compile_jobs.return_value = []

            orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                existing,
                _make_toolchain_mock(),
                _make_compiler_mock(),
                False,
            )

            # WiFi should still have prepare_compile_jobs called
            assert mock_mgr.prepare_compile_jobs.call_count == 1


# =============================================================================
# Test: Header-only library handling
# =============================================================================


class TestHeaderOnlyLibraries:
    """Tests that header-only libraries are handled correctly."""

    def test_header_only_lib_skips_compilation(self, tmp_path: Path) -> None:
        """Libraries with only .h files (no .cpp/.c) skip compilation."""
        libs_dir = _create_framework_libraries(
            tmp_path,
            {
                # Header-only: only .h files
                "HeaderOnly": ["HeaderOnly.h"],
            },
        )
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        orchestrator = _make_orchestrator()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            # Header-only: prepare_compile_jobs returns empty list
            mock_mgr.prepare_compile_jobs.return_value = []

            orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                [],
                _make_toolchain_mock(),
                _make_compiler_mock(),
                False,
            )

            # prepare_compile_jobs is called but returns empty (header-only)
            mock_mgr.prepare_compile_jobs.assert_called_once()
            # archive_library should NOT be called (no objects to archive)
            mock_mgr.archive_library.assert_not_called()

    def test_header_only_lib_still_adds_include_paths(self, tmp_path: Path) -> None:
        """Header-only libraries still contribute include paths."""
        libs_dir = _create_framework_libraries(
            tmp_path,
            {
                "HeaderOnly": ["HeaderOnly.h"],
            },
        )
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        orchestrator = _make_orchestrator()
        framework = _make_framework_mock(libs_dir)
        toolchain = _make_toolchain_mock()
        compiler = _make_compiler_mock()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            # Header-only: prepare_compile_jobs returns empty list
            mock_mgr.prepare_compile_jobs.return_value = []

            result = orchestrator._compile_all_framework_libraries(
                framework,
                build_dir,
                [],
                toolchain,
                compiler,
                False,
            )

            # Include paths should be non-empty (from the header-only lib)
            assert len(result.include_paths) > 0


# =============================================================================
# Test: Inter-library include paths (bug fix validation)
# =============================================================================


class TestInterLibraryIncludePaths:
    """Tests that framework library src/ dirs are added to include paths
    before compilation, so inter-library #includes resolve correctly.

    This validates the fix for the bug where BLE couldn't find <NetworkClient.h>
    because Network's src/ wasn't in the include paths during BLE compilation.
    """

    def test_all_framework_src_dirs_in_include_paths(self, tmp_path: Path) -> None:
        """prepare_compile_jobs receives include paths containing all framework library src/ dirs."""
        libs_dir = _create_framework_libraries(
            tmp_path,
            {
                "BLE": ["BLEDevice.cpp", "BLEDevice.h"],
                "Network": ["NetworkClient.cpp", "NetworkClient.h"],
                "WiFi": ["WiFi.cpp", "WiFi.h"],
            },
        )
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        orchestrator = _make_orchestrator()
        framework = _make_framework_mock(libs_dir)
        toolchain = _make_toolchain_mock()
        compiler = _make_compiler_mock()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            mock_mgr.prepare_compile_jobs.return_value = []

            orchestrator._compile_all_framework_libraries(
                framework,
                build_dir,
                [],
                toolchain,
                compiler,
                False,
            )

            # Every prepare_compile_jobs call should have include_paths containing
            # all three framework library src/ directories
            expected_src_dirs = {
                libs_dir / "BLE" / "src",
                libs_dir / "Network" / "src",
                libs_dir / "WiFi" / "src",
            }

            for call_args in mock_mgr.prepare_compile_jobs.call_args_list:
                # include_paths is the 4th positional arg (index 3)
                include_paths = call_args[0][3] if len(call_args[0]) > 3 else call_args[1].get("include_paths", [])
                include_paths_set = set(include_paths)
                for expected_dir in expected_src_dirs:
                    assert expected_dir in include_paths_set, f"Expected {expected_dir} in include_paths for prepare_compile_jobs call, but got: {include_paths}"

    def test_compiler_include_paths_preserved(self, tmp_path: Path) -> None:
        """Original compiler include paths are preserved alongside framework lib paths."""
        libs_dir = _create_framework_libraries(tmp_path, {"SPI": ["SPI.cpp"]})
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        original_include = Path("/mock/sdk/include")

        orchestrator = _make_orchestrator()
        framework = _make_framework_mock(libs_dir)
        toolchain = _make_toolchain_mock()
        compiler = _make_compiler_mock(include_paths=[original_include])

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            mock_mgr.prepare_compile_jobs.return_value = []

            orchestrator._compile_all_framework_libraries(
                framework,
                build_dir,
                [],
                toolchain,
                compiler,
                False,
            )

            # include_paths passed to prepare_compile_jobs should contain the original SDK include
            call_args = mock_mgr.prepare_compile_jobs.call_args
            include_paths = call_args[0][3] if len(call_args[0]) > 3 else call_args[1].get("include_paths", [])
            assert original_include in include_paths


# =============================================================================
# Test: Cached (no-rebuild) path
# =============================================================================


class TestCachedLibraryPath:
    """Tests behavior when libraries are already compiled (needs_rebuild=False)."""

    def test_cached_library_not_recompiled(self, tmp_path: Path) -> None:
        """When needs_rebuild returns False, prepare_compile_jobs is not called."""
        libs_dir = _create_framework_libraries(tmp_path, {"WiFi": ["WiFi.cpp", "WiFi.h"]})
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        orchestrator = _make_orchestrator()

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            # Already compiled, no rebuild needed
            mock_mgr.needs_rebuild.return_value = (False, "up to date")

            orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                [],
                _make_toolchain_mock(),
                _make_compiler_mock(),
                False,
            )

            mock_mgr.prepare_compile_jobs.assert_not_called()


# =============================================================================
# Test: Compilation failure resilience
# =============================================================================


class TestCompilationFailureResilience:
    """Tests that prepare_compile_jobs is called for all libraries even if compilation fails."""

    def test_all_libraries_prepared_even_when_setup_fails(self, tmp_path: Path) -> None:
        """prepare_compile_jobs is called for both libs; setup failure is caught."""
        libs_dir = _create_framework_libraries(
            tmp_path,
            {
                "AAA_Broken": ["Broken.cpp"],
                "ZZZ_Good": ["Good.cpp"],
            },
        )
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        orchestrator = _make_orchestrator()

        prepare_calls: list[str] = []

        def mock_prepare(library, *_args, **_kwargs):
            prepare_calls.append(library.name)
            return []  # Return empty (no actual compilation needed for this test)

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            mock_mgr.prepare_compile_jobs.side_effect = mock_prepare

            orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                [],
                _make_toolchain_mock(),
                _make_compiler_mock(),
                False,
            )

            # Both libraries should have prepare_compile_jobs called
            assert len(prepare_calls) == 2
            assert "aaa_broken" in prepare_calls
            assert "zzz_good" in prepare_calls


# =============================================================================
# Test: Sorted enumeration order
# =============================================================================


class TestEnumerationOrder:
    """Tests that libraries are enumerated in sorted order for deterministic builds."""

    def test_libraries_prepared_in_sorted_order(self, tmp_path: Path) -> None:
        """Libraries have prepare_compile_jobs called in alphabetical order."""
        libs_dir = _create_framework_libraries(
            tmp_path,
            {
                "Wire": ["Wire.cpp"],
                "BLE": ["BLE.cpp"],
                "SPI": ["SPI.cpp"],
            },
        )
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        orchestrator = _make_orchestrator()
        prepare_order: list[str] = []

        def track_prepare(library, *_args, **_kwargs):
            prepare_order.append(library.name)
            return []  # Return empty (no actual compilation needed for order test)

        with patch("fbuild.build.orchestrator_esp32.LibraryManagerESP32") as MockLibMgr:
            mock_mgr = MockLibMgr.return_value
            mock_mgr.libs_dir = build_dir / "libs"
            mock_mgr.libs_dir.mkdir(parents=True, exist_ok=True)
            mock_mgr.needs_rebuild.return_value = (True, "no archive")
            mock_mgr.prepare_compile_jobs.side_effect = track_prepare

            orchestrator._compile_all_framework_libraries(
                _make_framework_mock(libs_dir),
                build_dir,
                [],
                _make_toolchain_mock(),
                _make_compiler_mock(),
                False,
            )

            assert prepare_order == ["ble", "spi", "wire"]
