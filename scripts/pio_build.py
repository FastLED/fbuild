"""Launch PlatformIO build via cmd.exe with MSYS env stripped."""

import os
import subprocess
import sys


def main():
    # Start with current environment
    env = dict(os.environ)

    # Strip MSYS/MinGW variables that confuse PlatformIO
    strip_prefixes = ("MSYS", "MINGW", "CHERE", "ORIGINAL_PATH")
    strip_keys = [k for k in env if k.startswith(strip_prefixes) or k in (
        "SHELL", "SHLVL", "TERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION",
        "TMPDIR", "TMP", "TEMP", "_", "!",
        "POSIXLY_CORRECT", "EXECIGNORE", "HOSTTYPE", "MACHTYPE", "OSTYPE",
    )]
    for k in strip_keys:
        env.pop(k, None)

    # Fix PATH: convert MSYS paths to Windows paths
    # Remove /usr/bin, /bin, /mingw64/bin etc. and keep Windows paths
    if "PATH" in env:
        parts = env["PATH"].split(";")
        cleaned = [p for p in parts if not p.startswith("/")]
        env["PATH"] = ";".join(cleaned)

    # Build the pio command
    project_dir = sys.argv[1] if len(sys.argv) > 1 else "."
    pio_env = sys.argv[2] if len(sys.argv) > 2 else "esp32dev"

    cmd = ["pio", "run", "-d", project_dir, "-e", pio_env]

    print(f"Running: {' '.join(cmd)}")
    result = subprocess.run(cmd, env=env)
    sys.exit(result.returncode)


if __name__ == "__main__":
    main()
