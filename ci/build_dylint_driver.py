"""Retired source-build entry point for the Dylint driver.

The matching Dylint 6.0.3 driver for nightly-2026-05-28 is published in
soldr-toolchain for every native CI host. Keep this file as a fail-closed
pointer for callers that still use the old command.
"""

from __future__ import annotations

import sys


def main() -> int:
    print(
        "Dylint driver source builds are disabled; use `soldr dylint prepare` "
        "to fetch the verified prebuilt driver.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
