"""Unit tests for GitHub URL transformation utilities."""

import pytest

from fbuild.packages.github_url_utils import (
    GitHubURLError,
    PlatformIOURLError,
    resolve_and_verify_platformio_url,
    resolve_platformio_platform_url,
    transform_and_verify_github_url,
    transform_github_url,
    verify_github_url,
)


class TestTransformGitHubURL:
    """Tests for transform_github_url() function."""

    def test_simple_repo_url(self):
        """Test transformation of simple repository URLs."""
        url = "https://github.com/owner/repo"
        result = transform_github_url(url, prefer_zip=True)
        assert result == "https://github.com/owner/repo/archive/refs/heads/main.zip"

    def test_simple_repo_url_with_git_suffix(self):
        """Test transformation of repository URLs with .git suffix."""
        url = "https://github.com/owner/repo.git"
        result = transform_github_url(url, prefer_zip=True)
        assert result == "https://github.com/owner/repo/archive/refs/heads/main.zip"

    def test_simple_repo_url_with_trailing_slash(self):
        """Test transformation of repository URLs with trailing slash."""
        url = "https://github.com/owner/repo/"
        result = transform_github_url(url, prefer_zip=True)
        assert result == "https://github.com/owner/repo/archive/refs/heads/main.zip"

    def test_branch_url(self):
        """Test transformation of branch URLs."""
        url = "https://github.com/owner/repo/tree/develop"
        result = transform_github_url(url, prefer_zip=True)
        assert result == "https://github.com/owner/repo/archive/refs/heads/develop.zip"

    def test_tag_url_with_v_prefix(self):
        """Test transformation of tag URLs with 'v' prefix."""
        url = "https://github.com/owner/repo/tree/v1.0.0"
        result = transform_github_url(url, prefer_zip=True)
        assert result == "https://github.com/owner/repo/archive/refs/tags/v1.0.0.zip"

    def test_tag_url_with_v_and_patch(self):
        """Test transformation of semantic version tags."""
        url = "https://github.com/owner/repo/tree/v1.2.3"
        result = transform_github_url(url, prefer_zip=True)
        assert result == "https://github.com/owner/repo/archive/refs/tags/v1.2.3.zip"

    def test_commit_url(self):
        """Test transformation of commit URLs."""
        url = "https://github.com/owner/repo/commit/abc123def456"
        result = transform_github_url(url, prefer_zip=True)
        assert result == "https://github.com/owner/repo/archive/abc123def456.zip"

    def test_release_download_url(self):
        """Test transformation of release download URLs."""
        url = "https://github.com/owner/repo/releases/download/v4.2.1/platform-espressif8266.zip"
        result = transform_github_url(url, prefer_zip=True)
        assert result == "https://github.com/owner/repo/archive/refs/tags/v4.2.1.zip"

    def test_release_download_url_without_v_prefix(self):
        """Test transformation of release download URLs without 'v' prefix."""
        url = "https://github.com/owner/repo/releases/download/1.0.0/file.zip"
        result = transform_github_url(url, prefer_zip=True)
        assert result == "https://github.com/owner/repo/archive/refs/tags/1.0.0.zip"

    def test_prefer_tar_gz(self):
        """Test transformation with tar.gz preference."""
        url = "https://github.com/owner/repo/tree/v1.0.0"
        result = transform_github_url(url, prefer_zip=False)
        assert result == "https://github.com/owner/repo/archive/refs/tags/v1.0.0.tar.gz"

    def test_branch_with_slashes(self):
        """Test transformation of branch URLs with slashes in branch name."""
        url = "https://github.com/owner/repo/tree/feature/new-feature"
        result = transform_github_url(url, prefer_zip=True)
        assert result == "https://github.com/owner/repo/archive/refs/heads/feature/new-feature.zip"

    def test_non_github_url(self):
        """Test that non-GitHub URLs raise GitHubURLError."""
        url = "https://gitlab.com/owner/repo"
        with pytest.raises(GitHubURLError, match="Not a GitHub URL"):
            transform_github_url(url)

    def test_invalid_github_url_format(self):
        """Test that invalid GitHub URL formats raise GitHubURLError."""
        url = "https://github.com/owner"
        with pytest.raises(GitHubURLError, match="Invalid GitHub URL format"):
            transform_github_url(url)

    def test_unsupported_github_url_format(self):
        """Test that unsupported GitHub URL formats raise GitHubURLError."""
        url = "https://github.com/owner/repo/pull/123"
        with pytest.raises(GitHubURLError, match="Unsupported GitHub URL format"):
            transform_github_url(url)


class TestVerifyGitHubURL:
    """Tests for verify_github_url() function."""

    def test_verify_valid_url(self):
        """Test verification of a valid GitHub archive URL."""
        # This is a real URL that should exist
        url = "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"
        exists, final_url = verify_github_url(url, timeout=10)
        assert exists is True
        assert final_url is not None
        # GitHub redirects to codeload.github.com
        assert "codeload.github.com" in final_url or "github.com" in final_url

    def test_verify_invalid_url(self):
        """Test verification of an invalid GitHub archive URL."""
        # This URL should not exist (fake version)
        url = "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v999.999.999.zip"
        exists, final_url = verify_github_url(url, timeout=10)
        assert exists is False
        assert final_url is None

    def test_verify_invalid_domain(self):
        """Test verification of an invalid domain."""
        url = "https://invalid-domain-that-does-not-exist-12345.com/file.zip"
        exists, final_url = verify_github_url(url, timeout=5)
        assert exists is False
        assert final_url is None


class TestTransformAndVerifyGitHubURL:
    """Tests for transform_and_verify_github_url() function."""

    def test_transform_and_verify_valid_repo(self):
        """Test transformation and verification of a valid repository."""
        # Use a real repository that should exist
        url = "https://github.com/platformio/platform-espressif8266/tree/v4.2.1"
        transformed, exists, final = transform_and_verify_github_url(url, timeout=10)
        assert transformed == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"
        assert exists is True
        assert final is not None

    def test_transform_and_verify_invalid_url(self):
        """Test transformation and verification of an invalid URL."""
        url = "https://gitlab.com/owner/repo"
        with pytest.raises(GitHubURLError):
            transform_and_verify_github_url(url)

    def test_transform_and_verify_nonexistent_version(self):
        """Test transformation and verification of a non-existent version."""
        url = "https://github.com/platformio/platform-espressif8266/tree/v999.999.999"
        transformed, exists, final = transform_and_verify_github_url(url, timeout=10)
        assert transformed == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v999.999.999.zip"
        assert exists is False
        assert final is None


class TestRealWorldExamples:
    """Tests using real-world GitHub URLs from the ESP8266 issue."""

    def test_platformio_esp8266_release_url(self):
        """Test the problematic ESP8266 release URL from the issue."""
        # The old format that was failing
        old_url = "https://github.com/platformio/platform-espressif8266/releases/download/v4.2.1/platform-espressif8266.zip"
        result = transform_github_url(old_url)
        # Should transform to the working archive format
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"

        # Verify the transformed URL actually exists
        exists, _ = verify_github_url(result, timeout=10)
        assert exists is True

    def test_esp8266_arduino_framework(self):
        """Test ESP8266 Arduino framework URL."""
        url = "https://github.com/esp8266/Arduino/tree/3.1.2"
        result = transform_github_url(url)
        # 3.1.2 starts with a digit, so it's treated as a branch (not starting with 'v')
        assert result == "https://github.com/esp8266/Arduino/archive/refs/heads/3.1.2.zip"

    def test_esp8266_arduino_tag_with_v(self):
        """Test ESP8266 Arduino framework URL with 'v' prefix."""
        # If we had used v3.1.2 instead
        url = "https://github.com/esp8266/Arduino/tree/v3.1.2"
        result = transform_github_url(url)
        assert result == "https://github.com/esp8266/Arduino/archive/refs/tags/v3.1.2.zip"


class TestResolvePlatformIOPlatformURL:
    """Tests for resolve_platformio_platform_url() function."""

    def test_platform_with_version(self):
        """Test abbreviated platform with version."""
        spec = "espressif8266@4.2.1"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"

    def test_platform_with_version_already_has_v(self):
        """Test platform with version that already has 'v' prefix."""
        spec = "espressif8266@v4.2.1"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"

    def test_platform_without_version(self):
        """Test platform without version defaults to master branch."""
        spec = "espressif8266"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/heads/master.zip"

    def test_platform_with_owner_and_version(self):
        """Test platform with owner and version."""
        spec = "platformio/espressif8266@4.2.1"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"

    def test_platform_with_owner_no_version(self):
        """Test platform with owner but no version."""
        spec = "platformio/espressif8266"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/heads/master.zip"

    def test_platform_with_custom_owner(self):
        """Test platform with custom owner."""
        spec = "myuser/espressif8266@1.0.0"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/myuser/platform-espressif8266/archive/refs/tags/v1.0.0.zip"

    def test_platform_already_has_platform_prefix(self):
        """Test platform that already has 'platform-' prefix."""
        spec = "platform-espressif8266@4.2.1"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"

    def test_platform_with_tar_gz(self):
        """Test platform resolution with tar.gz preference."""
        spec = "espressif8266@4.2.1"
        result = resolve_platformio_platform_url(spec, prefer_zip=False)
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.tar.gz"

    def test_esp32_platform(self):
        """Test ESP32 platform resolution."""
        spec = "espressif32@6.5.0"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-espressif32/archive/refs/tags/v6.5.0.zip"

    def test_rp2040_platform(self):
        """Test RP2040 platform resolution."""
        spec = "raspberrypi@1.9.0"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-raspberrypi/archive/refs/tags/v1.9.0.zip"

    def test_github_url_passthrough(self):
        """Test that full GitHub URLs are passed through transform_github_url()."""
        spec = "https://github.com/platformio/platform-espressif8266/tree/v4.2.1"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"

    def test_github_git_url_passthrough(self):
        """Test that GitHub .git URLs are transformed correctly."""
        spec = "https://github.com/platformio/platform-espressif8266.git"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/heads/main.zip"

    def test_invalid_platform_spec(self):
        """Test that invalid platform specifications raise error."""
        spec = "invalid spec with spaces"
        with pytest.raises(PlatformIOURLError, match="Invalid platform specification"):
            resolve_platformio_platform_url(spec)

    def test_empty_platform_spec(self):
        """Test that empty platform specification raises error."""
        spec = ""
        with pytest.raises(PlatformIOURLError, match="Invalid platform specification"):
            resolve_platformio_platform_url(spec)

    def test_version_with_dots_and_dashes(self):
        """Test version with dots and dashes."""
        spec = "espressif8266@4.2.1-rc.1"
        result = resolve_platformio_platform_url(spec)
        assert result == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1-rc.1.zip"


class TestResolveAndVerifyPlatformIOURL:
    """Tests for resolve_and_verify_platformio_url() function."""

    def test_resolve_and_verify_valid_platform(self):
        """Test resolution and verification of a valid platform."""
        spec = "espressif8266@4.2.1"
        resolved, exists, final = resolve_and_verify_platformio_url(spec, timeout=10)
        assert resolved == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"
        assert exists is True
        assert final is not None

    def test_resolve_and_verify_invalid_version(self):
        """Test resolution of an invalid version."""
        spec = "espressif8266@999.999.999"
        resolved, exists, final = resolve_and_verify_platformio_url(spec, timeout=10)
        assert resolved == "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v999.999.999.zip"
        assert exists is False
        assert final is None

    def test_resolve_and_verify_invalid_spec(self):
        """Test that invalid specification raises error."""
        spec = "invalid spec"
        with pytest.raises(PlatformIOURLError):
            resolve_and_verify_platformio_url(spec)


class TestPlatformIOIntegration:
    """Integration tests for PlatformIO platform resolution."""

    def test_all_abbreviated_formats(self):
        """Test that all abbreviated formats resolve to the same URL."""
        expected = "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"

        # All these should resolve to the same URL
        specs = [
            "espressif8266@4.2.1",
            "espressif8266@v4.2.1",
            "platformio/espressif8266@4.2.1",
            "platformio/espressif8266@v4.2.1",
            "platform-espressif8266@4.2.1",
            "platformio/platform-espressif8266@4.2.1",
        ]

        for spec in specs:
            result = resolve_platformio_platform_url(spec)
            assert result == expected, f"Failed for spec: {spec}"

    def test_url_formats_equivalence(self):
        """Test that different URL formats produce the same result."""
        expected = "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"

        formats = [
            "espressif8266@4.2.1",
            "https://github.com/platformio/platform-espressif8266/tree/v4.2.1",
            "https://github.com/platformio/platform-espressif8266/releases/download/v4.2.1/file.zip",
        ]

        for fmt in formats:
            result = resolve_platformio_platform_url(fmt)
            assert result == expected, f"Failed for format: {fmt}"
