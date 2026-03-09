"""
Unit tests for build_src_flags feature.

Tests that build_src_flags from platformio.ini:
1. Are parsed correctly by ini_parser
2. Flow through BuildContext
3. Are applied only to sketch compilation (not core or libraries)
4. Work with the debug flag warning in quick profile
"""

from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from fbuild.build.build_context import BuildContext
from fbuild.build.build_profiles import BuildProfile, ProfileFlags
from fbuild.build.flag_builder import FlagBuilder
from fbuild.config.ini_parser import PlatformIOConfig
from fbuild.platform_configs import BoardConfigModel

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_ini(tmp_path: Path, content: str) -> Path:
    """Write a platformio.ini and return its path."""
    ini = tmp_path / "platformio.ini"
    ini.write_text(content)
    return ini


def _make_mock_context(
    user_build_flags: list[str],
    user_build_src_flags: list[str],
    profile: BuildProfile = BuildProfile.QUICK,
) -> MagicMock:
    """Create a minimal mock BuildContext for FlagBuilder tests."""
    ctx = MagicMock(spec=BuildContext)
    ctx.profile = profile
    ctx.profile_flags = ProfileFlags(
        name=profile.value,
        description="test",
        compile_flags=(),
        link_flags=(),
        controlled_patterns=(),
    )
    ctx.user_build_flags = user_build_flags
    ctx.user_build_src_flags = user_build_src_flags
    # Minimal BoardConfigModel-like object
    ctx.platform_config = MagicMock(spec=BoardConfigModel)
    ctx.platform_config.compiler_flags = MagicMock()
    ctx.platform_config.compiler_flags.common = ["-ffunction-sections"]
    ctx.platform_config.compiler_flags.c = ["-std=gnu17"]
    ctx.platform_config.compiler_flags.cxx = ["-std=gnu++2b"]
    ctx.platform_config.defines = []
    ctx.platform_config.profiles = {}
    ctx.board_config = {"build": {}}
    ctx.board_id = "test_board"
    ctx.variant = "test"
    return ctx


# ===========================================================================
# 1. ini_parser: get_build_src_flags
# ===========================================================================


class TestIniBuildSrcFlags:
    """Test build_src_flags parsing in PlatformIOConfig."""

    def test_build_src_flags_present(self, tmp_path: Path) -> None:
        ini = _make_ini(
            tmp_path,
            """
[env:demo]
platform = espressif32
board = esp32dev
framework = arduino
build_src_flags = -Wformat=2 -Wstack-usage=4096
""",
        )
        config = PlatformIOConfig(ini)
        flags = config.get_build_src_flags("demo")
        assert flags == ["-Wformat=2", "-Wstack-usage=4096"]

    def test_build_src_flags_absent(self, tmp_path: Path) -> None:
        ini = _make_ini(
            tmp_path,
            """
[env:demo]
platform = espressif32
board = esp32dev
framework = arduino
""",
        )
        config = PlatformIOConfig(ini)
        flags = config.get_build_src_flags("demo")
        assert flags == []

    def test_build_src_flags_multiline(self, tmp_path: Path) -> None:
        ini = _make_ini(
            tmp_path,
            """
[env:demo]
platform = espressif32
board = esp32dev
framework = arduino
build_src_flags =
    -Wformat=2
    -Wstack-usage=4096
    -g3
""",
        )
        config = PlatformIOConfig(ini)
        flags = config.get_build_src_flags("demo")
        assert flags == ["-Wformat=2", "-Wstack-usage=4096", "-g3"]

    def test_build_src_flags_independent_of_build_flags(self, tmp_path: Path) -> None:
        """build_flags and build_src_flags are separate."""
        ini = _make_ini(
            tmp_path,
            """
[env:demo]
platform = espressif32
board = esp32dev
framework = arduino
build_flags = -DGLOBAL -Os
build_src_flags = -DSKETCH_ONLY -g3
""",
        )
        config = PlatformIOConfig(ini)
        build = config.get_build_flags("demo")
        src = config.get_build_src_flags("demo")
        assert "-DGLOBAL" in build
        assert "-DGLOBAL" not in src
        assert "-DSKETCH_ONLY" in src
        assert "-DSKETCH_ONLY" not in build

    def test_build_src_flags_with_inheritance(self, tmp_path: Path) -> None:
        """build_src_flags should be inherited from base [env] section."""
        ini = _make_ini(
            tmp_path,
            """
[env]
framework = arduino
build_src_flags = -Wformat=2

[env:demo]
platform = espressif32
board = esp32dev
""",
        )
        config = PlatformIOConfig(ini)
        flags = config.get_build_src_flags("demo")
        assert "-Wformat=2" in flags


# ===========================================================================
# 2. BuildContext: user_build_src_flags field
# ===========================================================================


class TestBuildContextSrcFlags:
    """Test that BuildContext stores and exposes build_src_flags."""

    def test_context_stores_src_flags(self) -> None:
        """BuildContext.user_build_src_flags is set from from_request."""
        src_flags = ["-Wformat=2", "-g3"]

        mock_request = MagicMock()
        mock_request.project_dir = Path("/tmp/project")
        mock_request.env_name = "demo"
        mock_request.clean = False
        mock_request.profile = BuildProfile.QUICK
        mock_request.profile_flags = ProfileFlags(name="quick", description="", compile_flags=(), link_flags=(), controlled_patterns=())
        mock_request.queue = MagicMock()
        mock_request.build_dir = Path("/tmp/build")
        mock_request.verbose = False
        mock_request.compile_database = None
        mock_request.generate_compiledb = False

        ctx = BuildContext.from_request(
            request=mock_request,
            platform=MagicMock(),
            toolchain=MagicMock(),
            mcu="esp32",
            framework_version="3.0.7",
            compilation_executor=MagicMock(),
            cache=None,
            framework=MagicMock(),
            board_id="esp32dev",
            board_config={},
            platform_config=MagicMock(),
            variant="esp32",
            core="arduino",
            user_build_flags=["-Os"],
            user_build_src_flags=src_flags,
            env_config={},
        )

        assert ctx.user_build_src_flags == src_flags
        assert ctx.user_build_flags == ["-Os"]

    def test_context_empty_src_flags(self) -> None:
        mock_request = MagicMock()
        mock_request.project_dir = Path("/tmp/project")
        mock_request.env_name = "demo"
        mock_request.clean = False
        mock_request.profile = BuildProfile.QUICK
        mock_request.profile_flags = ProfileFlags(name="quick", description="", compile_flags=(), link_flags=(), controlled_patterns=())
        mock_request.queue = MagicMock()
        mock_request.build_dir = Path("/tmp/build")
        mock_request.verbose = False
        mock_request.compile_database = None
        mock_request.generate_compiledb = False

        ctx = BuildContext.from_request(
            request=mock_request,
            platform=MagicMock(),
            toolchain=MagicMock(),
            mcu="esp32",
            framework_version="3.0.7",
            compilation_executor=MagicMock(),
            cache=None,
            framework=MagicMock(),
            board_id="esp32dev",
            board_config={},
            platform_config=MagicMock(),
            variant="esp32",
            core="arduino",
            user_build_flags=[],
            user_build_src_flags=[],
            env_config={},
        )

        assert ctx.user_build_src_flags == []


# ===========================================================================
# 3. FlagBuilder: build_flags vs build_src_flags separation
# ===========================================================================


class TestFlagBuilderSrcFlags:
    """Test that FlagBuilder applies user_build_flags but NOT user_build_src_flags."""

    def test_build_flags_includes_user_flags(self) -> None:
        """build_flags() should contain user_build_flags."""
        ctx = _make_mock_context(
            user_build_flags=["-DGLOBAL", "-Os"],
            user_build_src_flags=["-DSKETCH_ONLY"],
        )
        fb = FlagBuilder(ctx)
        flags = fb.build_flags()
        all_flags = flags["common"] + flags["cflags"] + flags["cxxflags"]
        assert "-DGLOBAL" in all_flags
        assert "-Os" in all_flags

    def test_build_flags_excludes_src_flags(self) -> None:
        """build_flags() should NOT contain user_build_src_flags."""
        ctx = _make_mock_context(
            user_build_flags=["-DGLOBAL"],
            user_build_src_flags=["-DSKETCH_ONLY", "-g3"],
        )
        fb = FlagBuilder(ctx)
        flags = fb.build_flags()
        all_flags = flags["common"] + flags["cflags"] + flags["cxxflags"]
        assert "-DSKETCH_ONLY" not in all_flags
        assert "-g3" not in all_flags

    def test_library_flags_exclude_src_flags(self) -> None:
        """get_base_flags_for_library() should NOT include build_src_flags."""
        ctx = _make_mock_context(
            user_build_flags=["-Os"],
            user_build_src_flags=["-g3", "-Wformat=2"],
        )
        fb = FlagBuilder(ctx)
        lib_flags = fb.get_base_flags_for_library()
        assert "-g3" not in lib_flags
        assert "-Wformat=2" not in lib_flags
        assert "-Os" in lib_flags


# ===========================================================================
# 4. ConfigurableCompiler: compile_sketch applies src_flags
# ===========================================================================


class TestCompileSketchSrcFlags:
    """Test that compile_sketch passes build_src_flags to compile_source."""

    def test_compile_sketch_passes_src_flags(self, tmp_path: Path) -> None:
        """compile_sketch should forward user_build_src_flags as extra_flags."""
        from fbuild.build.configurable_compiler import ConfigurableCompiler

        ctx = MagicMock(spec=BuildContext)
        ctx.platform = MagicMock()
        ctx.toolchain = MagicMock()
        ctx.framework = MagicMock()
        ctx.board_id = "esp32dev"
        ctx.build_dir = tmp_path / "build"
        ctx.build_dir.mkdir()
        ctx.verbose = False
        ctx.user_build_flags = ["-Os"]
        ctx.user_build_src_flags = ["-g3", "-Wformat=2"]
        ctx.cache = None
        ctx.mcu = "esp32"
        ctx.queue = MagicMock()
        ctx.profile_flags = ProfileFlags(name="quick", description="", compile_flags=(), link_flags=(), controlled_patterns=())
        ctx.compilation_executor = MagicMock()
        ctx.board_config = {"build": {}}
        ctx.variant = "esp32"
        ctx.core = "arduino"
        ctx.platform_config = MagicMock(spec=BoardConfigModel)
        ctx.platform_config.compiler_flags = MagicMock()
        ctx.platform_config.compiler_flags.common = []
        ctx.platform_config.compiler_flags.c = []
        ctx.platform_config.compiler_flags.cxx = []
        ctx.platform_config.defines = []
        ctx.platform_config.profiles = {}
        ctx.profile = BuildProfile.QUICK
        ctx.compile_database = None
        ctx.generate_compiledb = False

        compiler = ConfigurableCompiler(ctx)

        # Create a fake .ino file
        sketch_dir = tmp_path / "sketch"
        sketch_dir.mkdir()
        ino = sketch_dir / "main.ino"
        ino.write_text("void setup() {} void loop() {}")

        # Mock compile_source and preprocess_ino to capture calls
        captured_extra_flags: list[list[str] | None] = []
        original_compile_source = compiler.compile_source

        def mock_compile_source(source_path: Path, output_path=None, extra_flags=None) -> Path:
            captured_extra_flags.append(extra_flags)
            obj_dir = tmp_path / "build" / "obj"
            obj_dir.mkdir(parents=True, exist_ok=True)
            out = obj_dir / f"{source_path.stem}.o"
            out.write_bytes(b"fake obj")
            return out

        compiler.compile_source = mock_compile_source  # type: ignore[assignment]

        # Mock preprocess_ino to just return a .cpp
        cpp_path = sketch_dir / "main.cpp"
        cpp_path.write_text("void setup() {} void loop() {}")
        compiler.preprocess_ino = MagicMock(return_value=cpp_path)  # type: ignore[assignment]

        # Mock wait_all_jobs
        compiler.wait_all_jobs = MagicMock()  # type: ignore[assignment]

        compiler.compile_sketch(ino)

        # At least one call should have passed src_flags
        assert len(captured_extra_flags) >= 1
        for flags in captured_extra_flags:
            assert flags == ["-g3", "-Wformat=2"]

    def test_compile_sketch_no_src_flags(self, tmp_path: Path) -> None:
        """When build_src_flags is empty, extra_flags should be None."""
        from fbuild.build.configurable_compiler import ConfigurableCompiler

        ctx = MagicMock(spec=BuildContext)
        ctx.platform = MagicMock()
        ctx.toolchain = MagicMock()
        ctx.framework = MagicMock()
        ctx.board_id = "esp32dev"
        ctx.build_dir = tmp_path / "build"
        ctx.build_dir.mkdir()
        ctx.verbose = False
        ctx.user_build_flags = ["-Os"]
        ctx.user_build_src_flags = []
        ctx.cache = None
        ctx.mcu = "esp32"
        ctx.queue = MagicMock()
        ctx.profile_flags = ProfileFlags(name="quick", description="", compile_flags=(), link_flags=(), controlled_patterns=())
        ctx.compilation_executor = MagicMock()
        ctx.board_config = {"build": {}}
        ctx.variant = "esp32"
        ctx.core = "arduino"
        ctx.platform_config = MagicMock(spec=BoardConfigModel)
        ctx.platform_config.compiler_flags = MagicMock()
        ctx.platform_config.compiler_flags.common = []
        ctx.platform_config.compiler_flags.c = []
        ctx.platform_config.compiler_flags.cxx = []
        ctx.platform_config.defines = []
        ctx.platform_config.profiles = {}
        ctx.profile = BuildProfile.QUICK
        ctx.compile_database = None
        ctx.generate_compiledb = False

        compiler = ConfigurableCompiler(ctx)

        sketch_dir = tmp_path / "sketch"
        sketch_dir.mkdir()
        ino = sketch_dir / "main.ino"
        ino.write_text("void setup() {} void loop() {}")

        captured_extra_flags: list[list[str] | None] = []

        def mock_compile_source(source_path: Path, output_path=None, extra_flags=None) -> Path:
            captured_extra_flags.append(extra_flags)
            obj_dir = tmp_path / "build" / "obj"
            obj_dir.mkdir(parents=True, exist_ok=True)
            out = obj_dir / f"{source_path.stem}.o"
            out.write_bytes(b"fake obj")
            return out

        compiler.compile_source = mock_compile_source  # type: ignore[assignment]
        cpp_path = sketch_dir / "main.cpp"
        cpp_path.write_text("void setup() {} void loop() {}")
        compiler.preprocess_ino = MagicMock(return_value=cpp_path)  # type: ignore[assignment]
        compiler.wait_all_jobs = MagicMock()  # type: ignore[assignment]

        compiler.compile_sketch(ino)

        # extra_flags should be None when src_flags is empty
        for flags in captured_extra_flags:
            assert flags is None

    def test_compile_sketch_additional_cpp_gets_src_flags(self, tmp_path: Path) -> None:
        """Additional .cpp files in sketch dir should also get build_src_flags."""
        from fbuild.build.configurable_compiler import ConfigurableCompiler

        ctx = MagicMock(spec=BuildContext)
        ctx.platform = MagicMock()
        ctx.toolchain = MagicMock()
        ctx.framework = MagicMock()
        ctx.board_id = "esp32dev"
        ctx.build_dir = tmp_path / "build"
        ctx.build_dir.mkdir()
        ctx.verbose = False
        ctx.user_build_flags = []
        ctx.user_build_src_flags = ["-DSKETCH"]
        ctx.cache = None
        ctx.mcu = "esp32"
        ctx.queue = MagicMock()
        ctx.profile_flags = ProfileFlags(name="quick", description="", compile_flags=(), link_flags=(), controlled_patterns=())
        ctx.compilation_executor = MagicMock()
        ctx.board_config = {"build": {}}
        ctx.variant = "esp32"
        ctx.core = "arduino"
        ctx.platform_config = MagicMock(spec=BoardConfigModel)
        ctx.platform_config.compiler_flags = MagicMock()
        ctx.platform_config.compiler_flags.common = []
        ctx.platform_config.compiler_flags.c = []
        ctx.platform_config.compiler_flags.cxx = []
        ctx.platform_config.defines = []
        ctx.platform_config.profiles = {}
        ctx.profile = BuildProfile.QUICK
        ctx.compile_database = None
        ctx.generate_compiledb = False

        compiler = ConfigurableCompiler(ctx)

        sketch_dir = tmp_path / "sketch"
        sketch_dir.mkdir()
        ino = sketch_dir / "main.ino"
        ino.write_text("void setup() {} void loop() {}")
        # Additional .cpp in sketch dir
        extra_cpp = sketch_dir / "helpers.cpp"
        extra_cpp.write_text("int helper() { return 42; }")

        captured: list[tuple[str, list[str] | None]] = []

        def mock_compile_source(source_path: Path, output_path=None, extra_flags=None) -> Path:
            captured.append((source_path.name, extra_flags))
            obj_dir = tmp_path / "build" / "obj"
            obj_dir.mkdir(parents=True, exist_ok=True)
            out = obj_dir / f"{source_path.stem}.o"
            out.write_bytes(b"fake obj")
            return out

        compiler.compile_source = mock_compile_source  # type: ignore[assignment]
        cpp_path = sketch_dir / "main.cpp"
        cpp_path.write_text("void setup() {} void loop() {}")
        compiler.preprocess_ino = MagicMock(return_value=cpp_path)  # type: ignore[assignment]
        compiler.wait_all_jobs = MagicMock()  # type: ignore[assignment]

        compiler.compile_sketch(ino)

        # Both main.cpp and helpers.cpp should have src_flags
        assert len(captured) >= 2
        for name, flags in captured:
            assert flags == ["-DSKETCH"], f"{name} got wrong flags: {flags}"


# ===========================================================================
# 5. Debug flag warning in quick profile
# ===========================================================================


class TestDebugFlagWarning:
    """Test that FlagBuilder warns when -g* is in build_flags during quick profile."""

    def test_warns_on_g3_in_quick_profile(self) -> None:
        ctx = _make_mock_context(
            user_build_flags=["-g3", "-Os"],
            user_build_src_flags=[],
            profile=BuildProfile.QUICK,
        )
        fb = FlagBuilder(ctx)
        with patch("fbuild.output.log_warning") as mock_warn:
            fb.build_flags()
            mock_warn.assert_called_once()
            msg = mock_warn.call_args[0][0]
            assert "-g3" in msg
            assert "build_src_flags" in msg

    def test_warns_on_g_in_quick_profile(self) -> None:
        ctx = _make_mock_context(
            user_build_flags=["-g"],
            user_build_src_flags=[],
            profile=BuildProfile.QUICK,
        )
        fb = FlagBuilder(ctx)
        with patch("fbuild.output.log_warning") as mock_warn:
            fb.build_flags()
            mock_warn.assert_called_once()

    def test_warns_on_ggdb3_in_quick_profile(self) -> None:
        ctx = _make_mock_context(
            user_build_flags=["-ggdb3"],
            user_build_src_flags=[],
            profile=BuildProfile.QUICK,
        )
        fb = FlagBuilder(ctx)
        with patch("fbuild.output.log_warning") as mock_warn:
            fb.build_flags()
            mock_warn.assert_called_once()

    def test_no_warn_on_g0(self) -> None:
        """-g0 means no debug — should not warn."""
        ctx = _make_mock_context(
            user_build_flags=["-g0"],
            user_build_src_flags=[],
            profile=BuildProfile.QUICK,
        )
        fb = FlagBuilder(ctx)
        with patch("fbuild.output.log_warning") as mock_warn:
            fb.build_flags()
            mock_warn.assert_not_called()

    def test_no_warn_in_release_profile(self) -> None:
        """-g3 in release profile is fine — no warning."""
        ctx = _make_mock_context(
            user_build_flags=["-g3"],
            user_build_src_flags=[],
            profile=BuildProfile.RELEASE,
        )
        fb = FlagBuilder(ctx)
        with patch("fbuild.output.log_warning") as mock_warn:
            fb.build_flags()
            mock_warn.assert_not_called()

    def test_no_warn_without_debug_flags(self) -> None:
        ctx = _make_mock_context(
            user_build_flags=["-Os", "-DDEBUG"],
            user_build_src_flags=[],
            profile=BuildProfile.QUICK,
        )
        fb = FlagBuilder(ctx)
        with patch("fbuild.output.log_warning") as mock_warn:
            fb.build_flags()
            mock_warn.assert_not_called()

    def test_warns_only_once(self) -> None:
        """Multiple calls to build_flags should only produce one warning."""
        ctx = _make_mock_context(
            user_build_flags=["-g3"],
            user_build_src_flags=[],
            profile=BuildProfile.QUICK,
        )
        fb = FlagBuilder(ctx)
        with patch("fbuild.output.log_warning") as mock_warn:
            fb.build_flags()
            fb.build_flags()
            fb.build_flags()
            assert mock_warn.call_count == 1
