"""
Platform-specific utilities for deployment operations.

This module provides shared functionality for handling platform-specific
quirks and workarounds during deployment.
"""

import os
import sys
from typing import Dict


def get_filtered_env() -> Dict[str, str]:
    """Get environment with MSYS paths filtered out.

    On Windows, MSYS paths in the PATH environment variable can cause issues
    with native Python tools like esptool. This function returns a copy of
    the environment with MSYS paths removed.

    Returns:
        Environment dictionary with filtered PATH
    """
    env = os.environ.copy()

    # Strip MSYS paths that cause issues on Windows
    if sys.platform == "win32" and "PATH" in env:
        paths = env["PATH"].split(os.pathsep)
        filtered_paths = [p for p in paths if "msys" not in p.lower()]
        env["PATH"] = os.pathsep.join(filtered_paths)

    return env
