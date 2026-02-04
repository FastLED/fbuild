"""GitHub URL transformation utilities.

This module provides utilities to transform GitHub URLs and PlatformIO platform
specifications into proper archive download URLs (tar.gz or .zip) instead of
requiring git clone operations.
"""

import re
from typing import Optional, Tuple
from urllib.parse import urlparse

import requests


class GitHubURLError(Exception):
    """Raised when GitHub URL transformation fails."""

    pass


class PlatformIOURLError(Exception):
    """Raised when PlatformIO platform URL resolution fails."""

    pass


def transform_github_url(url: str, prefer_zip: bool = True) -> str:
    """Transform a GitHub URL into a direct archive download URL.

    Handles various GitHub URL formats and transforms them into proper
    archive download URLs for tar.gz or .zip files.

    Supported URL formats:
        - https://github.com/owner/repo.git
        - https://github.com/owner/repo
        - https://github.com/owner/repo/tree/branch
        - https://github.com/owner/repo/tree/tag
        - https://github.com/owner/repo/commit/hash
        - https://github.com/owner/repo/releases/download/v1.0.0/file.zip

    Args:
        url: The GitHub URL to transform
        prefer_zip: If True, use .zip format; if False, use .tar.gz format

    Returns:
        Transformed archive download URL

    Raises:
        GitHubURLError: If URL cannot be transformed

    Examples:
        >>> transform_github_url("https://github.com/owner/repo")
        'https://github.com/owner/repo/archive/refs/heads/main.zip'

        >>> transform_github_url("https://github.com/owner/repo/tree/v1.0.0")
        'https://github.com/owner/repo/archive/refs/heads/v1.0.0.zip'

        >>> transform_github_url("https://github.com/owner/repo/commit/abc123")
        'https://github.com/owner/repo/archive/abc123.zip'
    """
    # Strip .git suffix if present
    url = url.rstrip("/")
    if url.endswith(".git"):
        url = url[:-4]

    # Parse URL
    parsed = urlparse(url)
    if parsed.netloc != "github.com":
        raise GitHubURLError(f"Not a GitHub URL: {url}")

    # Extract owner and repo from path
    path_parts = [p for p in parsed.path.split("/") if p]
    if len(path_parts) < 2:
        raise GitHubURLError(f"Invalid GitHub URL format: {url}")

    owner = path_parts[0]
    repo = path_parts[1]

    # Choose archive extension
    ext = "zip" if prefer_zip else "tar.gz"

    # Handle different URL patterns
    if len(path_parts) == 2:
        # Simple repo URL: https://github.com/owner/repo
        # Default to main branch
        return f"https://github.com/{owner}/{repo}/archive/refs/heads/main.{ext}"

    elif len(path_parts) >= 4 and path_parts[2] == "tree":
        # Branch or tag URL: https://github.com/owner/repo/tree/branch-or-tag
        ref = "/".join(path_parts[3:])  # Handle branches with slashes
        # Try to determine if it's a tag or branch (heuristic: tags often start with 'v')
        if ref.startswith("v") and re.match(r"v\d+", ref):
            return f"https://github.com/{owner}/{repo}/archive/refs/tags/{ref}.{ext}"
        else:
            return f"https://github.com/{owner}/{repo}/archive/refs/heads/{ref}.{ext}"

    elif len(path_parts) >= 4 and path_parts[2] == "commit":
        # Commit URL: https://github.com/owner/repo/commit/hash
        commit_hash = path_parts[3]
        return f"https://github.com/{owner}/{repo}/archive/{commit_hash}.{ext}"

    elif len(path_parts) >= 5 and path_parts[2] == "releases" and path_parts[3] == "download":
        # Release download URL: https://github.com/owner/repo/releases/download/v1.0.0/file.zip
        # Transform to archive URL using the version as the tag
        version = path_parts[4]
        return f"https://github.com/{owner}/{repo}/archive/refs/tags/{version}.{ext}"

    else:
        raise GitHubURLError(f"Unsupported GitHub URL format: {url}")


def verify_github_url(url: str, timeout: int = 10) -> Tuple[bool, Optional[str]]:
    """Verify that a GitHub archive URL exists using HTTP HEAD request.

    Args:
        url: The GitHub archive URL to verify
        timeout: Request timeout in seconds

    Returns:
        Tuple of (exists: bool, final_url: Optional[str])
        - exists: True if the URL is valid and returns 200
        - final_url: The final URL after redirects, or None if request failed

    Examples:
        >>> exists, final_url = verify_github_url(
        ...     "https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip"
        ... )
        >>> exists
        True
    """
    try:
        response = requests.head(url, allow_redirects=True, timeout=timeout)
        if response.status_code == 200:
            return True, response.url
        else:
            return False, None
    except requests.RequestException:
        return False, None


def transform_and_verify_github_url(url: str, prefer_zip: bool = True, timeout: int = 10) -> Tuple[str, bool, Optional[str]]:
    """Transform a GitHub URL and verify it exists.

    Convenience function that combines transform_github_url() and verify_github_url().

    Args:
        url: The GitHub URL to transform
        prefer_zip: If True, use .zip format; if False, use .tar.gz format
        timeout: Request timeout in seconds for verification

    Returns:
        Tuple of (transformed_url: str, exists: bool, final_url: Optional[str])
        - transformed_url: The transformed archive download URL
        - exists: True if the URL is valid and returns 200
        - final_url: The final URL after redirects, or None if request failed

    Raises:
        GitHubURLError: If URL cannot be transformed

    Examples:
        >>> url, exists, final = transform_and_verify_github_url(
        ...     "https://github.com/platformio/platform-espressif8266/tree/v4.2.1"
        ... )
        >>> exists
        True
    """
    transformed_url = transform_github_url(url, prefer_zip=prefer_zip)
    exists, final_url = verify_github_url(transformed_url, timeout=timeout)
    return transformed_url, exists, final_url


def resolve_platformio_platform_url(platform_spec: str, prefer_zip: bool = True) -> str:
    """Resolve a PlatformIO platform specification to a canonical download URL.

    PlatformIO supports various abbreviated formats for specifying platforms.
    This function resolves them to canonical GitHub archive download URLs.

    Supported formats:
        - "espressif8266" → latest version from platformio/platform-espressif8266
        - "espressif8266@4.2.1" → specific version
        - "platformio/espressif8266" → latest version from specific owner
        - "platformio/espressif8266@4.2.1" → owner + specific version
        - "https://github.com/owner/repo.git" → transformed to archive URL
        - Full URLs → passed through transform_github_url()

    Args:
        platform_spec: PlatformIO platform specification
        prefer_zip: If True, use .zip format; if False, use .tar.gz format

    Returns:
        Canonical GitHub archive download URL

    Raises:
        PlatformIOURLError: If platform specification is invalid

    Examples:
        >>> resolve_platformio_platform_url("espressif8266@4.2.1")
        'https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip'

        >>> resolve_platformio_platform_url("platformio/espressif8266@4.2.1")
        'https://github.com/platformio/platform-espressif8266/archive/refs/tags/v4.2.1.zip'

        >>> resolve_platformio_platform_url("espressif8266")
        'https://github.com/platformio/platform-espressif8266/archive/refs/heads/master.zip'
    """
    platform_spec = platform_spec.strip()

    # If it's already a URL, transform it using the GitHub URL handler
    if platform_spec.startswith("http://") or platform_spec.startswith("https://"):
        return transform_github_url(platform_spec, prefer_zip=prefer_zip)

    # Parse abbreviated format: [owner/]platform[@version]
    # Examples: "espressif8266@4.2.1", "platformio/espressif8266@4.2.1"
    version_match = re.match(r"^((?P<owner>[\w-]+)/)?(?P<platform>[\w-]+)(@(?P<version>[\w.-]+))?$", platform_spec)

    if not version_match:
        raise PlatformIOURLError(f"Invalid platform specification: {platform_spec}")

    owner = version_match.group("owner") or "platformio"
    platform = version_match.group("platform")
    version = version_match.group("version")

    # Ensure platform name has the "platform-" prefix for GitHub repo name
    if not platform.startswith("platform-"):
        repo_name = f"platform-{platform}"
    else:
        repo_name = platform

    # Choose archive extension
    ext = "zip" if prefer_zip else "tar.gz"

    # Construct GitHub archive URL
    if version:
        # Specific version - use tags
        # Add 'v' prefix if not present and version starts with a digit
        if not version.startswith("v") and version[0].isdigit():
            version = f"v{version}"
        return f"https://github.com/{owner}/{repo_name}/archive/refs/tags/{version}.{ext}"
    else:
        # No version specified - use default branch (usually 'master' for platformio)
        return f"https://github.com/{owner}/{repo_name}/archive/refs/heads/master.{ext}"


def resolve_and_verify_platformio_url(platform_spec: str, prefer_zip: bool = True, timeout: int = 10) -> Tuple[str, bool, Optional[str]]:
    """Resolve a PlatformIO platform specification and verify it exists.

    Convenience function that combines resolve_platformio_platform_url() and
    verify_github_url().

    Args:
        platform_spec: PlatformIO platform specification
        prefer_zip: If True, use .zip format; if False, use .tar.gz format
        timeout: Request timeout in seconds for verification

    Returns:
        Tuple of (resolved_url: str, exists: bool, final_url: Optional[str])
        - resolved_url: The resolved canonical download URL
        - exists: True if the URL is valid and returns 200
        - final_url: The final URL after redirects, or None if request failed

    Raises:
        PlatformIOURLError: If platform specification is invalid

    Examples:
        >>> url, exists, final = resolve_and_verify_platformio_url("espressif8266@4.2.1")
        >>> exists
        True
        >>> "platform-espressif8266" in url
        True
    """
    resolved_url = resolve_platformio_platform_url(platform_spec, prefer_zip=prefer_zip)
    exists, final_url = verify_github_url(resolved_url, timeout=timeout)
    return resolved_url, exists, final_url
