#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Linter: keep the workspace at no more than 8 integration-test binaries.

Every top-level `crates/<crate>/tests/*.rs` file, and every
`tests/<dir>/main.rs`, is a separate test binary. Each one links its crate's
whole dependency graph again, and before FastLED/fbuild#1577 that linking took
most of CI's `Test` step. The workspace now has a fixed set of category
binaries (soldr's layout, zackees/ci.yml RUST-005). A new integration test goes
into one of them as a module, never into a new top-level file or directory.

Adding a category is a deliberate act: add it to `CATEGORIES` below with a
one-line reason, and keep the total at no more than `MAX_CATEGORIES`.

Usage:
    uv run --script ci/check_test_layout.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

MAX_CATEGORIES = 8

# (crate dir, tests/ subdir) -> what belongs there.
CATEGORIES: dict[tuple[str, str], str] = {
    ("fbuild-build", "it"): "build: real and synthetic build pipelines",
    ("fbuild-build", "env_isolated"): "tests that mutate FBUILD_*/ZCCACHE_* env vars, behind ENV_LOCK",
    ("fbuild-daemon", "it"): "daemon: HTTP/WS server and managers",
    ("fbuild-cli", "it"): "cli: command-line front end",
    ("fbuild-core", "it"): "config_resolution: PlatformIO resolution and core types",
    ("fbuild-packages", "it"): "packages: caches, lnk, toolchain provisioning",
    ("fbuild-library-select", "it"): "library_select: LDF and library selection",
    ("fbuild-python", "python_facades"): "python: needs libpython at link time",
}

REPO = Path(__file__).resolve().parent.parent


def violations(repo: Path = REPO) -> list[str]:
    problems: list[str] = []
    if len(CATEGORIES) > MAX_CATEGORIES:
        problems.append(f"{len(CATEGORIES)} categories declared; the limit is {MAX_CATEGORIES}")
    for tests in sorted((repo / "crates").glob("*/tests")):
        crate = tests.parent.name
        for entry in sorted(tests.iterdir()):
            rel = entry.relative_to(repo).as_posix()
            if entry.is_file() and entry.suffix == ".rs":
                problems.append(f"{rel}: top-level test file is its own binary; add it as a module of a category in ci/check_test_layout.py")
            elif entry.is_dir() and (entry / "main.rs").exists() and (crate, entry.name) not in CATEGORIES:
                problems.append(f"{rel}/main.rs: unknown test category; use an existing one")
    manifests = sorted((repo / "crates").glob("*/Cargo.toml")) + sorted((repo / "bench").glob("*/Cargo.toml"))
    for manifest in manifests:
        text = manifest.read_text(encoding="utf-8")
        rel = manifest.relative_to(repo).as_posix()
        if re.search(r"^\[\[test\]\]", text, re.MULTILINE):
            problems.append(f"{rel}: [[test]] targets bypass the category layout")
        for source in test_disabled_sources(manifest, text):
            if TEST_ATTR.search(source.read_text(encoding="utf-8")):
                problems.append(
                    f"{source.relative_to(repo).as_posix()}: has #[test] but its target sets `test = false` in {rel}, so it never runs"
                )
    return problems


TEST_ATTR = re.compile(r"#\[(tokio::)?test\b")


def test_disabled_sources(manifest: Path, text: str) -> list[Path]:
    """Source files of `[lib]`/`[[bin]]` targets that set `test = false`."""
    crate = manifest.parent
    sources: list[Path] = []
    for header, body in re.findall(r"^(\[lib\]|\[\[bin\]\])\n((?:(?!\[).*\n?)*)", text, re.MULTILINE):
        if not re.search(r"^test\s*=\s*false", body, re.MULTILINE):
            continue
        path = re.search(r'^path\s*=\s*"([^"]+)"', body, re.MULTILINE)
        entry = crate / (path.group(1) if path else ("src/lib.rs" if header == "[lib]" else "src/main.rs"))
        # A lib's unit tests can live anywhere under src/; a bin's in its file.
        sources.extend(sorted(entry.parent.rglob("*.rs")) if header == "[lib]" else [entry])
    return [s for s in sources if s.is_file()]


def main() -> int:
    problems = violations()
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    if problems:
        print(f"\n{len(problems)} test-layout violation(s); see FastLED/fbuild#1577", file=sys.stderr)
        return 1
    print(f"ok: {len(CATEGORIES)} integration-test categories, no stray test binaries")
    return 0


if __name__ == "__main__":
    sys.exit(main())
