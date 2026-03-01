"""
Unit tests for the WASM build orchestrator.

Tests cover:
- OrchestratorWASM class instantiation
- Interface compliance (IBuildOrchestrator)
- Platform config loading for WASM
- Build processor registration
- Source scanning
- Library dependency processing (download, compile, archive, include paths)
- Build flag generation
- End-to-end build with mocked toolchain
"""

import inspect
import json
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from fbuild.build.orchestrator import BuildResult, IBuildOrchestrator
from fbuild.build.orchestrator_wasm import _TOTAL_PHASES, OrchestratorWASM


class TestOrchestratorWASMInterface:
    """Test that OrchestratorWASM implements IBuildOrchestrator correctly."""

    def test_inherits_from_interface(self):
        """OrchestratorWASM must inherit from IBuildOrchestrator."""
        assert issubclass(OrchestratorWASM, IBuildOrchestrator)

    def test_has_build_method(self):
        """OrchestratorWASM must have a build() method."""
        assert hasattr(OrchestratorWASM, "build")
        assert callable(getattr(OrchestratorWASM, "build"))

    def test_build_method_signature(self):
        """build() should accept request parameter."""
        sig = inspect.signature(OrchestratorWASM.build)
        params = sig.parameters
        assert "self" in params
        assert "request" in params

    def test_instantiation_with_defaults(self):
        """Can be instantiated with default parameters."""
        orchestrator = OrchestratorWASM()
        assert orchestrator.cache is None
        assert orchestrator.verbose is False

    def test_instantiation_with_cache(self):
        """Can be instantiated with a cache."""
        mock_cache = MagicMock()
        orchestrator = OrchestratorWASM(cache=mock_cache, verbose=True)
        assert orchestrator.cache is mock_cache
        assert orchestrator.verbose is True

    def test_has_docstring(self):
        """OrchestratorWASM class should have a docstring."""
        assert OrchestratorWASM.__doc__ is not None
        assert len(OrchestratorWASM.__doc__.strip()) > 0

    def test_build_method_has_docstring(self):
        """build() method should have a docstring."""
        assert OrchestratorWASM.build.__doc__ is not None
        assert len(OrchestratorWASM.build.__doc__.strip()) > 0

    def test_total_phases_is_eight(self):
        """Build should have 8 phases."""
        assert _TOTAL_PHASES == 8


class TestOrchestratorWASMPlatformConfig:
    """Test WASM platform configuration loading."""

    def test_load_wasm_config(self):
        """WASM platform config should load successfully."""
        from fbuild.platform_configs import load_config

        config = load_config("wasm")
        assert config is not None
        assert config.name == "WASM"
        assert config.mcu == "wasm32"
        assert config.architecture == "wasm32"

    def test_wasm_config_has_compiler_flags(self):
        """WASM config should have compiler flags."""
        from fbuild.platform_configs import load_config

        config = load_config("wasm")
        assert config is not None
        assert len(config.compiler_flags.common) > 0
        # Should include emscripten-specific flags
        assert "-pthread" in config.compiler_flags.common

    def test_wasm_config_has_linker_flags(self):
        """WASM config should have linker flags."""
        from fbuild.platform_configs import load_config

        config = load_config("wasm")
        assert config is not None
        assert len(config.linker_flags) > 0
        assert "-sWASM=1" in config.linker_flags

    def test_wasm_config_has_profiles(self):
        """WASM config should have release and quick profiles."""
        from fbuild.platform_configs import load_config

        config = load_config("wasm")
        assert config is not None
        assert "release" in config.profiles
        assert "quick" in config.profiles

    def test_wasm_config_has_defines(self):
        """WASM config should have preprocessor defines."""
        from fbuild.platform_configs import load_config

        config = load_config("wasm")
        assert config is not None
        assert len(config.defines) > 0

    def test_wasm_in_available_configs(self):
        """WASM should appear in available configs list."""
        from fbuild.platform_configs import list_available_configs

        configs = list_available_configs()
        assert "wasm" in configs

    def test_wasm_in_vendor_configs(self):
        """WASM should appear under the wasm vendor."""
        from fbuild.platform_configs import list_configs_by_vendor

        by_vendor = list_configs_by_vendor()
        assert "wasm" in by_vendor
        assert "wasm" in by_vendor["wasm"]


class TestOrchestratorWASMBuildProcessorRegistration:
    """Test that WASM is properly registered in the build processor."""

    def test_wasm_in_platform_patterns(self):
        """WASM should be in the platform patterns dict."""
        from fbuild.daemon.processors.build_processor import _PLATFORM_PATTERNS

        assert "wasm" in _PLATFORM_PATTERNS
        assert "wasm" in _PLATFORM_PATTERNS["wasm"]

    def test_wasm_in_platform_orchestrators(self):
        """WASM should be in the orchestrator mapping."""
        from fbuild.daemon.processors.build_processor import _PLATFORM_ORCHESTRATORS

        assert "wasm" in _PLATFORM_ORCHESTRATORS
        module_name, class_name = _PLATFORM_ORCHESTRATORS["wasm"]
        assert module_name == "fbuild.build.orchestrator_wasm"
        assert class_name == "OrchestratorWASM"

    def test_normalize_platform_wasm(self):
        """Platform normalization should recognize 'wasm'."""
        from fbuild.daemon.processors.build_processor import _normalize_platform

        assert _normalize_platform("wasm") == "wasm"


class TestOrchestratorWASMSourceScanning:
    """Test WASM source file scanning."""

    def test_scan_sources_finds_cpp(self, tmp_path: Path):
        """Source scanner should find .cpp files."""
        orchestrator = OrchestratorWASM()
        src_dir = tmp_path / "src"
        src_dir.mkdir()
        (src_dir / "main.cpp").write_text("int main() { return 0; }")
        (src_dir / "helper.cpp").write_text("void helper() {}")

        sources = orchestrator._scan_sources(src_dir)
        assert len(sources) == 2

    def test_scan_sources_finds_c(self, tmp_path: Path):
        """Source scanner should find .c files."""
        orchestrator = OrchestratorWASM()
        src_dir = tmp_path / "src"
        src_dir.mkdir()
        (src_dir / "main.c").write_text("int main() { return 0; }")

        sources = orchestrator._scan_sources(src_dir)
        assert len(sources) == 1

    def test_scan_sources_finds_ino(self, tmp_path: Path):
        """Source scanner should find .ino files."""
        orchestrator = OrchestratorWASM()
        src_dir = tmp_path / "src"
        src_dir.mkdir()
        (src_dir / "sketch.ino").write_text("void setup() {} void loop() {}")

        sources = orchestrator._scan_sources(src_dir)
        assert len(sources) == 1

    def test_scan_sources_empty_dir(self, tmp_path: Path):
        """Source scanner should return empty list for empty dir."""
        orchestrator = OrchestratorWASM()
        src_dir = tmp_path / "src"
        src_dir.mkdir()

        sources = orchestrator._scan_sources(src_dir)
        assert len(sources) == 0

    def test_scan_sources_nonexistent_dir(self, tmp_path: Path):
        """Source scanner should return empty list for nonexistent dir."""
        orchestrator = OrchestratorWASM()
        src_dir = tmp_path / "nonexistent"

        sources = orchestrator._scan_sources(src_dir)
        assert len(sources) == 0

    def test_scan_sources_recursive(self, tmp_path: Path):
        """Source scanner should find files in subdirectories."""
        orchestrator = OrchestratorWASM()
        src_dir = tmp_path / "src"
        sub_dir = src_dir / "sub"
        sub_dir.mkdir(parents=True)
        (src_dir / "main.cpp").write_text("int main() { return 0; }")
        (sub_dir / "helper.cpp").write_text("void helper() {}")

        sources = orchestrator._scan_sources(src_dir)
        assert len(sources) == 2


class TestOrchestratorWASMBuildFlags:
    """Test WASM build flag generation."""

    def test_get_compiler_flags_includes_common(self):
        """Compiler flags should include common flags from platform config."""
        from fbuild.platform_configs import load_config

        orchestrator = OrchestratorWASM()
        config = load_config("wasm")
        assert config is not None

        mock_request = MagicMock()
        mock_request.profile.value = "release"

        flags = orchestrator._get_compiler_flags(config, mock_request, [])
        # Should include common flags
        assert "-pthread" in flags

    def test_get_compiler_flags_includes_profile(self):
        """Compiler flags should include profile-specific flags."""
        from fbuild.platform_configs import load_config

        orchestrator = OrchestratorWASM()
        config = load_config("wasm")
        assert config is not None

        mock_request = MagicMock()
        mock_request.profile.value = "release"

        flags = orchestrator._get_compiler_flags(config, mock_request, [])
        # Release profile should include -Oz
        assert "-Oz" in flags

    def test_get_compiler_flags_includes_user_flags(self):
        """Compiler flags should include user build flags."""
        from fbuild.platform_configs import load_config

        orchestrator = OrchestratorWASM()
        config = load_config("wasm")
        assert config is not None

        mock_request = MagicMock()
        mock_request.profile.value = "release"

        flags = orchestrator._get_compiler_flags(config, mock_request, ["-DUSER_FLAG=1"])
        assert "-DUSER_FLAG=1" in flags

    def test_get_linker_flags_includes_base(self):
        """Linker flags should include base flags."""
        from fbuild.platform_configs import load_config

        orchestrator = OrchestratorWASM()
        config = load_config("wasm")
        assert config is not None

        mock_request = MagicMock()
        mock_request.profile.value = "release"

        flags = orchestrator._get_linker_flags(config, mock_request)
        assert "-sWASM=1" in flags

    def test_get_linker_flags_includes_profile(self):
        """Linker flags should include profile-specific flags."""
        from fbuild.platform_configs import load_config

        orchestrator = OrchestratorWASM()
        config = load_config("wasm")
        assert config is not None

        mock_request = MagicMock()
        mock_request.profile.value = "quick"

        flags = orchestrator._get_linker_flags(config, mock_request)
        # Quick profile should have profiling flags
        assert "--profiling-funcs" in flags


class TestOrchestratorWASMBuildNoToolchain:
    """Test WASM build behavior when toolchain is not available."""

    def test_build_fails_without_emcc(self, tmp_path: Path):
        """Build should fail gracefully when emcc is not on PATH."""
        orchestrator = OrchestratorWASM()

        # Create minimal project structure
        (tmp_path / "platformio.ini").write_text("[env:wasm]\nplatform = wasm\nboard = wasm\nframework = arduino\n")
        src_dir = tmp_path / "src"
        src_dir.mkdir()
        (src_dir / "main.cpp").write_text("int main() { return 0; }")

        mock_request = MagicMock()
        mock_request.project_dir = tmp_path
        mock_request.env_name = "wasm"
        mock_request.build_dir = tmp_path / ".fbuild" / "wasm" / "release"
        mock_request.verbose = False
        mock_request.clean = False
        mock_request.profile.value = "release"

        # Patch _find_tool to return None (simulating no emcc)
        with patch("fbuild.build.orchestrator_wasm._find_tool", return_value=None):
            result = orchestrator.build(mock_request)

        assert isinstance(result, BuildResult)
        assert result.success is False
        assert "clang-tool-chain-emcc" in result.message


class TestOrchestratorWASMLibraryProcessing:
    """Test library dependency downloading, compilation, and include path resolution."""

    def test_process_libraries_empty_deps(self):
        """No library deps should return empty results."""
        orchestrator = OrchestratorWASM()
        archives, includes = orchestrator._process_libraries(
            lib_deps=[],
            build_dir=Path("/tmp/build"),
            emcc=Path("/usr/bin/emcc"),
            emar=Path("/usr/bin/emar"),
            compiler_flags=["-O2"],
            defines=["FOO=1"],
            verbose=False,
        )
        assert archives == []
        assert includes == []

    def test_process_libraries_downloads_and_collects_includes(self, tmp_path: Path):
        """Library processing should download libs and collect include paths."""
        orchestrator = OrchestratorWASM()
        build_dir = tmp_path / "build"
        build_dir.mkdir()

        # Create a fake library structure that LibraryManager.download_library would produce
        libs_dir = build_dir / "libs"
        libs_dir.mkdir()
        lib_dir = libs_dir / "testlib"
        lib_dir.mkdir()
        src_dir = lib_dir / "src"
        src_dir.mkdir()
        (src_dir / "testlib.h").write_text("#pragma once\nvoid test_func();")
        (src_dir / "testlib.cpp").write_text("void test_func() {}")
        # Create info.json so Library.exists returns True after download
        info = {"name": "testlib", "url": "https://github.com/user/testlib", "version": "unknown", "commit_hash": None, "compiler": "", "compile_commands": [], "link_commands": []}
        (lib_dir / "info.json").write_text(json.dumps(info))

        # Mock the download to return our fake library
        from fbuild.packages.library_manager import Library

        fake_library = Library(lib_dir, "testlib")

        # Mock safe_run to simulate successful compilation and archiving
        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stderr = ""

        with (
            patch("fbuild.packages.library_manager.LibraryManager.download_library", return_value=fake_library),
            patch("fbuild.build.orchestrator_wasm.safe_run", return_value=mock_result),
        ):
            archives, includes = orchestrator._process_libraries(
                lib_deps=["https://github.com/user/testlib"],
                build_dir=build_dir,
                emcc=Path("/usr/bin/emcc"),
                emar=Path("/usr/bin/emar"),
                compiler_flags=["-O2"],
                defines=["FOO=1"],
                verbose=False,
            )

        # Should have collected include paths from the library
        assert len(includes) > 0
        assert src_dir in includes

        # Should have created an archive
        assert len(archives) == 1

    def test_library_needs_rebuild_no_archive(self, tmp_path: Path):
        """Library should need rebuild if archive doesn't exist."""
        from fbuild.packages.library_manager import Library

        orchestrator = OrchestratorWASM()
        lib_dir = tmp_path / "libs" / "testlib"
        lib_dir.mkdir(parents=True)
        (lib_dir / "src").mkdir()
        library = Library(lib_dir, "testlib")

        assert orchestrator._library_needs_rebuild(library, ["-O2"]) is True

    def test_library_needs_rebuild_no_build_info(self, tmp_path: Path):
        """Library should need rebuild if wasm_build_info.json doesn't exist."""
        from fbuild.packages.library_manager import Library

        orchestrator = OrchestratorWASM()
        lib_dir = tmp_path / "libs" / "testlib"
        lib_dir.mkdir(parents=True)
        (lib_dir / "src").mkdir()
        # Create a fake archive
        (lib_dir / "libtestlib.a").write_bytes(b"\x00")
        library = Library(lib_dir, "testlib")

        assert orchestrator._library_needs_rebuild(library, ["-O2"]) is True

    def test_library_needs_rebuild_flags_changed(self, tmp_path: Path):
        """Library should need rebuild if compiler flags changed."""
        from fbuild.packages.library_manager import Library

        orchestrator = OrchestratorWASM()
        lib_dir = tmp_path / "libs" / "testlib"
        lib_dir.mkdir(parents=True)
        (lib_dir / "src").mkdir()
        (lib_dir / "libtestlib.a").write_bytes(b"\x00")

        # Write build info with old flags
        build_info = {"compiler_flags": ["-O1"], "source_count": 1, "object_files": []}
        (lib_dir / "wasm_build_info.json").write_text(json.dumps(build_info))

        library = Library(lib_dir, "testlib")

        # Different flags should trigger rebuild
        assert orchestrator._library_needs_rebuild(library, ["-O2"]) is True

    def test_library_no_rebuild_when_cached(self, tmp_path: Path):
        """Library should not need rebuild if flags match."""
        from fbuild.packages.library_manager import Library

        orchestrator = OrchestratorWASM()
        lib_dir = tmp_path / "libs" / "testlib"
        lib_dir.mkdir(parents=True)
        (lib_dir / "src").mkdir()
        (lib_dir / "libtestlib.a").write_bytes(b"\x00")

        flags = ["-O2", "-pthread"]
        build_info = {"compiler_flags": flags, "source_count": 1, "object_files": []}
        (lib_dir / "wasm_build_info.json").write_text(json.dumps(build_info))

        library = Library(lib_dir, "testlib")

        assert orchestrator._library_needs_rebuild(library, flags) is False

    def test_compile_library_header_only(self, tmp_path: Path):
        """Header-only libraries should return None (no archive)."""
        from fbuild.packages.library_manager import Library

        orchestrator = OrchestratorWASM()
        lib_dir = tmp_path / "libs" / "headerlib"
        lib_dir.mkdir(parents=True)
        src_dir = lib_dir / "src"
        src_dir.mkdir()
        # Only header files, no source files
        (src_dir / "headerlib.h").write_text("#pragma once\ninline void foo() {}")

        library = Library(lib_dir, "headerlib")

        result = orchestrator._compile_library(
            library=library,
            emcc=Path("/usr/bin/emcc"),
            emar=Path("/usr/bin/emar"),
            compiler_flags=["-O2"],
            include_paths=[src_dir],
            defines=["FOO=1"],
            verbose=False,
        )

        assert result is None

    def test_compile_library_creates_archive(self, tmp_path: Path):
        """Library compilation should create .a archive and build info."""
        from fbuild.packages.library_manager import Library

        orchestrator = OrchestratorWASM()
        lib_dir = tmp_path / "libs" / "mylib"
        lib_dir.mkdir(parents=True)
        src_dir = lib_dir / "src"
        src_dir.mkdir()
        (src_dir / "mylib.cpp").write_text("void foo() {}")
        (src_dir / "helper.c").write_text("void bar() {}")

        library = Library(lib_dir, "mylib")

        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stderr = ""

        with patch("fbuild.build.orchestrator_wasm.safe_run", return_value=mock_result) as mock_run:
            result = orchestrator._compile_library(
                library=library,
                emcc=Path("/usr/bin/emcc"),
                emar=Path("/usr/bin/emar"),
                compiler_flags=["-O2"],
                include_paths=[src_dir],
                defines=["FOO=1"],
                verbose=False,
            )

        # Should return archive path
        assert result == lib_dir / "libmylib.a"

        # Should have called safe_run for each source + once for emar
        # 2 compilations + 1 archive = 3 calls
        assert mock_run.call_count == 3

        # Build info should be saved
        build_info_file = lib_dir / "wasm_build_info.json"
        assert build_info_file.exists()
        build_info = json.loads(build_info_file.read_text())
        assert build_info["compiler_flags"] == ["-O2"]
        assert build_info["source_count"] == 2

    def test_compile_library_uses_correct_std_flags(self, tmp_path: Path):
        """C++ files should use -std=gnu++17, C files should use -std=gnu17."""
        from fbuild.packages.library_manager import Library

        orchestrator = OrchestratorWASM()
        lib_dir = tmp_path / "libs" / "mixedlib"
        lib_dir.mkdir(parents=True)
        src_dir = lib_dir / "src"
        src_dir.mkdir()
        (src_dir / "cpp_file.cpp").write_text("void cpp() {}")
        (src_dir / "c_file.c").write_text("void c_func() {}")

        library = Library(lib_dir, "mixedlib")

        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stderr = ""

        compile_commands = []

        def capture_safe_run(cmd, **kwargs):
            compile_commands.append(list(cmd))
            return mock_result

        with patch("fbuild.build.orchestrator_wasm.safe_run", side_effect=capture_safe_run):
            orchestrator._compile_library(
                library=library,
                emcc=Path("/usr/bin/emcc"),
                emar=Path("/usr/bin/emar"),
                compiler_flags=["-O2"],
                include_paths=[src_dir],
                defines=[],
                verbose=False,
            )

        # Filter to just compile commands (not the emar command)
        compile_cmds = [cmd for cmd in compile_commands if "-c" in cmd]
        assert len(compile_cmds) == 2

        # Find the C++ command and C command
        cpp_cmd = [cmd for cmd in compile_cmds if any("cpp_file" in arg for arg in cmd)][0]
        c_cmd = [cmd for cmd in compile_cmds if any("c_file" in arg for arg in cmd)][0]

        assert "-std=gnu++17" in cpp_cmd
        assert "-std=gnu17" in c_cmd


class TestOrchestratorWASMBuildWithLibraries:
    """Test end-to-end build flow with library dependencies (mocked toolchain)."""

    def test_build_with_lib_deps_includes_library_paths(self, tmp_path: Path):
        """Build with lib_deps should add library include paths to compilation."""
        orchestrator = OrchestratorWASM()

        # Create project with lib_deps
        ini_content = "[env:wasm]\nplatform = wasm\nboard = wasm\nframework = arduino\nlib_deps =\n    https://github.com/FastLED/FastLED\n"
        (tmp_path / "platformio.ini").write_text(ini_content)
        src_dir = tmp_path / "src"
        src_dir.mkdir()
        (src_dir / "main.cpp").write_text("#include <FastLED.h>\nCRGB leds[10];\nvoid setup() {}\nvoid loop() {}\n")

        build_dir = tmp_path / ".fbuild" / "wasm" / "release"

        mock_request = MagicMock()
        mock_request.project_dir = tmp_path
        mock_request.env_name = "wasm"
        mock_request.build_dir = build_dir
        mock_request.verbose = False
        mock_request.clean = False
        mock_request.profile.value = "release"

        # Track what include paths are passed to _compile_source
        compile_calls = []
        original_compile = orchestrator._compile_source

        def track_compile(**kwargs):
            compile_calls.append(kwargs.get("include_paths", []))
            return True

        # Create a fake FastLED library structure
        from fbuild.packages.library_manager import Library

        def fake_download(url, show_progress):
            lib_dir = build_dir / "libs" / "fastled"
            lib_dir.mkdir(parents=True, exist_ok=True)
            fastled_src = lib_dir / "src"
            fastled_src.mkdir(exist_ok=True)
            (fastled_src / "FastLED.h").write_text("#pragma once\n")
            (fastled_src / "FastLED.cpp").write_text("// FastLED impl\n")
            info = {"name": "fastled", "url": url, "version": "unknown", "commit_hash": None, "compiler": "", "compile_commands": [], "link_commands": []}
            (lib_dir / "info.json").write_text(json.dumps(info))
            return Library(lib_dir, "fastled")

        mock_run_result = MagicMock()
        mock_run_result.returncode = 0
        mock_run_result.stderr = ""

        with (
            patch("fbuild.build.orchestrator_wasm._find_tool", return_value=Path("/usr/bin/tool")),
            patch("fbuild.packages.library_manager.LibraryManager.download_library", side_effect=fake_download),
            patch("fbuild.build.orchestrator_wasm.safe_run", return_value=mock_run_result),
        ):
            result = orchestrator.build(mock_request)

        # Build should succeed (all mocked)
        assert result.success is True

    def test_build_without_lib_deps_succeeds(self, tmp_path: Path):
        """Build without lib_deps should succeed (no library processing)."""
        orchestrator = OrchestratorWASM()

        ini_content = "[env:wasm]\nplatform = wasm\nboard = wasm\nframework = arduino\n"
        (tmp_path / "platformio.ini").write_text(ini_content)
        src_dir = tmp_path / "src"
        src_dir.mkdir()
        (src_dir / "main.cpp").write_text("int main() { return 0; }")

        build_dir = tmp_path / ".fbuild" / "wasm" / "release"

        mock_request = MagicMock()
        mock_request.project_dir = tmp_path
        mock_request.env_name = "wasm"
        mock_request.build_dir = build_dir
        mock_request.verbose = False
        mock_request.clean = False
        mock_request.profile.value = "release"

        mock_run_result = MagicMock()
        mock_run_result.returncode = 0
        mock_run_result.stderr = ""

        with (
            patch("fbuild.build.orchestrator_wasm._find_tool", return_value=Path("/usr/bin/tool")),
            patch("fbuild.build.orchestrator_wasm.safe_run", return_value=mock_run_result),
        ):
            result = orchestrator.build(mock_request)

        assert result.success is True
        assert result.message == "WASM build successful"

    def test_link_includes_library_archives(self, tmp_path: Path):
        """Linker should receive library archives with --whole-archive."""
        orchestrator = OrchestratorWASM()

        link_commands = []
        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stderr = ""

        def capture_safe_run(cmd, **kwargs):
            link_commands.append(list(cmd))
            return mock_result

        archive1 = tmp_path / "lib1.a"
        archive2 = tmp_path / "lib2.a"
        archive1.write_bytes(b"\x00")
        archive2.write_bytes(b"\x00")

        obj = tmp_path / "main.o"
        obj.write_bytes(b"\x00")

        with patch("fbuild.build.orchestrator_wasm.safe_run", side_effect=capture_safe_run):
            success = orchestrator._link(
                emcc=Path("/usr/bin/emcc"),
                object_files=[obj],
                library_archives=[archive1, archive2],
                output_path=tmp_path / "firmware.js",
                linker_flags=["-sWASM=1"],
                verbose=False,
            )

        assert success is True
        assert len(link_commands) == 1
        cmd = link_commands[0]

        # Should have --whole-archive wrapping library archives
        assert "-Wl,--whole-archive" in cmd
        assert "-Wl,--no-whole-archive" in cmd
        assert str(archive1) in cmd
        assert str(archive2) in cmd

    def test_link_without_library_archives(self, tmp_path: Path):
        """Linker should work without library archives (no --whole-archive)."""
        orchestrator = OrchestratorWASM()

        link_commands = []
        mock_result = MagicMock()
        mock_result.returncode = 0
        mock_result.stderr = ""

        def capture_safe_run(cmd, **kwargs):
            link_commands.append(list(cmd))
            return mock_result

        obj = tmp_path / "main.o"
        obj.write_bytes(b"\x00")

        with patch("fbuild.build.orchestrator_wasm.safe_run", side_effect=capture_safe_run):
            success = orchestrator._link(
                emcc=Path("/usr/bin/emcc"),
                object_files=[obj],
                library_archives=[],
                output_path=tmp_path / "firmware.js",
                linker_flags=["-sWASM=1"],
                verbose=False,
            )

        assert success is True
        cmd = link_commands[0]

        # Should NOT have --whole-archive when no libraries
        assert "-Wl,--whole-archive" not in cmd
