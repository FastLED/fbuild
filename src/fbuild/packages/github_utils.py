"""GitHub URL optimization utilities for fbuild.

This module provides utilities for working with GitHub repository URLs,
including converting them to optimized zip download URLs.
"""

from urllib.parse import urlparse


class GitHubURLOptimizer:
    """Optimizes GitHub URLs to use zip downloads instead of git clone."""

    @staticmethod
    def is_github_url(url: str) -> bool:
        """Check if a URL is a GitHub repository URL.

        Args:
            url: The URL to check

        Returns:
            True if the URL is a GitHub repository
        """
        parsed = urlparse(url)
        return parsed.netloc.lower() in ("github.com", "www.github.com")

    @staticmethod
    def detect_default_branch(url: str) -> str:
        """Detect the default branch name for a GitHub repository.

        Makes a HEAD request to determine if the repo uses 'main' or 'master'.

        Args:
            url: GitHub repository URL

        Returns:
            Default branch name ('main' or 'master')
        """
        try:
            import requests

            # Try main first (modern default)
            test_url = f"{url}/archive/refs/heads/main.zip"
            response = requests.head(test_url, timeout=5, allow_redirects=True)
            if response.status_code == 200:
                return "main"

            # Fall back to master
            test_url = f"{url}/archive/refs/heads/master.zip"
            response = requests.head(test_url, timeout=5, allow_redirects=True)
            if response.status_code == 200:
                return "master"

            # Default to main if both fail
            return "main"

        except KeyboardInterrupt as ke:
            from fbuild.interrupt_utils import handle_keyboard_interrupt_properly

            handle_keyboard_interrupt_properly(ke)
            raise  # Never reached, but satisfies type checker
        except Exception:
            # If we can't detect, default to main
            return "main"

    @classmethod
    def optimize_url(cls, url: str) -> str:
        """Convert a GitHub URL to use zip download instead of git clone.

        Transforms:
            https://github.com/FastLED/FastLED
        Into:
            https://github.com/FastLED/FastLED/archive/refs/heads/main.zip

        Also handles branch references:
            https://github.com/FastLED/FastLED#master
        Into:
            https://github.com/FastLED/FastLED/archive/refs/heads/master.zip

        Args:
            url: Original GitHub URL

        Returns:
            Optimized zip download URL
        """
        if not cls.is_github_url(url):
            return url

        # Extract branch from URL fragment (e.g., #master or #main)
        branch = None
        if "#" in url:
            url, branch = url.rsplit("#", 1)

        # Remove trailing slashes and .git suffix
        url = url.rstrip("/")
        if url.endswith(".git"):
            url = url[:-4]

        # Check if already a zip URL
        if "/archive/" in url:
            return url

        # If no branch specified, detect default branch
        if not branch:
            branch = cls.detect_default_branch(url)

        # Build zip URL
        return f"{url}/archive/refs/heads/{branch}.zip"
