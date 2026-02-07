"""Test that reproduces the toolchain extraction bug from CI.

The bug: After the downloader strips the single top-level directory from
the toolchain archive, the `glob("*esp*")` heuristic in ensure_toolchain()
picks up the target sysroot subdirectory (xtensa-esp-elf/) instead of using
the full extracted contents. This results in only unprefixed binutils being
copied, with no gcc/g++/ar compiler binaries.

CI error: "Archiver (ar) path not found"
GitHub Actions: https://github.com/FastLED/fbuild/actions/runs/21772504898/job/62822709646
"""

from pathlib import Path
from unittest.mock import MagicMock, patch

from fbuild.packages.cache import Cache
from fbuild.packages.toolchain_esp32 import ToolchainESP32


def _create_fake_toolchain_archive_structure(temp_extract: Path) -> None:
    """Create directory structure matching what the downloader produces after stripping.

    After the downloader extracts a tar.xz/zip and strips the single top-level directory,
    the temp_extract directory contains the toolchain contents directly:

        temp_extract/
          bin/                        <- compiler binaries (gcc, g++, ar, etc.)
            xtensa-esp-elf-gcc
            xtensa-esp32-elf-gcc
            xtensa-esp-elf-ar
            xtensa-esp32-elf-ar
            ar                        <- unprefixed binutils
            as
            ...
          lib/
          include/
          libexec/
          picolibc/
          share/
          xtensa-esp-elf/             <- TARGET SYSROOT (matches *esp* glob!)
            bin/                      <- unprefixed binutils only (ar, as, ld...)
              ar
              as
              ld
            include/
            lib/
          package.json
    """
    # Top-level dirs (from stripped archive)
    bin_dir = temp_extract / "bin"
    bin_dir.mkdir(parents=True)
    lib_dir = temp_extract / "lib"
    lib_dir.mkdir()
    include_dir = temp_extract / "include"
    include_dir.mkdir()
    libexec_dir = temp_extract / "libexec"
    libexec_dir.mkdir()
    share_dir = temp_extract / "share"
    share_dir.mkdir()

    # Prefixed compiler binaries in bin/
    (bin_dir / "xtensa-esp-elf-gcc").touch()
    (bin_dir / "xtensa-esp-elf-g++").touch()
    (bin_dir / "xtensa-esp-elf-ar").touch()
    (bin_dir / "xtensa-esp-elf-objcopy").touch()
    (bin_dir / "xtensa-esp32-elf-gcc").touch()
    (bin_dir / "xtensa-esp32-elf-g++").touch()
    (bin_dir / "xtensa-esp32-elf-ar").touch()
    (bin_dir / "xtensa-esp32-elf-objcopy").touch()

    # Target sysroot subdirectory (this is what glob("*esp*") incorrectly picks up)
    sysroot = temp_extract / "xtensa-esp-elf"
    sysroot.mkdir()
    sysroot_bin = sysroot / "bin"
    sysroot_bin.mkdir()
    (sysroot_bin / "ar").touch()  # Unprefixed binutils only
    (sysroot_bin / "as").touch()
    (sysroot_bin / "ld").touch()
    sysroot_include = sysroot / "include"
    sysroot_include.mkdir()
    sysroot_lib = sysroot / "lib"
    sysroot_lib.mkdir()

    # package.json
    (temp_extract / "package.json").write_text("{}")


def test_glob_esp_picks_wrong_directory_after_strip(tmp_path):
    """Reproduce: glob('*esp*') finds the sysroot subdir, not the full toolchain.

    This is the core bug. After the downloader strips the single top-level directory,
    glob('*esp*') matches the inner xtensa-esp-elf/ sysroot instead of using
    the full temp_extract directory.
    """
    temp_extract = tmp_path / "temp_extract"
    temp_extract.mkdir()
    _create_fake_toolchain_archive_structure(temp_extract)

    # This is the buggy code from ensure_toolchain()
    extracted_dirs = list(temp_extract.glob("*esp*"))

    # BUG: glob finds the sysroot subdir
    assert len(extracted_dirs) == 1
    assert extracted_dirs[0].name == "xtensa-esp-elf"

    # BUG: source_dir points to the sysroot, not the full toolchain
    source_dir = extracted_dirs[0]

    # The sysroot's bin/ only has unprefixed binutils, no gcc!
    sysroot_binaries = list((source_dir / "bin").iterdir())
    sysroot_binary_names = [f.name for f in sysroot_binaries]
    assert "ar" in sysroot_binary_names  # Unprefixed ar exists
    assert "xtensa-esp-elf-gcc" not in sysroot_binary_names  # No gcc!
    assert "xtensa-esp32-elf-gcc" not in sysroot_binary_names  # No gcc!

    # The correct source should be temp_extract itself (after strip)
    correct_binaries = list((temp_extract / "bin").iterdir())
    correct_binary_names = [f.name for f in correct_binaries]
    assert "xtensa-esp-elf-gcc" in correct_binary_names
    assert "xtensa-esp32-elf-gcc" in correct_binary_names
    assert "xtensa-esp-elf-ar" in correct_binary_names


def test_ensure_toolchain_installs_compiler_binaries(tmp_path, monkeypatch):
    """Test that ensure_toolchain() correctly installs prefixed compiler binaries.

    After the fix, the toolchain's bin/ directory should contain prefixed
    gcc, g++, ar, etc. - not just unprefixed binutils from the sysroot.
    """
    monkeypatch.setenv("FBUILD_CACHE_DIR", str(tmp_path / "cache"))
    cache = Cache(tmp_path)

    toolchain_url = "https://example.com/xtensa-esp-elf-1.0.0.zip"

    with patch("fbuild.packages.toolchain_esp32.PackageDownloader") as mock_downloader_cls:
        mock_downloader = MagicMock()
        mock_downloader_cls.return_value = mock_downloader

        # Mock download to do nothing
        mock_downloader.download.return_value = None

        # Mock extract_archive to create the post-strip structure
        def mock_extract(archive_path, dest_dir, show_progress=True):
            _create_fake_toolchain_archive_structure(dest_dir)

        mock_downloader.extract_archive.side_effect = mock_extract

        toolchain = ToolchainESP32(cache, toolchain_url, "xtensa-esp-elf", show_progress=False)

        # Mock metadata parsing to return a fake platform URL
        with patch.object(toolchain, "_get_platform_url_from_metadata", return_value="https://example.com/xtensa-esp-elf-1.0.0-linux-amd64.tar.xz"):
            # Mock the archive file existing (so download is skipped)
            toolchain_cache_dir = toolchain.toolchain_path.parent / "bin"
            toolchain_cache_dir.mkdir(parents=True, exist_ok=True)
            archive_path = toolchain_cache_dir / "xtensa-esp-elf-1.0.0-linux-amd64.tar.xz"
            archive_path.touch()

            toolchain.ensure_toolchain()

        # Verify that the installed bin directory has prefixed compiler binaries
        bin_dir = toolchain.toolchain_path.parent / "bin" / "bin"

        # These should exist after correct installation
        assert bin_dir.exists(), f"bin/bin/ directory should exist at {bin_dir}"

        binaries = [f.name for f in bin_dir.iterdir()]

        # The key assertion: prefixed gcc must be present
        has_prefixed_gcc = any("gcc" in b and ("xtensa-esp-elf-" in b or "xtensa-esp32-elf-" in b) for b in binaries)
        assert has_prefixed_gcc, f"Expected prefixed gcc binary (xtensa-esp-elf-gcc or xtensa-esp32-elf-gcc) " f"in bin directory, but found: {binaries}"

        has_prefixed_ar = any("xtensa-esp-elf-ar" in b or "xtensa-esp32-elf-ar" in b for b in binaries)
        assert has_prefixed_ar, f"Expected prefixed ar binary in bin directory, but found: {binaries}"
