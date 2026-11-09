"""Contract tests for publishing per-chip toolchain slices (FastLED/fbuild#1944)."""

import json
import re
import tempfile
import unittest
from pathlib import Path

from ci.publish_toolchain_slices import (
    HOST_KEYS,
    OWNER,
    build_component,
    check,
    load_hosts,
    publish,
    register,
    slice_key,
    upsert_component,
    validate_host,
    whole_key,
)

VENDOR_BYTES = b"vendor-bytes-that-the-metadata-pins"

VENDOR = (
    "https://github.com/espressif/crosstool-NG/releases/download/"
    "esp-14.2.0_20260121/xtensa-esp-elf-14.2.0_20260121-x86_64-linux-gnu.tar.xz"
)
SHA = "da31f36d79d4e99f24e55a90a71e65d5694714f16199960bf7885724b706a48c"
INDEX = {
    "kind": "Index",
    "schema_version": 1,
    "tools": {
        "@esp-libs": {
            "descriptor": {
                "url": "shards/esp-libs.json",
                "sha256": "a" * 64,
                "size_bytes": 7,
            },
            "kind_hint": "data",
        }
    },
}


class FakeS3:
    """Records every write; `fail_on` raises for a key containing that text."""

    def __init__(self, fail_on: str = ""):
        self.objects = {}
        self.acls = []
        self.fail_on = fail_on

    def upload_file(self, path, bucket, key):
        if self.fail_on and self.fail_on in key:
            raise RuntimeError("upload refused")
        self.objects[key] = Path(path).read_bytes()

    def put_object_acl(self, Bucket, Key, ACL):
        self.acls.append((Key, ACL))

    def head_object(self, Bucket, Key):
        return {"ContentLength": len(self.objects[Key])}


def metadata(
    version="esp-14.2.0_20260121", sha=SHA, size=176559292, status="recommended"
):
    return {
        "tools": [
            {
                "name": "toolchain-xtensa-esp-elf",
                "versions": [
                    {
                        "name": version,
                        "status": status,
                        "linux-amd64": {"url": VENDOR, "sha256": sha, "size": size},
                        "macos-arm64": {
                            "url": VENDOR + ".mac",
                            "sha256": sha,
                            "size": size,
                        },
                    }
                ],
            }
        ]
    }


def slices(chips=("esp32s3", "esp32")):
    return {
        "slices": [
            {
                "chip": chip,
                "name": f"xtensa-esp-elf-{chip}.tar.zst",
                "sha256": f"{i + 1:02x}" * 32,
                "size_bytes": 1000 + i,
            }
            for i, chip in enumerate(chips)
        ]
    }


def sha256(data: bytes) -> str:
    import hashlib

    return hashlib.sha256(data).hexdigest()


def write(path: Path, text: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    return path


def tree(root: Path) -> Path:
    """A minimal toolchain tree the real slicer can select and archive."""
    (root / "bin").mkdir(parents=True)
    for chip in ("esp32", "esp32s3"):
        write(root / "lib" / chip / "chip.a", chip)
    write(root / "lib" / "libc.a", "shared")
    write(root / "bin" / "xtensa-esp-elf-gcc", "#!/bin/sh\nexit 0\n")
    write(
        root / "package.json",
        json.dumps({"name": "xtensa-esp-elf", "version": "14.2.0_20260121"}),
    )
    return root


class Temp(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="toolchain-publish-")
        self.root = Path(self._tmp.name)
        self.shard = write(self.root / "mirror" / "shards" / "esp-toolchain.json", "{}")
        self.index = write(self.root / "mirror" / "manifest.json", json.dumps(INDEX))
        self.out = self.root / "slices"

    def tearDown(self):
        self._tmp.cleanup()

    def run_publish(self, s3=None, **kwargs):
        s3 = s3 or FakeS3()
        kwargs.setdefault("slices", slices())
        kwargs.setdefault("slices_dir", self.prepared())
        kwargs.setdefault("archive", self.archive())
        result = publish(
            self.meta(),
            "toolchain-xtensa-esp-elf",
            "linux-amd64",
            self.shard,
            self.index,
            s3,
            **kwargs,
        )
        return s3, result

    def archive(self):
        """A stand-in vendor bundle whose pins are what `metadata()` declares."""
        path = self.root / "xtensa-esp-elf-14.2.0_20260121-x86_64-linux-gnu.tar.xz"
        path.write_bytes(VENDOR_BYTES)
        return path

    def meta(self):
        return metadata(sha=sha256(VENDOR_BYTES), size=len(VENDOR_BYTES))

    def prepared(self, chips=("esp32s3", "esp32")):
        """Slice files on disk matching the fabricated index."""
        for i, chip in enumerate(chips):
            self.out.mkdir(parents=True, exist_ok=True)
            (self.out / f"xtensa-esp-elf-{chip}.tar.zst").write_bytes(b"s" * (1000 + i))
        return self.out

    def shard_doc(self):
        return json.loads(self.shard.read_text())

    def index_doc(self):
        return json.loads(self.index.read_text())


class LoadHostsTests(unittest.TestCase):
    def test_recommended_version_is_selected_and_the_vendor_prefix_is_stripped(self):
        version, hosts = load_hosts(metadata(), "toolchain-xtensa-esp-elf")
        self.assertEqual(version, "14.2.0_20260121")
        self.assertEqual(set(hosts), {"linux-amd64", "macos-arm64"})

    def test_a_version_with_no_recommended_status_is_an_error_not_a_guess(self):
        with self.assertRaisesRegex(ValueError, "recommended"):
            load_hosts(metadata(status=None), "toolchain-xtensa-esp-elf")

    def test_an_unknown_tool_never_falls_back_to_another_one(self):
        with self.assertRaisesRegex(ValueError, "0 tools named"):
            load_hosts(metadata(), "toolchain-riscv32-esp")

    def test_a_version_name_with_a_path_separator_is_refused(self):
        with self.assertRaisesRegex(ValueError, "not a safe path token"):
            load_hosts(metadata(version="esp-../../etc"), "toolchain-xtensa-esp-elf")


class PinTests(unittest.TestCase):
    def test_a_missing_or_short_sha256_is_fatal(self):
        _, hosts = load_hosts(metadata(), "toolchain-xtensa-esp-elf")
        hosts["linux-amd64"]["sha256"] = "abc"
        with self.assertRaisesRegex(ValueError, "sha256"):
            validate_host(hosts, "linux-amd64")

    def test_a_zero_size_is_fatal(self):
        with self.assertRaisesRegex(ValueError, "positive integer"):
            validate_host(
                {"linux-amd64": {"url": VENDOR, "sha256": SHA, "size": 0}},
                "linux-amd64",
            )

    def test_an_unpublished_host_is_fatal(self):
        _, hosts = load_hosts(metadata(), "toolchain-xtensa-esp-elf")
        with self.assertRaisesRegex(ValueError, "win32"):
            validate_host(hosts, "win32")


class ComponentTests(unittest.TestCase):
    def entry(self):
        _, hosts = load_hosts(metadata(), "toolchain-xtensa-esp-elf")
        return validate_host(hosts, "linux-amd64")

    def test_the_bucket_copy_comes_first_and_the_vendor_url_stays_as_fallback(self):
        component = build_component(
            "toolchain-xtensa-esp-elf",
            "14.2.0_20260121",
            "linux-amd64",
            self.entry(),
            {"esp32s3": {}},
            "https://bucket/whole.tar.xz",
        )
        self.assertEqual(
            component["asset"]["urls"], ["https://bucket/whole.tar.xz", VENDOR]
        )
        self.assertEqual(component["asset"]["sha256"], SHA)
        self.assertEqual(component["asset"]["size_bytes"], 176559292)

    def test_an_unmirrored_component_is_refused(self):
        # The committed-manifest invariant every client relies on: bucket first.
        with self.assertRaisesRegex(ValueError, "mirrored"):
            build_component(
                "toolchain-xtensa-esp-elf",
                "14.2.0_20260121",
                "linux-amd64",
                self.entry(),
                {"esp32s3": {}},
                "",
            )

    def test_a_component_with_no_slice_is_refused(self):
        with self.assertRaisesRegex(ValueError, "no slices"):
            build_component(
                "toolchain-xtensa-esp-elf",
                "14.2.0_20260121",
                "linux-amd64",
                self.entry(),
                {},
                None,
            )

    def test_a_later_publish_keeps_the_chips_an_earlier_run_published(self):
        shard = {
            "components": [
                {
                    "id": "t/1/h",
                    "asset": {"slices": {"esp32s3": {"url": "old-s3"}}},
                }
            ]
        }
        merged = upsert_component(
            shard, {"id": "t/1/h", "asset": {"slices": {"esp32": {"url": "new-p2"}}}}
        )
        self.assertEqual(
            merged["components"][0]["asset"]["slices"],
            {"esp32s3": {"url": "old-s3"}, "esp32": {"url": "new-p2"}},
        )

    def test_republishing_a_chip_replaces_its_slice(self):
        shard = {
            "components": [
                {"id": "t/1/h", "asset": {"slices": {"esp32": {"url": "stale"}}}}
            ]
        }
        merged = upsert_component(
            shard, {"id": "t/1/h", "asset": {"slices": {"esp32": {"url": "fresh"}}}}
        )
        self.assertEqual(
            merged["components"][0]["asset"]["slices"], {"esp32": {"url": "fresh"}}
        )

    def test_slice_keys_are_content_addressed_so_changed_bytes_never_reuse_a_key(self):
        record = {"name": "xtensa-esp-elf-esp32s3.tar.zst", "sha256": "ab" * 32}
        key = slice_key(
            "14.2.0_20260121",
            "linux-amd64",
            "esp32s3",
            record,
            "toolchain-xtensa-esp-elf",
        )
        self.assertEqual(
            key,
            "v1/espressif/toolchain-xtensa-esp-elf/release/14.2.0_20260121/"
            "linux-amd64/esp32s3/xtensa-esp-elf-esp32s3.abababababababab.tar.zst",
        )

    def test_upsert_replaces_the_same_component_and_keeps_the_others(self):
        shard = {"components": [{"id": "a/1", "asset": {}}, {"id": "b/1", "asset": {}}]}
        upsert_component(shard, {"id": "a/1", "asset": {"urls": ["u"]}})
        self.assertEqual([c["id"] for c in shard["components"]], ["b/1", "a/1"])
        self.assertEqual(shard["components"][1]["asset"], {"urls": ["u"]})


class WholeKeyTests(unittest.TestCase):
    def test_the_key_follows_the_vendor_filename_given_to_it(self):
        self.assertEqual(
            whole_key("1.0", "linux-amd64", "a-b.tar.xz", "toolchain-x"),
            "v1/espressif/toolchain-x/release/1.0/linux-amd64/whole/a-b.tar.xz",
        )

    def test_a_filename_that_is_not_a_safe_token_is_refused(self):
        with self.assertRaisesRegex(ValueError, "unsafe"):
            whole_key("1.0", "linux-amd64", "bundle name.tar.xz", "toolchain-x")

    def test_a_path_in_the_filename_cannot_escape_the_whole_prefix(self):
        # `.name` keeps the traversal out of the key; the prefix is still fixed.
        key = whole_key("1.0", "linux-amd64", "../../etc/passwd", "toolchain-x")
        self.assertEqual(
            key, "v1/espressif/toolchain-x/release/1.0/linux-amd64/whole/passwd"
        )


class PublishTests(Temp):
    def test_the_whole_archive_is_stored_under_the_vendor_filename(self):
        # An operator downloads into whatever name is convenient (`arm64.tar.xz`
        # is how the linux-arm64 bundle arrived). The key must still be the
        # vendor asset name, or the shard's filename and object diverge.
        alias = self.root / "arm64.tar.xz"
        alias.write_bytes(VENDOR_BYTES)
        s3, _ = self.run_publish(archive=alias)
        whole = [key for key in s3.objects if "/whole/" in key]
        self.assertEqual(len(whole), 1)
        self.assertTrue(
            whole[0].endswith(
                "/xtensa-esp-elf-14.2.0_20260121-x86_64-linux-gnu.tar.xz"
            ),
            whole[0],
        )
        asset = self.shard_doc()["components"][0]["asset"]
        self.assertEqual(Path(asset["urls"][0]).name, asset["filename"])

    def test_every_slice_is_uploaded_public_read_and_head_checked(self):
        s3, result = self.run_publish()
        self.assertEqual(result["chips"], ["esp32", "esp32s3"])
        slices = {key for key in s3.objects if "/whole/" not in key}
        whole = {key for key in s3.objects if "/whole/" in key}
        self.assertEqual(len(slices), 2)
        self.assertEqual(len(whole), 1, "the whole archive is mirrored bucket-first")
        self.assertEqual({acl for _, acl in s3.acls}, {"public-read"})
        self.assertTrue(
            all(
                key.startswith("v1/espressif/toolchain-xtensa-esp-elf/release/")
                for key in s3.objects
            )
        )

    def test_the_shard_lists_only_published_chips_so_others_fall_back(self):
        self.run_publish()
        asset = self.shard_doc()["components"][0]["asset"]
        self.assertEqual(set(asset["slices"]), {"esp32", "esp32s3"})
        self.assertEqual(asset["slices"]["esp32s3"]["format"], "tar.zst")

    def test_the_index_descriptor_hashes_exactly_the_shard_bytes(self):
        import hashlib

        self.run_publish()
        payload = self.shard.read_bytes()
        descriptor = self.index_doc()["tools"][OWNER]["descriptor"]
        self.assertEqual(descriptor["sha256"], hashlib.sha256(payload).hexdigest())
        self.assertEqual(descriptor["size_bytes"], len(payload))
        self.assertEqual(descriptor["url"], "shards/esp-toolchain.json")

    def test_registering_preserves_other_index_owners(self):
        self.run_publish()
        self.assertIn("@esp-libs", self.index_doc()["tools"])

    def test_a_failed_upload_leaves_the_shard_and_index_untouched(self):
        before_shard, before_index = self.shard.read_text(), self.index.read_text()
        s3 = FakeS3(fail_on="/esp32/")
        with self.assertRaises(RuntimeError):
            self.run_publish(s3)
        self.assertEqual(self.shard.read_text(), before_shard)
        self.assertEqual(self.index.read_text(), before_index)

    def test_a_short_upload_is_caught_by_the_head_check_and_publishes_nothing(self):
        before_shard, before_index = self.shard.read_text(), self.index.read_text()

        class Short(FakeS3):
            def upload_file(self, path, bucket, key):
                super().upload_file(path, bucket, key)
                self.objects[key] = self.objects[key][:10]

        with self.assertRaisesRegex(ValueError, "uploaded 10 bytes"):
            self.run_publish(Short())
        self.assertEqual(self.shard.read_text(), before_shard)
        self.assertEqual(self.index.read_text(), before_index)

    def test_a_second_run_is_byte_identical_and_does_not_duplicate_the_component(self):
        self.run_publish()
        first = self.shard.read_bytes()
        self.run_publish()
        self.assertEqual(self.shard.read_bytes(), first)
        self.assertEqual(len(self.shard_doc()["components"]), 1)

    def test_a_whole_archive_mismatching_the_metadata_is_refused_before_upload(self):
        archive = self.root / "xtensa-esp-elf.tar.xz"
        archive.write_bytes(b"not the pinned bytes")
        with self.assertRaisesRegex(ValueError, "does not match the metadata pins"):
            self.run_publish(archive=archive)
        self.assertEqual(self.shard_doc(), {})

    def test_the_real_slicer_output_is_uploaded_under_the_chip_key(self):
        src = tree(self.root / "tree")
        s3 = FakeS3()
        result = publish(
            self.meta(),
            "toolchain-xtensa-esp-elf",
            "linux-amd64",
            self.shard,
            self.index,
            s3,
            tree=src,
            chips=("esp32s3",),
            slices_dir=self.out,
            archive=self.archive(),
        )
        self.assertEqual(result["chips"], ["esp32s3"])
        self.assertEqual(len(s3.objects), 2)
        key = next(k for k in s3.objects if "/whole/" not in k)
        self.assertIn(
            "/linux-amd64/esp32s3/xtensa-esp-elf-14.2.0_20260121-esp32s3.", key
        )
        self.assertEqual(
            s3.objects[key],
            (self.out / "xtensa-esp-elf-14.2.0_20260121-esp32s3.tar.zst").read_bytes(),
        )


class CheckTests(Temp):
    def test_a_published_shard_and_index_agree(self):
        self.run_publish()
        result = check(self.shard, self.index)
        self.assertEqual(result["chips"], ["esp32", "esp32s3"])

    def test_a_regenerated_index_that_dropped_the_owner_is_caught(self):
        self.run_publish()
        regenerated = dict(INDEX)
        self.index.write_text(json.dumps(regenerated))
        with self.assertRaisesRegex(ValueError, "does not list"):
            check(self.shard, self.index)

    def test_a_descriptor_for_different_shard_bytes_is_caught(self):
        self.run_publish()
        index = self.index_doc()
        index["tools"][OWNER]["descriptor"]["sha256"] = "b" * 64
        self.index.write_text(json.dumps(index))
        with self.assertRaisesRegex(ValueError, "does not describe"):
            check(self.shard, self.index)


class RegisterTests(Temp):
    def test_register_refuses_an_index_that_is_not_an_index(self):
        self.index.write_text(json.dumps({"kind": "Release", "tools": {}}))
        with self.assertRaisesRegex(ValueError, "not an Index"):
            register(self.shard, self.index, {"id": "x", "asset": {"urls": ["u"]}})

    def test_register_creates_a_shard_when_none_exists_yet(self):
        self.shard.unlink()
        register(self.shard, self.index, {"id": "x", "asset": {"urls": ["u"]}})
        self.assertEqual(
            self.shard_doc()["tool"], "fbuild-download-mirror/esp-toolchain"
        )
        self.assertEqual(self.shard_doc()["components"][0]["id"], "x")


class HostKeyParityTests(unittest.TestCase):
    """The publisher must know every host key the client can ask for (#1956)."""

    detect = (
        Path(__file__).resolve().parents[1]
        / "crates"
        / "fbuild-toolchain"
        / "src"
        / "toolchain"
        / "esp32_metadata.rs"
    )

    def client_keys(self) -> set[str]:
        """The string literals `detect_platform()` can return."""
        source = self.detect.read_text()
        start = source.index("pub fn detect_platform()")
        body = source[start : source.index("\n}\n\n", start)]
        return set(re.findall(r'"([a-z0-9-]+)"', body))

    def test_every_host_the_client_can_resolve_is_publishable(self):
        keys = self.client_keys()
        self.assertIn("win64", keys)  # the Windows host key, not the vendor's win32
        self.assertEqual(keys - set(HOST_KEYS), set())

    def test_a_win64_package_is_pinned_like_any_other_host(self):
        doc = metadata()
        doc["tools"][0]["versions"][0]["win64"] = {
            "url": VENDOR + ".zip",
            "sha256": SHA,
            "size": 413859845,
        }
        _, hosts = load_hosts(doc, "toolchain-xtensa-esp-elf")
        self.assertEqual(validate_host(hosts, "win64")["size"], 413859845)


if __name__ == "__main__":
    unittest.main()
