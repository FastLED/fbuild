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

if __name__ == "__main__": unittest.main()
