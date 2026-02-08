"""MSYS environment sanitizer for PlatformIO invocation.

On Windows/MSYS, PlatformIO fails because MSYS environment variables and
PATH entries confuse native Windows tools. This module strips those variables
before invoking pio.

On non-Windows platforms, the environment is returned unchanged.
"""

import os
import sys


def get_pio_safe_env() -> dict[str, str]:
    """Return a copy of os.environ with MSYS/MinGW variables stripped.

    Strips:
    - Variables with prefixes: MSYS*, MINGW*, CHERE*, ORIGINAL_PATH
    - Exact keys: SHELL, SHLVL, TERM, TERM_PROGRAM, TERM_PROGRAM_VERSION,
      TMPDIR, TMP, TEMP, _, !, POSIXLY_CORRECT, EXECIGNORE,
      HOSTTYPE, MACHTYPE, OSTYPE
    - PATH entries starting with "/" (MSYS-style paths)

    On non-Windows platforms, returns os.environ.copy() unchanged.
    """
    env = os.environ.copy()

    if sys.platform != "win32":
        return env

    # Prefixes and exact keys to strip
    strip_prefixes = ("MSYS", "MINGW", "CHERE", "ORIGINAL_PATH")
    strip_exact = frozenset(
        {
            "SHELL",
            "SHLVL",
            "TERM",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
            "TMPDIR",
            "TMP",
            "TEMP",
            "_",
            "!",
            "POSIXLY_CORRECT",
            "EXECIGNORE",
            "HOSTTYPE",
            "MACHTYPE",
            "OSTYPE",
        }
    )

    keys_to_strip = [k for k in env if k.startswith(strip_prefixes) or k in strip_exact]
    for k in keys_to_strip:
        env.pop(k, None)

    # Clean PATH: remove MSYS-style entries (start with "/")
    if "PATH" in env:
        parts = env["PATH"].split(";")
        cleaned = [p for p in parts if not p.startswith("/")]
        env["PATH"] = ";".join(cleaned)

    return env
