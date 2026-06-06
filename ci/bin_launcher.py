"""Console-script entry point for the bundled `fbuild` binary.

Wheels produced by `pip install .` (see `setup.py`) ship a native
`fbuild[.exe]` binary inside `ci/bin/`. The wheel also declares this module
as the `fbuild` console script via `[project.scripts]` in `pyproject.toml`,
so `which fbuild` resolves to a tiny Python shim that lives next to this
launcher. The launcher then `execv`s the real binary, replacing the Python
process — so process semantics (PID, signal handling, stdio inheritance,
exit code) match what bare-cargo `target/release/fbuild` would give.

The release wheels published to PyPI by `ci/publish.py` follow the same
layout: pre-built native binary at `ci/bin/fbuild[.exe]`, this launcher as
the entry point.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

# IMPORTANT: keep this filename matching `TARGET_BINARY_NAME` in setup.py.
_BINARY_NAME = "fbuild.exe" if sys.platform == "win32" else "fbuild"


def main() -> None:
    binary = Path(__file__).resolve().parent / "bin" / _BINARY_NAME
    if not binary.exists():
        sys.stderr.write(
            f"ERROR: bundled fbuild binary not found at {binary}.\n"
            f"This wheel may be incomplete. Reinstall with `pip install --force-reinstall .` "
            f"from the fbuild source tree, or install a release wheel from PyPI.\n"
        )
        sys.exit(1)

    # execv replaces the Python process. argv[0] convention: pass the
    # binary path so help text and error messages reference the real
    # command, not the Python shim.
    os.execv(str(binary), [str(binary)] + sys.argv[1:])
