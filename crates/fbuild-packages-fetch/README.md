# fbuild-packages-fetch

Extracted from `fbuild-packages` for compile parallelism (FastLED/fbuild#1008 Phase B). Re-exported by the `fbuild-packages` facade at unchanged paths.

## Download retry policy and mirrors (FastLED/fbuild#1463)

`downloader.rs` retries transient failures (connect/timeout/body/truncation, 5xx, 429, 408) up to 5 attempts with waits of 5, 15, 30 and 60 s, each equal-jittered to 50-100 %, and honours a server's `Retry-After` (capped at 120 s). Operator overrides:

| Variable | Meaning |
|---|---|
| `FBUILD_DOWNLOAD_MAX_ATTEMPTS` | Attempts per URL, clamped to 1-20 (default 5). For streaming downloads it bounds *consecutive attempts that make no progress*: an attempt that advances the partial file resets the count (a separate 40-attempt ceiling remains), so `1` does not disable resuming a download that is making headway. |
| `FBUILD_DOWNLOAD_MAX_WAIT` | Ceiling in seconds on any single *scheduled* backoff (default 60). A server's `Retry-After` is not bound by it (only by the separate 120 s cap). |
| `FBUILD_DOWNLOAD_MIRRORS` | Comma/whitespace separated mirrors. Each is a base URL (the file name is appended) or a template containing `{filename}` or `{path}`. **Unset** means fbuild's own bucket, `https://fbuild.sfo3.digitaloceanspaces.com/{path}`, for PlatformIO registry and GitHub downloads (FastLED/fbuild#1806); set it to replace that list, or to an empty string to opt out. Downloaded bytes are still checked against the caller's SHA-256. |
| `FBUILD_DOWNLOAD_MIRROR_FIRST` | Truthy (anything but empty, `0`, `false`) tries every mirror *before* the primary URL; falsy tries the primary first. The primary is always in the list, so a mirror missing the file still falls through. Unset means mirror-first for the built-in bucket and primary-first for mirrors you configured. |

`{path}` is the key a mirror bucket stores the file under:

- a `dl.registry.platformio.org` download maps to the part after `/download/`,
  e.g. `platformio/tool/framework-arduino-avr/5.4.0/framework-arduino-avr-5.4.0.tar.gz`;
- any other URL degrades to the file name, exactly as `{filename}` does.

fbuild's own bucket uses this layout, so builds hit it by default. Downloads
from other hosts skip the built-in mirror and go straight to the vendor.

```bash
# Vendor first, bucket as fallback:
export FBUILD_DOWNLOAD_MIRROR_FIRST=0
# No mirror at all:
export FBUILD_DOWNLOAD_MIRRORS=
```

### Server-side fallback settings (FastLED/fbuild#1810, #1815)

Before falling back to the built-in bucket rule, fbuild checks
[zackees/manifest.json](https://github.com/zackees/manifest.json) v1 documents
published at <https://fastled.github.io/fbuild/mirror/manifest.json> (source:
[`site/mirror/`](../../site/mirror/README.md)). That document is an `Index`
mapping each registry owner to a shard `Release` via a
`{url, sha256, size_bytes}` descriptor. fbuild fetches the shard only for the
owner of the URL being downloaded, and only uses it if it hashes to the
descriptor. Verified shards are cached under
`<cache root>/mirror-manifest/shards/`; when no source serves the current
shard, the last cached one is used. A plain `Release` is also accepted as the
top document. The shards carry a fbuild-computed `sha256` for every object in
the bucket, so bucket bytes are verified for every registry download, not
just the ones named in the code. An asset listing
the requested URL decides the candidate URLs and their order. Its `sha256`
guards mirror bytes: a mirror serving anything else is skipped. Vendor bytes
are never rejected for a manifest mismatch; verifying them is the caller's job.
`yanked: true` means vendor only.

Precedence: `FBUILD_DOWNLOAD_MIRRORS` (set, even empty) > manifest entry >
built-in bucket > vendor alone. `FBUILD_DOWNLOAD_MIRROR_FIRST`, when set,
overrides the manifest's order.

| Variable | Meaning |
|---|---|
| `FBUILD_MIRROR_MANIFEST_URL` | Manifest source(s), comma/whitespace separated. Empty disables the manifest. Default: the Pages URL, then the raw file on `main`. |

The manifest is fetched once per process (10 s budget per source) and cached
under `<cache root>/mirror-manifest/` for 24 h. If the server is down or serves
an invalid document, fbuild uses the stale cache; with no cache, it uses the
built-in rule. One invalid entry (non-HTTPS URL, duplicate URL, bad sha256,
wrong `kind`/`schema_version`, unsafe shard path) rejects the whole document.
A body over 8 MiB is also rejected. Downloads to the loopback host never
consult the manifest.

fbuild contacts PlatformIO only for the primary download. It never asks the
registry for checksums or alternative mirrors, and no PlatformIO mirror host is
ever a candidate. The bucket mirrors only the registry layout; GitHub URLs are
not mirrored.

A failed fetch reports the attempt count and time spent, and names any mirrors tried.
