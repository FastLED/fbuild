"""The mirror-manifest generator must not lose work it already recorded.

Regressions for FastLED/fbuild#1825:

- ``--version`` claimed to keep the shards' version but wrote ``"1"``, so a
  bare re-run would rewrite every shard's version;
- the incremental change check compared size only, so an object replaced with
  same-size bytes was never re-hashed and its committed SHA-256 went stale.
"""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from ci import gen_mirror_manifest as gen


class FakePaginator:
    def __init__(self, pages: list[dict]) -> None:
        self.pages = pages

    def paginate(self, **kwargs: object) -> list[dict]:
        return self.pages


class FakeS3:
    def __init__(self, pages: list[dict]) -> None:
        self.pages = pages

    def get_paginator(self, name: str) -> FakePaginator:
        return FakePaginator(self.pages)


def component(key: str, size: int, sha: str) -> dict:
    return {"id": key, "asset": {"size_bytes": size, "sha256": sha}}


class TestListing(unittest.TestCase):
    def test_keeps_registry_keys_and_normalizes_etags(self) -> None:
        pages = [
            {
                "Contents": [
                    {"Key": "own/pkg/tool/1.0/a.tar.gz", "Size": 7, "ETag": '"abc"'},
                    {"Key": "own/pkg/tool/1.0/b.tar.gz", "Size": 9, "ETag": '"def-3"'},
                    {"Key": "_state/manifest.db", "Size": 1, "ETag": '"x"'},
                ]
            }
        ]
        objects = gen.list_bucket(FakeS3(pages))
        self.assertEqual(sorted(objects), ["own/pkg/tool/1.0/a.tar.gz", "own/pkg/tool/1.0/b.tar.gz"])
        self.assertEqual(objects["own/pkg/tool/1.0/a.tar.gz"], gen.ListedObject(7, "abc"))
        self.assertEqual(objects["own/pkg/tool/1.0/b.tar.gz"].etag, "def-3")

    def test_object_without_etag_is_still_listed(self) -> None:
        pages = [{"Contents": [{"Key": "own/pkg/tool/1.0/a.tar.gz", "Size": 7}]}]
        self.assertIsNone(gen.list_bucket(FakeS3(pages))["own/pkg/tool/1.0/a.tar.gz"].etag)


class TestStaleness(unittest.TestCase):
    def test_new_object_is_stale(self) -> None:
        self.assertTrue(gen.is_stale(gen.ListedObject(5, "a"), None))

    def test_resized_object_is_stale(self) -> None:
        self.assertTrue(gen.is_stale(gen.ListedObject(6, "a"), gen.RecordedObject(5, "a", "s")))

    def test_same_size_replacement_is_stale(self) -> None:
        listed = gen.ListedObject(5, "etag-new")
        self.assertTrue(gen.is_stale(listed, gen.RecordedObject(5, "etag-old", "s")))

    def test_unchanged_object_is_not_stale(self) -> None:
        listed = gen.ListedObject(5, "etag")
        self.assertFalse(gen.is_stale(listed, gen.RecordedObject(5, "etag", "s")))

    def test_shard_only_records_do_not_force_a_full_rehash(self) -> None:
        # A record read from a committed shard has no ETag; comparing it by
        # size keeps an upgrade from re-downloading the whole mirror.
        listed = gen.ListedObject(5, "etag")
        self.assertFalse(gen.is_stale(listed, gen.RecordedObject(5, None, "s")))

    def test_stale_keys_is_sorted(self) -> None:
        objects = {"b": gen.ListedObject(1, None), "a": gen.ListedObject(1, None)}
        self.assertEqual(gen.stale_keys(objects, {}), ["a", "b"])


class TestState(unittest.TestCase):
    def test_round_trips_size_etag_and_sha(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / gen.STATE_NAME
            path.write_text("5\tetag\tdeadbeef\town/pkg/tool/1.0/a.tar.gz\n", encoding="utf-8")
            self.assertEqual(
                gen.load_state(path),
                {
                    "own/pkg/tool/1.0/a.tar.gz": gen.RecordedObject(5, "etag", "deadbeef"),
                },
            )

    def test_reads_the_old_three_column_format(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / gen.STATE_NAME
            path.write_text("5\ndeadbeef\town/pkg/tool/1.0/a.tar.gz\n".replace("\ndea", "\tdea"), encoding="utf-8")
            self.assertEqual(gen.load_state(path)["own/pkg/tool/1.0/a.tar.gz"].etag, None)

    def test_missing_or_blank_file_is_empty(self) -> None:
        self.assertEqual(gen.load_state(None), {})
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / gen.STATE_NAME
            path.write_text("\n\n", encoding="utf-8")
            self.assertEqual(gen.load_state(path), {})

    def test_default_state_path_sits_beside_the_published_tree(self) -> None:
        out_dir = Path("/repo/site/mirror")
        self.assertEqual(gen.default_state_path(out_dir), Path("/repo/site") / gen.STATE_NAME)


class TestVersion(unittest.TestCase):
    def make_shards(self, root: Path, versions: list[str]) -> Path:
        out_dir = root / "site" / "mirror"
        (out_dir / "shards").mkdir(parents=True)
        for index, version in enumerate(versions):
            shard = {"version": version, "components": []}
            (out_dir / "shards" / f"{index:016x}.json").write_text(json.dumps(shard))
        (out_dir / "shards" / "names-0.json").write_text(json.dumps({"version": "ignored"}))
        return out_dir

    def test_keeps_the_version_the_shards_carry(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out_dir = self.make_shards(Path(tmp), ["2026.10.31", "2026.10.31", "2026.10.30"])
            self.assertEqual(gen.existing_version(out_dir), "2026.10.31")

    def test_falls_back_when_there_are_no_shards(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            self.assertEqual(gen.existing_version(Path(tmp)), gen.DEFAULT_VERSION)

    def test_shard_loader_keeps_size_and_sha_without_an_etag(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out_dir = Path(tmp)
            (out_dir / "shards").mkdir()
            shard = {"components": [component("own/pkg/tool/1.0/a.tar.gz", 5, "deadbeef")]}
            (out_dir / "shards" / "0123456789abcdef.json").write_text(json.dumps(shard))
            (out_dir / "shards" / "names-0.json").write_text(
                json.dumps({"components": [component("own/pkg/tool/own", 5, "deadbeef")]})
            )
            self.assertEqual(
                gen.load_existing(out_dir),
                {"own/pkg/tool/1.0/a.tar.gz": gen.RecordedObject(5, None, "deadbeef")},
            )


class TestIncrementalHash(unittest.TestCase):
    def run_hash_missing(self, objects: dict, known: dict, state: Path) -> dict:
        with mock.patch.object(gen, "hash_object", return_value="newsha") as hasher:
            recorded = gen.hash_missing(objects, known, 1, state)
        self.hashed = [call.args[0] for call in hasher.call_args_list]
        return recorded

    def test_same_size_replacement_is_hashed_again_and_the_etag_recorded(self) -> None:
        key = "own/pkg/tool/1.0/a.tar.gz"
        objects = {key: gen.ListedObject(5, "etag-new")}
        known = {key: gen.RecordedObject(5, "etag-old", "oldsha")}
        with tempfile.TemporaryDirectory() as tmp:
            state = Path(tmp) / gen.STATE_NAME
            recorded = self.run_hash_missing(objects, known, state)
            self.assertEqual(self.hashed, [key])
            self.assertEqual(recorded[key], gen.RecordedObject(5, "etag-new", "newsha"))
            self.assertIn(f"5\tetag-new\tnewsha\t{key}", state.read_text(encoding="utf-8"))

    def test_unchanged_object_is_reused_without_a_download(self) -> None:
        key = "own/pkg/tool/1.0/a.tar.gz"
        objects = {key: gen.ListedObject(5, "etag")}
        known = {key: gen.RecordedObject(5, "etag", "oldsha")}
        with tempfile.TemporaryDirectory() as tmp:
            recorded = self.run_hash_missing(objects, known, Path(tmp) / gen.STATE_NAME)
            self.assertEqual(self.hashed, [])
            self.assertEqual(recorded[key].sha256, "oldsha")


if __name__ == "__main__":
    unittest.main()
