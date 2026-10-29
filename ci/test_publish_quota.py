"""PyPI storage preflight must refuse a release before it is stranded.

Regression for FastLED/fbuild#1756: the project hit PyPI's 10 GB limit and the
publish job uploaded 1 of 5 wheels before PyPI started returning
``400 Project size too large``, leaving the newest version uninstallable.
"""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from ci import publish

GB = 1000**3
WHEEL_MB = 42 * 1000**2


def release(files: int, size: int = WHEEL_MB) -> list[dict]:
    return [{"filename": f"fbuild-2.5.46-{index}.whl", "size": size} for index in range(files)]


def project(versions: int, files: int = 5, size: int = WHEEL_MB) -> dict:
    return {"releases": {f"2.4.{index}": release(files, size) for index in range(versions)}}


class TestProjectSize(unittest.TestCase):
    def test_sums_every_file_of_every_version(self):
        data = {"releases": {"2.4.0": release(5), "2.4.1": release(3)}}
        self.assertEqual(publish.pypi_project_bytes(data), 8 * WHEEL_MB)

    def test_missing_and_malformed_sizes_are_zero(self):
        data = {"releases": {"2.4.0": [{"filename": "a"}, {"filename": "b", "size": None},
                                       {"filename": "c", "size": "42"}]}}
        self.assertEqual(publish.pypi_project_bytes(data), 42)

    def test_empty_payload_is_zero(self):
        self.assertEqual(publish.pypi_project_bytes({}), 0)


class TestEstimateUploadStorage(unittest.TestCase):
    def test_stranded_version_uses_its_own_uploaded_file_size(self):
        data = project(10)
        data["releases"]["2.5.46"] = release(1)
        storage = publish.estimate_upload_storage(data, "2.5.46", 5)
        self.assertEqual(storage.required_bytes, 4 * WHEEL_MB)
        self.assertIn("4 of 5 missing wheel(s)", storage.basis)

    def test_fresh_version_falls_back_to_largest_stored_file(self):
        data = project(3, size=WHEEL_MB)
        data["releases"]["2.3.0"] = release(1, size=90 * 1000**2)
        storage = publish.estimate_upload_storage(data, "2.6.0", 5)
        self.assertEqual(storage.required_bytes, 5 * 90 * 1000**2)
        self.assertIn("largest stored file", storage.basis)

    def test_no_stored_files_cannot_overrun(self):
        storage = publish.estimate_upload_storage({}, "2.6.0", 5)
        self.assertEqual(storage.required_bytes, 0)
        self.assertTrue(storage.fits)

    def test_full_version_needs_nothing(self):
        data = project(2)
        data["releases"]["2.5.46"] = release(5)
        storage = publish.estimate_upload_storage(data, "2.5.46", 5)
        self.assertEqual(storage.required_bytes, 0)
        self.assertTrue(storage.fits)


class TestVerdict(unittest.TestCase):
    def test_at_the_limit_is_refused_with_actionable_message(self):
        # 180 releases x 5 wheels x 42 MB = 37.8 GB, i.e. well over the limit;
        # this is the shape the real project reached.
        data = project(180)
        data["releases"]["2.5.46"] = release(1)
        storage = publish.estimate_upload_storage(data, "2.5.46", 5)
        self.assertFalse(storage.fits)
        report = storage.report()
        self.assertIn("refusing to publish", report)
        self.assertIn("strands a version", report)
        self.assertIn(publish.PYPI_STORAGE_LIMITS_URL, report)
        self.assertIn("4 of 5 missing wheel(s)", report)

    def test_exact_headroom_is_allowed(self):
        limit = 10 * WHEEL_MB
        data = {"releases": {"2.4.0": release(5)}}
        storage = publish.estimate_upload_storage(data, "2.5.0", 5, limit)
        self.assertEqual(storage.remaining_bytes, 5 * WHEEL_MB)
        self.assertTrue(storage.fits)
        self.assertIn("headroom is sufficient", storage.report())

    def test_one_byte_over_headroom_is_refused(self):
        limit = 10 * WHEEL_MB - 1
        data = {"releases": {"2.4.0": release(5)}}
        storage = publish.estimate_upload_storage(data, "2.5.0", 5, limit)
        self.assertFalse(storage.fits)
        self.assertEqual(storage.remaining_bytes, 4 * WHEEL_MB + (WHEEL_MB - 1))

    def test_remaining_never_negative(self):
        storage = publish.PyPIStorage(project_bytes=12 * GB, required_bytes=0)
        self.assertEqual(storage.remaining_bytes, 0)
        self.assertFalse(storage.fits)


class TestMeasuredUploadStorage(unittest.TestCase):
    def test_uses_real_wheel_sizes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            wheels = [root / f"fbuild-2.5.46-{index}.whl" for index in range(2)]
            for index, wheel in enumerate(wheels):
                wheel.write_bytes(b"x" * (WHEEL_MB + index))
            storage = publish.measured_upload_storage({"releases": {}}, wheels)
        self.assertEqual(storage.required_bytes, 2 * WHEEL_MB + 1)
        self.assertIn("2 built wheel(s)", storage.basis)

    def test_refuses_when_project_already_near_the_limit(self):
        data = {"releases": {"2.4.9": release(5, size=2 * GB)}}
        with tempfile.TemporaryDirectory() as tmp:
            wheel = Path(tmp) / "fbuild-2.5.46-x.whl"
            wheel.write_bytes(b"x" * WHEEL_MB)
            storage = publish.measured_upload_storage(data, [wheel])
        self.assertFalse(storage.fits)


class TestCli(unittest.TestCase):
    def write_json(self, root: Path, data: dict) -> Path:
        path = root / "pypi.json"
        path.write_text(json.dumps(data), encoding="utf-8")
        return path

    def test_exit_code_zero_when_headroom_is_sufficient(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = self.write_json(Path(tmp), project(2))
            self.assertEqual(
                publish.main(["--pypi-json", str(path), "--version", "2.6.0",
                              "--expected-wheels", "5"]),
                0,
            )

    def test_exit_code_one_when_the_project_is_full(self):
        data = project(180)
        data["releases"]["2.5.46"] = release(1)
        with tempfile.TemporaryDirectory() as tmp:
            path = self.write_json(Path(tmp), data)
            self.assertEqual(
                publish.main(["--pypi-json", str(path), "--version", "2.5.46",
                              "--expected-wheels", "5"]),
                1,
            )


if __name__ == "__main__":
    unittest.main()
