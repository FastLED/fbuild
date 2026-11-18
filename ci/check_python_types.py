"""Type-check every tracked Python file with basedpyright.

All Python in this repository is checked by basedpyright, configured in
``pyproject.toml`` (``[tool.basedpyright]``). This module decides only *which
files* are checked and *which interpreter* resolves their imports:

* the files are every ``git ls-files`` Python path, the same selection as the
  ruff complexity ratchet (``check_python_complexity.tracked_python_files``),
  so scripts that normal discovery skips are still covered;
* the interpreter is a uv environment holding the pinned basedpyright plus the
  third-party packages the scripts import (PEP 723 inline deps, workflow
  ``--with`` flags). basedpyright does not discover that environment on its
  own, so the launcher passes its own ``sys.executable`` as ``--pythonpath``.

``ci/test_check_python_types.py`` drives the same seams end to end, so a
dropped file, a loosened mode or an unresolved dependency fails a test instead
of passing silently.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_python_complexity import RatchetError, tracked_python_files  # noqa: E402
from render_workflows import CI_LINT_SHA  # noqa: E402

BASEDPYRIGHT_VERSION = "basedpyright==1.40.2"
PYTHON_VERSION = "3.12"
# Third-party modules imported by tracked scripts and tests. They are only
# needed to resolve imports; nothing here is executed.
TYPECHECK_DEPENDENCIES = (
    "boto3",
    "httpx",
    "pyserial",
    "pytest",
    "pyyaml",
    "zstandard",
    f"ci-lint @ git+https://github.com/zackees/ci.yml@{CI_LINT_SHA}",
)
# Runs inside the uv environment: hands basedpyright that environment's own
# interpreter, so the dependencies above resolve.
LAUNCHER = (
    "import subprocess, sys; "
    "raise SystemExit(subprocess.call([sys.executable, '-m', 'basedpyright', "
    "'--pythonpath', sys.executable, *sys.argv[1:]]))"
)


def basedpyright_command(paths: Sequence[str]) -> list[str]:
    """The pinned basedpyright invocation; ``paths`` are relative to the root."""

    command = ["uv", "run", "--no-project", "--python", PYTHON_VERSION]
    for requirement in (BASEDPYRIGHT_VERSION, *TYPECHECK_DEPENDENCIES):
        command.extend(["--with", requirement])
    return [*command, "python", "-c", LAUNCHER, *paths]


def check(root: Path) -> int:
    """Type-check ``root`` and return basedpyright's exit code."""

    try:
        paths = tracked_python_files(root)
    except RatchetError as error:
        print(error, file=sys.stderr)
        return 1
    return subprocess.run(
        basedpyright_command(paths), cwd=root, check=False
    ).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo", type=Path, default=Path(__file__).resolve().parents[1]
    )
    return check(parser.parse_args().repo.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
