# site/mirror

`manifest.json` is fbuild's server-side download fallback settings
(FastLED/fbuild#1810): a [zackees/manifest.json](https://github.com/zackees/manifest.json)
v1 `Release` whose `components[].asset.urls` list, in order, where each
artifact can be fetched. Each asset has a `sha256`, and fbuild rejects bytes
from a mirror that don't match it.

Clients fetch it from <https://fastled.github.io/fbuild/mirror/manifest.json>.
If that fails they try the raw copy on `main`. See
`crates/fbuild-packages-fetch/README.md` for the client behaviour.

Do not hand-edit it. Regenerate it after the bucket or fbuild's download URLs
change:

```bash
uv run --no-project python ci/gen_mirror_manifest.py
uvx --from git+https://github.com/zackees/manifest.json manifest-validate site/mirror/manifest.json
```
