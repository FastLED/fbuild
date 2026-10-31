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
- a `github.com` URL maps to `github.com/<owner>/<repo>/…`, so equal archive
  names in different repos never collide;
- any other URL degrades to the file name, exactly as `{filename}` does.

fbuild's own bucket uses this layout, so builds hit it by default. Downloads
from other hosts skip the built-in mirror and go straight to the vendor.

```bash
# Vendor first, bucket as fallback:
export FBUILD_DOWNLOAD_MIRROR_FIRST=0
# No mirror at all:
export FBUILD_DOWNLOAD_MIRRORS=
```

A failed fetch reports the attempt count and time spent, and names any mirrors tried.
