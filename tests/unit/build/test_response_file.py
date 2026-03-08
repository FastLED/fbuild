"""Tests for GCC response file utility."""

from pathlib import Path

from fbuild.build.response_file import write_response_file


def test_basic_write_and_return(tmp_path: Path) -> None:
    """Response file is written and @path returned."""
    flags = ["-I/sdk/freertos/include", "-I/sdk/esp_system/include"]

    result = write_response_file(tmp_path, flags, "test")

    assert result.startswith("@")
    rsp_path = Path(result[1:])
    assert rsp_path.exists()
    content = rsp_path.read_text(encoding="utf-8")
    assert "-I/sdk/freertos/include" in content
    assert "-I/sdk/esp_system/include" in content


def test_one_flag_per_line(tmp_path: Path) -> None:
    """Each flag occupies its own line."""
    flags = ["-Dfoo", "-Dbar", "-Dbaz"]

    result = write_response_file(tmp_path, flags, "multi")
    rsp_path = Path(result[1:])
    lines = rsp_path.read_text(encoding="utf-8").splitlines()

    assert lines == ["-Dfoo", "-Dbar", "-Dbaz"]


def test_paths_with_spaces_are_quoted(tmp_path: Path) -> None:
    """Flags containing spaces are wrapped in double quotes."""
    flags = ["-I/path with spaces/include", "-Dnormal"]

    result = write_response_file(tmp_path, flags, "spaces")
    rsp_path = Path(result[1:])
    lines = rsp_path.read_text(encoding="utf-8").splitlines()

    assert lines[0] == '"-I/path with spaces/include"'
    assert lines[1] == "-Dnormal"


def test_unique_prefix_prevents_collisions(tmp_path: Path) -> None:
    """Different prefixes produce different .rsp files."""
    write_response_file(tmp_path, ["-Da"], "file_a")
    write_response_file(tmp_path, ["-Db"], "file_b")

    assert (tmp_path / "file_a.rsp").exists()
    assert (tmp_path / "file_b.rsp").exists()

    content_a = (tmp_path / "file_a.rsp").read_text(encoding="utf-8")
    content_b = (tmp_path / "file_b.rsp").read_text(encoding="utf-8")
    assert "-Da" in content_a
    assert "-Db" in content_b


def test_return_uses_forward_slashes(tmp_path: Path) -> None:
    """Returned @path uses forward slashes for GCC compatibility."""
    result = write_response_file(tmp_path, ["-Dfoo"], "slashes")

    # After the leading @, path should use forward slashes
    path_part = result[1:]
    assert "\\" not in path_part


def test_creates_output_dir(tmp_path: Path) -> None:
    """Output directory is created if it doesn't exist."""
    nested_dir = tmp_path / "a" / "b" / "c"
    assert not nested_dir.exists()

    write_response_file(nested_dir, ["-Dfoo"], "nested")

    assert nested_dir.exists()
    assert (nested_dir / "nested.rsp").exists()


def test_empty_flags(tmp_path: Path) -> None:
    """Empty flags list produces an empty file."""
    result = write_response_file(tmp_path, [], "empty")
    rsp_path = Path(result[1:])

    assert rsp_path.exists()
    assert rsp_path.read_text(encoding="utf-8") == ""


def test_backslashes_converted_to_forward_slashes(tmp_path: Path) -> None:
    """Windows backslash paths are converted to forward slashes.

    GCC's response file parser treats backslash as an escape character.
    Without conversion, paths like C:\\Users\\niteris would have \\n and \\t
    interpreted as newline and tab escape sequences.
    """
    flags = [
        "-LC:\\Users\\niteris\\.fbuild\\dev\\cache\\tools\\sdk\\esp32c6\\ld",
        "-Tmemory.ld",
        "C:\\Users\\niteris\\.fbuild\\build\\core.a",
    ]

    result = write_response_file(tmp_path, flags, "backslash")
    rsp_path = Path(result[1:])
    lines = rsp_path.read_text(encoding="utf-8").splitlines()

    # All backslashes should be converted to forward slashes
    assert lines[0] == "-LC:/Users/niteris/.fbuild/dev/cache/tools/sdk/esp32c6/ld"
    assert lines[1] == "-Tmemory.ld"
    assert lines[2] == "C:/Users/niteris/.fbuild/build/core.a"


def test_many_flags_simulating_esp32(tmp_path: Path) -> None:
    """Simulates ESP32's ~300 include paths to verify the approach scales."""
    flags = [f"-IC:/Users/dev/.fbuild/cache/platforms/abc123/3.3.4/tools/sdk/esp32c6/include/component_{i}/include" for i in range(300)]

    result = write_response_file(tmp_path, flags, "esp32_sim")
    rsp_path = Path(result[1:])

    lines = rsp_path.read_text(encoding="utf-8").splitlines()
    assert len(lines) == 300

    # The @path itself is very short (well under 32K)
    assert len(result) < 500
