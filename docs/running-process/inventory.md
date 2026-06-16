# fbuild ↔ running-process v1 broker: wire & cache inventory

This is the required inventory the first fbuild broker-adoption PR records before
changing runtime behavior (per the running-process fbuild adoption guide). It
captures the exact current fbuild daemon model, request/response wire, cache
roots, CI trust grouping, and rollback path, then selects the migration lane
from the guide's encoding decision table.

Tracker: zackees/running-process#437 · FastLED/fbuild#510 ·
payload-protocol registration: zackees/running-process#440.

## Required inventory

| Area | Record |
|---|---|
| **daemon model** | A single **persistent per-user daemon** (`fbuild-daemon`), an axum HTTP/WebSocket server. The CLI spawns it detached (`ensure_daemon_running`) and it self-evicts on idle. It is **not** one-worker-per-build and **not** direct child processes. Dev/prod isolation: `FBUILD_DEV_MODE=1` → port 8865 + `~/.fbuild/dev/`; prod → port 8765 + `~/.fbuild/prod/`. |
| **request wire** | **JSON over loopback HTTP** (`http://127.0.0.1:<port>`). Requests are `serde_json`-serialized structs POSTed to fixed paths: `/api/build`, `/api/deploy`, `/api/monitor`, `/api/test-emu`, plus `GET /api/daemon/info`, `GET /api/cache/stats`, `POST /api/cache/gc`, `GET /api/locks/status`, `GET /health`. Build supports an NDJSON streaming variant. **No explicit protocol-version constant exists today** — compatibility is the implicit "same endpoints + JSON schemas as the Python FastAPI daemon" contract (see `crates/fbuild-daemon/README.md`). This PR introduces an explicit `FBUILD_PROTOCOL_VERSION = 1` for the fbuild payload schema. |
| **response wire** | **JSON** (`OperationResponse`, `DaemonInfoResponse`, `CacheStatsResponse`, …). Error envelope: non-2xx HTTP status with a JSON body (or a plain `OperationResponse { success: false, message }`). Retry behavior: the **client** retries daemon *spawn* with backoff `[0.0s, 0.5s, 2.0s]` and polls `/health`; individual RPCs are not auto-retried (the 100 ms connect-timeout fails fast on ECONNREFUSED). |
| **cache roots** | Resolved from `fbuild-paths`: **artifact** = `get_cache_root()` (`~/.fbuild/<mode>/cache`, or `FBUILD_CACHE_DIR`); **index** = `<cache>/index`; **temp** = `<fbuild_root>/tmp`; **log** = daemon dir (`~/.fbuild/<mode>/daemon/daemon.log`); **lock** = daemon dir (`fbuild_daemon.pid`, `daemon.port`, in-memory project/port locks) plus per-install lock dirs co-located with package/toolchain/framework final install paths; **runtime** = directory holding the relocated `fbuild-daemon` binary; **config** = `get_fbuild_root()` (`~/.fbuild/<mode>`). The artifact root is the ownership boundary for large shared packages/toolchains/frameworks/sidecars. Runtime is provenance only and must not fork artifact ownership by daemon version. |
| **CI trust grouping** | Local dev = a single per-user shared cache (`SHARED_BROKER`). CI jobs that intentionally isolate trust groups use a dedicated `EXPLICIT_INSTANCE` named **`ci-trusted`** so a poisoned/untrusted job cannot reach another group's negotiated backend. |
| **rollback path** | `RUNNING_PROCESS_DISABLE=1` selects the legacy direct loopback-HTTP path (`DaemonClient`). `FbuildBrokerSession::adopt` honours it first and returns `AdoptOutcome::UseDirectPath` without dialing the broker. (`FBUILD_RUNNING_PROCESS_BROKER=1` remains the opt-in broker request flag from the prior minimal seam.) |

## Local daemon/cache identity

Local fbuild uses one authoritative artifact repository per daemon/cache
identity:

```text
(user, dev/prod mode, canonical cache root, trust domain)
```

The canonical cache root is `FBUILD_CACHE_DIR` when set, otherwise
`~/.fbuild/<mode>/cache`. The local trust domain is `local-shared`; CI may opt
into a separate `EXPLICIT_INSTANCE` trust domain such as `ci-trusted`.

Daemon/backend version is intentionally not part of this identity. A broker may
select a different fbuild runtime, but that selection must not create a private
package/toolchain/framework/managed-sidecar store. Local `SHARED_BROKER`
service definitions therefore publish cache identity labels in addition to the
`CacheManifest`, and implementations should either reuse/upgrade/refuse a
daemon for the same cache identity rather than silently forking the artifact
repository.

Package/toolchain/framework installs take a process-shared sibling lock before
network fetch or extraction begins, then re-check the final install path while
holding that lock. This keeps different projects or daemon processes from
duplicating large downloads for the same global artifact root.

Dependency installs publish best-effort phase updates (`waiting_for_lock`,
`downloading`, `verifying`, `extracting`, `installed`) through daemon status.
`/api/daemon/info` and `/ws/status` expose the latest dependency install object
with package name, version, phase, installer/waiter role, message, and lock path
when a caller is blocked on another installer.

## Encoding decision

| Inventory result | Selected lane (from the guide's table) |
|---|---|
| Current fbuild request/response wire is **JSON** | **JSON lane**: keep the JSON direct path, add a prost broker path, and run golden-message **parity tests for both encodings.** |

### Why this lane works for fbuild

The daemon already has exactly one JSON body parser per endpoint. The broker
path therefore wraps the **verbatim** JSON body inside a thin prost envelope
(`FbuildRequest { protocol_version, op, payload_json, request_id }`) carried over
the v1 `Frame` lane (`FBUILD_PAYLOAD_PROTOCOL = 0x7EB1`). The daemon dispatches
on `op` and feeds `payload_json` to its existing parser — so the broker path is a
**pure framing change, not a schema fork**. The single internal model
(`BrokerRequest` / `BrokerResponse`, in `crates/fbuild-daemon/src/broker/protocol.rs`)
serializes to JSON for the direct path and to prost for the broker path, and the
parity tests assert the two encodings of the same message stay in lock-step.

## Target state

| Component | Target |
|---|---|
| broker control plane | v1 `Frame` / `Hello` / `HelloReply` / `Refused` |
| service payloads | prost `FbuildRequest` / `FbuildResponse` (this PR) |
| service name | `fbuild` |
| default isolation | `SHARED_BROKER` (per-user local builds) |
| CI isolation | `EXPLICIT_INSTANCE "ci-trusted"` |
| rollback | `RUNNING_PROCESS_DISABLE=1` → direct loopback-HTTP path |

## Platform endpoints

fbuild uses the broker endpoint strings running-process returns and follows its
platform contract: Linux Unix-domain socket under
`$XDG_RUNTIME_DIR/running-process/broker` (with `/tmp/running-process-{uid}/broker`
fallback); macOS socket under `$TMPDIR/.rp-{uid}`; Windows bare named-pipe name
(no `\\.\pipe\` prefix at the API boundary). Service definitions and manifests use
the platform service/manifest directories already encoded in
`fbuild_paths::running_process` and the broker builders.
