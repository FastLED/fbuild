"""Unit tests for cache management."""

import os
import tempfile
from pathlib import Path

from fbuild.packages.cache import Cache


class TestCache:
    """Test cases for Cache class."""

    def test_init_default_directory(self):
        """Test initialization with default directory."""
        cache = Cache()
        assert cache.project_dir == Path.cwd().resolve()
        # Dev mode uses cache_dev, production uses cache (in home directory, not project)
        expected_cache_name = "cache_dev" if os.environ.get("FBUILD_DEV_MODE") == "1" else "cache"
        assert cache.cache_root == Path.home() / ".fbuild" / expected_cache_name
        assert cache.build_root == cache.project_dir / ".fbuild" / "build"

    def test_init_custom_directory(self):
        """Test initialization with custom project directory."""
        with tempfile.TemporaryDirectory() as temp_dir:
            project_dir = Path(temp_dir)
            cache = Cache(project_dir)
            assert cache.project_dir == project_dir.resolve()
            # Dev mode uses cache_dev, production uses cache (in home directory, not project)
            expected_cache_name = "cache_dev" if os.environ.get("FBUILD_DEV_MODE") == "1" else "cache"
            assert cache.cache_root == Path.home() / ".fbuild" / expected_cache_name

    def test_init_with_env_override(self):
        """Test cache directory override via environment variable."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache_dir = Path(temp_dir) / "custom_cache"
            os.environ["FBUILD_CACHE_DIR"] = str(cache_dir)

            try:
                cache = Cache()
                assert cache.cache_root == cache_dir.resolve()
            finally:
                del os.environ["FBUILD_CACHE_DIR"]

    def test_hash_url(self):
        """Test URL hashing function."""
        url1 = "https://example.com/toolchain"
        url2 = "https://example.com/platform"

        hash1 = Cache.hash_url(url1)
        hash2 = Cache.hash_url(url2)

        # Hashes should be deterministic
        assert Cache.hash_url(url1) == hash1
        assert Cache.hash_url(url2) == hash2

        # Different URLs should produce different hashes
        assert hash1 != hash2

        # Hash should be 16 characters
        assert len(hash1) == 16
        assert len(hash2) == 16

    def test_packages_dir(self):
        """Test packages directory property."""
        cache = Cache()
        assert cache.packages_dir == cache.cache_root / "packages"

    def test_toolchains_dir(self):
        """Test toolchains directory property."""
        cache = Cache()
        assert cache.toolchains_dir == cache.cache_root / "toolchains"

    def test_platforms_dir(self):
        """Test platforms directory property."""
        cache = Cache()
        assert cache.platforms_dir == cache.cache_root / "platforms"

    def test_libraries_dir(self):
        """Test libraries directory property."""
        cache = Cache()
        assert cache.libraries_dir == cache.cache_root / "libraries"

    def test_get_build_dir(self):
        """Test getting build directory for environment and profile."""
        cache = Cache()
        # Default profile is 'release'
        build_dir = cache.get_build_dir("uno")
        assert build_dir == cache.build_root / "uno" / "release"
        # Explicit quick profile
        build_dir_quick = cache.get_build_dir("uno", profile="quick")
        assert build_dir_quick == cache.build_root / "uno" / "quick"

    def test_get_core_build_dir(self):
        """Test getting core build directory."""
        cache = Cache()
        core_dir = cache.get_core_build_dir("uno")
        assert core_dir == cache.build_root / "uno" / "release" / "core"

    def test_get_src_build_dir(self):
        """Test getting sketch build directory."""
        cache = Cache()
        src_dir = cache.get_src_build_dir("uno")
        assert src_dir == cache.build_root / "uno" / "release" / "src"

    def test_ensure_directories(self):
        """Test creating cache directories."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            cache.ensure_directories()

            assert cache.packages_dir.exists()
            assert cache.toolchains_dir.exists()
            assert cache.platforms_dir.exists()
            assert cache.libraries_dir.exists()

    def test_ensure_build_directories(self):
        """Test creating build directories for environment."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            cache.ensure_build_directories("uno")

            assert cache.get_build_dir("uno").exists()
            assert cache.get_core_build_dir("uno").exists()
            assert cache.get_src_build_dir("uno").exists()

    def test_clean_build(self):
        """Test cleaning build artifacts."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            cache.ensure_build_directories("uno")

            # Create some files
            build_dir = cache.get_build_dir("uno")
            (build_dir / "test.o").touch()
            (cache.get_core_build_dir("uno") / "core.a").touch()

            # Clean
            cache.clean_build("uno")

            assert not build_dir.exists()

    def test_clean_build_nonexistent(self):
        """Test cleaning build that doesn't exist."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            # Should not raise error
            cache.clean_build("nonexistent")

    def test_get_package_path_with_url_hashing(self):
        """Test getting package path with URL hashing."""
        cache = Cache()
        url = "https://example.com/toolchain"
        version = "1.0.0"
        filename = "test-package.tar.gz"

        path = cache.get_package_path(url, version, filename)
        url_hash = Cache.hash_url(url)

        assert path == cache.packages_dir / url_hash / version / filename

    def test_get_toolchain_path_with_url_hashing(self):
        """Test getting toolchain path with URL hashing."""
        cache = Cache()
        url = "https://example.com/toolchain"
        version = "7.3.0"

        path = cache.get_toolchain_path(url, version)
        url_hash = Cache.hash_url(url)

        assert path == cache.toolchains_dir / url_hash / version

    def test_get_platform_path_with_url_hashing(self):
        """Test getting platform path with URL hashing."""
        cache = Cache()
        url = "https://example.com/platform"
        version = "1.8.6"

        path = cache.get_platform_path(url, version)
        url_hash = Cache.hash_url(url)

        assert path == cache.platforms_dir / url_hash / version

    def test_is_package_cached(self):
        """Test checking if package is cached."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))
            cache.ensure_directories()

            url = "https://example.com/test"
            version = "1.0.0"
            filename = "test.tar.gz"

            # Create a package
            package_path = cache.get_package_path(url, version, filename)
            package_path.parent.mkdir(parents=True, exist_ok=True)
            package_path.touch()

            assert cache.is_package_cached(url, version, filename)
            assert not cache.is_package_cached(url, version, "missing.tar.gz")
            assert not cache.is_package_cached(url, "2.0.0", filename)

    def test_is_toolchain_cached(self):
        """Test checking if toolchain is cached."""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Set FBUILD_CACHE_DIR to use temp directory for cache
            old_cache_dir = os.environ.get("FBUILD_CACHE_DIR")
            os.environ["FBUILD_CACHE_DIR"] = temp_dir
            try:
                cache = Cache(Path(temp_dir))
                cache.ensure_directories()

                url = "https://example.com/toolchain"
                version = "7.3.0"

                # Create a toolchain directory
                toolchain_path = cache.get_toolchain_path(url, version)
                toolchain_path.mkdir(parents=True)

                assert cache.is_toolchain_cached(url, version)
                assert not cache.is_toolchain_cached(url, "8.0.0")
            finally:
                if old_cache_dir is None:
                    del os.environ["FBUILD_CACHE_DIR"]
                else:
                    os.environ["FBUILD_CACHE_DIR"] = old_cache_dir

    def test_is_toolchain_cached_file_not_dir(self):
        """Test that toolchain check fails for files."""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Set FBUILD_CACHE_DIR to use temp directory for cache
            old_cache_dir = os.environ.get("FBUILD_CACHE_DIR")
            os.environ["FBUILD_CACHE_DIR"] = temp_dir
            try:
                cache = Cache(Path(temp_dir))
                cache.ensure_directories()

                url = "https://example.com/toolchain"
                version = "7.3.0"

                # Create a file instead of directory
                toolchain_path = cache.get_toolchain_path(url, version)
                toolchain_path.parent.mkdir(parents=True, exist_ok=True)
                toolchain_path.touch()

                assert not cache.is_toolchain_cached(url, version)
            finally:
                if old_cache_dir is None:
                    del os.environ["FBUILD_CACHE_DIR"]
                else:
                    os.environ["FBUILD_CACHE_DIR"] = old_cache_dir

    def test_is_platform_cached(self):
        """Test checking if platform is cached."""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Set FBUILD_CACHE_DIR to use temp directory for cache
            old_cache_dir = os.environ.get("FBUILD_CACHE_DIR")
            os.environ["FBUILD_CACHE_DIR"] = temp_dir
            try:
                cache = Cache(Path(temp_dir))
                cache.ensure_directories()

                url = "https://example.com/platform"
                version = "1.8.6"

                # Create a platform directory
                platform_path = cache.get_platform_path(url, version)
                platform_path.mkdir(parents=True)

                assert cache.is_platform_cached(url, version)
                assert not cache.is_platform_cached(url, "2.0.0")
            finally:
                if old_cache_dir is None:
                    del os.environ["FBUILD_CACHE_DIR"]
                else:
                    os.environ["FBUILD_CACHE_DIR"] = old_cache_dir

    def test_multiple_environments(self):
        """Test managing multiple build environments."""
        with tempfile.TemporaryDirectory() as temp_dir:
            cache = Cache(Path(temp_dir))

            # Create directories for multiple environments
            cache.ensure_build_directories("uno")
            cache.ensure_build_directories("mega")

            assert cache.get_build_dir("uno").exists()
            assert cache.get_build_dir("mega").exists()

            # Clean one environment
            cache.clean_build("uno")
            assert not cache.get_build_dir("uno").exists()
            assert cache.get_build_dir("mega").exists()

    def test_version_isolation(self):
        """Test that different versions don't conflict."""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Set FBUILD_CACHE_DIR to use temp directory for cache
            old_cache_dir = os.environ.get("FBUILD_CACHE_DIR")
            os.environ["FBUILD_CACHE_DIR"] = temp_dir
            try:
                cache = Cache(Path(temp_dir))
                cache.ensure_directories()

                url = "https://example.com/toolchain"
                version1 = "7.3.0"
                version2 = "8.0.0"

                # Create toolchains for both versions
                path1 = cache.get_toolchain_path(url, version1)
                path2 = cache.get_toolchain_path(url, version2)

                path1.mkdir(parents=True)
                path2.mkdir(parents=True)

                # Both should be cached independently
                assert cache.is_toolchain_cached(url, version1)
                assert cache.is_toolchain_cached(url, version2)

                # Paths should be different
                assert path1 != path2

                # Same URL hash, different versions
                url_hash = Cache.hash_url(url)
                assert path1.name == version1
                assert path2.name == version2
                assert path1.parent.name == url_hash
                assert path2.parent.name == url_hash
            finally:
                if old_cache_dir is None:
                    del os.environ["FBUILD_CACHE_DIR"]
                else:
                    os.environ["FBUILD_CACHE_DIR"] = old_cache_dir
