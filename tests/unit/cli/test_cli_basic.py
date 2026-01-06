"""
Unit test file.
"""

import os
import unittest

COMMAND = "zap"


class MainTester(unittest.TestCase):
    """Main tester class."""

    def test_imports(self) -> None:
        """Test command line interface (CLI)."""
        # Test that the CLI can be invoked with --help (which returns 0)
        rtn = os.system(f"{COMMAND} --help")
        self.assertEqual(0, rtn)


if __name__ == "__main__":
    unittest.main()
