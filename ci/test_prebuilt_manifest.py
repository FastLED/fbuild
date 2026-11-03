"""Real-filesystem fixture tests for prebuilt Catalog production."""
import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from ci import gen_mirror_manifest as gen

class PrebuiltManifestTests(unittest.TestCase):
    def test_golden_catalog_and_missing_sidecar(self):
        with tempfile.TemporaryDirectory() as root:
            base = Path(root)
            fp = "a" * 64
            key = f"prebuilt/arduino-esp32/3.1.1/esp32/fp-{fp[:16]}/prebuilt.tar.zst"
            sidecar = {"platform":{"os":"none","arch":"esp32","abi":"gcc@1"},
                "variant":{"optimization":"Os","flavor":f"fp-{fp[:16]}"},
                "source":{"vcs":"git","repo_url":"https://example.com/framework","ref":"abc"},
                "fp":fp,"size_bytes":7,"sha256":"b"*64}
            path = base / (key + ".release.json")
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps(sidecar))
            docs = gen.prebuilt_documents([key, key.replace("/esp32/", "/esp32s3/")],
                lambda key: (base/key).read_bytes(), ["https://bucket.example/"])
            shard = json.loads(docs["shards/prebuilt-arduino-esp32.json"])
            self.assertEqual(shard["channels"], {"3.1.1":"3.1.1"})
            self.assertEqual(shard["releases"][0]["platforms"][0]["asset"]["urls"], ["https://bucket.example/"+key])
            self.assertEqual(shard["releases"][0]["platforms"][0]["asset"]["provides"], ["fbuild-fp:"+fp])
            self.assertEqual(shard["releases"][0]["source"],sidecar["source"])
            index = json.loads(gen.build_documents({}, {}, "1", prebuilt=docs)["manifest.json"])
            desc = index["tools"]["prebuilt-arduino-esp32"]["descriptor"]
            data = docs[desc["url"]].encode()
            self.assertEqual(desc["sha256"],hashlib.sha256(data).hexdigest())
            self.assertEqual(desc["size_bytes"],len(data))



class PrebuiltBuilderTests(unittest.TestCase):
    def test_packages_engine_object_mapping_and_canonical_inputs(self):
        import io
        import tarfile
        import zstandard
        from ci import build_prebuilt as builder
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            obj = root / "random-output-name.o"
            obj.write_bytes(b"real filesystem object fixture")
            (root / "framework.h").write_text("#define FRAMEWORK_VALUE 17\n")
            identity = {"id": "fixture", "version": "1", "sha256": "a" * 64}
            plan = {"abi_version": 1, "toolchain": identity, "framework": identity,
                "mcu": "esp32", "variant_dir": "/fbuild/framework/variants/esp32",
                "argv": [["/fbuild/toolchain/gcc", "-Os", "-c", "/fbuild/framework/core.c", "-o", "/fbuild/obj/0.o"]],
                "config_sha256": {}, "path_map_prefix": "/fbuild"}
            fp = hashlib.sha256(builder.canonical(plan)).hexdigest()
            metadata = {"fp": fp, "plan": plan, "objects": {"obj/0.o": str(obj)}, "include_dirs": [str(root)]}
            source = {"vcs": "git", "repo_url": "https://example.com/framework", "ref": "a" * 40}
            artifact, sidecar, key = builder.package(metadata, root / "out", source)
            with zstandard.ZstdDecompressor().stream_reader(io.BytesIO(artifact.read_bytes())) as stream:
                data = stream.read()
            with tarfile.open(fileobj=io.BytesIO(data)) as archive:
                self.assertEqual(archive.getmembers()[0].name, "fbuild-prebuilt.json")
                manifest = json.load(archive.extractfile("fbuild-prebuilt.json"))
                self.assertEqual(manifest["fp"], fp)
                self.assertEqual(manifest["inputs"], plan)
                for name, digest in manifest["files"].items():
                    self.assertEqual(hashlib.sha256(archive.extractfile(name).read()).hexdigest(), digest)
            descriptor = json.loads(sidecar.read_bytes())
            self.assertEqual(descriptor["sha256"], hashlib.sha256(artifact.read_bytes()).hexdigest())
            self.assertEqual(descriptor["size_bytes"], artifact.stat().st_size)
            self.assertEqual(key, f"prebuilt/arduino-esp32/1/esp32/fp-{fp[:16]}/prebuilt.tar.zst")

if __name__ == "__main__": unittest.main()
