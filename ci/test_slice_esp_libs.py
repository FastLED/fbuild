"""Offline contract tests for self-contained ESP slices."""

import io
import tarfile
import tempfile
import unittest
from pathlib import Path

import zstandard

from ci.slice_esp_libs import digest, safe_name, slice_archive


class SliceTests(unittest.TestCase):
    def fixture(self, root, extra=None):
        source = root / "libs.tar.xz"
        with tarfile.open(source, "w:xz") as archive:
            for name in [
                "esp32s3/lib.a",
                "esp32c3/lib.a",
                "hosted/lib.a",
                "package.json",
                "esp32-version.txt",
            ]:
                member = tarfile.TarInfo("esp32-arduino-libs/" + name)
                member.size = 3
                member.mtime = 123456
                member.uid = 42
                archive.addfile(member, io.BytesIO(b"abc"))
            if extra:
                archive.addfile(extra)
        return source

    def test_self_contained_deterministic_checked(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self.fixture(root)
            first = slice_archive(str(source), "3.3.5", root / "one")
            second = slice_archive(str(source), "3.3.5", root / "two")
            self.assertEqual(first, second)
            self.assertEqual(len(first["slices"]), 2)
            for record in first["slices"]:
                path = root / "one" / record["name"]
                parameters = zstandard.get_frame_parameters(path.read_bytes())
                self.assertTrue(parameters.has_checksum)
                self.assertLessEqual(parameters.window_size, 8 * 1024 * 1024)
                self.assertEqual(
                    path.read_bytes(), (root / "two" / record["name"]).read_bytes()
                )
                self.assertEqual(
                    digest(path), {k: record[k] for k in ("sha256", "size_bytes")}
                )
                with (
                    path.open("rb") as raw,
                    zstandard.ZstdDecompressor().stream_reader(raw) as stream,
                    tarfile.open(fileobj=stream, mode="r|") as archive,
                ):
                    names = [m.name for m in archive]
                self.assertEqual(
                    names,
                    sorted(
                        [
                            f"esp32-arduino-libs/{record['chip']}/lib.a",
                            "esp32-arduino-libs/hosted/lib.a",
                            "esp32-arduino-libs/package.json",
                            "esp32-arduino-libs/esp32-version.txt",
                        ]
                    ),
                )
            slice_archive(str(source), "3.3.5", root / "one", check=True)
            path.write_bytes(b"tampered")
            with self.assertRaisesRegex(ValueError, "slice differs"):
                slice_archive(str(source), "3.3.5", root / "one", check=True)

    def test_empty_paths(self):
        for name in ("", ".", "./"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                safe_name(name)

    def test_tar_slip(self):
        for name in [
            "/absolute",
            "esp32-arduino-libs/../escape",
            "esp32-arduino-libs/hosted",
        ]:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                member = tarfile.TarInfo(name)
                if name.endswith("hosted"):
                    member.type = tarfile.SYMTYPE
                    member.linkname = "esp32s3"
                source = self.fixture(root, member)
                with self.assertRaises(ValueError):
                    slice_archive(str(source), "3.3.5", root / "out")
