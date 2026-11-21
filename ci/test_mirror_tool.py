"""The registry mirror tool in ``ci/mirror/`` (FastLED/fbuild#2040).

``mirror.py`` fills the bucket and writes the state DB that
``ci/gen_mirror_manifest.py`` reads; ``rotate.py`` supervises it on rotating
droplets; ``update.py`` runs the incremental pass (item 4). These tests cover
the pure helpers, the catalog scan, and the download/upload path against
in-process fakes. Nothing here touches the network or the bucket.
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
import update  # noqa: E402


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
        assert result[1] is not None
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

    def test_forbidden_stops_the_run_at_once(self) -> None:
        # #2040 item 3: a refused UA used to be retried 6 times per file and
        # then recorded as that file's error, file after file.
        body = b"This download method is not supported. Please use PlatformIO Core"
        client = FakeClient([FakeResponse(403, body) for _ in range(6)])
        s3 = FakeS3()
        row = (URL, "tool-x-1.0.0.tar.gz", None, KEY, 0)
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(mirror.RegistryRefused, "not supported"):
                asyncio.run(mirror.fetch_one(client, s3, row, Path(tmp)))
            self.assertEqual(list(Path(tmp).iterdir()), [])
        self.assertEqual(len(client.urls), 1, "a 403 is not retried")
        self.assertEqual(s3.uploads, [])


class RefusalTest(unittest.TestCase):
    """A refusal stops the pass and leaves every other row queued."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        state = Path(self.tmp.name)
        patcher = mock.patch.multiple(
            mirror,
            REFUSED_FILE=state / "registry_refused",
            QUOTA_FILE=state / "quota_until",
        )
        patcher.start()
        self.addCleanup(patcher.stop)

    def run_chunk(self, failure: BaseException) -> tuple[int | None, list[int]]:
        recorded: list[int] = []

        async def worker(row: tuple[int]) -> None:
            if row[0] == 0:
                raise failure
            await asyncio.sleep(0.05)
            recorded.append(row[0])

        status = asyncio.run(mirror.run_chunk(worker, [(i,) for i in range(5)]))
        return status, recorded

    def test_refusal_cancels_the_chunk_and_is_recorded(self) -> None:
        status, recorded = self.run_chunk(mirror.RegistryRefused("HTTP 403 from x"))
        self.assertEqual(status, mirror.EXIT_REFUSED)
        self.assertEqual(recorded, [], "no other row may be recorded")
        self.assertIn("HTTP 403", mirror.REFUSED_FILE.read_text())

    def test_quota_still_pauses_instead_of_stopping(self) -> None:
        status, _ = self.run_chunk(mirror._QuotaExhausted("Download limit exceeded"))
        self.assertEqual(status, 0)
        self.assertTrue(mirror.QUOTA_FILE.exists())
        self.assertFalse(mirror.REFUSED_FILE.exists())

    def test_other_failures_still_propagate(self) -> None:
        with self.assertRaises(ExceptionGroup):
            self.run_chunk(ValueError("bug"))

    def test_run_will_not_restart_after_a_refusal(self) -> None:
        mirror.REFUSED_FILE.write_text("HTTP 403 from x\n")
        self.assertEqual(mirror.run("PlatformIO/6.2.0 (fbuild/1.0.0)"), 3)

    def test_api_403_is_not_retried(self) -> None:
        client = FakeApi({})
        with self.assertRaises(mirror.RegistryRefused):
            asyncio.run(mirror._search_page(client, 1))
        self.assertEqual(len(client.calls), 1)


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
        assert match is not None
        self.assertEqual(
            mirror.default_user_agent("1.2.3"), match.group(1).replace("{}", "1.2.3")
        )

    def test_the_repo_checkout_resolves(self) -> None:
        version = mirror.fbuild_version(ROOT / "Cargo.toml")
        self.assertTrue(version)
        assert version is not None
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
        # A refused client exits 3; systemd must not restart it in a loop.
        self.assertIn("RestartPreventExitStatus=3\n", payloads[0])
        restore.assert_not_called()


class FullPassUserAgentTest(unittest.TestCase):
    """#2040 UA decision: the bulk pass never sends the production UA by default.

    The CDN started refusing `fbuild-mirror` after a ~102 GB pass. If the full
    pass ran under fbuild's production UA and that string were refused the same
    way, every real fbuild client's registry fallback would break. The small
    incremental pass (update.py) keeps the production default."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.cargo = write_cargo(Path(self.tmp.name), "9.8.7")

    def test_full_pass_requires_an_explicit_user_agent(self) -> None:
        for env in ({}, {"FBUILD_MIRROR_UA": "  "}):
            with (
                self.subTest(env=env),
                self.assertRaisesRegex(ValueError, "FBUILD_MIRROR_UA"),
            ):
                rotate.full_pass_user_agent(env, self.cargo)

    def test_explicit_user_agent_is_validated_and_used(self) -> None:
        env = {"FBUILD_MIRROR_UA": "PlatformIO/6.2.0 (fbuild/9.8.7; bulk)"}
        self.assertEqual(
            rotate.full_pass_user_agent(env, self.cargo),
            "PlatformIO/6.2.0 (fbuild/9.8.7; bulk)",
        )
        with self.assertRaisesRegex(ValueError, "PlatformIO/"):
            rotate.full_pass_user_agent({"FBUILD_MIRROR_UA": "curl/8"}, self.cargo)

    def test_supervisor_stops_before_any_droplet_without_one(self) -> None:
        with (
            mock.patch.object(sys, "argv", ["rotate.py"]),
            mock.patch.dict(rotate.os.environ, {}, clear=True),
            mock.patch.object(rotate, "SSH_KEY_ID", "123"),
            mock.patch.object(rotate, "droplets", return_value=[]) as listed,
            mock.patch.object(
                rotate, "_start_droplet", side_effect=AssertionError("droplet")
            ),
            self.assertRaisesRegex(SystemExit, "FBUILD_MIRROR_UA"),
        ):
            rotate.main()
        listed.assert_not_called()

    def test_print_user_agent_still_shows_the_default(self) -> None:
        with (
            mock.patch.object(sys, "argv", ["rotate.py", "--print-user-agent"]),
            mock.patch.dict(rotate.os.environ, {}, clear=True),
            mock.patch("builtins.print") as printed,
        ):
            self.assertEqual(rotate.main(), 0)
        self.assertTrue(printed.call_args[0][0].startswith("PlatformIO/"))


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

            BucketS3 = bucket_s3(bucket_db)

            with (
                mock.patch.object(rotate, "WORK", work),
                mock.patch.object(rotate, "s3", BucketS3),
                mock.patch.dict(
                    sys.modules, {"botocore.exceptions": FakeBotocoreExceptions}
                ),
            ):
                source = rotate.plan_restore()
            self.assertIsNotNone(source)
            assert source is not None
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

            BucketS3 = bucket_s3(bucket_db)

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
    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.response = {"Error": {"Code": code}}


def bucket_s3(bucket_db: Path | None, etag: str = '"e1"', put_error: str = ""):
    """A fake `rotate.s3` serving `bucket_db` as the state DB; records writes."""

    class BucketS3:
        calls: list[tuple[str, dict]] = []

        def __init__(self) -> None:
            pass

        def get_object(self, Bucket: str, Key: str) -> dict:
            assert (Bucket, Key) == (rotate.BUCKET, rotate.STATE_KEY)
            if bucket_db is None:
                raise FakeClientError("NoSuchKey")
            return {"Body": open(bucket_db, "rb"), "ETag": etag}

        def put_object(self, **kwargs: object) -> None:
            BucketS3.calls.append(("put_object", kwargs))
            if put_error:
                raise FakeClientError(put_error)

        def copy_object(self, **kwargs: object) -> None:
            BucketS3.calls.append(("copy_object", kwargs))

    return BucketS3


class ConditionalStateDbWriteTest(unittest.TestCase):
    """#2040: `snapshot_manifest` uploaded with no ETag condition."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.db = make_db(Path(self.tmp.name) / "m.db", [("a", "done", 5)])
        patcher = mock.patch.dict(
            sys.modules, {"botocore.exceptions": FakeBotocoreExceptions}
        )
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_download_keeps_the_etag_of_the_bytes_it_read(self) -> None:
        dest = Path(self.tmp.name) / "got.db"
        with mock.patch.object(rotate, "s3", bucket_s3(self.db, '"abc-3"')):
            read = rotate.download_bucket_manifest(dest)
        self.assertEqual(read, rotate.BucketCopy(S(1, 1, 5), '"abc-3"'))
        self.assertEqual(dest.read_bytes(), self.db.read_bytes())

    def test_upload_is_conditional_on_the_etag_read(self) -> None:
        fake = bucket_s3(self.db)
        with mock.patch.object(rotate, "s3", fake):
            rotate.put_bucket_manifest(self.db, rotate.BucketCopy(S(1, 1, 5), '"e1"'))
            rotate.put_bucket_manifest(self.db, None)
        (_, first), (_, second) = fake.calls
        self.assertEqual(first["IfMatch"], '"e1"')
        self.assertNotIn("IfNoneMatch", first)
        self.assertEqual(first["Key"], rotate.STATE_KEY)
        self.assertEqual(second["IfNoneMatch"], "*")

    def test_a_concurrent_write_is_a_conflict_not_an_overwrite(self) -> None:
        for code in ("PreconditionFailed", "412"):
            with (
                self.subTest(code=code),
                mock.patch.object(rotate, "s3", bucket_s3(self.db, put_error=code)),
                self.assertRaisesRegex(rotate.ManifestConflict, "changed since"),
            ):
                rotate.put_bucket_manifest(self.db, rotate.BucketCopy(S(1, 1, 5), "e"))

    def test_backup_copies_only_the_copy_read(self) -> None:
        fake = bucket_s3(self.db)
        with mock.patch.object(rotate, "s3", fake):
            key = rotate.backup_bucket_manifest(rotate.BucketCopy(S(1, 1, 5), '"e1"'))
        ((name, kwargs),) = fake.calls
        self.assertEqual(name, "copy_object")
        self.assertEqual(kwargs["CopySourceIfMatch"], '"e1"')
        self.assertEqual(kwargs["Key"], key)
        self.assertTrue(key.startswith(rotate.STATE_KEY + ".bak-"))


FakeBotocoreExceptions = types.ModuleType("botocore.exceptions")
setattr(FakeBotocoreExceptions, "ClientError", FakeClientError)


class FakeApiResponse:
    def __init__(self, status: int, body: object = None) -> None:
        self.status_code = status
        self.body = body
        self.text = "This download method is not supported" if status == 403 else ""

    def json(self) -> object:
        return self.body

    def raise_for_status(self) -> None:
        if self.status_code >= 400:
            raise RuntimeError(f"HTTP {self.status_code}")


class FakeApi:
    """The registry API: `search` is a newest-updated-first list of items,
    `packages` maps `owner/type/name` to a `/packages` body. Anything else 403s."""

    def __init__(self, packages: dict[str, dict], search: list[dict] | None = None):
        self.packages = packages
        self.search = search or []
        self.calls: list[str] = []

    async def get(self, url: str, params: dict | None = None) -> FakeApiResponse:
        self.calls.append(url)
        if url == f"{mirror.API}/search" and self.search:
            assert params is not None
            assert params["sort"] == "updated"
            start = (params["page"] - 1) * params["limit"]
            items = self.search[start : start + params["limit"]]
            return FakeApiResponse(200, {"total": len(self.search), "items": items})
        label = url.removeprefix(f"{mirror.API}/packages/")
        if label in self.packages:
            return FakeApiResponse(200, self.packages[label])
        return FakeApiResponse(403)


def item(owner: str, name: str, updated: str) -> dict:
    return {
        "owner": {"username": owner},
        "type": "library",
        "name": name,
        "updated_at": updated,
    }


def versions(owner: str, name: str, *names: str) -> dict:
    return {
        "versions": [
            {
                "name": v,
                "files": [
                    {
                        "download_url": f"{mirror.DL_PREFIX}{owner}/library/{name}/{v}/{name}-{v}.tar.gz",
                        "size": 10,
                        "system": "*",
                    }
                ],
            }
            for v in names
        ]
    }


_T0 = mirror.registry_time("2026-10-30T08:41:57Z")
assert _T0 is not None
T0 = _T0


class IncrementalCatalogTest(unittest.TestCase):
    """#2040 item 4: `catalog` only read versions of packages it had never seen,
    so a new release of a known package (FastLED 3.10.9) was never queued."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.conn = sqlite3.connect(Path(self.tmp.name) / "m.db")
        self.addCleanup(self.conn.close)
        self.conn.executescript(mirror.SCHEMA)
        # The bucket DB's shape: every package fully catalogued, all done.
        mirror._record_packages(
            self.conn,
            [item("fastled", "FastLED", ""), item("old", "Stale", "")],
            T0,
        )
        for owner, name, body in (
            ("fastled", "FastLED", versions("fastled", "FastLED", "3.10.8")),
            ("old", "Stale", versions("old", "Stale", "1.0")),
        ):
            mirror._store_files(
                self.conn,
                mirror.file_rows(body, owner, "library", name, T0),
                owner,
                "library",
                name,
            )
        self.conn.execute("update files set state = 'done'")
        self.conn.commit()

    def pending(self) -> list[str]:
        return [
            key
            for (key,) in self.conn.execute(
                "select key from files where state = 'pending' order by key"
            )
        ]

    def scan(self, api: FakeApi, since: int | None) -> None:
        async def go() -> None:
            await mirror._scan(api, self.conn, T0 + 10, 500, since)
            await mirror._fetch_pending_versions(api, self.conn, 0)

        with mock.patch.object(mirror.asyncio, "sleep", mock.AsyncMock()):
            asyncio.run(go())

    def registry(self) -> FakeApi:
        return FakeApi(
            {
                "fastled/library/FastLED": versions(
                    "fastled", "FastLED", "3.10.9", "3.10.8"
                ),
                "new/library/Fresh": versions("new", "Fresh", "0.1"),
            },
            [
                item("new", "Fresh", "2026-11-17T21:24:52Z"),
                item("fastled", "FastLED", "2026-11-02T14:20:58Z"),
                item("old", "Stale", "2026-10-01T00:00:00Z"),
            ],
        )

    def test_new_release_of_a_known_package_is_queued(self) -> None:
        api = self.registry()
        self.scan(api, mirror.registry_time("2026-10-29T00:00:00Z"))
        self.assertEqual(
            self.pending(),
            [
                "fastled/library/FastLED/3.10.9/FastLED-3.10.9.tar.gz",
                "new/library/Fresh/0.1/Fresh-0.1.tar.gz",
            ],
        )
        self.assertNotIn(f"{mirror.API}/packages/old/library/Stale", api.calls)
        self.assertEqual(
            mirror.stored_watermark(self.conn),
            mirror.registry_time("2026-11-17T21:24:52Z"),
        )

    def test_rescan_from_the_watermark_queues_nothing_new(self) -> None:
        self.scan(self.registry(), mirror.registry_time("2026-10-29T00:00:00Z"))
        self.conn.execute("update files set state = 'done'")
        api = self.registry()
        self.scan(api, None)
        self.assertEqual(self.pending(), [])
        # Only the package inside the overlap window is read again.
        self.assertEqual(
            [c for c in api.calls if "/packages/" in c],
            [f"{mirror.API}/packages/new/library/Fresh"],
        )

    def test_legacy_db_without_watermark_only_adds_unknown_packages(self) -> None:
        self.scan(self.registry(), None)
        self.assertEqual(self.pending(), ["new/library/Fresh/0.1/Fresh-0.1.tar.gz"])
        self.assertIsNone(mirror.stored_watermark(self.conn))

    def test_named_package_refresh_leaves_the_watermark_alone(self) -> None:
        api = self.registry()
        mirror.request_refresh(self.conn, [("fastled", "library", "FastLED")], T0)

        async def go() -> None:
            await mirror._fetch_pending_versions(api, self.conn, 0)

        asyncio.run(go())
        self.assertEqual(
            self.pending(), ["fastled/library/FastLED/3.10.9/FastLED-3.10.9.tar.gz"]
        )
        self.assertNotIn(f"{mirror.API}/search", api.calls)
        self.assertIsNone(mirror.stored_watermark(self.conn))


class WatermarkTest(unittest.TestCase):
    def test_registry_time(self) -> None:
        self.assertEqual(mirror.registry_time("1970-01-01T00:01:00Z"), 60)
        for bad in (None, "", "yesterday", "2026-11-17T00:00:00"):
            self.assertIsNone(mirror.registry_time(bad))

    def test_split_stops_at_the_first_item_older_than_the_cutoff(self) -> None:
        items = [
            {"updated_at": "1970-01-01T00:03:00Z"},
            {"updated_at": None},
            {"updated_at": "1970-01-01T00:01:00Z"},
            {"updated_at": "1970-01-01T00:05:00Z"},
        ]
        self.assertEqual(mirror.split_updated(items, 120), (items[:2], True))
        self.assertEqual(mirror.split_updated(items, None), (items, False))
        self.assertEqual(mirror.newest_update(items, None), 300)

    def test_watermark_only_advances_over_a_covered_scan(self) -> None:
        advance = mirror.watermark_may_advance
        self.assertTrue(advance(None, None, fresh_db=True))  # first full pass
        self.assertFalse(advance(None, None, fresh_db=False))  # legacy DB
        self.assertTrue(advance(None, 100, fresh_db=False))  # operator's --since
        self.assertTrue(advance(100, 100, fresh_db=False))
        self.assertTrue(advance(100, 50, fresh_db=False))
        self.assertFalse(advance(100, 150, fresh_db=False))  # a gap behind it


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

    def test_watermark_table_does_not_disturb_the_generator(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            db = make_db(Path(tmp) / "m.db", [("o/tool/p/1/f", "done", 1)])
            conn = sqlite3.connect(db)
            mirror.store_watermark(conn, 42)
            self.assertEqual(mirror.stored_watermark(conn), 42)
            conn.close()
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


class UpdateScriptTest(unittest.TestCase):
    """ci/mirror/update.py: the guards around the incremental pass."""

    def test_since_accepts_a_date_or_datetime_in_utc(self) -> None:
        self.assertEqual(update.parse_since("1970-01-02"), 86400)
        self.assertEqual(update.parse_since("1970-01-01T00:01:00Z"), 60)
        with self.assertRaises(update.argparse.ArgumentTypeError):
            update.parse_since("last week")

    def test_package_must_be_owner_type_name(self) -> None:
        self.assertEqual(
            update.parse_package("fastled/library/FastLED"),
            ("fastled", "library", "FastLED"),
        )
        for bad in ("FastLED", "fastled//FastLED", "a/b/c/d"):
            with (
                self.subTest(bad=bad),
                self.assertRaises(update.argparse.ArgumentTypeError),
            ):
                update.parse_package(bad)

    def test_queue_lists_exactly_what_transfer_picks_up(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            db = make_db(
                Path(tmp) / "m.db",
                [("b", "pending", 1), ("a", "error", 1), ("c", "done", 1)],
            )
            self.assertEqual(update.queued(db), [("a", 0), ("b", 0)])

    def test_queue_over_the_cap_is_refused(self) -> None:
        update.check_queue([("a", 5), ("b", 5)], 10)
        with self.assertRaisesRegex(update.GuardError, "max-bytes"):
            update.check_queue([("a", 5), ("b", 6)], 10)

    def test_a_scan_needs_a_watermark_or_since(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            db = make_db(Path(tmp) / "m.db", [("a", "done", 1)])
            args = update.parse_args([])
            with self.assertRaisesRegex(update.GuardError, "--since"):
                update.check_scan_source(db, args)
            update.check_scan_source(db, update.parse_args(["--since", "2026-10-01"]))
            update.check_scan_source(
                db, update.parse_args(["--package", "fastled/library/FastLED"])
            )
            conn = sqlite3.connect(db)
            mirror.store_watermark(conn, 1)
            conn.close()
            update.check_scan_source(db, args)

    def test_upload_copy_is_self_contained(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            db = make_db(Path(tmp) / "manifest.db", [("a", "done", 3)])
            conn = sqlite3.connect(db)
            conn.execute("pragma journal_mode=wal")
            mirror.store_watermark(conn, 7)  # lives in the WAL until checkpoint
            out = update.snapshot_db(db)
            conn.close()
            self.assertEqual(rotate.summarize_manifest(out), S(1, 1, 3))
            copy = sqlite3.connect(out)
            self.assertEqual(mirror.stored_watermark(copy), 7)
            copy.close()


def backup_listing_s3(objects: list[tuple[str, int]]):
    """A fake `rotate.s3` listing `(key, unix mtime)` under `_state/`, two per page."""
    from datetime import datetime, timezone

    class ListingS3:
        deleted: list[dict] = []

        def list_objects_v2(self, **kwargs: object) -> dict:
            assert kwargs["Bucket"] == rotate.BUCKET
            assert kwargs["Prefix"] == rotate.STATE_KEY + ".bak-"
            start = int(str(kwargs.get("ContinuationToken", "0")))
            page = objects[start : start + 2]
            out: dict = {
                "Contents": [
                    {
                        "Key": key,
                        "Size": 10,
                        "LastModified": datetime.fromtimestamp(ts, timezone.utc),
                    }
                    for key, ts in page
                ]
            }
            if start + 2 < len(objects):
                out["IsTruncated"] = True
                out["NextContinuationToken"] = str(start + 2)
            return out

        def delete_object(self, **kwargs: object) -> None:
            ListingS3.deleted.append(kwargs)

    return ListingS3


BAK = rotate.STATE_KEY + ".bak-"


class BackupRetentionTest(unittest.TestCase):
    """#2040: `_state/manifest.db.bak-*` piles up, one per publishing run."""

    def backups(self) -> list[rotate.Backup]:
        return [
            rotate.Backup(BAK + "2026-11-17", 10, 100),
            rotate.Backup(BAK + "20261117T214822Z", 10, 300),
            rotate.Backup(BAK + "20261118T000000Z", 10, 200),
        ]

    def test_keeps_the_newest_by_modification_time(self) -> None:
        doomed = rotate.plan_backup_prune(self.backups(), keep=2)
        self.assertEqual([b.key for b in doomed], [BAK + "2026-11-17"])
        self.assertEqual(rotate.plan_backup_prune(self.backups(), keep=3), [])
        self.assertEqual(rotate.plan_backup_prune(self.backups(), keep=9), [])

    def test_always_keeps_at_least_one(self) -> None:
        for keep in (0, -1):
            with self.subTest(keep=keep), self.assertRaises(ValueError):
                rotate.plan_backup_prune(self.backups(), keep=keep)

    def test_never_touches_anything_but_a_backup(self) -> None:
        odd = self.backups() + [rotate.Backup(rotate.STATE_KEY, 10, 1)]
        with self.assertRaisesRegex(ValueError, "not a state DB backup"):
            rotate.plan_backup_prune(odd, keep=1)

    def test_lists_every_page(self) -> None:
        fake = backup_listing_s3([(BAK + "a", 1), (BAK + "b", 2), (BAK + "c", 3)])
        with mock.patch.object(rotate, "s3", fake):
            got = rotate.list_backups()
        self.assertEqual(
            [(b.key, b.mtime) for b in got],
            [(BAK + "a", 1), (BAK + "b", 2), (BAK + "c", 3)],
        )

    def run_prune(self, argv: list[str]):
        fake = backup_listing_s3([(BAK + "old", 1), (BAK + "mid", 2), (BAK + "new", 3)])
        fake.deleted = []
        with (
            mock.patch.object(rotate, "s3", fake),
            mock.patch.object(update, "fresh_work_db") as work,
            mock.patch("builtins.print"),
        ):
            status = update.main(argv)
        work.assert_not_called()  # pruning never runs a pass
        return status, fake.deleted

    def test_prune_is_a_dry_run_by_default(self) -> None:
        status, deleted = self.run_prune(["--prune-backups", "1"])
        self.assertEqual(status, 0)
        self.assertEqual(deleted, [])

    def test_prune_deletes_only_with_the_explicit_flag(self) -> None:
        status, deleted = self.run_prune(["--prune-backups", "1", "--delete"])
        self.assertEqual(status, 0)
        self.assertEqual(
            deleted,
            [
                {"Bucket": rotate.BUCKET, "Key": BAK + "mid"},
                {"Bucket": rotate.BUCKET, "Key": BAK + "old"},
            ],
        )

    def test_delete_without_prune_is_refused(self) -> None:
        with self.assertRaises(SystemExit):
            update.parse_args(["--delete"])

    def test_prune_does_not_mix_with_a_pass(self) -> None:
        for extra in (["--since", "2026-10-01"], ["--package", "a/b/c"]):
            with self.subTest(extra=extra), self.assertRaises(SystemExit):
                update.parse_args(["--prune-backups", "2", *extra])
