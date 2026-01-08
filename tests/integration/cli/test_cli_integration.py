"""
Integration test for CLI command invocation.

This test validates that the CLI can be properly invoked and responds correctly.
"""

import os
import unittest

import pytest

COMMAND = "zap"


@pytest.mark.integration
class TestCLIIntegration(unittest.TestCase):
    """CLI integration test class."""

    def test_cli_help_invocation(self) -> None:
        """Test command line interface help flag."""
        # Test that the CLI can be invoked with --help (which returns 0)
        rtn = os.system(f"{COMMAND} --help")
        self.assertEqual(0, rtn)


if __name__ == "__main__":
    unittest.main()
