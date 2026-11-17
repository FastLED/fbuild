"""The registry mirror tool in ``ci/mirror/`` (FastLED/fbuild#2040).

``mirror.py`` fills the bucket and writes the state DB that
``ci/gen_mirror_manifest.py`` reads; ``rotate.py`` supervises it on rotating
droplets. These tests cover the pure helpers and the download/upload path
against in-process fakes. Nothing here touches the network or the bucket.
"""

from __future__ import annotations

import asyncio
import hashlib
import re
import shutil
import sqlite3
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

from ci import gen_mirror_manifest as gen

MIRROR_DIR = Path(__file__).resolve().parent / "mirror"
# The scripts run standalone (`uv run ci/mirror/rotate.py`), so they import
# each other as top-level modules; do the same here.
if str(MIRROR_DIR) not in sys.path:
    sys.path.insert(0, str(MIRROR_DIR))

import mirror  # noqa: E402
import rotate  # noqa: E402


class FakeResponse:
    def __init__(self, status: int, body: bytes = b"", headers: dict | None = None):
        self.status_code = status
        self.body = body
        self.headers = headers or {}

    async def __aenter__(self) -> FakeResponse:
        return self

    async def __aexit__(self, *exc: object) -> None:
        return None

    async def aread(self) -> bytes:
        return self.body

    async def aiter_bytes(self, size: int):
        yield self.body

    def raise_for_status(self) -> None:
        if self.status_code >= 400:
            raise RuntimeError(f"HTTP {self.status_code}")


class FakeClient:
    def __init__(self, responses: list[FakeResponse]):
        self.responses = list(responses)
        self.urls: list[str] = []

    def stream(self, method: str, url: str) -> FakeResponse:
        self.urls.append(url)
        return self.responses.pop(0)


class FakeS3:
    def __init__(self) -> None:
        self.uploads: list[tuple[str, str, str, dict, bytes]] = []

    def upload_file(self, path: str, bucket: str, key: str, ExtraArgs: dict) -> None:
        self.uploads.append((path, bucket, key, ExtraArgs, Path(path).read_bytes()))


URL = mirror.DL_PREFIX + "platformio/tool/tool-x/1.0.0/tool-x-1.0.0.tar.gz"
KEY = "platformio/tool/tool-x/1.0.0/tool-x-1.0.0.tar.gz"


def fetch(responses: list[FakeResponse], sha: str | None, size: int = 0):
    client = FakeClient(responses)
    s3 = FakeS3()
    row = (URL, "tool-x-1.0.0.tar.gz", sha, KEY, size)
    with tempfile.TemporaryDirectory() as tmp:
        result = asyncio.run(mirror.fetch_one(client, s3, row, Path(tmp)))
        leftovers = list(Path(tmp).iterdir())
    return result, s3, leftovers


class FetchOneTest(unittest.TestCase):
    def setUp(self) -> None:
        mirror._COOLDOWN_DEADLINE = 0.0

    def test_uploads_verified_bytes_under_the_registry_key(self) -> None:
        body = b"archive bytes"
        sha = hashlib.sha256(body).hexdigest()
        result, s3, leftovers = fetch([FakeResponse(200, body)], sha, len(body))
        self.assertEqual(result, ("done", None, len(body)))
        self.assertEqual(len(s3.uploads), 1)
        _, bucket, key, extra, data = s3.uploads[0]
        self.assertEqual((bucket, key, data), (mirror.BUCKET, KEY, body))
        self.assertEqual(extra, {"ACL": "public-read"})
        self.assertEqual(leftovers, [], "the temp download must be removed")

    def test_size_disagreement_is_recorded_as_metadata(self) -> None:
        body = b"abc"
        result, s3, _ = fetch([FakeResponse(200, body)], None, 99)
        self.assertEqual(result, ("done", None, 3))
        self.assertEqual(s3.uploads[0][3]["Metadata"], {"x-registry-size": "99"})

    def test_sha_mismatch_is_an_error_and_nothing_is_uploaded(self) -> None:
        result, s3, leftovers = fetch([FakeResponse(200, b"x")], "0" * 64)
        self.assertEqual(result[0], "error")
        self.assertIn("sha256 mismatch", result[1])
        self.assertEqual(s3.uploads, [])
        self.assertEqual(leftovers, [])

    def test_404_is_gone(self) -> None:
        result, s3, _ = fetch([FakeResponse(404)], None)
        self.assertEqual(result, ("gone", "HTTP 404", 0))
        self.assertEqual(s3.uploads, [])

    def test_daily_quota_escapes_to_the_transfer_pass(self) -> None:
        quota = FakeResponse(429, b"Download limit exceeded. Try again in 24 hours")
        with self.assertRaises(mirror._QuotaExhausted):
            fetch([quota], None)

    def test_forbidden_is_an_error_not_gone(self) -> None:
        forbidden = [FakeResponse(403) for _ in range(6)]
        original = mirror.asyncio.sleep

        async def no_sleep(_: float) -> None:
            return None

        mirror.asyncio.sleep = no_sleep
        try:
            result, s3, _ = fetch(forbidden, None)
        finally:
            mirror.asyncio.sleep = original
        self.assertEqual(result, ("error", "HTTP 403", 0))
        self.assertEqual(s3.uploads, [])


class PureHelpersTest(unittest.TestCase):
    def test_retry_after_is_capped_and_defaulted(self) -> None:
        self.assertEqual(mirror.retry_after_delay("3600", 0), 180.0)
        self.assertEqual(mirror.retry_after_delay("7", 0), 7.0)
        self.assertEqual(mirror.retry_after_delay(None, 2), 30.0)
        self.assertEqual(mirror.retry_after_delay("soon", 0), 10.0)

    def test_file_rows_map_registry_files_to_bucket_keys(self) -> None:
        body = {
            "versions": [
                {
                    "name": "1.0.0",
                    "files": [
                        {
                            "download_url": URL,
                            "system": ["linux_x86_64", "darwin_arm64"],
                            "size": 5,
                            "checksum": {"sha256": "ab"},
                        },
                        {"name": "no-url"},
                        {
                            "download_url": "https://example.com/a/b.zip",
                            "name": "b.zip",
                            "system": "*",
                        },
                    ],
                },
                {"name": "2.0.0", "files": None},
            ]
        }
        rows = mirror.file_rows(body, "platformio", "tool", "tool-x", 42)
        self.assertEqual(
            rows,
            [
                (
                    URL,
                    "platformio",
                    "tool",
                    "tool-x",
                    "1.0.0",
                    "tool-x-1.0.0.tar.gz",
                    '["linux_x86_64", "darwin_arm64"]',  # order kept
                    5,
                    "ab",
                    KEY,
                    42,
                ),
                (
                    "https://example.com/a/b.zip",
                    "platformio",
                    "tool",
                    "tool-x",
                    "1.0.0",
                    "b.zip",
                    "*",
                    0,
                    None,
                    "external/example.com/a/b.zip",
                    42,
                ),
            ],
        )

    def test_stall_action_restarts_once_then_rotates(self) -> None:
        self.assertIsNone(rotate.stall_action(2, False))
        self.assertEqual(rotate.stall_action(3, False), "restart")
        self.assertIsNone(rotate.stall_action(3, True))
        self.assertEqual(rotate.stall_action(6, True), "rotate")


ROOT = Path(__file__).resolve().parent.parent


def write_cargo(directory: Path, version: str) -> Path:
    cargo = directory / "Cargo.toml"
    cargo.write_text(
        f'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "{version}"\n'
    )
    return cargo


class UserAgentTest(unittest.TestCase):
    """FastLED/fbuild#2040 item 3: the CDN refuses the old `fbuild-mirror` UA."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.cargo = write_cargo(Path(self.tmp.name), "9.8.7")

    def test_default_is_the_production_format_for_the_checkout_version(self) -> None:
        self.assertEqual(
            mirror.resolve_user_agent({}, self.cargo), "PlatformIO/6.2.0 (fbuild/9.8.7)"
        )

    def test_default_never_carries_the_refused_token(self) -> None:
        self.assertNotIn("fbuild-mirror", mirror.resolve_user_agent({}, self.cargo))

    def test_default_matches_fbuild_core_registry_user_agent(self) -> None:
        source = (ROOT / "crates/fbuild-core/src/http.rs").read_text()
        match = re.search(
            r'pub fn registry_user_agent\(\) -> String \{\s*format!\("([^"]*)",'
            r'\s*env!\("CARGO_PKG_VERSION"\)\)',
            source,
        )
        self.assertIsNotNone(
            match, "registry_user_agent() changed shape; update this test"
        )
        self.assertEqual(
            mirror.default_user_agent("1.2.3"), match.group(1).replace("{}", "1.2.3")
        )

    def test_the_repo_checkout_resolves(self) -> None:
        version = mirror.fbuild_version(ROOT / "Cargo.toml")
        self.assertTrue(version)
        self.assertEqual(
            mirror.resolve_user_agent({}), mirror.default_user_agent(version)
        )

    def test_override_wins_and_blank_override_is_ignored(self) -> None:
        env = {"FBUILD_MIRROR_UA": " PlatformIO/6.1.16 (fbuild/2.0.0) "}
        self.assertEqual(
            mirror.resolve_user_agent(env, self.cargo),
            "PlatformIO/6.1.16 (fbuild/2.0.0)",
        )
        self.assertEqual(
            mirror.resolve_user_agent({"FBUILD_MIRROR_UA": "  "}, self.cargo),
            "PlatformIO/6.2.0 (fbuild/9.8.7)",
        )

    def test_override_must_keep_the_platformio_prefix(self) -> None:
        with self.assertRaisesRegex(ValueError, "PlatformIO/"):
            mirror.resolve_user_agent({"FBUILD_MIRROR_UA": "curl/8.0"}, self.cargo)

    def test_override_must_be_safe_in_a_systemd_environment_line(self) -> None:
        for bad in (
            'PlatformIO/6.2.0 "x"',
            "PlatformIO/6.2.0 %h",
            "PlatformIO/6.2.0\nX=1",
        ):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                mirror.resolve_user_agent({"FBUILD_MIRROR_UA": bad}, self.cargo)

    def test_no_checkout_and_no_override_fails_fast(self) -> None:
        missing = Path(self.tmp.name) / "nowhere" / "Cargo.toml"
        with self.assertRaisesRegex(ValueError, "FBUILD_MIRROR_UA is unset"):
            mirror.resolve_user_agent({}, missing)

    def test_droplet_unit_carries_the_user_agent(self) -> None:
        creds = Path(self.tmp.name) / "creds.json"
        creds.write_text("{}")
        payloads: list[str] = []
        with (
            mock.patch.object(rotate, "CREDS", creds),
            mock.patch.object(
                rotate, "ssh", lambda ip, payload, **kw: payloads.append(payload)
            ),
            mock.patch.object(rotate, "restore_manifest") as restore,
        ):
            rotate.provision("192.0.2.1", None, "PlatformIO/6.2.0 (fbuild/9.8.7)")
        self.assertIn(
            'Environment="FBUILD_MIRROR_UA=PlatformIO/6.2.0 (fbuild/9.8.7)"\n',
            payloads[0],
        )
        restore.assert_not_called()


def make_db(path: Path, rows: list[tuple[str, str, int]]) -> Path:
    """A state DB in mirror.py's schema: (key, state, updated_at) per file."""
    conn = sqlite3.connect(path)
    conn.executescript(mirror.SCHEMA)
    with conn:
        conn.executemany(
            "insert into files (url, owner, type, package, version, filename, system,"
            " key, state, updated_at) values (?, 'o', 'tool', 'p', '1', 'f', ?, ?, ?, ?)",
            [
                (mirror.DL_PREFIX + key, '["linux_x86_64"]', key, state, ts)
                for key, state, ts in rows
            ],
        )
    conn.close()
    return path


S = rotate.ManifestSummary


class StaleManifestGuardTest(unittest.TestCase):
    """FastLED/fbuild#2040: restore_manifest() preferred a stale local copy."""

    # The shapes found on 2026-11-17: the local rotation-work copy and the
    # bucket copy before the item-1 fix had the SAME row count, so a row-count
    # comparison alone cannot tell them apart.
    STALE_LOCAL = S(rows=29302, done=29085, newest=1793366844)
    OLD_BUCKET = S(rows=29302, done=29299, newest=1793367333)
    BUCKET = S(rows=29303, done=29300, newest=1794890138)

    def test_stale_local_copy_is_ignored_in_favour_of_the_bucket(self) -> None:
        self.assertTrue(rotate.check_restore_sources(self.STALE_LOCAL, self.BUCKET))
        self.assertTrue(rotate.check_restore_sources(self.STALE_LOCAL, self.OLD_BUCKET))

    def test_equal_copies_restore_the_bucket(self) -> None:
        self.assertTrue(rotate.check_restore_sources(self.BUCKET, self.BUCKET))

    def test_local_progress_the_bucket_lacks_stops_the_run(self) -> None:
        for local in (
            S(rows=29303, done=29301, newest=1794890138),
            S(rows=29304, done=29300, newest=1794890138),
            S(rows=29303, done=29300, newest=1794890139),
            S(rows=29302, done=29301, newest=1),  # diverged: ahead in one measure
        ):
            with self.subTest(local=local), self.assertRaises(rotate.ManifestConflict):
                rotate.check_restore_sources(local, self.BUCKET)

    def test_first_ever_run_starts_empty(self) -> None:
        self.assertFalse(rotate.check_restore_sources(None, None))

    def test_local_copy_without_a_bucket_copy_stops_the_run(self) -> None:
        with self.assertRaises(rotate.ManifestConflict):
            rotate.check_restore_sources(self.STALE_LOCAL, None)

    def test_snapshot_behind_the_bucket_is_never_uploaded(self) -> None:
        with self.assertRaises(rotate.ManifestConflict):
            rotate.check_snapshot_upload(self.STALE_LOCAL, self.BUCKET)
        rotate.check_snapshot_upload(self.BUCKET, self.BUCKET)
        rotate.check_snapshot_upload(
            S(rows=29303, done=29303, newest=1794890200), self.BUCKET
        )
        rotate.check_snapshot_upload(self.STALE_LOCAL, None)

    def test_summarize_reads_rows_done_and_newest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            db = make_db(
                Path(tmp) / "m.db",
                [("a", "done", 5), ("b", "pending", 9), ("c", "gone", 7)],
            )
            self.assertEqual(rotate.summarize_manifest(db), S(rows=3, done=1, newest=9))

    def test_plan_restore_returns_the_bucket_copy_and_keeps_the_local_one(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            work = tmp_path / "work"
            work.mkdir()
            make_db(work / "manifest.db", [("a", "done", 5), ("b", "pending", 6)])
            bucket_db = make_db(
                tmp_path / "bucket.db", [("a", "done", 5), ("b", "done", 8)]
            )
            local_before = (work / "manifest.db").read_bytes()

            class BucketS3:
                def download_file(self, bucket: str, key: str, dest: str) -> None:
                    assert (bucket, key) == (rotate.BUCKET, rotate.STATE_KEY)
                    shutil.copyfile(bucket_db, dest)

            with (
                mock.patch.object(rotate, "WORK", work),
                mock.patch.object(rotate, "s3", BucketS3),
                mock.patch.dict(
                    sys.modules, {"botocore.exceptions": FakeBotocoreExceptions}
                ),
            ):
                source = rotate.plan_restore()
            self.assertIsNotNone(source)
            self.assertEqual(source.read_bytes(), bucket_db.read_bytes())
            self.assertEqual((work / "manifest.db").read_bytes(), local_before)

    def test_plan_restore_refuses_a_local_copy_ahead_of_the_bucket(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            work = tmp_path / "work"
            work.mkdir()
            make_db(work / "manifest.db", [("a", "done", 5), ("b", "done", 9)])
            bucket_db = make_db(
                tmp_path / "bucket.db", [("a", "done", 5), ("b", "pending", 6)]
            )

            class BucketS3:
                def download_file(self, bucket: str, key: str, dest: str) -> None:
                    shutil.copyfile(bucket_db, dest)

            with (
                mock.patch.object(rotate, "WORK", work),
                mock.patch.object(rotate, "s3", BucketS3),
                mock.patch.dict(
                    sys.modules, {"botocore.exceptions": FakeBotocoreExceptions}
                ),
                self.assertRaises(rotate.ManifestConflict),
            ):
                rotate.plan_restore()


class FakeClientError(Exception):
    pass


FakeBotocoreExceptions = types.ModuleType("botocore.exceptions")
FakeBotocoreExceptions.ClientError = FakeClientError


class SharedStateDbTest(unittest.TestCase):
    """mirror.py writes the state DB that gen_mirror_manifest.py reads."""

    def test_generator_reads_systems_from_a_mirror_schema_db(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            db = make_db(
                Path(tmp) / "m.db",
                [("o/tool/p/1/f", "done", 1), ("o/tool/p/2/f", "pending", 1)],
            )
            self.assertEqual(
                gen.load_systems(None, db), {"o/tool/p/1/f": ["linux_x86_64"]}
            )

    def test_bucket_and_key_constants_agree(self) -> None:
        self.assertEqual(rotate.STATE_KEY, gen.STATE_DB_KEY)
        self.assertEqual(rotate.BUCKET, gen.BUCKET_NAME)
        self.assertEqual(mirror.BUCKET, gen.BUCKET_NAME)
        self.assertEqual(mirror.DL_PREFIX, gen.REGISTRY_PREFIX)


if __name__ == "__main__":
    unittest.main()
