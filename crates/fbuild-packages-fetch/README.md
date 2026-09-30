# fbuild-packages-fetch

Extracted from `fbuild-packages` for compile parallelism (FastLED/fbuild#1008 Phase B). Re-exported by the `fbuild-packages` facade at unchanged paths.

## Download retry policy and mirrors (FastLED/fbuild#1463)

`downloader.rs` retries transient failures (connect/timeout/body/truncation, 5xx, 429, 408) up to 5 attempts with waits of 5, 15, 30 and 60 s, each equal-jittered to 50-100 %, and honours a server's `Retry-After` (capped at 120 s). Operator overrides:

| Variable | Meaning |
|---|---|
| `FBUILD_DOWNLOAD_MAX_ATTEMPTS` | Attempts per URL, clamped to 1-20 (default 5). For streaming downloads it bounds *consecutive attempts that make no progress*: an attempt that advances the partial file resets the count (a separate 40-attempt ceiling remains), so `1` does not disable resuming a download that is making headway. |
| `FBUILD_DOWNLOAD_MAX_WAIT` | Ceiling in seconds on any single *scheduled* backoff (default 60). A server's `Retry-After` is not bound by it (only by the separate 120 s cap). |
| `FBUILD_DOWNLOAD_MIRRORS` | Comma/whitespace separated fallbacks tried, in order, only after the primary URL exhausts its budget. Each is a base URL (the file name is appended) or a template containing `{filename}`. Downloaded bytes are still checked against the caller's SHA-256. |

A failed fetch reports the attempt count and time spent, and names any mirrors tried.
