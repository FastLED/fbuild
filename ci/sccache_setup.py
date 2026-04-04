#!/usr/bin/env python3
"""Auto-configure sccache for local development with per-compiler-version cache isolation.

Usage:
    uv run python ci/sccache_setup.py

If sccache is not installed, exits gracefully (sccache is optional for local dev).
Creates a versioned cache directory at .sccache/<version>-<commit>/ and updates
.cargo/config.toml with rustc-wrapper and SCCACHE_DIR settings.
"""

import re
import shutil
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent.resolve()
PROJECT_ROOT = SCRIPT_DIR.parent
CARGO_CONFIG = PROJECT_ROOT / ".cargo" / "config.toml"

# Sections we preserve from the existing config
PRESERVED_SECTIONS = {"registries", "net"}


def find_sccache():
    """Check if sccache is installed."""
    return shutil.which("sccache")


def get_rustc_version_info():
    """Get rustc version and commit hash.

    Returns (version, commit_short) e.g. ("1.85.1", "abc12345") or None.
    """
    try:
        result = subprocess.run(
            ["rustc", "--version", "--verbose"],
            capture_output=True, text=True, check=True,
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        return None

    version = None
    commit = None
    for line in result.stdout.splitlines():
        line = line.strip()
        if line.startswith("release:"):
            version = line.split(":", 1)[1].strip()
        elif line.startswith("commit-hash:"):
            full_hash = line.split(":", 1)[1].strip()
            commit = full_hash[:8]

    if version and commit:
        return (version, commit)
    return None


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


def build_config(sccache_dir):
    """Build .cargo/config.toml merging preserved sections with sccache settings."""
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
            # Accumulate under the top-level key
            if top not in preserved:
                preserved[top] = ""
            preserved[top] += text

    # Build new config
    parts = []

    # Preserved sections first
    for section_name in sorted(preserved.keys()):
        parts.append(preserved[section_name].strip())

    # sccache build section
    sccache_path = str(sccache_dir).replace("\\", "/")
    parts.append(f'[build]\nrustc-wrapper = "sccache"')
    parts.append(f'[env]\nSCCACHE_DIR = "{sccache_path}"')

    return "\n\n".join(parts) + "\n"


def current_sccache_dir_from_config():
    """Read the current SCCACHE_DIR from .cargo/config.toml, or None."""
    if not CARGO_CONFIG.is_file():
        return None
    text = CARGO_CONFIG.read_text(encoding="utf-8")
    m = re.search(r'SCCACHE_DIR\s*=\s*"([^"]+)"', text)
    return m.group(1) if m else None


def main():
    # Step 1: Check sccache
    if not find_sccache():
        print("sccache not found — skipping local cache setup (optional).")
        return 0

    # Step 2: Get rustc version info
    info = get_rustc_version_info()
    if not info:
        print("Cannot determine rustc version. Ensure rustc is on PATH.", file=sys.stderr)
        return 1

    version, commit = info
    print(f"rustc {version} (commit {commit})")

    # Step 3: Create versioned cache directory
    cache_dir = PROJECT_ROOT / ".sccache" / f"{version}-{commit}"
    cache_dir.mkdir(parents=True, exist_ok=True)
    cache_dir_str = str(cache_dir.resolve()).replace("\\", "/")
    print(f"Cache directory: {cache_dir_str}")

    # Step 4: Check idempotency
    current = current_sccache_dir_from_config()
    if current == cache_dir_str:
        print("Config already up to date — nothing to do.")
        return 0

    # Step 5: Write .cargo/config.toml
    CARGO_CONFIG.parent.mkdir(parents=True, exist_ok=True)
    new_config = build_config(cache_dir.resolve())
    CARGO_CONFIG.write_text(new_config, encoding="utf-8")
    print(f"Updated {CARGO_CONFIG}")

    # Step 6: Restart sccache if cache dir changed
    if current and current != cache_dir_str:
        print("Cache directory changed — restarting sccache server...")
        subprocess.run(["sccache", "--stop-server"], capture_output=True)

    print("Done. sccache is configured for local dev.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
