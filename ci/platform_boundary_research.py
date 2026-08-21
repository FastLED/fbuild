"""Generate the phase-1 host-platform boundary research inventory.

This source walker is deliberately host independent: it scans every handwritten
Rust file under ``crates/`` without relying on cfg expansion, so Windows, Linux,
and macOS observe the same inactive branches.  Phase 2 replaces this research
inventory with the authoritative Dylint/parser ledger; this file provides the
reviewed input and a cross-host drift check for FastLED/fbuild#1307.
"""

from __future__ import annotations

import argparse
import dataclasses
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INVENTORY = ROOT / "ci/platform_boundary_research.tsv"

HOST_KEYS = {
    "unix",
    "windows",
    "target_abi",
    "target_arch",
    "target_endian",
    "target_env",
    "target_family",
    "target_feature",
    "target_os",
    "target_pointer_width",
    "target_vendor",
}
NATIVE_ROOTS = {
    "interprocess",
    "libc",
    "mach2",
    "nix",
    "portable_pty",
    "socket2",
    "winapi",
    "windows",
    "windows_sys",
}

CFG_ATTRIBUTE_START = re.compile(r"#\s*!?\s*\[\s*cfg(?:_attr)?\s*\(")
CFG_MACRO_START = re.compile(r"\bcfg\s*!\s*\(")
IDENTIFIER = re.compile(r"\b[A-Za-z_][A-Za-z0-9_]*\b")
NATIVE_PATHS = (
    re.compile(r"\bstd\s*::\s*env\s*::\s*current_exe\b"),
    re.compile(
        r"\bstd\s*::\s*os\s*::\s*(?:windows|unix|linux|macos)\b"
        r"(?:\s*::\s*[A-Za-z_][A-Za-z0-9_]*)*"
    ),
    re.compile(r"\b(?:windows_sys|winapi|libc|mach2|nix|portable_pty|socket2)\s*::"),
    re.compile(r"\bwindows\s*::\s*Win32\b"),
    re.compile(r"\binterprocess\s*::\s*os\s*::\s*(?:windows|unix)\b"),
    re.compile(r"\binterprocess\s*::\s*local_socket\b"),
    re.compile(r"\btokio\s*::\s*net\s*::\s*(?:windows|UnixListener|UnixStream)\b"),
    re.compile(r"\bwindows\s*::"),
)
SINGLE_NATIVE_USE = re.compile(
    r"\buse\s+(interprocess|libc|mach2|nix|portable_pty|socket2|winapi|windows|windows_sys)"
    r"\s*(?:as\s+[A-Za-z_][A-Za-z0-9_]*\s*)?;"
)
COMPILE_HOST_CONST = re.compile(r"\bstd\s*::\s*env\s*::\s*consts\s*::\s*(?:OS|ARCH)\b")
COMPILE_HOST_MACRO = re.compile(r"\b(?:env|option_env)\s*!\s*\(\s*\"CARGO_CFG_TARGET_[A-Z_]+\"")
CONCRETE_MODULE = re.compile(r"\b(?:platform_imp|platform_win|platform_windows|platform_linux|platform_macos)\b")
TARGET_TABLE = re.compile(r"^\s*\[target\.(.+)\.(?:build-|dev-)?dependencies\]\s*$")
DEPENDENCY = re.compile(r"^\s*([A-Za-z0-9_-]+)\s*=")
FUNCTION_START = re.compile(r"\b(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(")
CHAR_LITERAL = re.compile(r"(?:b)?'(?:\\.|[^\\'\n])'")

ESP_QEMU_FS_CONTEXTS = {
    "preflight_ok_when_binary_runs_version_successfully",
    "preflight_linux_detects_missing_shared_library_exit_127",
}


@dataclasses.dataclass(frozen=True, order=True)
class Finding:
    path: str
    line: int
    kind: str
    normalized: str
    capability: str
    classification: str

    def tsv(self) -> str:
        return "\t".join(
            (
                self.path,
                str(self.line),
                self.kind,
                self.normalized,
                self.capability,
                self.classification,
            )
        )


def code_only(text: str) -> str:
    """Blank Rust comments and string contents while preserving offsets/newlines."""
    out = list(text)
    index = 0
    block_depth = 0
    while index < len(text):
        pair = text[index : index + 2]
        if block_depth:
            if pair == "/*":
                block_depth += 1
                out[index] = out[index + 1] = " "
                index += 2
            elif pair == "*/":
                block_depth -= 1
                out[index] = out[index + 1] = " "
                index += 2
            else:
                if text[index] != "\n":
                    out[index] = " "
                index += 1
            continue
        raw = None
        if index == 0 or not (text[index - 1].isalnum() or text[index - 1] == "_"):
            raw = re.match(r'(?:b)?r(#{0,255})"', text[index:])
        if raw:
            terminator = '"' + raw.group(1)
            cursor = text.find(terminator, index + raw.end())
            cursor = len(text) if cursor < 0 else cursor + len(terminator)
            for position in range(index, cursor):
                if text[position] != "\n":
                    out[position] = " "
            index = cursor
            continue
        character = CHAR_LITERAL.match(text, index)
        if character:
            for position in range(index, character.end()):
                out[position] = " "
            index = character.end()
            continue
        if pair == "//":
            end = text.find("\n", index)
            end = len(text) if end < 0 else end
            for cursor in range(index, end):
                out[cursor] = " "
            index = end
            continue
        if pair == "/*":
            block_depth = 1
            out[index] = out[index + 1] = " "
            index += 2
            continue
        if text[index] == '"':
            cursor = index + 1
            while cursor < len(text):
                if text[cursor] == "\\":
                    cursor += 2
                    continue
                if text[cursor] == '"':
                    cursor += 1
                    break
                cursor += 1
            for position in range(index, min(cursor, len(text))):
                if text[position] != "\n":
                    out[position] = " "
            index = cursor
            continue
        index += 1
    return "".join(out)


def matching_delimiter(text: str, opening: int, left: str, right: str) -> int:
    depth = 0
    for index in range(opening, len(text)):
        if text[index] == left:
            depth += 1
        elif text[index] == right:
            depth -= 1
            if depth == 0:
                return index
    return len(text) - 1


def normalized_construct(text: str) -> str:
    return re.sub(r"\s+", "", text)


def line_at(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def enclosing_function(text: str, offset: int) -> str:
    """Return the function owning an offset, including an attribute on that function."""
    starts = list(FUNCTION_START.finditer(text))
    for match in reversed(starts):
        if match.start() > offset:
            continue
        opening = text.find("{", match.end())
        if opening >= 0 and offset <= matching_delimiter(text, opening, "{", "}"):
            return match.group(1)

    for match in starts:
        if match.start() <= offset:
            continue
        prefix = text[offset : match.start()]
        if len(prefix) <= 256 and "{" not in prefix and ";" not in prefix:
            return match.group(1)
        break
    return ""


def classify(path: str, kind: str, normalized: str = "", context: str = "") -> tuple[str, str]:
    """Assign the phase-1 owner class; phase 2 validates this per occurrence."""
    if path.startswith("crates/fbuild-core/src/platform/") and path.endswith("/ipc.rs"):
        return "ipc", "host_mechanic"
    if path == "crates/fbuild-core/\x43argo.toml":
        unix_table = "[target.'cfg(unix)'.dependencies]"
        windows_table = "[target.'cfg(windows)'.dependencies]"
        if kind == "target_dependency_table" and normalized in {unix_table, windows_table}:
            return "fs", "host_mechanic"
        if kind == "native_dependency" and (normalized, context) in {
            ("libc", unix_table),
            ("windows-sys", windows_table),
        }:
            return "fs", "host_mechanic"
        if kind == "native_dependency" and normalized in {"interprocess", "socket2"}:
            return "ipc", "host_mechanic"
    if kind in {"native_import", "native_path", "native_dependency"}:
        if normalized == "std::env::current_exe":
            return "host_executable", "host_mechanic"
        if "::fs" in normalized or "permissions" in normalized.lower():
            return "fs", "host_mechanic"
        if "/fbuild-serial/" in f"/{path}/" or "/fbuild-deploy/" in f"/{path}/":
            return "device", "host_mechanic"
        return "process", "host_mechanic"
    if "/fbuild-toolchain/" in f"/{path}/":
        # esp_qemu mixes artifact selection with concrete host runtime and
        # test-fixture mechanics. Review these occurrences individually instead
        # of granting the whole file the artifact-policy classification.
        if path.endswith("/esp_qemu.rs") and context in ESP_QEMU_FS_CONTEXTS:
            return "fs", "host_mechanic"
        if path.endswith("/esp_qemu.rs") and kind == "attr_cfg":
            return "host_executable", "host_mechanic"
        return "host_executable", "host_artifact_policy"
    if "/fbuild-serial/" in f"/{path}/" or "/fbuild-deploy/" in f"/{path}/":
        return "device", "host_mechanic"
    if "/fbuild-daemon/src/broker/" in f"/{path}/" or "daemon_client" in path:
        return "ipc", "host_mechanic"
    if any(token in path for token in ("containment", "subprocess", "process_identity")):
        return "process", "host_mechanic"
    if any(token in path for token in ("path.rs", "response_file", "disk_cache", "install_lock")):
        return "fs", "host_mechanic"
    if any(token in path for token in ("emulator", "esptool", "library_compiler")):
        return "host_executable", "host_artifact_policy"
    return "host", "host_mechanic"


def source_files(root: Path = ROOT) -> list[Path]:
    return sorted(path for path in (root / "crates").rglob("*.rs") if "target" not in path.parts and not any(part.startswith(".") for part in path.parts))


def scan_rust(path: Path, root: Path = ROOT) -> list[Finding]:
    relative = path.relative_to(root).as_posix()
    original = path.read_text(encoding="utf-8")
    code = code_only(original)
    findings: list[Finding] = []

    for start_pattern, kind, closing in (
        (CFG_ATTRIBUTE_START, "attr_cfg", "]"),
        (CFG_MACRO_START, "cfg_macro", ")"),
    ):
        for match in start_pattern.finditer(code):
            opening = code.find("[" if closing == "]" else "(", match.start())
            end = matching_delimiter(code, opening, "[" if closing == "]" else "(", closing)
            construct = code[match.start() : end + 1]
            if not (HOST_KEYS & set(IDENTIFIER.findall(construct))):
                continue
            normalized = normalized_construct(construct)
            line = line_at(original, match.start())
            capability, classification = classify(relative, kind, normalized, enclosing_function(code, match.start()))
            findings.append(
                Finding(
                    relative,
                    line,
                    kind,
                    normalized,
                    capability,
                    classification,
                )
            )

    native_spans: list[tuple[int, int]] = []
    for match in SINGLE_NATIVE_USE.finditer(code):
        start, end = match.span(1)
        native_spans.append((start, end))
        normalized = match.group(1)
        line = line_at(original, start)
        capability, classification = classify(
            relative,
            "native_path",
            normalized,
            enclosing_function(code, start),
        )
        findings.append(
            Finding(
                relative,
                line,
                "native_path",
                normalized,
                capability,
                classification,
            )
        )
    for pattern in NATIVE_PATHS:
        for match in pattern.finditer(code):
            if any(start <= match.start() < end for start, end in native_spans):
                continue
            native_spans.append(match.span())
            normalized = normalized_construct(match.group(0))
            line = line_at(original, match.start())
            capability, classification = classify(
                relative,
                "native_path",
                normalized,
                enclosing_function(code, match.start()),
            )
            findings.append(
                Finding(
                    relative,
                    line,
                    "native_path",
                    normalized,
                    capability,
                    classification,
                )
            )
    compile_host_matches = list(COMPILE_HOST_CONST.finditer(code))
    compile_host_matches.extend(match for match in COMPILE_HOST_MACRO.finditer(original) if code[match.start() : match.start() + len(match.group(0).split("!", 1)[0])].strip())
    for match in sorted(compile_host_matches, key=lambda item: item.start()):
        normalized = normalized_construct(match.group(0))
        line = line_at(original, match.start())
        capability, classification = classify(
            relative,
            "compile_host_fact",
            normalized,
            enclosing_function(code, match.start()),
        )
        findings.append(
            Finding(
                relative,
                line,
                "compile_host_fact",
                normalized,
                capability,
                classification,
            )
        )
    for match in CONCRETE_MODULE.finditer(code):
        line = line_at(original, match.start())
        capability, classification = classify(
            relative,
            "concrete_module_ref",
            match.group(0),
            enclosing_function(code, match.start()),
        )
        findings.append(
            Finding(
                relative,
                line_at(original, match.start()),
                "concrete_module_ref",
                match.group(0),
                capability,
                classification,
            )
        )
    return findings


def scan_manifests(root: Path = ROOT) -> list[Finding]:
    findings: list[Finding] = []
    for manifest in sorted((root / "crates").glob("*/Cargo.toml")):
        relative = manifest.relative_to(root).as_posix()
        current_target: str | None = None
        for line_number, line in enumerate(manifest.read_text(encoding="utf-8").splitlines(), 1):
            table = TARGET_TABLE.match(line)
            if table:
                normalized = normalized_construct(table.group(0))
                current_target = normalized
                capability, classification = classify(relative, "target_dependency_table", normalized)
                findings.append(
                    Finding(
                        relative,
                        line_number,
                        "target_dependency_table",
                        normalized,
                        capability,
                        classification,
                    )
                )
                continue
            if line.lstrip().startswith("["):
                current_target = None
            dependency = DEPENDENCY.match(line)
            if dependency and dependency.group(1).replace("-", "_") in NATIVE_ROOTS:
                capability, classification = classify(
                    relative,
                    "native_dependency",
                    dependency.group(1),
                    current_target or "",
                )
                findings.append(
                    Finding(
                        relative,
                        line_number,
                        "native_dependency",
                        dependency.group(1),
                        capability,
                        classification,
                    )
                )
            elif current_target and dependency:
                # The table itself is inventoried once; ordinary dependencies inside it
                # are not native-boundary findings unless named above.
                continue
    return findings


def inventory(root: Path = ROOT) -> list[Finding]:
    findings = [finding for path in source_files(root) for finding in scan_rust(path, root)]
    findings.extend(scan_manifests(root))
    return sorted(findings)


def render(findings: list[Finding]) -> str:
    header = "path\tline\tkind\tnormalized\tcapability\tclassification"
    return header + "\n" + "\n".join(finding.tsv() for finding in findings) + "\n"


def totals(findings: list[Finding]) -> str:
    from collections import Counter

    kinds = Counter(finding.kind for finding in findings)
    capabilities = Counter(finding.capability for finding in findings)
    classifications = Counter(finding.classification for finding in findings)
    return "; ".join(
        (
            f"rows={len(findings)}",
            "kinds=" + ",".join(f"{key}:{kinds[key]}" for key in sorted(kinds)),
            "capabilities=" + ",".join(f"{key}:{capabilities[key]}" for key in sorted(capabilities)),
            "classifications=" + ",".join(f"{key}:{classifications[key]}" for key in sorted(classifications)),
        )
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if the committed inventory drifts")
    parser.add_argument("--write", action="store_true", help="rewrite the research inventory")
    parser.add_argument("--host-label", help="label printed for cross-host CI evidence")
    parser.add_argument("--print-totals", action="store_true")
    args = parser.parse_args(argv)
    findings = inventory()
    rendered = render(findings)
    if args.write:
        INVENTORY.write_text(rendered, encoding="utf-8", newline="\n")
    if args.check:
        try:
            committed = INVENTORY.read_text(encoding="utf-8")
        except OSError as error:
            print(f"platform-boundary-research: {error}", file=sys.stderr)
            return 1
        if committed != rendered:
            print(
                "platform-boundary-research: committed inventory is stale; run `uv run --no-project python ci/platform_boundary_research.py --write`",
                file=sys.stderr,
            )
            return 1
    if args.print_totals or args.host_label:
        prefix = f"host={args.host_label}; " if args.host_label else ""
        print(prefix + totals(findings))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
