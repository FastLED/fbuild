# Daemon-owned prebuilt storage

Single-flight fetching, compressed-byte resume, verified staging and cache GC.
Only daemon instances construct this service; build callers receive Hit or Miss.
No file locks are used. Withdrawal requires deletion of the bucket object.

The daemon exposes a callback in each opted-in build request. Eight callers for
one asset share an in-memory OnceCell. Downloads retain raw compressed bytes
with URL/ETag/owner/progress metadata; a resumed request uses Range + If-Range and
feeds the retained prefix through a fresh decoder before incoming bytes.
Extraction runs on a blocking worker while the network stream is downloaded.

No staging directory is published until whole-stream size/SHA, first-member
fingerprint/ABI and every regular file hash pass. Entries are renamed atomically
and registered with the existing disk-cache LRU and completion sentinel. Active
build clients lease entries. Failed integrity pairs and failed-hit fingerprints
are tombstoned for 24 hours in daemon memory. A compile/link failure after a Hit
retries once after discarding project outputs; Hits never populate the reusable
source framework-core cache.

V1 is deliberately limited to the two pilot boards with conventional `src/`
projects and default compile inputs. Custom flags, unflags, extra scripts, board
overrides, forced includes, response files and ambiguous symlinked roots Miss.
Source packages are still provisioned before planning; this slice does not save
framework source-download bytes or implement background source population.
