"""Tests for package fingerprinting and caching performance.

These tests ensure that:
1. Fingerprint validation is fast for cached packages
2. File system scanning is only done when necessary
3. Consecutive cache checks are near-instant
"""

import time
from pathlib import Path

from fbuild.packages.fingerprint import FingerprintRegistry, PackageFingerprint


class TestPackageFingerprintPerformance:
    """Performance tests for PackageFingerprint validation."""

    def test_validate_installation_fast_for_valid_cache(self, tmp_path: Path):
        """Test that validate_installation is fast when key files exist.

        This tests Issue: Expensive rglob in validate_installation().
        The file count check should be skipped or cached after initial validation.
        """
        # Create a directory with many files to simulate a toolchain
        extracted_dir = tmp_path / "toolchain"
        extracted_dir.mkdir()

        # Create 1000 files to simulate a real toolchain
        for i in range(1000):
            subdir = extracted_dir / f"subdir_{i // 100}"
            subdir.mkdir(exist_ok=True)
            (subdir / f"file_{i}.txt").write_text(f"content {i}")

        # Create key files
        key_files = ["bin/gcc", "bin/g++"]
        for key_file in key_files:
            key_path = extracted_dir / key_file
            key_path.parent.mkdir(parents=True, exist_ok=True)
            key_path.write_text("fake binary")

        # Create a fingerprint with known file count
        fingerprint = PackageFingerprint(
            url="https://example.com/toolchain.zip",
            version="1.0.0",
            url_hash="abc123",
            content_hash="def456",
            extracted_files=key_files,
            file_count=1002,  # 1000 files + 2 key files
            total_size=1000 * 10 + 2 * 11,  # Approximate sizes
        )

        # First validation should work
        is_valid, reason = fingerprint.validate_installation(extracted_dir)
        assert is_valid, f"Validation failed: {reason}"

        # Measure time for 10 consecutive validations
        start = time.perf_counter()
        for _ in range(10):
            is_valid, reason = fingerprint.validate_installation(extracted_dir)
            assert is_valid
        elapsed = time.perf_counter() - start

        # 10 validations should complete in reasonable time
        # Windows file system I/O is slower, especially with antivirus scanning
        # Allow up to 5 seconds to account for slow machines and Windows overhead
        assert elapsed < 5.0, f"Validation too slow: {elapsed:.2f}s for 10 iterations"

    def test_validate_installation_skips_file_count_when_zero(self, tmp_path: Path):
        """Test that file count check is skipped when file_count is 0."""
        extracted_dir = tmp_path / "package"
        extracted_dir.mkdir()

        # Create a fingerprint with file_count=0 (should skip expensive check)
        fingerprint = PackageFingerprint(
            url="https://example.com/package.zip",
            version="1.0.0",
            url_hash="abc123",
            content_hash="def456",
            extracted_files=[],  # No key files to check
            file_count=0,  # Should skip file count check
        )

        # Should be valid without scanning
        is_valid, reason = fingerprint.validate_installation(extracted_dir)
        assert is_valid, f"Validation failed: {reason}"

    def test_validate_installation_fails_fast_on_missing_key_files(self, tmp_path: Path):
        """Test that validation fails quickly when key files are missing."""
        extracted_dir = tmp_path / "package"
        extracted_dir.mkdir()

        # Create many files but missing key files
        for i in range(100):
            (extracted_dir / f"file_{i}.txt").write_text(f"content {i}")

        fingerprint = PackageFingerprint(
            url="https://example.com/package.zip",
            version="1.0.0",
            url_hash="abc123",
            content_hash="def456",
            extracted_files=["bin/missing_file"],  # Key file that doesn't exist
            file_count=100,
        )

        # Should fail fast without scanning all files
        start = time.perf_counter()
        is_valid, reason = fingerprint.validate_installation(extracted_dir)
        elapsed = time.perf_counter() - start

        assert not is_valid
        assert "Missing key file" in reason
        assert elapsed < 0.1, f"Failure detection too slow: {elapsed:.2f}s"


class TestFingerprintRegistryPerformance:
    """Performance tests for FingerprintRegistry operations."""

    def test_is_installed_fast_for_cached_packages(self, tmp_path: Path):
        """Test that is_installed() is fast for validated packages."""
        cache_root = tmp_path / "cache"
        cache_root.mkdir()

        registry = FingerprintRegistry(cache_root)

        # Create a package directory with minimal files
        package_dir = tmp_path / "package"
        package_dir.mkdir()
        (package_dir / "key_file.txt").write_text("content")

        # Create and register a fingerprint
        fingerprint = PackageFingerprint(
            url="https://example.com/package.zip",
            version="1.0.0",
            url_hash=PackageFingerprint.hash_url("https://example.com/package.zip"),
            content_hash="abc123",
            extracted_files=["key_file.txt"],
            file_count=1,
        )
        registry.register(fingerprint, package_dir)

        # First call should be valid
        assert registry.is_installed("https://example.com/package.zip", "1.0.0")

        # Measure time for 100 consecutive checks
        start = time.perf_counter()
        for _ in range(100):
            result = registry.is_installed("https://example.com/package.zip", "1.0.0")
            assert result
        elapsed = time.perf_counter() - start

        # 100 checks should complete in under 1 second
        assert elapsed < 1.0, f"is_installed too slow: {elapsed:.2f}s for 100 iterations"

    def test_list_packages_does_not_validate_all(self, tmp_path: Path):
        """Test that list_packages doesn't do expensive validation on all packages."""
        cache_root = tmp_path / "cache"
        cache_root.mkdir()

        registry = FingerprintRegistry(cache_root)

        # Register multiple packages
        for i in range(10):
            package_dir = tmp_path / f"package_{i}"
            package_dir.mkdir()
            (package_dir / "file.txt").write_text(f"content {i}")

            fingerprint = PackageFingerprint(
                url=f"https://example.com/package_{i}.zip",
                version="1.0.0",
                url_hash=PackageFingerprint.hash_url(f"https://example.com/package_{i}.zip"),
                content_hash=f"hash_{i}",
                extracted_files=["file.txt"],
                file_count=1,
            )
            registry.register(fingerprint, package_dir)

        # Measure list_packages time
        start = time.perf_counter()
        packages = registry.list_packages()
        elapsed = time.perf_counter() - start

        assert len(packages) == 10
        # Should complete quickly (validation for each, but minimal)
        assert elapsed < 0.5, f"list_packages too slow: {elapsed:.2f}s"


class TestConsecutiveBuildCacheCheck:
    """Test that consecutive build cache checks are fast."""

    def test_build_state_check_fast_when_unchanged(self, tmp_path: Path):
        """Test that build state comparison is fast when nothing changed."""
        from fbuild.build.build_state import BuildStateTracker

        build_dir = tmp_path / "build"
        build_dir.mkdir()

        # Create a platformio.ini file
        platformio_ini = tmp_path / "platformio.ini"
        platformio_ini.write_text("[env:test]\nboard = esp32dev\n")

        # Create some source files
        src_dir = tmp_path / "src"
        src_dir.mkdir()
        (src_dir / "main.cpp").write_text("void setup() {} void loop() {}")

        tracker = BuildStateTracker(build_dir)

        # First check - creates new state
        needs_rebuild, reasons, state = tracker.check_invalidation(
            platformio_ini_path=platformio_ini,
            platform="esp32",
            board="esp32dev",
            framework="arduino",
            source_dir=src_dir,
        )
        assert needs_rebuild  # First time always needs rebuild
        tracker.save_state(state)

        # Measure consecutive checks
        start = time.perf_counter()
        for _ in range(100):
            needs_rebuild, reasons, state = tracker.check_invalidation(
                platformio_ini_path=platformio_ini,
                platform="esp32",
                board="esp32dev",
                framework="arduino",
                source_dir=src_dir,
            )
            assert not needs_rebuild, f"Unexpected rebuild needed: {reasons}"
        elapsed = time.perf_counter() - start

        # 100 checks should complete in under 2 seconds
        assert elapsed < 2.0, f"Cache check too slow: {elapsed:.2f}s for 100 iterations"


class TestSizeInfoParsing:
    """Test firmware size info parsing for different platforms."""

    def test_parse_esp32_size_output(self):
        """Test parsing ESP32 size tool output format (Berkeley format)."""
        from fbuild.build.linker import SizeInfo

        # Typical output from xtensa-esp32-elf-size or riscv32-esp-elf-size
        # Berkeley format output (default format, no -A flag)
        esp32_output = """   text    data     bss     dec     hex filename
 123456   12345    4567  140368   2242c firmware.elf
"""
        # Parse and verify
        size_info = SizeInfo.parse(esp32_output, max_flash=1310720, max_ram=327680)

        # Should parse Berkeley format correctly
        assert size_info.text == 123456, f"Expected text=123456, got {size_info.text}"
        assert size_info.data == 12345, f"Expected data=12345, got {size_info.data}"
        assert size_info.bss == 4567, f"Expected bss=4567, got {size_info.bss}"
        assert size_info.total_flash == 123456 + 12345
        assert size_info.total_ram == 12345 + 4567

    def test_parse_esp32_size_real_output(self):
        """Test parsing real ESP32 size output with larger numbers."""
        from fbuild.build.linker import SizeInfo

        # Real output example from ESP32-S3 build
        esp32_output = """   text    data     bss     dec     hex filename
 858256  193280   69328 1120864  111a60 firmware.elf
"""
        size_info = SizeInfo.parse(esp32_output, max_flash=8388608, max_ram=327680)

        assert size_info.text == 858256
        assert size_info.data == 193280
        assert size_info.bss == 69328
        assert size_info.total_flash == 858256 + 193280
        assert size_info.total_ram == 193280 + 69328

    def test_parse_avr_size_output(self):
        """Test parsing AVR size tool output format (section-based)."""
        from fbuild.build.linker import SizeInfo

        # AVR size -A output format
        avr_output = """firmware.elf  :
section           size       addr
.data               50   8388864
.text            13558          0
.bss               580   8388914
Total            14188
"""
        size_info = SizeInfo.parse(avr_output, max_flash=32256, max_ram=2048)

        assert size_info.text == 13558
        assert size_info.data == 50
        assert size_info.bss == 580
        assert size_info.total_flash == 13558 + 50
        assert size_info.total_ram == 50 + 580

    def test_parse_empty_output(self):
        """Test parsing empty output returns zeros."""
        from fbuild.build.linker import SizeInfo

        size_info = SizeInfo.parse("", max_flash=32256, max_ram=2048)

        assert size_info.text == 0
        assert size_info.data == 0
        assert size_info.bss == 0

    def test_parse_invalid_output(self):
        """Test parsing invalid output returns zeros."""
        from fbuild.build.linker import SizeInfo

        size_info = SizeInfo.parse("invalid output\nno numbers here", max_flash=32256, max_ram=2048)

        assert size_info.text == 0
        assert size_info.data == 0
        assert size_info.bss == 0
