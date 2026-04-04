#!/usr/bin/env python3
"""Auto-configure zccache for local development as the Cargo rustc-wrapper.

Usage:
    uv run python ci/zccache_setup.py

If zccache is not installed, exits gracefully (zccache is optional for local dev).
Sets RUSTC_WRAPPER=zccache in .cargo/config.toml and starts the zccache daemon.
"""

import shutil
import subprocess
import re
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent.resolve()
PROJECT_ROOT = SCRIPT_DIR.parent
CARGO_CONFIG = PROJECT_ROOT / ".cargo" / "config.toml"

# Sections we preserve from the existing config
PRESERVED_SECTIONS = {"registries", "net"}


def find_zccache():
    """Check if zccache is installed."""
    return shutil.which("zccache")


def parse_config_sections(text):
    """Parse a TOML file into ordered sections.

    Returns list of (section_name, section_text) tuples.
    section_name is "" for content before the first section header.
    """
    sections = []
    current_name = ""
    current_lines = []

    for line in text.splitlines(keepends=True):
        # Match [section] or [section.subsection] headers
        m = re.match(r"^\s*\[([^\]]+)\]\s*$", line)
        if m:
            sections.append((current_name, "".join(current_lines)))
            current_name = m.group(1).split(".")[0].strip()
            current_lines = [line]
        else:
            current_lines.append(line)

    sections.append((current_name, "".join(current_lines)))
    return sections


def build_config():
    """Build .cargo/config.toml merging preserved sections with zccache settings."""
    # Read existing config
    existing_text = ""
    if CARGO_CONFIG.is_file():
        existing_text = CARGO_CONFIG.read_text(encoding="utf-8")

    # Parse existing sections
    existing_sections = parse_config_sections(existing_text)

    # Collect preserved sections
    preserved = {}
    for name, text in existing_sections:
        top = name.split(".")[0] if name else name
        if top in PRESERVED_SECTIONS:
            if top not in preserved:
                preserved[top] = ""
            preserved[top] += text

    # Build new config
    parts = []

    # Preserved sections first
    for section_name in sorted(preserved.keys()):
        parts.append(preserved[section_name].strip())

    # zccache as rustc-wrapper
    parts.append('[build]\nrustc-wrapper = "zccache"')

    return "\n\n".join(parts) + "\n"


def current_wrapper_from_config():
    """Read the current rustc-wrapper from .cargo/config.toml, or None."""
    if not CARGO_CONFIG.is_file():
        return None
    text = CARGO_CONFIG.read_text(encoding="utf-8")
    m = re.search(r'rustc-wrapper\s*=\s*"([^"]+)"', text)
    return m.group(1) if m else None


def main():
    # Step 1: Check zccache
    zccache_path = find_zccache()
    if not zccache_path:
        print("zccache not found — skipping local cache setup (optional).")
        return 0

    # Step 2: Get version
    try:
        result = subprocess.run(
            [zccache_path, "--version"],
            capture_output=True, text=True, check=True,
        )
        print(f"zccache {result.stdout.strip()}")
    except (FileNotFoundError, subprocess.CalledProcessError):
        pass

    # Step 3: Check idempotency
    current = current_wrapper_from_config()
    if current == "zccache":
        print("Config already up to date — nothing to do.")
    else:
        # Step 4: Write .cargo/config.toml
        CARGO_CONFIG.parent.mkdir(parents=True, exist_ok=True)
        new_config = build_config()
        CARGO_CONFIG.write_text(new_config, encoding="utf-8")
        print(f"Updated {CARGO_CONFIG}")

    # Step 5: Start the zccache daemon
    subprocess.run(
        [zccache_path, "start"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    print("Done. zccache is configured as rustc-wrapper for local dev.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
