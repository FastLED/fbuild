"""GCC Response File Utility.

GCC supports response files (@filename): all flags are written to a file,
and the compiler is invoked with @path instead of the flags inline. This
avoids Windows' 32K character CreateProcess limit.

zccache handles @file arguments natively — it expands them internally for cache key
computation but passes them through unchanged to the compiler.
"""

from pathlib import Path
from typing import List


def write_response_file(output_dir: Path, flags: List[str], prefix: str) -> str:
    """Write flags to a .rsp file, return '@path' argument for GCC.

    Args:
        output_dir: Directory to write the .rsp file into
        flags: List of flags to write (one per line)
        prefix: Unique prefix for the .rsp filename (e.g., source file stem, library name)

    Returns:
        '@{absolute_path}' string ready to insert into compiler command
    """
    output_dir.mkdir(parents=True, exist_ok=True)
    rsp_path = output_dir / f"{prefix}.rsp"

    lines = []
    for flag in flags:
        # GCC's response file parser treats backslash as an escape character.
        # Convert all backslashes to forward slashes — GCC/G++/LD accept
        # forward slashes on Windows, and this prevents \n, \t, etc. in
        # Windows paths from being interpreted as escape sequences.
        flag = flag.replace(chr(92), "/")
        # Quote paths that contain spaces
        if " " in flag:
            lines.append(f'"{flag}"')
        else:
            lines.append(flag)

    rsp_path.write_text("\n".join(lines), encoding="utf-8")

    # Use forward slashes for GCC compatibility on Windows
    return f"@{str(rsp_path.resolve()).replace(chr(92), '/')}"
