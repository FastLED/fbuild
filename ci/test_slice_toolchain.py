"""Offline contract tests for per-chip toolchain slices (FastLED/fbuild#1944)."""

import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path

import zstandard

from ci.slice_toolchain import (
    INSTALL_MARKER,
    discover_chips,
    scan,
    select,
    slice_toolchain,
)

GCC = """#!/bin/sh
root=$(cd "$(dirname "$0")/.." && pwd)
if [ "$1" = "-print-libgcc-file-name" ]; then
  echo "%(libgcc)s"
  exit 0
fi
if [ "$1" = "--version" ]; then
  echo "fake-gcc 0.0.0"
  exit 0
fi
echo "compiled" > /dev/null
exit 0
"""


def write(path: Path, text: str, mode: int = 0o644) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(mode)
    return path


def toolchain(root: Path, libgcc: str = "$root/lib/libgcc.a") -> Path:
    write(root / "package.json", json.dumps({"name": "toolchain-xtensa-esp-elf", "version": "13.2.0+x"}))
    for chip in ("esp32", "esp32s3"):
        write(root / "bin" / f"xtensa-{chip}-elf-gcc", GCC % {"libgcc": libgcc}, 0o755)
    write(root / "bin" / "xtensa-esp-elf-ar", "#!/bin/sh\n", 0o755)
    write(root / "lib" / "libc.a", "shared")
    write(root / "lib" / "libgcc.a", "shared")
    write(root / "lib" / "esp32" / "chip.a", "esp32-only")
    write(root / "lib" / "esp32s3" / "chip.a", "esp32s3-only")
    write(root / "include" / "esp_idf_version.h", "shared")
    (root / "lib" / "esp32s3" / "alias.a").symlink_to("chip.a")
    write(root / "libexec" / "liblto_plugin.so", "plugin")
    (root / "lib" / "bfd-plugins").mkdir(parents=True, exist_ok=True)
    (root / "lib" / "bfd-plugins" / "liblto_plugin.so").symlink_to(
        "../../libexec/liblto_plugin.so"
    )
    write(root / INSTALL_MARKER, "")
    return root


def members(archive: Path) -> list[str]:
    with archive.open("rb") as raw, zstandard.ZstdDecompressor().stream_reader(raw) as stream:
        with tarfile.open(fileobj=stream, mode="r|") as tar:
            return [m.name for m in tar]


class ChipSelectionTests(unittest.TestCase):
    def test_discovery_and_selection_drop_siblings_keep_shared(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = toolchain(Path(tmp) / "tc")
            entries = scan(root)
            chips = discover_chips(entries)
            self.assertEqual(chips, ("esp32", "esp32s3"))
            kept = {entry.rel for entry in select(entries, "esp32s3", chips)}
            self.assertIn("lib/esp32s3/chip.a", kept)
            self.assertIn("lib/libc.a", kept)
            self.assertIn("include/esp_idf_version.h", kept)
            self.assertNotIn("lib/esp32/chip.a", kept)
            self.assertNotIn(INSTALL_MARKER, {entry.rel for entry in entries})

    def test_default_run_ships_every_chip_and_manifest_entries(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = toolchain(Path(tmp) / "tc")
            result = slice_toolchain(root, Path(tmp) / "out")
            self.assertEqual([r["chip"] for r in result["slices"]], ["esp32", "esp32s3"])
            self.assertEqual(sorted(result["manifest_slices"]), ["esp32", "esp32s3"])
            for record in result["slices"]:
                self.assertEqual(record["name"].split("-")[-1], record["chip"] + ".tar.zst")
                self.assertNotIn(INSTALL_MARKER, members(Path(tmp) / "out" / record["name"]))

    def test_unknown_chip_refuses_and_lists_available(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = toolchain(Path(tmp) / "tc")
            with self.assertRaisesRegex(ValueError, "have: esp32, esp32s3"):
                slice_toolchain(root, Path(tmp) / "out", ("esp32c6",))

    def test_tree_without_chip_dirs_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "flat"
            write(root / "bin" / "gcc", "x", 0o755)
            write(root / "lib" / "libc.a", "x")
            with self.assertRaisesRegex(ValueError, "no per-chip directories"):
                slice_toolchain(root, Path(tmp) / "out")


class ArchiveShapeTests(unittest.TestCase):
    def test_deterministic_bytes_mode_and_symlink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = toolchain(Path(tmp) / "tc")
            first = slice_toolchain(root, Path(tmp) / "one", ("esp32s3",))
            second = slice_toolchain(root, Path(tmp) / "two", ("esp32s3",))
            name = first["slices"][0]["name"]
            left = (Path(tmp) / "one" / name).read_bytes()
            self.assertEqual(left, (Path(tmp) / "two" / name).read_bytes())
            self.assertTrue(zstandard.get_frame_parameters(left).has_checksum)
            with (Path(tmp) / "one" / name).open("rb") as raw:
                with zstandard.ZstdDecompressor().stream_reader(raw) as stream:
                    with tarfile.open(fileobj=stream, mode="r|") as tar:
                        infos = {m.name: m for m in tar}
            gcc = infos["bin/xtensa-esp32s3-elf-gcc"]
            self.assertEqual(gcc.mode, 0o755)
            self.assertEqual((gcc.mtime, gcc.uid, gcc.gid), (0, 0, 0))
            self.assertNotIn("lib/esp32/chip.a", infos)
            alias = infos["lib/esp32s3/alias.a"]
            self.assertTrue(alias.issym())
            self.assertEqual(alias.linkname, "chip.a")
            # An in-tree `..` link is legal: real toolchains ship one.
            plugin = infos["lib/bfd-plugins/liblto_plugin.so"]
            self.assertEqual(plugin.linkname, "../../libexec/liblto_plugin.so")

    def test_link_into_a_dropped_chip_dir_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = toolchain(Path(tmp) / "tc")
            (root / "lib" / "esp32s3" / "stale.a").symlink_to("../esp32/chip.a")
            with self.assertRaisesRegex(ValueError, "drops symlink targets"):
                slice_toolchain(root, Path(tmp) / "out", ("esp32s3",))

    def test_unsafe_symlink_is_refused(self):
        for target in ("/etc/hostname", "../../../../etc/hostname"):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as tmp:
                root = toolchain(Path(tmp) / "tc")
                (root / "lib" / "esp32s3" / "escape.a").symlink_to(target)
                with self.assertRaisesRegex(ValueError, "unsafe symlink"):
                    slice_toolchain(root, Path(tmp) / "out", ("esp32s3",))


class VerifyTests(unittest.TestCase):
    def test_verify_runs_the_chip_compiler_from_the_slice(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = toolchain(Path(tmp) / "tc")
            result = slice_toolchain(root, Path(tmp) / "out", ("esp32s3",), verify=True)
            verified = result["slices"][0]["verified"]
            self.assertEqual(verified["compiler"], "xtensa-esp32s3-elf-gcc")
            self.assertEqual(verified["version"], "fake-gcc 0.0.0")
            self.assertEqual(verified["libgcc"], "lib/libgcc.a")

    def test_verify_rejects_a_libgcc_outside_the_slice(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = toolchain(Path(tmp) / "tc", libgcc="/usr/lib/libgcc.a")
            with self.assertRaisesRegex(ValueError, "outside the slice"):
                slice_toolchain(root, Path(tmp) / "out", ("esp32s3",), verify=True)


class CheckTests(unittest.TestCase):
    def test_check_accepts_a_faithful_rerun(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = toolchain(Path(tmp) / "tc")
            out = Path(tmp) / "out"
            slice_toolchain(root, out, ("esp32s3",))
            slice_toolchain(root, out, ("esp32s3",), check=True)

    def test_check_detects_a_tampered_slice(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = toolchain(Path(tmp) / "tc")
            out = Path(tmp) / "out"
            result = slice_toolchain(root, out, ("esp32s3",))
            archive = out / result["slices"][0]["name"]
            payload = bytearray(archive.read_bytes())
            payload[-1] ^= 0xFF
            archive.write_bytes(bytes(payload))
            with self.assertRaisesRegex(ValueError, "slice differs"):
                slice_toolchain(root, out, ("esp32s3",), check=True)


if __name__ == "__main__":
    unittest.main()
