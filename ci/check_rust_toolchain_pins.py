"""Reject drift among fbuild-owned Rust MSRV/toolchain declarations."""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PIN = "1.95.0"
OLD_PIN = "1.94" + ".1"
OWNED_FILES = (
    ".clippy.toml",
    "Cargo.toml",
    "rust-toolchain.toml",
    "CLAUDE.md",
    "docs/DEVELOPMENT.md",
    ".github/workflows/msrv.yml",
    ".github/workflows/fmt.yml",
    ".github/workflows/dylint.yml",
    ".github/workflows/template_native_build.yml",
    ".github/workflows/platform-boundary-research.yml",
    ".github/workflows/README.md",
    "ci/docker-test-serial/run-test.sh",
    "ci/docker-mac-cross/README.md",
    "dylints/README.md",
)
HISTORICAL_1941 = {
    "docs/SOLDR_BUILD_PERF.md",
    "docs/platform-boundary-research.md",
    "tasks/baseline-205.md",
}
PIN_SCAN_EXCLUDED = HISTORICAL_1941 | {
    "ci/check_rust_toolchain_pins.py",
    "ci/test_rust_toolchain_pins.py",
}
SELECTOR = r"(?P<selector>[A-Za-z0-9][A-Za-z0-9._-]*)"
PATH_SELECTOR = r"(?P<selector>\d+(?:\.\d+){0,2}|stable|beta|nightly(?:-\d{4}-\d{2}-\d{2})?)"
PIN_FIELD_PATTERNS = tuple(
    re.compile(pattern, re.IGNORECASE)
    for pattern in (
        rf"^\s*(?:rust-version|msrv)\s*=\s*[\"']?{SELECTOR}",
        rf"^\s*toolchain\s*:\s*[\"']?{SELECTOR}",
        rf"\brustup\s+toolchain\s+install\s+{SELECTOR}",
        rf"--default-toolchain(?:=|\s+){SELECTOR}",
        rf"\btoolchains/{PATH_SELECTOR}-",
    )
)
PIN_FILE_CHANNEL = re.compile(rf"^\s*channel\s*=\s*[\"']?{SELECTOR}", re.IGNORECASE)
NIGHTLY_PIN = "nightly-2026-04-16"

PIN_PATTERNS = {
    ".clippy.toml": (r'^msrv\s*=\s*"{pin}"$',),
    "Cargo.toml": (r'^rust-version\s*=\s*"{pin}"$',),
    "rust-toolchain.toml": (r'^channel\s*=\s*"{pin}"$',),
    ".github/workflows/msrv.yml": (
        r"^name: Min Rust Version \({pin}\)$",
        r"^\s+name: Min Rust Version \({pin}\)$",
        r"^\s+toolchain: {pin}$",
    ),
    ".github/workflows/dylint.yml": (r"^\s+toolchain: {pin}$",),
    ".github/workflows/platform-boundary-research.yml": (r"^\s+toolchain: {pin}$",),
    ".github/workflows/template_native_build.yml": (
        r"rustup toolchain install {pin} --profile minimal",
        r'TOOLCHAIN_DIR="\$RUSTUP_HOME/toolchains/{pin}-',
    ),
    ".github/workflows/fmt.yml": (r"pinned {pin}",),
    ".github/workflows/README.md": (r"MSRV {pin} verification",),
    "CLAUDE.md": (r"MSRV: {pin} .* Toolchain: {pin} pinned",),
    "docs/DEVELOPMENT.md": (
        r"MSRV: {pin}",
        r"Toolchain: {pin} pinned",
    ),
    "ci/docker-test-serial/run-test.sh": (r"--default-toolchain {pin}",),
    "ci/docker-mac-cross/README.md": (r"pinned {pin} channel",),
    "dylints/README.md": (r"stable {pin}",),
}


def validate_owned_text(relative: str, text: str) -> list[str]:
    failures: list[str] = []
    if OLD_PIN in text:
        failures.append(f"stale fbuild-owned {OLD_PIN} pin in {relative}")
    for template in PIN_PATTERNS.get(relative, ()):
        pattern = template.format(pin=re.escape(PIN))
        if not re.search(pattern, text, re.MULTILINE):
            failures.append(f"canonical pin field drifted in {relative}: {template}")
    failures.extend(validate_discovered_pins(relative, text))
    return failures


def validate_discovered_pins(relative: str, text: str) -> list[str]:
    """Reject every noncanonical Rust pin field, including in new files."""
    if relative in PIN_SCAN_EXCLUDED:
        return []
    failures: list[str] = []
    patterns = list(PIN_FIELD_PATTERNS)
    if Path(relative).name in {"rust-toolchain", "rust-toolchain.toml"}:
        patterns.append(PIN_FILE_CHANNEL)
    for line_number, line in enumerate(text.splitlines(), 1):
        versions: set[str] = set()
        for pattern in patterns:
            match = pattern.search(line)
            if match is not None:
                versions.add(match.group("selector"))
        for version in versions:
            if version != PIN and not is_intentional_exception(relative, version):
                failures.append(f"noncanonical Rust pin {version} in {relative}:{line_number}")
    return failures


def is_intentional_exception(relative: str, selector: str) -> bool:
    """Allow the separately pinned nightly used only by Dylint."""
    if selector != NIGHTLY_PIN:
        return False
    return relative in {
        ".github/workflows/dylint.yml",
        "dylints/README.md",
        "dylints/ban_raw_subprocess/README.md",
    } or (relative.startswith("dylints/") and relative.endswith("/rust-toolchain.toml"))


def validate(root: Path = ROOT) -> list[str]:
    failures: list[str] = []
    for relative in OWNED_FILES:
        text = (root / relative).read_text(encoding="utf-8")
        failures.extend(validate_owned_text(relative, text))

    cargo = (root / "Cargo.toml").read_text(encoding="utf-8")
    toolchain = (root / "rust-toolchain.toml").read_text(encoding="utf-8")
    if not re.search(rf'^rust-version\s*=\s*"{re.escape(PIN)}"$', cargo, re.MULTILINE):
        failures.append("workspace rust-version does not equal the selected pin")
    if not re.search(rf'^channel\s*=\s*"{re.escape(PIN)}"$', toolchain, re.MULTILINE):
        failures.append("rust-toolchain channel does not equal the selected pin")
    clippy = (root / ".clippy.toml").read_text(encoding="utf-8")
    if not re.search(rf'^msrv\s*=\s*"{re.escape(PIN)}"$', clippy, re.MULTILINE):
        failures.append("Clippy MSRV does not equal the selected pin")

    for path in root.rglob("*"):
        if not path.is_file() or {
            ".git",
            ".cargo",
            ".venv",
            ".extern-repos",
            ".clud",
            ".fbuild",
            "node_modules",
            "target",
        } & set(path.parts):
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        relative = path.relative_to(root).as_posix()
        failures.extend(validate_discovered_pins(relative, text))
        if OLD_PIN not in text:
            continue
        if relative not in HISTORICAL_1941:
            failures.append(f"unexpected {OLD_PIN} declaration in {relative}")
    return sorted(set(failures))


def main() -> int:
    failures = validate()
    for failure in failures:
        print(f"rust-toolchain-pins: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
