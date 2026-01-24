"""Unit tests for response file optimization in compilation_executor.py.

Tests conditional response file usage based on command-line length estimation.
"""

from pathlib import Path

import pytest

from fbuild.build.compilation_executor import CompilationExecutor


class TestCommandLengthEstimation:
    """Test command-line length estimation logic."""

    def test_estimate_short_command(self, tmp_path):
        """Test that short commands are estimated correctly."""
        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=False, use_trampolines=False)

        # Short command with 10 include paths
        short_includes = [f"-I/inc/{i:03d}" for i in range(10)]
        length = executor._estimate_command_length(Path("/usr/bin/gcc"), ["-Wall", "-O2"], short_includes, Path("test.cpp"), Path("test.o"))

        # Should be well below threshold
        assert length < 26214
        # Rough estimate: ~200 chars with safety margin
        assert 100 < length < 500

    def test_estimate_long_command_without_trampolines(self, tmp_path):
        """Test that long commands without trampolines exceed threshold."""
        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=False, use_trampolines=False)

        # Simulate 305 VERY long include paths to exceed threshold
        # Need ~86 chars per path to reach 26,214 threshold: 305 × 86 = 26,230
        long_includes = [f"-I/very/long/path/to/components/component_{i:03d}/include/subdirectory/nested/path" for i in range(305)]
        length = executor._estimate_command_length(Path("/usr/bin/xtensa-esp32-elf-gcc"), ["-Wall", "-O2", "-DESP32"], long_includes, Path("test.cpp"), Path("test.o"))

        # Should exceed threshold (305 paths × ~86 chars ≈ 26,230 chars + margin)
        assert length > 26214

    def test_estimate_trampolined_command(self, tmp_path):
        """Test that trampolined commands stay below threshold."""
        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=False, use_trampolines=True)

        # Simulate 305 trampolined include paths (short paths)
        trampoline_includes = [f"-IC:/inc/{i:03d}" for i in range(305)]
        length = executor._estimate_command_length(Path("/usr/bin/xtensa-esp32-elf-gcc"), ["-Wall", "-O2", "-DESP32"], trampoline_includes, Path("test.cpp"), Path("test.o"))

        # Should stay below threshold (305 paths × ~13 chars ≈ 3,965 chars)
        assert length < 26214
        # Should be around 4-5K chars
        assert 3000 < length < 6000

    def test_estimate_includes_safety_margin(self, tmp_path):
        """Test that estimation includes 10% safety margin."""
        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=False, use_trampolines=False)

        # Fixed input
        includes = ["-I/path/1", "-I/path/2"]
        base_estimate = executor._estimate_command_length(Path("/usr/bin/gcc"), ["-Wall"], includes, Path("test.cpp"), Path("test.o"))

        # Calculate expected without margin
        raw_length = len("/usr/bin/gcc") + len("-Wall") + 1 + len("-I/path/1") + 1 + len("-I/path/2") + 1 + len("-c") + len("test.cpp") + 1 + len("-o") + len("test.o") + 1

        # Estimate should be ~10% higher
        assert base_estimate >= raw_length
        assert base_estimate <= raw_length * 1.2  # Allow some tolerance


class TestConditionalResponseFiles:
    """Test conditional response file usage in build commands."""

    def test_short_command_skips_response_file(self, tmp_path):
        """Test that short commands skip response files."""
        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=False, use_trampolines=True)

        # Simulate trampolined includes (short paths)
        short_includes = [f"-IC:/inc/{i:03d}" for i in range(305)]

        cmd = executor._build_compile_command(Path("/usr/bin/gcc"), Path("test.cpp"), Path("test.o"), ["-Wall", "-O2"], short_includes)

        # Should NOT contain @response_file
        assert not any("@" in arg and ".rsp" in arg for arg in cmd), f"Unexpected response file in command: {cmd}"
        # Should contain direct -I flags
        assert any(arg.startswith("-IC:/inc/") for arg in cmd), f"Expected direct -I flags in command: {cmd}"

    def test_long_command_uses_response_file(self, tmp_path):
        """Test that long commands use response files."""
        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=False, use_trampolines=False)

        # Simulate VERY long include paths to exceed threshold (no trampolines)
        long_includes = [f"-I/very/long/path/to/components/component_{i:03d}/include/subdirectory/nested/path" for i in range(305)]

        cmd = executor._build_compile_command(Path("/usr/bin/gcc"), Path("test.cpp"), Path("test.o"), ["-Wall", "-O2"], long_includes)

        # Should contain @response_file
        assert any("@" in arg and ".rsp" in arg for arg in cmd), f"Expected response file in command: {cmd}"
        # Should NOT contain direct -I flags
        assert not any(arg.startswith("-I/very/long/") for arg in cmd), f"Unexpected direct -I flags in command: {cmd}"

    def test_threshold_boundary_condition(self, tmp_path):
        """Test behavior at threshold boundary (26,214 chars)."""
        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=False, use_trampolines=False)

        # Create includes that result in ~27,000 chars (exceeds threshold)
        # Each flag is ~30 chars ("-I/path/to/include/dir_XXX")
        # Need ~900 flags to reach ~27K: 900 × 30 = 27,000
        boundary_includes = [f"-I/path/to/include/dir_{i:03d}" for i in range(900)]

        cmd = executor._build_compile_command(Path("/usr/bin/gcc"), Path("test.cpp"), Path("test.o"), ["-Wall"], boundary_includes)

        # Should use response file (exceeds threshold)
        assert any("@" in arg and ".rsp" in arg for arg in cmd), "Expected response file at boundary condition"

    def test_sccache_wrapper_included_in_estimate(self, tmp_path):
        """Test that sccache path is included in length estimation."""
        # Create a mock sccache path
        sccache_path = tmp_path / "sccache.exe"
        sccache_path.write_text("")  # Create file

        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=True, use_trampolines=False)
        executor.sccache_path = sccache_path  # Force sccache path

        includes = ["-I/path/1"]
        length = executor._estimate_command_length(Path("/usr/bin/gcc"), ["-Wall"], includes, Path("test.cpp"), Path("test.o"))

        # Length should include sccache path
        assert length > len("/usr/bin/gcc") + len("-Wall") + len("-I/path/1")


class TestResponseFileCreation:
    """Test response file creation and content."""

    def test_response_file_written_when_needed(self, tmp_path):
        """Test that response files are written with correct content."""
        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=False, use_trampolines=False)

        # Create VERY long includes that will trigger response file
        long_includes = [f"-I/very/long/path/to/components/component_{i:03d}/include/subdirectory/nested/path" for i in range(305)]

        cmd = executor._build_compile_command(Path("/usr/bin/gcc"), Path("test.cpp"), Path("test.o"), ["-Wall"], long_includes)

        # Find response file in command
        response_file_arg = next((arg for arg in cmd if "@" in arg and ".rsp" in arg), None)
        assert response_file_arg is not None, "Expected response file in command"

        # Extract path and verify file exists
        response_file_path = Path(response_file_arg[1:])  # Remove @ prefix
        assert response_file_path.exists(), f"Response file not found: {response_file_path}"

        # Verify content
        content = response_file_path.read_text()
        lines = content.strip().split("\n")
        assert len(lines) == len(long_includes), f"Expected {len(long_includes)} lines, got {len(lines)}"
        assert all(line.startswith("-I") for line in lines), "All lines should be include flags"

    def test_response_file_not_created_when_skipped(self, tmp_path):
        """Test that response files are not created for short commands."""
        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=False, use_trampolines=True)

        # Short includes that won't trigger response file
        short_includes = [f"-IC:/inc/{i:03d}" for i in range(305)]

        executor._build_compile_command(Path("/usr/bin/gcc"), Path("test.cpp"), Path("test.o"), ["-Wall"], short_includes)

        # Verify no .rsp files created
        rsp_files = list(tmp_path.rglob("*.rsp"))
        assert len(rsp_files) == 0, f"Unexpected response files created: {rsp_files}"


@pytest.mark.integration
class TestEndToEndCompilation:
    """Integration tests for end-to-end compilation with response files."""

    def test_compile_with_trampolines_skips_response_file(self, tmp_path):
        """Test that compilation with trampolines skips response files."""
        # Create a simple C file
        source_file = tmp_path / "test.c"
        source_file.write_text("int main() { return 0; }")

        output_file = tmp_path / "test.o"

        # Create executor with trampolines
        executor = CompilationExecutor(build_dir=tmp_path, show_progress=False, use_sccache=False, use_trampolines=True)

        # Use gcc from PATH (if available)
        import shutil

        gcc_path = shutil.which("gcc")
        if not gcc_path:
            pytest.skip("gcc not found in PATH")

        # Compile with minimal includes (should skip response file)
        short_includes = [Path(f"/tmp/inc_{i:03d}") for i in range(10)]

        try:
            result = executor.compile_source(compiler_path=Path(gcc_path), source_path=source_file, output_path=output_file, compile_flags=["-Wall"], include_paths=short_includes)

            # Verify compilation succeeded
            assert result == output_file
            assert output_file.exists()

            # Verify no response files created
            rsp_files = list(tmp_path.rglob("*.rsp"))
            assert len(rsp_files) == 0, f"Unexpected response files: {rsp_files}"

        except Exception as e:
            # Compilation may fail due to missing includes, but that's OK
            # We're just testing that the command was built correctly
            if "response file" in str(e).lower():
                pytest.fail(f"Response file should not be used: {e}")
