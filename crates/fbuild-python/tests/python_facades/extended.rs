use super::*;
/// AT-P10: FastLED #3219 replay. The adapter pattern (a reader thread per
/// request, abandoned via `interrupt_reads()` in `finally` after the
/// reply), 100 RPCs — every one succeeds, none waits for its timeout.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p10_fastled_3219_replay() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs::default());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import faulthandler, threading, time
faulthandler.dump_traceback_later(60, exit=True)
from _native import SerialMonitor

mon = SerialMonitor(port="TEST_PORT", baud_rate=115200)
mon.__enter__()
try:
    def rpc_via_adapter(i):
        # Mirrors FastLED's adapter: a dedicated reader thread per request
        # that the caller abandons via interrupt_reads() once it has its
        # reply, instead of waiting out a polling cap.
        result = {}
        done = threading.Event()

        def reader():
            while not done.is_set():
                for line in mon.read_lines(timeout=1.0):
                    if line == f"echo:req{i}":
                        result["line"] = line
                        done.set()
                        return

        t = threading.Thread(target=reader)
        t.start()
        try:
            start = time.monotonic()
            mon.write(f"req{i}")
            done.wait(timeout=5.0)
            elapsed = time.monotonic() - start
            assert done.is_set(), f"request {i} timed out after {elapsed}s"
            assert elapsed < 4.0, f"request {i} waited {elapsed}s (should resolve well under its 5s cap)"
        finally:
            mon.interrupt_reads()
            t.join(timeout=5)

    for i in range(100):
        rpc_via_adapter(i)
finally:
    mon.__exit__(None, None, None)
"#,
        );
    });
}

/// AT-P11: GIL-release matrix. A second Python thread counts while each
/// operation is blocked against a slowed daemon, and verifies it ran before
/// the operation returned.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p11_gil_release_matrix() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs {
        attach_delay: Duration::from_millis(500),
        ack_delay: Duration::from_millis(500),
        reset_delay: Duration::from_millis(500),
        reset_success: true,
        ..Default::default()
    });
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import faulthandler, json, threading, time
faulthandler.dump_traceback_later(60, exit=True)
from _native import SerialMonitor

mon = SerialMonitor(port="TEST_PORT", baud_rate=115200)
def measure(label, call):
    counter = [0]
    done = threading.Event()
    ran_during_call = threading.Event()
    def counting():
        time.sleep(0.1)
        while not done.is_set():
            counter[0] += 1
            ran_during_call.set()
    t = threading.Thread(target=counting)
    t.start()
    try:
        call()
    finally:
        done.set()
        t.join(timeout=5)
    assert ran_during_call.is_set() and counter[0] > 500, (
        f"{label}: Python counter did not run while the call was blocked: {counter[0]}"
    )

measure("__enter__", mon.__enter__)
try:
    measure("write", lambda: mon.write("x"))
    measure("write_json_rpc", lambda: mon.write_json_rpc({"id": 1}, timeout=5.0))
    measure("in_waiting", lambda: mon.in_waiting)
    mon.reset_input_buffer()
    measure("read_lines", lambda: mon.read_lines(timeout=0.6))
    measure("reset_device", lambda: mon.reset_device())
    # reset_input_buffer/clear_input is fire-and-forget by design (no
    # daemon reply to wait for -- see serial_session.rs's §4.2 notes), so
    # it is not part of this "blocked for a while" timing sample; it is
    # still exercised (GIL-released) directly by AT-P9's contract pin and
    # by ordinary use throughout this file.
finally:
    mon.__exit__(None, None, None)
"#,
        );
    });
}

/// AT-P12: in-process peer deadlock check. The fake daemon runs as an
/// asyncio server in a Python thread of the **same** process (no Rust-side
/// daemon at all for this test): `__enter__`, `write` and `read_lines` must
/// all succeed, which is only possible once the GIL is released on every
/// blocking path (otherwise the Python-thread daemon can't run while the
/// Rust runtime blocks waiting on it — the deadlock this PR's design
/// exists to prevent, per the #1484 validation note).
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p12_in_process_asyncio_peer() {
    init_python();
    let _env_guard = DAEMON_PORT_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import asyncio, base64, faulthandler, json, os, socket, struct, threading, time
faulthandler.dump_traceback_later(30, exit=True)
from _native import SerialMonitor

# A tiny hand-rolled WebSocket server: no `websockets` package dependency.
# Handles exactly the frames fbuild's protocol needs: text frames in,
# unmasked text frames out.

def _accept_key(key: str) -> str:
    import hashlib
    GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
    sha1 = hashlib.sha1((key + GUID).encode()).digest()
    return base64.b64encode(sha1).decode()

def _read_http_headers(sock):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = sock.recv(4096)
        if not chunk:
            break
        data += chunk
    return data.decode(errors="ignore")

def _send_text_frame(sock, text: str):
    payload = text.encode()
    header = bytearray([0x81])
    length = len(payload)
    if length < 126:
        header.append(length)
    else:
        header.append(126)
        header += struct.pack(">H", length)
    sock.sendall(bytes(header) + payload)

def _read_frame(sock):
    first2 = sock.recv(2)
    if len(first2) < 2:
        return None
    length = first2[1] & 0x7F
    masked = (first2[1] & 0x80) != 0
    if length == 126:
        length = struct.unpack(">H", sock.recv(2))[0]
    elif length == 127:
        length = struct.unpack(">Q", sock.recv(8))[0]
    mask = sock.recv(4) if masked else b"\x00\x00\x00\x00"
    payload = b""
    while len(payload) < length:
        payload += sock.recv(length - len(payload))
    if masked:
        payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    return payload.decode(errors="ignore")

def serve_one(server_sock, stop_flag):
    server_sock.settimeout(10.0)
    try:
        conn, _ = server_sock.accept()
    except socket.timeout:
        return
    conn.settimeout(10.0)
    headers = _read_http_headers(conn)
    key = None
    for line in headers.split("\r\n"):
        if line.lower().startswith("sec-websocket-key:"):
            key = line.split(":", 1)[1].strip()
    accept = _accept_key(key or "")
    resp = (
        "HTTP/1.1 101 Switching Protocols\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept}\r\n\r\n"
    )
    conn.sendall(resp.encode())

    attached = False
    while not stop_flag.is_set():
        try:
            text = _read_frame(conn)
        except (socket.timeout, OSError):
            break
        if text is None:
            break
        request = json.loads(text)
        if not attached:
            attached = True
            _send_text_frame(conn, json.dumps({
                "type": "attached", "success": True, "message": "ok", "writer_pre_acquired": True
            }))
            continue
        if request.get("type") == "write":
            decoded = base64.b64decode(request["data"])
            _send_text_frame(conn, json.dumps({
                "type": "write_ack", "success": True, "bytes_written": len(decoded), "message": None
            }))
            _send_text_frame(conn, json.dumps({
                "type": "data", "lines": [f"echo:{decoded.decode()}"], "current_index": 0
            }))
    conn.close()

server_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server_sock.bind(("127.0.0.1", 0))
server_sock.listen(1)
port = server_sock.getsockname()[1]
os.environ["FBUILD_DAEMON_PORT"] = str(port)

stop_flag = threading.Event()
server_thread = threading.Thread(target=serve_one, args=(server_sock, stop_flag))
server_thread.start()

mon = SerialMonitor(port="TEST_PORT", baud_rate=115200)
mon.__enter__()
try:
    n = mon.write("ping")
    assert isinstance(n, int) and n > 0, n
    lines = mon.read_lines(timeout=5.0)
    assert lines == ["echo:ping"], lines
finally:
    stop_flag.set()
    mon.__exit__(None, None, None)
    server_thread.join(timeout=5)
    server_sock.close()
"#,
        );
    });
}

/// AT-P13: re-entrancy. A `hooks=[...]` callback calls `mon.write()` and
/// `mon.write_json_rpc()`; a `run_until` condition calls `mon.write()`.
/// Must complete without deadlock, with replies delivered.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p13_reentrant_hooks_and_run_until() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs::default());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import faulthandler
faulthandler.dump_traceback_later(30, exit=True)
from _native import SerialMonitor

reentrant_results = []

def hook(line):
    if line == "echo:trigger":
        n = mon.write("from-hook")
        reentrant_results.append(("write", n))
        reply = mon.write_json_rpc({"id": 99}, timeout=5.0)
        reentrant_results.append(("rpc", reply))

mon = SerialMonitor(port="TEST_PORT", baud_rate=115200, hooks=[hook])
mon.__enter__()
try:
    mon.write("trigger")
    deadline_lines = []
    import time
    deadline = time.monotonic() + 5.0
    while len(reentrant_results) < 2 and time.monotonic() < deadline:
        mon.read_lines(timeout=0.2)
    assert len(reentrant_results) == 2, reentrant_results
    assert reentrant_results[0][0] == "write" and reentrant_results[0][1] > 0, reentrant_results
    assert reentrant_results[1][0] == "rpc" and reentrant_results[1][1] == {"id": 99}, reentrant_results

    # run_until's condition also calls mon.write() re-entrantly.
    condition_calls = []
    def condition(line):
        if not condition_calls:
            condition_calls.append(mon.write("from-condition"))
        return line == "echo:from-condition"

    ok = mon.run_until(condition, timeout=5.0)
    assert ok, "run_until did not observe its own re-entrant write's echo"
finally:
    mon.__exit__(None, None, None)
"#,
        );
    });
}

/// AT-P14: a sync method called from inside a coroutine on the pyo3 runtime
/// thread (via an `AsyncSerialMonitor` callback) raises `RuntimeError` with
/// the guidance message — no panic, no hang.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p14_sync_from_async_runtime_raises() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs::default());
    // `asyncio.run(...)` drives the coroutine on the *calling* Python
    // thread via `call_soon_threadsafe`, not literally on a tokio worker
    // thread — so a plain `async with AsyncSerialMonitor(...): sync_mon.__enter__()`
    // snippet does not actually reproduce "called from inside the async
    // runtime". `rt.block_on(...)` genuinely puts the *current* thread
    // inside the tokio runtime's context for its whole duration
    // (`tokio::runtime::Handle::try_current()` returns `Some` there
    // regardless of whether the thread is one of the runtime's own
    // worker threads), which is exactly the scenario §4.9 rule 5 guards:
    // a callback invoked synchronously from Rust code that is itself
    // running inside the shared runtime (e.g. a future spawned via
    // `future_into_py`, which `AsyncSerialMonitor` uses for every
    // awaitable).
    let rt = pyo3_async_runtimes::tokio::get_runtime();
    rt.block_on(async {
        pyo3::Python::attach(|py| {
            run_snippet(
                py,
                r#"
import faulthandler
faulthandler.dump_traceback_later(30, exit=True)
from _native import SerialMonitor

sync_mon = SerialMonitor(port="TEST_PORT_SYNC", baud_rate=115200)
try:
    sync_mon.__enter__()
    raised = False
except RuntimeError:
    raised = True
assert raised, "SerialMonitor.__enter__ from inside the async runtime must raise RuntimeError"
"#,
            );
        });
    });
}

/// AT-P15 (async): `__aexit__` while `read_lines`/`write`/`write_json_rpc`
/// are awaited in other tasks — those awaitables must raise
/// `ConnectionError` within 1s; `__aexit__` completes.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p15_aexit_while_calls_in_flight() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs {
        ack_delay: Duration::from_secs(2),
        ..Default::default()
    });
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import asyncio, faulthandler, time
faulthandler.dump_traceback_later(30, exit=True)
from _native import AsyncSerialMonitor

async def main():
    mon = AsyncSerialMonitor(port="TEST_PORT", baud_rate=115200)
    await mon.__aenter__()

    read_task = asyncio.ensure_future(mon.read_lines(timeout=10.0))
    write_task = asyncio.ensure_future(mon.write("slow"))
    rpc_task = asyncio.ensure_future(mon.write_json_rpc({"id": 1}, timeout=10.0))
    await asyncio.sleep(0.1)

    start = time.monotonic()
    await mon.__aexit__(None, None, None)

    for task, name in ((write_task, "write"), (rpc_task, "write_json_rpc")):
        try:
            await task
            raised = False
        except ConnectionError:
            raised = True
        assert raised, f"{name} must raise ConnectionError after __aexit__"
    elapsed = time.monotonic() - start
    assert elapsed < 1.0, f"pending calls took {elapsed}s to unblock after __aexit__"

    # The public async contract reports a closed session as ConnectionError.
    try:
        await asyncio.wait_for(read_task, timeout=1.0)
        raised = False
    except ConnectionError:
        raised = True
    assert raised, "read_lines must raise ConnectionError after __aexit__"

asyncio.run(main())
"#,
        );
    });
}

/// AT-P16: drop without exit. Create, enter, start a read, drop every
/// reference and `gc.collect()` — the reader task terminates (the
/// `_live_session_count()` test-only counter returns to 0).
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p16_drop_without_exit() {
    init_python();
    let (_port, daemon) = start_daemon_and_set_env(DaemonKnobs::default());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import faulthandler, gc, time
faulthandler.dump_traceback_later(30, exit=True)
from _native import SerialMonitor, _live_session_count

baseline = _live_session_count()

mon = SerialMonitor(port="TEST_PORT", baud_rate=115200)
mon.__enter__()
assert _live_session_count() == baseline + 1

# Start a read that we never wait for, then drop every reference.
import threading
t = threading.Thread(target=lambda: mon.read_lines(timeout=2.0))
t.start()
time.sleep(0.05)

del mon
gc.collect()

deadline = time.monotonic() + 5.0
while _live_session_count() != baseline and time.monotonic() < deadline:
    time.sleep(0.05)
assert _live_session_count() == baseline, "session leaked past drop + gc.collect()"
t.join(timeout=5)
"#,
        );
    });
    assert_daemon_saw_teardown(&daemon);
}

/// AT-P16 (async half): cancellation and garbage collection without
/// `__aexit__` must drop the session and stop its reader task.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p16_async_drop_without_exit() {
    init_python();
    let (_port, daemon) = start_daemon_and_set_env(DaemonKnobs::default());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import asyncio, faulthandler, gc, time
faulthandler.dump_traceback_later(30, exit=True)
from _native import AsyncSerialMonitor, _live_session_count

async def main():
    baseline = _live_session_count()
    mon = AsyncSerialMonitor(port="TEST_PORT", baud_rate=115200)
    await mon.__aenter__()
    assert _live_session_count() == baseline + 1
    read_task = asyncio.ensure_future(mon.read_lines(timeout=10.0))
    await asyncio.sleep(0.05)
    read_task.cancel()
    await asyncio.gather(read_task, return_exceptions=True)
    del read_task, mon
    gc.collect()
    deadline = time.monotonic() + 5.0
    while _live_session_count() != baseline and time.monotonic() < deadline:
        await asyncio.sleep(0.05)
        gc.collect()
    assert _live_session_count() == baseline, "async session leaked past drop + gc.collect()"

asyncio.run(main())
"#,
        );
    });
    assert_daemon_saw_teardown(&daemon);
}
