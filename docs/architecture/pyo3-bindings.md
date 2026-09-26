# PyO3 Python Bindings

## Build Contract

The binding crate uses the PyO3 0.29 dependency family in lockstep:
`pyo3`, `pyo3-build-config`, and `pyo3-async-runtimes`. It enables
`abi3-py310`, so one extension binary supports CPython 3.10 and newer.

Cross builds set `PYO3_NO_PYTHON=1` explicitly. PyO3 0.29 uses native
raw-dylib linking for stable-ABI Windows extensions, so these builds do not
download a target Python installation or set `PYO3_CROSS_LIB_DIR`,
`PYO3_CROSS_PYTHON_VERSION`, or `PYO3_CROSS_PYTHON_IMPLEMENTATION`. Target OS
SDKs and linkers are still required; those are separate from a target Python
runtime or import library.

## Consumer Contract

FastLED (`~/dev/fastled`) imports these from the `fbuild` Python package:

```python
from fbuild import connect_daemon, Daemon, find_firmware
from fbuild.api import SerialMonitor
from fbuild.daemon import ensure_daemon_running, stop_daemon
```

`find_firmware(project_dir, environment, firmware_name=None)` is a
non-mutating query backed by `fbuild_paths::find_firmware`; consumers must use
it instead of reconstructing `.fbuild/build` paths. Structured build/deploy
results also preserve the daemon's `output_file` and `output_dir` fields.

## SerialMonitor API

Must be a context manager with these methods:

```python
class SerialMonitor:
    def __init__(self, port: str, baud_rate: int = 115200,
                 hooks: list | None = None, auto_reconnect: bool = True,
                 verbose: bool = False): ...

    def __enter__(self) -> SerialMonitor: ...
    def __exit__(self, *args) -> bool: ...

    def read_lines(self, timeout: float = 30.0) -> Iterator[str]: ...
    def write(self, data: str) -> int: ...
    def write_json_rpc(self, request: dict, timeout: float = 5.0) -> dict: ...
    def interrupt_reads(self) -> None: ...  # additive, FastLED/fbuild#1485
```

### Implementation Strategy

`__enter__` attaches the shared `SerialSession` to the daemon's
`/ws/serial-monitor` WebSocket. The session's reader task dispatches data and
FIFO replies while `read_lines`, `write`, and `write_json_rpc` operate through
the shared core. `__exit__` detaches, closes the socket, and joins that reader.
The sync facade uses the process-shared `pyo3-async-runtimes` Tokio runtime and
releases the GIL around blocking calls. The daemon does not correlate RPC IDs;
`REMOTE:` serial lines go to RPC waiters in FIFO order.

### Concurrency: the shared session core (FastLED/fbuild#1485)

`SerialMonitor` (sync) and `AsyncSerialMonitor` (async) are thin facades over
one shared, PyO3-free core: `crate::serial_session::SerialSession`
(`crates/fbuild-python/src/serial_session.rs`). This superseded the #1431/#1484
polling hand-off (`ws_session::ReadYield`/`RpcRoute`) with a dedicated reader
task per session:

```
                       ┌────────────────────────── serial_session.rs ───────────────────────────┐
 WebSocket read half ──► reader task (one per session, owns WsSource)                            │
                       │   data lines ─(REMOTE:, RPC waiting)──► rpc slot    (oneshot per RPC)    │
                       │   data lines ─(everything else)───────► line queue  (bounded, Notify)    │
                       │   write_ack / in_waiting / error ─────► reply FIFO  (oneshot per request) │
                       │   port events ────────────────────────► status      (watch)              │
                       │ write path: sink mutex + "register pending reply, then send", atomically │
                       └───────────────▲──────────────────────────────────────▲───────────────────┘
                                       │                                      │
              serial_monitor.rs (sync facade)              async_serial_monitor.rs (async facade)
              waits on channels with py.detach              future_into_py over the same calls
```

The reader task is the **sole owner** of the WebSocket read half; nothing
else ever calls `.next()` on it. `read_lines`, `write`, `write_json_rpc` and
`in_waiting` all talk to the reader through channels, so a long `read_lines`
never blocks a concurrent `write` — the sync facade releases the GIL
(`py.detach`) for every call that can block, and the async facade awaits the
same core methods via `pyo3_async_runtimes::tokio::future_into_py`.

**Fixed lock order:** `sink` -> `request gate` -> `reply FIFO` -> `line queue` -> `status`. The
reader task never takes `sink`.

**Write registration is atomic.** A writer takes the `sink` lock, pushes its
oneshot onto the reply FIFO, *then* sends — under the same lock — so two
concurrent writers cannot register in one order and have their acks arrive in
the other. A `write_json_rpc` registers its RPC-reply oneshot *before*
writing, for the same reason.

**Replies are matched by FIFO order**, not by request id (the daemon has none
today); an `error` frame is treated as the reply to whatever the head of the
FIFO expected (the daemon can emit `error` in place of a `write_ack`, e.g. on
a bad-base64 write). A reply kind that doesn't match the head is a protocol
desync: it fails that request loudly (`RuntimeError`) rather than silently
reordering. `clear_input` (`clear_buffer`) registers no reply.

**Line-queue overflow policy:** bounded at `max_buffered_lines` (default
10,000); the *oldest* lines are dropped and counted
(`SerialSession::lines_dropped`, exposed as `lines_dropped` on both facades,
plus a `tracing::warn!` once per overflow burst). The
reader never stops draining the socket to apply backpressure — that would
also stall `write_ack`s, which travel on the same connection.

`read_lines` is cancel-safe: lines leave the queue only inside a synchronous
critical section that also returns them, so a dropped/cancelled future (or a
cancelled asyncio task) removes nothing. The async facade also keeps a pending
delivery copy until Python's Future completes; if asyncio cancels after the
Rust read resolves but before Python receives it, the batch returns to the
front of the queue. For the sync facade, a blocked
Python thread can't be cancelled that way, so `SerialMonitor.interrupt_reads()`
is an **additive** method that wakes every blocked reader; each returns `[]`
without draining, so queued lines stay for the next reader (FastLED #3219 —
the "every 2nd RPC times out" abandoned-reader failure mode).

### Async API (`AsyncSerialMonitor`, §4.7 — a supported API, not experimental)

| method | signature | returns |
|---|---|---|
| ctor | `AsyncSerialMonitor(port, baud_rate=115200, auto_reconnect=True, verbose=False)` | — |
| `__aenter__` / `__aexit__` | — | self / `False` |
| `read_lines` | `read_lines(timeout=30.0)` (`timeout_secs=` kept as a deprecated alias for one release) | `list[str]` |
| `write` | `write(data: str)` | **`int`** bytes written (breaking change from the pre-#1485 `bool`) |
| `write_json_rpc` | `write_json_rpc(request: dict, timeout=5.0)` | `dict` |
| `in_waiting` | `await mon.in_waiting()` (awaitable method; a property can't be awaited) | `int` |
| `reset_input_buffer` | `await mon.reset_input_buffer()` | `None` |
| `reset_device` | `await mon.reset_device(board=None, wait_for_output=False, timeout=5.0)` | `bool` |
| `lines_dropped` | `mon.lines_dropped` | `int` |

Migration note for the first release of this API: async `write()` now returns
the number of bytes written instead of `bool`. The old `read_lines(timeout_secs=)`
keyword remains accepted for one release, emits `DeprecationWarning`, and should
be replaced with `timeout=`. The synchronous `SerialMonitor.write()` contract is
unchanged; it still returns `0` on failure, whereas async `write()` raises.

### Error mapping (§4.8)

`SerialSession`'s `SessionError` is mapped once, in each facade, to Python
exceptions:

| `SessionError` | Python exception |
|---|---|
| `Timeout` | `TimeoutError` |
| `Closed`, `ConnectionFailed`, `PortGone`, `Preempted` | `ConnectionError` |
| `ProtocolDesync`, `WriteFailed` | `RuntimeError` |

The sync `write` keeps returning `0` on failure (compatibility); the async
`write` raises instead — document this difference to callers porting from
sync to async.

### Deadlock-freedom rules (enforced, not just documented)

1. No lock is held across `.await` — the `serial_session` module is
   `#[deny(clippy::await_holding_lock, clippy::await_holding_refcell_ref)]`.
2. No lock is held while calling into Python (hooks, `run_until` conditions,
   exception construction all run after every core lock is released).
3. The fixed lock order above.
4. The GIL is released for every sync call that can block, including
   `__enter__`/`__exit__`, not just `read_lines`.
5. `SerialMonitor` detects `tokio::runtime::Handle::try_current()` before
   blocking and raises `RuntimeError` instead of panicking/deadlocking if
   called from inside the async runtime (use `AsyncSerialMonitor` there).
6. The reader task runs under a drop guard: on panic or exit it marks the
   session `Closed` and fails every pending reply/RPC/line-waiter, so nobody
   waits on a dead task.
7. A `Drop` without `close()`/`__exit__` (GC, an exception in `with`, a
   leaked object) runs the same teardown: mark closed, abort the reader,
   wake everyone.

## DaemonConnection API

```python
class DaemonConnection:
    def __init__(self, project_dir: str, environment: str): ...
    def __enter__(self) -> DaemonConnection: ...
    def __exit__(self, *args) -> bool: ...

    def build(self, clean: bool = False, verbose: bool = False,
              timeout: float = 1800.0) -> bool: ...
    def deploy(self, port: str | None = None, clean: bool = False,
               skip_build: bool = False, monitor_after: bool = False,
               timeout: float = 1800.0) -> bool: ...
    def monitor(self, port: str | None = None, baud_rate: int | None = None,
                timeout: float | None = None) -> bool: ...
```

Uses `reqwest` internally to make HTTP requests to the daemon.

## FbuildSerialAdapter (FastLED side)

FastLED wraps `SerialMonitor` in a `ThreadPoolExecutor` because the sync `read_lines()` blocks:

```python
class FbuildSerialAdapter:
    async def read_lines(self, timeout):
        queue = asyncio.Queue()
        def _producer():
            for line in self._monitor.read_lines(timeout=timeout):
                loop.call_soon_threadsafe(queue.put_nowait, line)
            loop.call_soon_threadsafe(queue.put_nowait, None)
        self._executor.submit(_producer)
        while True:
            item = await queue.get()
            if item is None: break
            yield item
```

This pattern must continue working with the Rust-backed `SerialMonitor`.
