"""The registry mirror tool in ``ci/mirror/`` (FastLED/fbuild#2040).

``mirror.py`` fills the bucket and writes the state DB that
``ci/gen_mirror_manifest.py`` reads; ``rotate.py`` supervises it on rotating
droplets. These tests cover the pure helpers and the download/upload path
against in-process fakes. Nothing here touches the network or the bucket.
"""

from __future__ import annotations

import asyncio
import hashlib
import sys
import tempfile
import unittest
from pathlib import Path

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


if __name__ == "__main__":
    unittest.main()
