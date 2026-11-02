# Heap profiling the daemon

The `fbuild-daemon` outlives a build, so it is where a slow leak hides: #1360
caught one at ~3.9 GB resident while *idle*, still climbing, until the health
probe failed and every build reported a compile error. A sampled heap profile
is how you find what is retaining that memory. This page covers the two ways to
capture one and the step people skip: **symbolizing it**.

## Capture from process start

```bash
FBUILD_HEAP_PROFILE=1 fbuild-daemon --dev --port 8977
```

Set it *before* the daemon starts. Startup and static-initialization
allocations are only visible this way — anything allocated before the profiler
starts is untracked. The value may also be a sample rate in bytes
(`FBUILD_HEAP_PROFILE=65536` samples about every 64 KiB; finer is more
resolution and more overhead).

## Capture from a daemon that is already wedged

```bash
curl -X POST http://127.0.0.1:8977/api/daemon/heap-dump
```

This is the path #1360 needs: restarting the daemon to turn profiling on would
destroy the very leak under investigation. The endpoint starts a session on
demand when none is running, but then the snapshot only covers allocations made
*after* the call — the response says so rather than letting a thin profile read
as "nothing is leaking".

The endpoint is **loopback-only**. A dump is not a read of a log line: it can
switch process-wide profiling on and make the daemon serialize its heap. Treat
the file as sensitive.

## Where dumps land

Snapshots are written under the daemon's own root, respecting dev/prod
isolation:

```
<fbuild root>/tmp/heap-profiles/heap-<pid>-<epoch-ms>-<n>.pb
```

The filename carries the daemon PID so repeated dumps do not overwrite each
other. The format is pprof `profile.proto`, the same file `go tool pprof` and
every flame-graph viewer already read.

## Symbolizing a dump (the part that is easy to get wrong)

A dump is **addresses plus a mapping table**, not a function table. Measured on
a `--release` daemon: 5 samples, 12 locations, **0 functions**, with mappings
for the daemon binary and each loaded system library. That is by design — the
capture keeps the raw addresses and leaves naming to an external symbolizer.

So the dump only resolves against the **same build's debug info**:

```bash
go tool pprof -http=:0 /path/to/fbuild-daemon heap-<pid>-<ms>-<n>.pb
```

### Which binary to point it at

- **A local `--release` build**: use the binary you built. `[profile.release]`
  sets `debug = "line-tables-only"` and `split-debuginfo = "packed"`, and Cargo
  leaves it unstripped, so the frames resolve directly. Measured sizes for the
  daemon: 171 MB unstripped vs 46 MB after `strip`.
- **A wheel / release install**: the shipped binaries are stripped, so the
  installed `fbuild-daemon` alone will *not* name frames — you get the nearest
  dynamic symbol or `??:?`. Download the matching debug sidecar from the same
  GitHub release, `fbuild-<version>-<target>-debug` (it contains
  `fbuild-daemon.dwp` on Linux, `fbuild-daemon.dSYM` on macOS, and
  `fbuild-daemon.pdb` on Windows), and point the symbolizer at that pair.

### Check the build actually matches

`GET /health` returns a `build_id` alongside `version` and `pid`. A sidecar from
a different build will resolve addresses to the *wrong* function names without
failing — worse than `??`. Confirm the version in the health payload against the
release tag before trusting a symbolized report.

### Evidence this works

Offsets from a real dump, base-subtracted against the daemon mapping, resolved
with `addr2line -f -C -e <binary>`:

| offset | unstripped release binary | stripped copy |
|---|---|---|
| `0x2adb09a` | `_mi_prof_stack_intern` (mimalloc-pprof-amalgamated.c:21444) | `pthread_create` |
| `0x2adcb0b` | `_mi_prof_on_alloc` (mimalloc-pprof-amalgamated.c:21253) | `pthread_create` |
| `0x2adfc69` | `mi_page_malloc_zero` (mimalloc-pprof-amalgamated.c:7171) | `pthread_create` |
| `0x2aa4b8a` | `tokio::runtime::scheduler::multi_thread::worker::create` | `??:?` |
| `0x2764ab8` | `fbuild_core::usb::profiles::validate_and_index` (profiles.rs:49) | `??:?` |

Same file, same addresses: only the unstripped binary names the frames. If your
report is full of `??`, you are pointing at a stripped binary, not at a leak
that "does not exist".

## Related

- [Symbol bloat analysis](../symbols.md) — `fbuild symbols`, for target ELFs.
- [Daemon architecture](../architecture/runtime.md) — what the daemon holds.
