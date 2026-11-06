"""Tests for ci/build_backend.py -- the in-tree PEP 517 / PEP 660 backend.

`pyproject.toml` declares `build-backend = "build_backend"` with
`backend-path = ["ci"]`, so every `pip install .`, `uv sync` and editable install
in this repo goes through this module. Nothing exercised it
(`ci/gate_script_tests.toml` carried it as untested), and two of its paths
succeeded while shipping the wrong thing:

  * the "staged artifacts are up-to-date" fast path compared mtimes only, so
    the cargo **profile** was not part of the cache key: a plain
    `pip install .` after `FBUILD_BUILD_RELEASE=1` (or the reverse) silently
    installed the other profile's binary and reported success;
  * a staged artifact was adopted as current when it was zero bytes -- and
    `shutil.copy2` writes in place, so an install killed mid-copy left a
    truncated `ci/bin/fbuild` whose mtime was the newest in the tree, i.e. the
    corruption was permanent and packaged into every later wheel.

The same selection loop also existed three times (`fbuild`, `fbuild-daemon`,
`_native`) with its JSON tolerance never exercised; those are now one
`_cargo_artifacts()` generator.

The fix keeps the fast path but makes it fail closed: a profile stamp
(`ci/bin/.build-profile`), a non-empty requirement, and atomic staging. These
tests drive the real staleness/staging/fast-path logic against a synthetic
repo and the wheel writer end to end against a real `.whl`.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import tempfile
import unittest
import zipfile
from contextlib import redirect_stderr
from io import StringIO
from pathlib import Path
from unittest import mock

from ci import build_backend as bb

REAL_ROOT = Path(bb.__file__).resolve().parent.parent


def _touch(path: Path, *, age: float, size: int = 8) -> Path:
    """Write `size` bytes and stamp the mtime `age` seconds before "now"."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"x" * size)
    stamp = os.stat(path.parent).st_mtime - age
    os.utime(path, (stamp, stamp))
    return path


class FakeRepo(unittest.TestCase):
    """A throwaway repo layout with every module-level path patched into it."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.repo = self.tmp / "repo"
        (self.repo / "crates" / "fbuild-cli" / "src").mkdir(parents=True)
        (self.repo / "crates" / "fbuild-cli" / "Cargo.toml").write_text(
            '[package]\nname = "fbuild-cli"\n', encoding="utf-8"
        )
        (self.repo / "crates" / "fbuild-cli" / "src" / "main.rs").write_text(
            "fn main() {}\n", encoding="utf-8"
        )
        (self.repo / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        (self.repo / "pyproject.toml").write_text(
            '[project]\nname = "fbuild"\nversion = "9.9.9"\n'
            'description = "test project"\nlicense = "MIT"\n'
            'requires-python = ">=3.10"\nreadme = "README.md"\n',
            encoding="utf-8",
        )
        (self.repo / "README.md").write_text("# test\n", encoding="utf-8")

        self.bin_dir = self.repo / "ci" / "bin"
        self.binary = self.bin_dir / "fbuild"
        self.daemon = self.bin_dir / "fbuild-daemon"
        self.stamp = self.bin_dir / ".build-profile"
        self.pyext = self.repo / "python" / "fbuild"
        (self.pyext / "api").mkdir(parents=True)
        (self.pyext / "__init__.py").write_text("from . import api\n", encoding="utf-8")
        (self.pyext / "api" / "core.py").write_text("X = 1\n", encoding="utf-8")
        self.native = self.pyext / "_native.so"
        self.target = self.tmp / "target"

        for name, value in (
            ("REPO_ROOT", self.repo),
            ("STAGED_BIN_DIR", self.bin_dir),
            ("STAGED_BINARY_PATH", self.binary),
            ("STAGED_DAEMON_PATH", self.daemon),
            ("STAGED_PROFILE_PATH", self.stamp),
            ("PYTHON_EXT_DIR", self.pyext),
            ("STAGED_NATIVE_EXT_PATH", self.native),
        ):
            patcher = mock.patch.object(bb, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        env = mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": str(self.target)})
        env.start()
        self.addCleanup(env.stop)

    # -- fixtures ------------------------------------------------------------

    def stage(
        self,
        *,
        profile: str | None = "debug",
        age: float = -60.0,
        size: int = 8,
    ) -> None:
        """Lay down staged artifacts (optionally with a profile stamp)."""
        for path in (self.binary, self.daemon, self.native):
            _touch(path, age=age, size=size)
        if profile is None:
            self.stamp.unlink(missing_ok=True)
            return
        self.stamp.write_text(f"v1 {profile}\n", encoding="utf-8")
        stamp_at = time_of(self.stamp)
        os.utime(self.stamp, (stamp_at, stamp_at))

    def builders(self):
        """Patch out soldr + both cargo builds; each writes a fresh artifact.

        The mocks return SOURCE paths under `src/`, never the staging paths, so
        a test can tell a rebuild (staged bytes changed) from a reuse.
        """
        src_dir = self.tmp / "src"

        def build_binaries():
            return (
                _touch(src_dir / "fbuild", age=0, size=4),
                _touch(src_dir / "fbuild-daemon", age=0, size=4),
            )

        def build_python():
            return _touch(src_dir / "lib_native.so", age=0, size=4)

        require = mock.patch.object(bb, "_require_soldr")
        binaries = mock.patch.object(
            bb, "_build_fbuild_binaries", side_effect=build_binaries
        )
        python = mock.patch.object(bb, "_build_fbuild_python", side_effect=build_python)
        started = [require.start(), binaries.start(), python.start()]
        for patcher in (require, binaries, python):
            self.addCleanup(patcher.stop)
        return started[0], started[1], started[2]


def time_of(path: Path) -> float:
    return Path(path).stat().st_mtime


class StalenessTests(FakeRepo):
    def test_current_stamp_and_fresh_artifacts_are_reused(self):
        self.stage()
        self.assertTrue(bb._staged_profile_is_current())
        self.assertTrue(bb._staged_artifacts_are_current())

    def test_zero_byte_artifact_is_never_current(self):
        self.stage(size=0)
        self.assertFalse(bb._staged_artifact_is_current(self.binary))
        self.assertFalse(bb._staged_artifacts_are_current())

    def test_artifact_older_than_a_cargo_input_is_stale(self):
        self.stage(age=60.0)
        _touch(self.repo / "crates" / "fbuild-cli" / "src" / "main.rs", age=0)
        self.assertFalse(bb._staged_artifact_is_current(self.binary))

    def test_missing_artifact_is_stale(self):
        self.stage()
        self.binary.unlink()
        self.assertFalse(bb._staged_artifact_is_current(self.binary))

    def test_missing_profile_stamp_is_not_current(self):
        self.stage(profile=None)
        self.assertFalse(bb._staged_profile_is_current())
        self.assertFalse(bb._staged_artifacts_are_current())

    def test_other_profile_stamp_is_not_current(self):
        """A release request never adopts debug artifacts (and vice versa)."""
        self.stage(profile="debug")
        with mock.patch.dict(os.environ, {"FBUILD_BUILD_RELEASE": "1"}):
            self.assertEqual(bb._profile_subdir(), "release")
            self.assertFalse(bb._staged_profile_is_current())
            self.assertFalse(bb._staged_artifacts_are_current())

    def test_malformed_profile_stamp_is_not_current(self):
        self.stage()
        for content in ("", "garbage", "v2 debug", "v1 debug extra", "v1 "):
            self.stamp.write_text(content, encoding="utf-8")
            self.assertFalse(
                bb._staged_profile_is_current(), f"stamp {content!r} accepted"
            )

    def test_cargo_input_that_vanished_counts_as_changed(self):
        self.stage()
        ghost = mock.Mock(spec=Path)
        ghost.stat.side_effect = FileNotFoundError
        with mock.patch.object(bb, "_iter_cargo_inputs", return_value=[ghost]):
            self.assertFalse(bb._staged_artifact_is_current(self.binary))

    def test_release_marker_is_read_not_just_truthiness(self):
        for value, expected in (
            ("1", True),
            ("true", True),
            ("YES", True),
            ("0", False),
            ("false", False),
            ("", False),
        ):
            with mock.patch.dict(os.environ, {"FBUILD_BUILD_RELEASE": value}):
                self.assertIs(bb._use_release_profile(), expected, value)


class StageArtifactsTests(FakeRepo):
    def test_fast_path_skips_cargo(self):
        self.stage()
        require, binaries, python = self.builders()
        stderr = StringIO()
        with redirect_stderr(stderr):
            bb._stage_artifacts()
        binaries.assert_not_called()
        python.assert_not_called()
        require.assert_not_called()
        self.assertIn("up-to-date for the debug profile", stderr.getvalue())

    def test_profile_change_forces_a_rebuild(self):
        """The load-bearing regression: debug artifacts must not satisfy a
        release request."""
        self.stage(profile="debug")
        _, binaries, python = self.builders()
        with mock.patch.dict(os.environ, {"FBUILD_BUILD_RELEASE": "1"}):
            with redirect_stderr(StringIO()):
                bb._stage_artifacts()
            binaries.assert_called_once()
            python.assert_called_once()
            self.assertTrue(bb._staged_profile_is_current())
            self.assertEqual(
                self.stamp.read_text(encoding="utf-8").strip(), "v1 release"
            )

    def test_zero_byte_staged_binary_forces_a_rebuild(self):
        self.stage(size=0)
        _, binaries, _ = self.builders()
        with redirect_stderr(StringIO()):
            bb._stage_artifacts()
        binaries.assert_called_once()

    def test_staging_is_atomic_and_leaves_no_temp_file(self):
        self.stage(profile=None)
        self.builders()
        with redirect_stderr(StringIO()):
            bb._stage_artifacts()
        # 4 fresh bytes from the mock build, not the 8 stale staged ones.
        self.assertEqual(self.binary.read_bytes(), b"x" * 4)
        self.assertEqual(self.native.read_bytes(), b"x" * 4)
        self.assertEqual(
            sorted(p.name for p in self.bin_dir.iterdir()),
            [
                ".build-profile",
                "fbuild",
                "fbuild-daemon",
            ],
        )

    def test_failed_stage_never_replaces_the_previous_artifact(self):
        self.stage(profile=None)
        self.binary.write_bytes(b"OLD")
        self.builders()[1].side_effect = SystemExit(2)
        with redirect_stderr(StringIO()):
            with self.assertRaises(SystemExit):
                bb._stage_artifacts()
        self.assertEqual(self.binary.read_bytes(), b"OLD")

    def test_empty_payload_from_cargo_refuses_to_build(self):
        self.stage(profile=None)
        _, binaries, _ = self.builders()
        empty = self.tmp / "empty-bin"
        empty.write_bytes(b"")
        binaries.side_effect = None
        binaries.return_value = (empty, self.daemon)
        stderr = StringIO()
        with redirect_stderr(stderr):
            with self.assertRaises(SystemExit) as raised:
                bb._stage_artifacts()
        self.assertEqual(raised.exception.code, 1)
        self.assertIn("missing or empty", stderr.getvalue())

    def test_interrupted_copy_leaves_the_previous_artifact_intact(self):
        """copy2 in place + newest mtime = permanent corruption. Stage through
        a temp file so a failure can only orphan the temp."""
        self.binary.parent.mkdir(parents=True, exist_ok=True)
        self.binary.write_bytes(b"OLD")

        def partial(src, dst, *args, **kwargs):
            Path(dst).write_bytes(b"PAR")
            raise OSError("disk full")

        with mock.patch.object(bb.shutil, "copy2", side_effect=partial):
            with self.assertRaises(OSError):
                bb._atomic_copy(self.tmp / "src" / "fbuild", self.binary)
        self.assertEqual(self.binary.read_bytes(), b"OLD")

    def test_assert_staged_payload_rejects_a_zero_byte_daemon(self):
        self.stage()
        self.daemon.write_bytes(b"")
        with redirect_stderr(StringIO()):
            with self.assertRaises(SystemExit):
                bb._assert_staged_payload()

    def test_require_soldr_names_the_install_command(self):
        with mock.patch.object(bb.shutil, "which", return_value=None):
            stderr = StringIO()
            with redirect_stderr(stderr):
                with self.assertRaises(SystemExit):
                    bb._require_soldr()
        self.assertIn("uv tool install soldr", stderr.getvalue())


class CargoArtifactTests(unittest.TestCase):
    @staticmethod
    def _line(name: str, **extra) -> str:
        payload = {"reason": "compiler-artifact", "target": {"name": name}}
        target_extra = extra.pop("target", {})
        payload["target"].update(target_extra)
        payload.update(extra)
        return json.dumps(payload)

    def test_last_matching_binary_wins(self):
        stream = "\n".join(
            [
                self._line("fbuild", executable="/t/old/fbuild"),
                "some progress noise",
                self._line("fbuild", executable="/t/new/fbuild"),
            ]
        )
        self.assertEqual(
            bb._find_fbuild_executable_from_json(stream), Path("/t/new/fbuild")
        )

    def test_binary_search_ignores_daemon_and_null_executables(self):
        stream = "\n".join(
            [
                self._line("fbuild-daemon", executable="/t/fbuild-daemon"),
                self._line("fbuild", executable=None),
            ]
        )
        self.assertIsNone(bb._find_fbuild_executable_from_json(stream))

    def test_daemon_search_picks_its_own_target(self):
        stream = "\n".join(
            [
                self._line("fbuild", executable="/t/fbuild"),
                self._line("fbuild-daemon", executable="/t/fbuild-daemon"),
            ]
        )
        self.assertEqual(
            bb._find_fbuild_daemon_from_json(stream), Path("/t/fbuild-daemon")
        )

    def test_non_object_json_is_tolerated(self):
        stream = "\n".join(
            [
                "[1, 2, 3]",
                '"a string"',
                "null",
                "{not json",
                self._line("fbuild", executable="/t/fbuild"),
            ]
        )
        self.assertEqual(
            bb._find_fbuild_executable_from_json(stream), Path("/t/fbuild")
        )

    def test_native_cdylib_needs_the_cdylib_kind_and_a_native_suffix(self):
        stream = "\n".join(
            [
                self._line(
                    "_native",
                    target={"kind": ["rlib"]},
                    filenames=["/t/lib_native.rlib"],
                ),
                self._line(
                    "_native",
                    target={"kind": ["cdylib"]},
                    filenames=["/t/lib_native.so"],
                ),
            ]
        )
        self.assertEqual(
            bb._find_native_cdylib_from_json(stream), Path("/t/lib_native.so")
        )

    def test_native_cdylib_without_a_native_filename_is_none(self):
        stream = self._line(
            "_native", target={"kind": ["cdylib"]}, filenames=["/t/x.rlib"]
        )
        self.assertIsNone(bb._find_native_cdylib_from_json(stream))


class SearchFallbackTests(FakeRepo):
    def test_finds_the_plain_profile_directory(self):
        built = _touch(self.target / "debug" / "fbuild", age=0)
        self.assertEqual(bb._find_fbuild_executable_by_search(), built)

    def test_finds_a_per_host_triple_directory(self):
        built = _touch(
            self.target / "x86_64-unknown-linux-gnu" / "debug" / "fbuild", age=0
        )
        self.assertEqual(bb._find_fbuild_executable_by_search(), built)

    def test_absent_binary_is_none(self):
        self.target.mkdir(parents=True, exist_ok=True)
        self.assertIsNone(bb._find_fbuild_executable_by_search())
        self.assertIsNone(bb._find_fbuild_daemon_by_search())

    def test_native_cdylib_name_follows_the_host(self):
        _touch(self.target / "debug" / "lib_native.so", age=0)
        self.assertIsNotNone(bb._find_native_cdylib_by_search())
        with mock.patch.object(bb.sys, "platform", "win32"):
            _touch(self.target / "debug" / "_native.dll", age=0)
            self.assertEqual(
                bb._find_native_cdylib_by_search(),
                self.target / "debug" / "_native.dll",
            )


class WheelAssemblyTests(FakeRepo):
    def test_metadata_is_read_from_pyproject(self):
        text = bb._metadata().decode("utf-8")
        self.assertIn("Name: fbuild", text)
        self.assertIn("Version: 9.9.9", text)
        self.assertIn("Requires-Python: >=3.10", text)
        self.assertIn("License: MIT", text)
        self.assertTrue(text.endswith("# test\n"))

    def test_missing_project_table_is_a_clear_error(self):
        (self.repo / "pyproject.toml").write_text("[build-system]\n", encoding="utf-8")
        with self.assertRaises(RuntimeError) as raised:
            bb._version()
        self.assertIn("[project]", str(raised.exception))

    def test_missing_field_names_the_field(self):
        (self.repo / "pyproject.toml").write_text(
            '[project]\nname = "fbuild"\n', encoding="utf-8"
        )
        with self.assertRaises(RuntimeError) as raised:
            bb._version()
        self.assertIn("version", str(raised.exception))

    def test_wheel_tag_uses_the_running_interpreter(self):
        tag = bb._wheel_tag()
        interp = f"cp{bb.sys.version_info.major}{bb.sys.version_info.minor}"
        parts = tag.split("-")
        self.assertEqual(parts[:2], [interp, interp])
        self.assertNotIn(".", parts[2])
        self.assertNotIn("-", parts[2])

    def test_record_hashes_modes_and_sizes_match_the_payload(self):
        out = self.tmp / "w"
        out.mkdir()
        wheel = out / "t.whl"
        payload = self.tmp / "payload.bin"
        payload.write_bytes(b"payload!")

        writer = bb._WheelWriter(wheel)
        writer.add_bytes("pkg/mod.py", b"X = 1\n")
        writer.add_file("pkg/bin/fbuild", payload, executable=True)
        writer.close("fbuild-9.9.9.dist-info")

        with zipfile.ZipFile(wheel) as archive:
            record = archive.read("fbuild-9.9.9.dist-info/RECORD").decode("utf-8")
            rows = [line.split(",") for line in record.splitlines()]
            by_name = {row[0]: row for row in rows}
            self.assertEqual(
                by_name["pkg/mod.py"][1],
                "sha256="
                + base64.urlsafe_b64encode(hashlib.sha256(b"X = 1\n").digest())
                .rstrip(b"=")
                .decode(),
            )
            self.assertEqual(by_name["pkg/mod.py"][2], str(len(b"X = 1\n")))
            self.assertEqual(by_name["pkg/bin/fbuild"][2], str(len(b"payload!")))
            self.assertEqual(by_name["fbuild-9.9.9.dist-info/RECORD"][1:], ["", ""])
            self.assertTrue(
                archive.getinfo("pkg/bin/fbuild").external_attr >> 16 & 0o111
            )
            self.assertFalse(archive.getinfo("pkg/mod.py").external_attr >> 16 & 0o111)

    def test_add_package_skips_pycache_and_keeps_the_native_extension(self):
        self.stage()
        cache = self.pyext / "__pycache__"
        cache.mkdir()
        (cache / "junk.py").write_text("", encoding="utf-8")
        out = self.tmp / "w2"
        out.mkdir()
        writer = bb._WheelWriter(out / "t.whl")
        bb._add_package(writer)
        names = [row[0] for row in writer._records]
        self.assertIn("fbuild/__init__.py", names)
        self.assertIn("fbuild/api/core.py", names)
        self.assertIn("fbuild/_native.so", names)
        self.assertFalse([name for name in names if "__pycache__" in name])

    def test_editable_wheel_is_a_single_pth_line(self):
        out = self.tmp / "w3"
        out.mkdir()
        writer = bb._WheelWriter(out / "t.whl")
        bb._add_editable_pth(writer)
        self.assertEqual(writer._records[0][0], "__editable__.fbuild.pth")

    def test_build_wheel_produces_a_complete_wheel(self):
        self.stage()
        self.builders()
        out = self.tmp / "dist"
        out.mkdir()
        with redirect_stderr(StringIO()):
            name = bb.build_wheel(str(out))
        self.assertEqual(name, f"fbuild-9.9.9-{bb._wheel_tag()}.whl")
        with zipfile.ZipFile(out / name) as archive:
            names = archive.namelist()
            self.assertIn("fbuild-9.9.9.data/scripts/fbuild", names)
            self.assertIn("fbuild-9.9.9.data/scripts/fbuild-daemon", names)
            self.assertIn("fbuild/__init__.py", names)
            self.assertIn("fbuild-9.9.9.dist-info/METADATA", names)
            wheel_meta = archive.read("fbuild-9.9.9.dist-info/WHEEL").decode("utf-8")
            self.assertIn("Root-Is-Purelib: false", wheel_meta)
            self.assertIn(f"Tag: {bb._wheel_tag()}", wheel_meta)

    def test_build_wheel_refuses_an_empty_staged_binary(self):
        """Packaging is the last line of defence: a wheel around a 0-byte
        `fbuild` installs a command that cannot run."""
        self.stage(size=0)
        out = self.tmp / "dist2"
        out.mkdir()
        with mock.patch.object(bb, "_stage_artifacts"):
            with redirect_stderr(StringIO()):
                with self.assertRaises(SystemExit):
                    bb.build_wheel(str(out))
        self.assertEqual(list(out.iterdir()), [])

    def test_no_build_requirements(self):
        self.assertEqual(bb.get_requires_for_build_wheel(), [])
        self.assertEqual(bb.get_requires_for_build_editable(), [])


class RealRepoReconciliationTests(unittest.TestCase):
    """The shipped pyproject must still satisfy what the backend reads."""

    def test_every_field_the_backend_reads_exists(self):
        for key in ("version", "description", "license", "readme", "requires-python"):
            self.assertTrue(bb._project_field(key), f"[project] {key} is empty")

    def test_wheel_version_matches_pyproject(self):
        text = (REAL_ROOT / "pyproject.toml").read_text(encoding="utf-8")
        self.assertIn(f'version = "{bb._version()}"', text)

    def test_profile_stamp_lives_in_the_ignored_staging_dir(self):
        self.assertEqual(bb.STAGED_PROFILE_PATH.parent, bb.STAGED_BIN_DIR)
        ignored = (REAL_ROOT / ".gitignore").read_text(encoding="utf-8")
        self.assertIn("/ci/bin/", ignored)

    def test_pyproject_declares_this_backend(self):
        text = (REAL_ROOT / "pyproject.toml").read_text(encoding="utf-8")
        self.assertIn('build-backend = "build_backend"', text)
        self.assertIn('backend-path = ["ci"]', text)


if __name__ == "__main__":
    unittest.main()
