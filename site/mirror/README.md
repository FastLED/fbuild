# site/mirror

fbuild's server-side download fallback settings (FastLED/fbuild#1810, #1815),
in [zackees/manifest.json](https://github.com/zackees/manifest.json) v1 format:

- `manifest.json`: an `Index` mapping each registry owner (the first segment of
  a `dl.registry.platformio.org/download/` path) to a shard via
  `{url, sha256, size_bytes}`.
- `shards/`: one `Release` per owner. Each component lists
  `urls: [bucket, registry]`, `size_bytes`, and a `sha256` that fbuild computed
  from the bucket's own copy. fbuild rejects bucket bytes that don't match it.

Clients fetch from <https://fastled.github.io/fbuild/mirror/manifest.json>,
then from the raw copy on `main`. See `crates/fbuild-packages-fetch/README.md`
for client behaviour.

Do not hand-edit these files. Regenerate them after the bucket changes. Only
new or resized objects are downloaded and hashed:

```bash
uv run --no-project --with boto3 python ci/gen_mirror_manifest.py --credentials <spaces-credentials.json>
uvx --from git+https://github.com/zackees/manifest.json manifest-validate site/mirror/manifest.json site/mirror/shards/*.json
```
