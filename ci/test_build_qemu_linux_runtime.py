"""Tests for ci/build_qemu_linux_runtime.py -- the QEMU runtime bundle builder.

`ci/build_qemu_linux_runtime.py` produces the artifact every Linux host
downloads when it cannot start Espressif's QEMU on its own: a `tar.zst` of the
emulators' transitive library closure. `esp_qemu_runtime.rs` names that artifact
by composing `qemu-esp-linux-runtime-<arch>-<RUNTIME_VERSION>.tar.zst`, so the
builder and the consumer are joined only by hand-copied constants -- and by a
comment asking the next person to keep them in sync.

Nothing exercised this script (`ci/gate_script_tests.toml` carried it as the last
untested workflow-invoked gate), and three of its "proofs" were not proofs:

  * the closure walk ran `for _round in 1 2 3 4 5 6 7 8` and simply *ended* when
    the frontier was still non-empty, so a graph deeper than the cap produced a
    silently truncated archive;
  * completeness was judged by five hand-typed names, while the `--version`
    selftest that was supposed to prove the closure removed only three of the
    five apt packages (`libgcrypt20` and `zlib1g` stay installed in the image,
    `zlib1g` being `priority: required`), so a gap behind libgcrypt or zlib
    could never be observed;
  * a truncated `tar | zstd` output copies out of the container with exit code
    0 and is hashed and published like any other archive.

The tests below cover the fixed contract three ways: the Python half is driven
end to end through a fake `docker` executable on `PATH`, the bash closure logic
is extracted from the rendered payload and executed against a stubbed `ldd`
(so the real walk runs, not a paraphrase of it), and the constants shared with
Rust are parsed out of the `.rs` sources and compared.
"""

from __future__ import annotations

import hashlib
import os
import re
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from ci import build_qemu_linux_runtime as bq

ROOT = Path(__file__).resolve().parents[1]
ESP_QEMU_RS = ROOT / "crates/fbuild-toolchain/src/toolchain/esp_qemu.rs"
RUNTIME_RS = ROOT / "crates/fbuild-toolchain/src/toolchain/esp_qemu_runtime.rs"
CI_README = ROOT / "ci/README.md"


def _render() -> str:
    return bq.render_script("x86_64")


def _rust_const(path: Path, name: str) -> str:
    text = path.read_text(encoding="utf-8")
    match = re.search(rf'const\s+{name}:\s*&str\s*=\s*"([^"]+)"', text)
    if match is None:
        raise AssertionError(f"{name} not found in {path}")
    return match.group(1)


def _rust_str_array(path: Path, name: str) -> list[str]:
    text = path.read_text(encoding="utf-8")
    match = re.search(rf"const\s+{name}:\s*&\[&str\]\s*=\s*&\[(.*?)\];", text, re.S)
    if match is None:
        raise AssertionError(f"{name} not found in {path}")
    return re.findall(r'"([^"]+)"', match.group(1))


def _logic_block(rendered: str) -> str:
    start = rendered.index(bq.LOGIC_BEGIN)
    end = rendered.index(bq.LOGIC_END, start)
    return rendered[start : end + len(bq.LOGIC_END)]


class PayloadRendering(unittest.TestCase):
    """The payload must be fully substituted and carry the shared constants."""

    def test_no_placeholder_survives(self) -> None:
        for arch in bq.DOCKER_PLATFORM:
            self.assertIsNone(
                re.search(r"__[A-Z][A-Z_]*__", bq.render_script(arch)),
                f"{arch} payload kept a placeholder",
            )
            self.assertNotIn("__", bq.render_script(arch))

    def test_architecture_specific_tokens(self) -> None:
        x86 = bq.render_script("x86_64")
        arm = bq.render_script("aarch64")
        self.assertIn('ARCH_SUFFIX="x86_64-linux-gnu"', x86)
        self.assertIn('ARCH_SUFFIX="aarch64-linux-gnu"', arm)
        self.assertIn(bq.out_name("x86_64"), x86)
        self.assertIn(bq.out_name("aarch64"), arm)
        self.assertNotIn(bq.out_name("aarch64"), x86)

    def test_tag_and_version_come_from_the_module_constants(self) -> None:
        rendered = _render()
        self.assertIn(f'TAG="{bq.QEMU_RELEASE_TAG}"', rendered)
        self.assertIn(f'VERSION="{bq.QEMU_ARCHIVE_VERSION}"', rendered)

    def test_required_libraries_are_injected_once(self) -> None:
        rendered = _render()
        self.assertIn(
            "for required in " + " ".join(bq.REQUIRED_LIBS) + "; do", rendered
        )
        self.assertIn(
            f"fewer than the {len(bq.REQUIRED_LIBS)} required libraries", rendered
        )

    def test_glibc_family_pattern_is_generated(self) -> None:
        rendered = _render()
        for name in bq.GLIBC_FAMILY:
            self.assertIn(name, rendered)
        self.assertIn("ld-linux*", rendered)
        self.assertIn("is_glibc_family", rendered)

    def test_round_cap_is_a_testable_number(self) -> None:
        rendered = _render()
        self.assertIn(f"MAX_CLOSURE_ROUNDS={bq.MAX_CLOSURE_ROUNDS}", rendered)
        # The silent-truncation form this replaced.
        self.assertNotIn("for _round in 1 2 3", rendered)

    def test_the_payload_is_valid_bash(self) -> None:
        """`bash -n` the rendered script: a typo here only shows up in a container."""
        for arch in bq.DOCKER_PLATFORM:
            proc = subprocess.run(
                ["bash", "-n"],
                input=bq.render_script(arch),
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, f"{arch}: {proc.stderr}")

    def test_the_archive_is_inspected_without_tar_zstd(self) -> None:
        """tar 1.30 (the build image) has no `--zstd`; `zstd -dc | tar -t` works."""
        rendered = _render()
        self.assertNotIn("tar --zstd -tf", rendered)
        self.assertIn('zstd -dc "$WORK/${OUT_NAME}" | tar -tf -', rendered)

    def test_logic_markers_survive(self) -> None:
        rendered = _render()
        self.assertGreater(len(_logic_block(rendered)), 500)


class SharedConstants(unittest.TestCase):
    """Constants duplicated with Rust must be equal, not "kept in sync"."""

    def test_bundle_tag_matches_the_qemu_release(self) -> None:
        self.assertEqual(
            bq.QEMU_RELEASE_TAG, _rust_const(ESP_QEMU_RS, "QEMU_RELEASE_TAG")
        )

    def test_bundle_tag_matches_the_runtime_consumer(self) -> None:
        self.assertEqual(
            bq.QEMU_RELEASE_TAG, _rust_const(RUNTIME_RS, "RUNTIME_VERSION")
        )

    def test_archive_version_is_the_tag_with_underscores(self) -> None:
        self.assertEqual(bq.QEMU_ARCHIVE_VERSION, bq.QEMU_RELEASE_TAG.replace("-", "_"))

    def test_required_libraries_match_the_rust_install_check(self) -> None:
        rust = _rust_str_array(RUNTIME_RS, "REQUIRED_LIBS")
        self.assertEqual(len(rust), len(set(rust)), "duplicate entry in the Rust list")
        self.assertEqual(sorted(bq.REQUIRED_LIBS), sorted(rust))

    def test_architectures_agree_across_the_three_tables(self) -> None:
        self.assertEqual(sorted(bq.DOCKER_PLATFORM), sorted(bq.QEMU_ARCHIVE_SUFFIX))
        self.assertEqual(sorted(bq.DOCKER_PLATFORM), [a for a in ("aarch64", "x86_64")])

    def test_readme_documents_the_build_image_actually_used(self) -> None:
        text = CI_README.read_text(encoding="utf-8")
        self.assertIn(bq.BUILD_IMAGE, text)


class FakeDocker:
    """A `docker` on PATH that records argv and fabricates container output."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.drop = root / "drop"
        self.drop.mkdir(parents=True, exist_ok=True)
        self.payloads = root / "payloads"
        self.payloads.mkdir(parents=True, exist_ok=True)
        self.log = root / "docker.log"
        self.bin = root / "bin"
        self.bin.mkdir(parents=True, exist_ok=True)
        self.log.touch()
        script = self.bin / "docker"
        script.write_text(_FAKE_DOCKER.format(python=sys.executable), encoding="utf-8")
        script.chmod(script.stat().st_mode | stat.S_IEXEC)
        self.env = {
            "PATH": f"{self.bin}{os.pathsep}{os.environ.get('PATH', '')}",
            "FAKE_DROP": str(self.drop),
            "FAKE_PAYLOADS": str(self.payloads),
            "FAKE_LOG": str(self.log),
            "FAKE_MODE": "ok",
        }

    def calls(self) -> list[list[str]]:
        return [line.split("\x00") for line in self.log.read_text().splitlines()]

    def run(self, fn):
        with mock.patch.dict(os.environ, self.env, clear=False):
            return fn()


def _fake_docker_script() -> str:
    return """#!{python}
import base64
import os
import re
import sys

args = sys.argv[1:]
with open(os.environ["FAKE_LOG"], "a", encoding="utf-8") as log:
    log.write("\\x00".join(args) + "\\n")
drop = os.environ["FAKE_DROP"]
mode = os.environ.get("FAKE_MODE", "ok")

if args[:2] == ["rm", "-f"]:
    sys.exit(0)

if args[0] == "run":
    command = args[-1]
    match = re.search(r"echo (\\S+) \\| base64 -d", command)
    if match is None:
        sys.stderr.write("fake docker: no base64 payload in: " + command)
        sys.exit(9)
    payload = base64.b64decode(match.group(1)).decode()
    name = re.search(r'OUT_NAME="([^"]+)"', payload).group(1)
    with open(os.path.join(os.environ["FAKE_PAYLOADS"], name), "w", encoding="utf-8") as f:
        f.write(payload)
    if mode == "run-fails":
        sys.stderr.write("container exploded\\n")
        sys.exit(17)
    if mode != "no-artifact":
        size = 0 if mode == "empty-archive" else 64
        with open(os.path.join(drop, name), "wb") as f:
            f.write(b"Z" * size)
        manifest = 0 if mode == "empty-manifest" else 48
        with open(os.path.join(drop, name + ".manifest.txt"), "wb") as f:
            f.write(b"M" * manifest)
    sys.exit(0)

if args[0] == "cp":
    remote = args[1].split(":", 1)[1]
    try:
        data = open(os.path.join(drop, os.path.basename(remote)), "rb").read()
    except FileNotFoundError:
        sys.stderr.write("no such file in container: " + remote + "\\n")
        sys.exit(1)
    with open(args[2], "wb") as f:
        f.write(data)
    sys.exit(0)

sys.stderr.write("fake docker: unsupported " + " ".join(args))
sys.exit(8)
"""


# The module writes the executable at import time from this template.
_FAKE_DOCKER = _fake_docker_script()
FakeDocker.__doc__ = FakeDocker.__doc__


class Build(unittest.TestCase):
    """`build()` must pass the right argv and never bless a bad artifact."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.docker = FakeDocker(self.tmp)
        self.out = self.tmp / "dist"

    def test_success_copies_both_artifacts_and_cleans_up(self) -> None:
        archive = self.docker.run(lambda: bq.build("x86_64", self.out))
        self.assertEqual(archive, self.out / bq.out_name("x86_64"))
        self.assertEqual(archive.read_bytes(), b"Z" * 64)
        self.assertTrue(Path(f"{archive}.manifest.txt").is_file())
        calls = self.docker.calls()
        self.assertEqual(calls[0], ["rm", "-f", "fbuild-qemu-runtime-x86_64"])
        run = [c for c in calls if c[0] == "run"][0]
        self.assertIn("--platform", run)
        self.assertEqual(run[run.index("--platform") + 1], "linux/amd64")
        self.assertEqual(run[run.index("--name") + 1], "fbuild-qemu-runtime-x86_64")
        self.assertIn(bq.BUILD_IMAGE, run)
        self.assertEqual(calls[-1], ["rm", "-f", "fbuild-qemu-runtime-x86_64"])

    def test_aarch64_uses_the_arm64_platform(self) -> None:
        self.docker.run(lambda: bq.build("aarch64", self.out))
        run = [c for c in self.docker.calls() if c[0] == "run"][0]
        self.assertEqual(run[run.index("--platform") + 1], "linux/arm64")

    def test_the_container_receives_the_rendered_script(self) -> None:
        self.docker.run(lambda: bq.build("x86_64", self.out))
        delivered = (self.docker.payloads / bq.out_name("x86_64")).read_text()
        self.assertEqual(delivered, bq.render_script("x86_64"))

    def test_a_failed_container_build_is_an_error(self) -> None:
        self.docker.env["FAKE_MODE"] = "run-fails"
        with self.assertRaises(SystemExit) as caught:
            self.docker.run(lambda: bq.build("x86_64", self.out))
        self.assertIn("17", str(caught.exception))
        self.assertFalse((self.out / bq.out_name("x86_64")).exists())

    def test_a_missing_artifact_is_not_a_success(self) -> None:
        self.docker.env["FAKE_MODE"] = "no-artifact"
        with self.assertRaises(SystemExit) as caught:
            self.docker.run(lambda: bq.build("x86_64", self.out))
        self.assertIn("no such file", str(caught.exception))

    def test_an_empty_archive_is_rejected(self) -> None:
        self.docker.env["FAKE_MODE"] = "empty-archive"
        with self.assertRaises(SystemExit) as caught:
            self.docker.run(lambda: bq.build("x86_64", self.out))
        self.assertIn("empty", str(caught.exception))

    def test_an_empty_manifest_is_rejected(self) -> None:
        self.docker.env["FAKE_MODE"] = "empty-manifest"
        with self.assertRaises(SystemExit) as caught:
            self.docker.run(lambda: bq.build("x86_64", self.out))
        self.assertIn("empty", str(caught.exception))


class CommandLine(unittest.TestCase):
    """`main()` end to end, still without a container daemon."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.docker = FakeDocker(self.tmp)

    def _run(self, *args: str) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env.update(self.docker.env)
        return subprocess.run(
            [sys.executable, str(ROOT / "ci/build_qemu_linux_runtime.py"), *args],
            cwd=ROOT,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_prints_the_digest_of_the_bytes_it_produced(self) -> None:
        out = self.tmp / "dist"
        proc = self._run("--arch", "x86_64", "--out", str(out))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        archive = out / bq.out_name("x86_64")
        expected = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.assertIn(expected, proc.stdout)
        self.assertIn(f"size:    {archive.stat().st_size} bytes", proc.stdout)

    def test_default_arch_is_x86_64(self) -> None:
        out = self.tmp / "dist"
        proc = self._run("--out", str(out))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue((out / bq.out_name("x86_64")).is_file())

    def test_unknown_arch_is_rejected_by_argparse(self) -> None:
        proc = self._run("--arch", "riscv64", "--out", str(self.tmp / "dist"))
        self.assertEqual(proc.returncode, 2)
        self.assertIn("invalid choice", proc.stderr)


class ClosureLogic(unittest.TestCase):
    """The bash walker/verifier, extracted from the payload and executed."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.fake = self.tmp / "fake"
        self.fake.mkdir()
        self.bundle = self.tmp / "bundle"

    def _library(self, name: str, deps: tuple[tuple[str, str], ...] = ()) -> None:
        """A stub library plus the `ldd` output the walker will see for it."""
        (self.fake / name).write_text(name, encoding="utf-8")
        lines = "".join(f"{soname} {state}\n" for soname, state in deps)
        (self.fake / f"{name}.map").write_text(lines, encoding="utf-8")

    def _run(self) -> subprocess.CompletedProcess[str]:
        harness = self.tmp / "harness.sh"
        harness.write_text(
            "\n".join(
                [
                    "set -euo pipefail",
                    f"FAKE={self.fake}",
                    f"BUNDLE={self.bundle}",
                    f"XTENSA={self.fake / 'qemu-system-xtensa'}",
                    f"RISCV={self.fake / 'qemu-system-riscv32'}",
                    f"MAX_CLOSURE_ROUNDS={bq.MAX_CLOSURE_ROUNDS}",
                    _LDD_STUB,
                    _logic_block(_render()),
                    'mkdir -p "$BUNDLE/lib"',
                    "collect_closure",
                    "verify_closure",
                    "",
                ]
            ),
            encoding="utf-8",
        )
        return subprocess.run(
            ["bash", str(harness)], capture_output=True, text=True, check=False
        )

    def _bundled(self) -> list[str]:
        return sorted(p.name for p in (self.bundle / "lib").iterdir())

    def test_a_complete_closure_passes(self) -> None:
        self._library("qemu-system-xtensa", (("libz.so.1", "found"),))
        self._library("qemu-system-riscv32", (("libz.so.1", "found"),))
        self._library("libz.so.1", (("libc.so.6", "found"),))
        proc = self._run()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(self._bundled(), ["libz.so.1"])

    def test_the_glibc_family_and_the_loader_are_never_bundled(self) -> None:
        self._library(
            "qemu-system-xtensa",
            (
                ("libz.so.1", "found"),
                ("libm.so.6", "found"),
                ("ld-linux-x86-64.so.2", "found"),
            ),
        )
        self._library("qemu-system-riscv32", ())
        self._library("libz.so.1", (("libc.so.6", "found"),))
        self._library("libm.so.6", ())
        self._library("ld-linux-x86-64.so.2", ())
        proc = self._run()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(self._bundled(), ["libz.so.1"])

    def test_a_gap_behind_a_bundled_library_is_reported(self) -> None:
        """The class the `--version` selftest cannot see: libgcrypt's own deps."""
        self._library("qemu-system-xtensa", (("libgcrypt.so.20", "found"),))
        self._library("qemu-system-riscv32", ())
        self._library("libgcrypt.so.20", (("libgpg-error.so.0", "missing"),))
        proc = self._run()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("libgcrypt.so.20->libgpg-error.so.0", proc.stderr)

    def test_a_walk_that_does_not_settle_fails_loudly(self) -> None:
        depth = bq.MAX_CLOSURE_ROUNDS + 1
        self._library("qemu-system-xtensa", (("libd1.so", "found"),))
        self._library("qemu-system-riscv32", ())
        for level in range(1, depth + 1):
            deeper = level + 1
            deps = ((f"libd{deeper}.so", "found"),) if deeper <= depth else ()
            self._library(f"libd{level}.so", deps)
        proc = self._run()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("did not settle", proc.stderr)

    def test_a_walk_at_the_cap_still_succeeds(self) -> None:
        """Every real library links libc, so the deepest level must not eat a round."""
        depth = bq.MAX_CLOSURE_ROUNDS
        self._library(
            "qemu-system-xtensa", (("libd1.so", "found"), ("libc.so.6", "found"))
        )
        self._library("qemu-system-riscv32", ())
        for level in range(1, depth + 1):
            deps = [(f"libd{level + 1}.so", "found")] if level < depth else []
            deps.append(("libc.so.6", "found"))
            self._library(f"libd{level}.so", tuple(deps))
        proc = self._run()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(len(self._bundled()), depth)

    def test_an_unreadable_elf_is_fatal(self) -> None:
        self._library("qemu-system-xtensa", ())
        self._library("qemu-system-riscv32", ())
        (self.fake / "fail").write_text("qemu-system-xtensa\n", encoding="utf-8")
        proc = self._run()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("cannot read the dynamic dependencies", proc.stderr)


_LDD_STUB = """
ldd() {
    local base map line soname state
    base="$(basename "$1")"
    if [ -f "$FAKE/fail" ] && grep -qx "$base" "$FAKE/fail"; then
        return 1
    fi
    printf '\\tlinux-vdso.so.1 (0x00007ffd)\\n'
    map="$FAKE/$base.map"
    if [ ! -f "$map" ]; then
        return 0
    fi
    while read -r soname state; do
        if [ -z "$soname" ]; then continue; fi
        if [ "$state" = "missing" ]; then
            printf '\\t%s => not found\\n' "$soname"
        else
            printf '\\t%s => %s/%s (0x00007f)\\n' "$soname" "$FAKE" "$soname"
        fi
    done < "$map"
}
"""


if __name__ == "__main__":
    unittest.main()
