# Experimental prebuilt frameworks

Catalog resolution and integrity-checked artifacts. The independent Catalog reader
preserves the mirror Index's 24-hour cache and offline fallback without changing
PlatformIO's Release-only reader. Every shard read checks its Index descriptor.

The Index location uses `FBUILD_MIRROR_MANIFEST_URL`. Separate source-keyed
Index and Catalog files live beside the existing Index under `mirror-manifest/`.
They use 24-hour freshness and last-good offline fallback. Both descriptor SHA
and exact size are required on every Catalog read, including stale fallback.

`archive` implements synchronous, strict extraction used on daemon blocking
workers; `gc` implements synchronous collection under the daemon's live-map
mutex. These are fetch primitives; only the daemon owns transfer/publication.
