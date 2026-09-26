use super::*;
/// AT-P1: Python thread A blocked in `read_lines(timeout=10)`, thread B
/// calls `write` x10 — no exception, each `write` returns its byte count
/// quickly, and A receives all the echoes. Regression guard for #1431.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p1_write_during_long_read_from_python_threads() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs::default());

    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import faulthandler, threading, time
faulthandler.dump_traceback_later(30, exit=True)

from _native import SerialMonitor

mon = SerialMonitor(port="TEST_PORT", baud_rate=115200)
mon.__enter__()
try:
    results = []
    def reader():
        results.append(mon.read_lines(timeout=10.0))

    t = threading.Thread(target=reader)
    t.start()
    time.sleep(0.2)

    write_times = []
    for i in range(10):
        start = time.monotonic()
        n = mon.write(f"m{i}")
        write_times.append(time.monotonic() - start)
        assert isinstance(n, int) and n > 0, f"write() must return a positive byte count, got {n}"
    assert all(t < 0.05 for t in write_times), f"a write waited behind read_lines: {write_times}"

    # read_lines() returns as soon as anything is queued (may be a partial
    # batch), so keep reading until all 10 echoes are collected.
    t.join(timeout=5)
    assert not t.is_alive(), "reader thread did not return"
    lines = list(results[0])
    deadline = time.monotonic() + 5.0
    while len(lines) < 10 and time.monotonic() < deadline:
        lines.extend(mon.read_lines(timeout=0.5))
    assert len(lines) == 10, f"expected 10 echoes, got {lines}"
finally:
    mon.__exit__(None, None, None)
"#,
        );
    });
}

/// AT-P2: a sync JSON-RPC caller runs while another Python thread is
/// blocked in `read_lines`; the reader must never steal `REMOTE:` replies.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p2_sync_rpc_during_long_read() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs::default());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import faulthandler, threading, time
faulthandler.dump_traceback_later(30, exit=True)
from _native import SerialMonitor

mon = SerialMonitor(port="TEST_PORT", baud_rate=115200)
mon.__enter__()
seen = []
done = threading.Event()
def reader():
    while not done.is_set():
        seen.extend(mon.read_lines(timeout=10.0))

t = threading.Thread(target=reader)
t.start()
try:
    time.sleep(0.1)
    for i in range(10):
        assert mon.write_json_rpc({"id": i}, timeout=2.0) == {"id": i}
    assert all(not line.startswith("REMOTE:") for line in seen), seen
finally:
    done.set()
    mon.interrupt_reads()
    t.join(timeout=2.0)
    assert not t.is_alive(), "RPC reader remained blocked"
    mon.__exit__(None, None, None)
"#,
        );
    });
}

/// AT-P9 (contract pin): both facades' Python-visible signatures
/// must exactly match the §4.6/§4.7 tables, so a change here is caught
/// immediately instead of silently drifting from the documented contract.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p9_api_contract_pin() {
    init_python();
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import faulthandler, inspect
faulthandler.dump_traceback_later(30, exit=True)
from _native import SerialMonitor, AsyncSerialMonitor

sig = inspect.signature(SerialMonitor)
names = list(sig.parameters.keys())
assert names == ["port", "baud_rate", "hooks", "auto_reconnect", "verbose"], names
assert {p.name: p.default for p in list(sig.parameters.values())[1:]} == {
    "baud_rate": 115200, "hooks": None, "auto_reconnect": True, "verbose": False
}

read_lines_sig = inspect.signature(SerialMonitor.read_lines)
assert {p.name: p.default for p in list(read_lines_sig.parameters.values())[1:]} == {"timeout": 30.0}
assert {p.name: p.default for p in list(inspect.signature(SerialMonitor.write_json_rpc).parameters.values())[1:]} == {
    "request": inspect.Parameter.empty, "timeout": 5.0
}
assert {p.name: p.default for p in list(inspect.signature(SerialMonitor.run_until).parameters.values())[1:]} == {
    "condition": inspect.Parameter.empty, "timeout": 30.0
}

reset_sig = inspect.signature(SerialMonitor.reset_device)
reset_params = {p.name: p.default for p in list(reset_sig.parameters.values())[1:]}
assert reset_params == {"board": None, "wait_for_output": False, "timeout": 5.0}, reset_params

assert hasattr(SerialMonitor, "interrupt_reads"), "interrupt_reads must be additive on SerialMonitor"
assert hasattr(SerialMonitor, "lines_dropped")

async_sig = inspect.signature(AsyncSerialMonitor)
async_names = list(async_sig.parameters.keys())
assert async_names == ["port", "baud_rate", "auto_reconnect", "verbose"], async_names
assert {p.name: p.default for p in list(async_sig.parameters.values())[1:]} == {
    "baud_rate": 115200, "auto_reconnect": True, "verbose": False
}

async_read_lines = inspect.signature(AsyncSerialMonitor.read_lines)
async_params = {p.name: p.default for p in list(async_read_lines.parameters.values())[1:]}
assert async_params == {"timeout": 30.0, "timeout_secs": None}, async_params
assert {p.name: p.default for p in list(inspect.signature(AsyncSerialMonitor.write_json_rpc).parameters.values())[1:]} == {
    "request": inspect.Parameter.empty, "timeout": 5.0
}

async_reset_sig = inspect.signature(AsyncSerialMonitor.reset_device)
async_reset_params = {p.name: p.default for p in list(async_reset_sig.parameters.values())[1:]}
assert async_reset_params == {"board": None, "wait_for_output": False, "timeout": 5.0}, async_reset_params

for method in ("write", "write_json_rpc", "in_waiting", "reset_input_buffer", "reset_device", "__aenter__", "__aexit__"):
    assert hasattr(AsyncSerialMonitor, method), f"AsyncSerialMonitor missing {method}"
assert hasattr(AsyncSerialMonitor, "lines_dropped")

import asyncio, warnings
async def check_timeout_alias_warning():
    mon = AsyncSerialMonitor(port="TEST_PORT")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always", DeprecationWarning)
        try:
            await mon.read_lines(timeout_secs=0)
        except ConnectionError:
            pass  # no session was entered; only the alias warning matters
    assert any(issubclass(w.category, DeprecationWarning) for w in caught), caught

asyncio.run(check_timeout_alias_warning())
"#,
        );
    });
}

/// A negative daemon ACK preserves the sync `0` compatibility behavior but
/// must raise from the supported async API, and neither facade may shift the
/// next write's FIFO slot.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn failed_write_ack_maps_differently_in_sync_and_async_facades() {
    let (_port, _daemon) = start_daemon_and_set_env(DaemonKnobs {
        fail_first_write: true,
        ..Default::default()
    });
    init_python();
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import asyncio, faulthandler
faulthandler.dump_traceback_later(30, exit=True)
from _native import SerialMonitor, AsyncSerialMonitor

with SerialMonitor(port="COM_TEST") as mon:
    assert mon.write("x") == 0
    assert mon.write("next") == 4

async def check_async():
    async with AsyncSerialMonitor(port="COM_TEST") as mon:
        try:
            await mon.write("x")
        except RuntimeError as error:
            assert "injected serial failure" in str(error), str(error)
        else:
            raise AssertionError("async write accepted a failed daemon ACK")
        assert await mon.write("next") == 4

asyncio.run(check_async())
"#,
        );
    });
}

/// AT-P3: GIL release. While Python thread A blocks in `read_lines(2)`,
/// thread B runs a pure-Python counter loop; B must make measurable
/// progress during A's wait.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p3_gil_released_during_read_lines() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs::default());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import faulthandler, threading, time
faulthandler.dump_traceback_later(30, exit=True)
from _native import SerialMonitor

mon = SerialMonitor(port="TEST_PORT", baud_rate=115200)
mon.__enter__()
try:
    counter = [0]
    stop = threading.Event()
    def counting():
        while not stop.is_set():
            counter[0] += 1

    t = threading.Thread(target=counting)
    t.start()
    time.sleep(0.05)
    before = counter[0]
    mon.read_lines(timeout=2.0)
    after = counter[0]
    stop.set()
    t.join(timeout=5)
    assert after - before > 1000, f"counter barely moved during read_lines: before={before} after={after}"
finally:
    mon.__exit__(None, None, None)
"#,
        );
    });
}

/// AT-P4: `__exit__` while another thread reads. Must be a clean return
/// (or `RuntimeError` on the borrow) — never a hang or crash — and the
/// reader thread must return.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p4_exit_while_reading() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs::default());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import faulthandler, threading, time
faulthandler.dump_traceback_later(30, exit=True)
from _native import SerialMonitor

mon = SerialMonitor(port="TEST_PORT", baud_rate=115200)
mon.__enter__()

t = threading.Thread(target=lambda: mon.read_lines(timeout=5.0))
t.start()
time.sleep(0.1)
try:
    mon.__exit__(None, None, None)
except RuntimeError:
    pass  # acceptable: PyO3 "already borrowed"
t.join(timeout=5)
assert not t.is_alive(), "reader thread did not return after __exit__"
"#,
        );
    });
}

/// AT-P5 (async): `asyncio.gather(mon.read_lines(10), writes x10)` — all
/// writes return ints, all echoes delivered, wall time under 2s.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p5_async_gather_read_and_writes() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs::default());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import asyncio, faulthandler, time
faulthandler.dump_traceback_later(30, exit=True)
from _native import AsyncSerialMonitor

async def main():
    async with AsyncSerialMonitor(port="TEST_PORT", baud_rate=115200) as mon:
        start = time.monotonic()

        async def writer(i):
            n = await mon.write(f"m{i}")
            assert isinstance(n, int) and n > 0, n
            return n

        reader_task = asyncio.ensure_future(mon.read_lines(timeout=10.0))
        writer_tasks = [asyncio.ensure_future(writer(i)) for i in range(10)]
        results = await asyncio.gather(reader_task, *writer_tasks)
        elapsed = time.monotonic() - start
        assert elapsed < 2.0, f"gather took {elapsed}s"

        lines = list(results[0])
        deadline = time.monotonic() + 5.0
        while len(lines) < 10 and time.monotonic() < deadline:
            lines.extend(await mon.read_lines(timeout=0.5))
        assert len(lines) == 10, f"expected 10 echoes, got {lines}"

asyncio.run(main())
"#,
        );
    });
}

/// AT-P6 (async, folds in AT-P2's scenario): `write_json_rpc` concurrent
/// with a long `read_lines` — correct replies, the reader never sees
/// `REMOTE:`.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p6_async_write_json_rpc_concurrent_with_read() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs::default());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import asyncio, faulthandler
faulthandler.dump_traceback_later(30, exit=True)
from _native import AsyncSerialMonitor

async def main():
    async with AsyncSerialMonitor(port="TEST_PORT", baud_rate=115200) as mon:
        reader_task = asyncio.ensure_future(mon.read_lines(timeout=3.0))

        async def rpc(i):
            reply = await mon.write_json_rpc({"id": i}, timeout=5.0)
            assert reply == {"id": i}, reply

        await asyncio.gather(*[rpc(i) for i in range(10)])
        lines = await reader_task
        assert all(not l.startswith("REMOTE:") for l in lines), lines

asyncio.run(main())
"#,
        );
    });
}

/// AT-P7 (async): cancel `read_lines` tasks repeatedly
/// (`asyncio.wait_for(..., 0.001)` x500) while lines stream — no lines lost
/// or duplicated, no `InvalidStateError`/panic.
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p7_async_cancel_read_lines_repeatedly() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs::default());
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import asyncio, faulthandler
faulthandler.dump_traceback_later(60, exit=True)
from _native import AsyncSerialMonitor

async def main():
    async with AsyncSerialMonitor(port="TEST_PORT", baud_rate=115200) as mon:
        total_sent = 50

        async def sender():
            for i in range(total_sent):
                await mon.write(f"m{i}")

        sender_task = asyncio.ensure_future(sender())

        delivered = []
        for _ in range(500):
            try:
                lines = await asyncio.wait_for(mon.read_lines(timeout=2.0), timeout=0.001)
                delivered.extend(lines)
            except asyncio.TimeoutError:
                continue

        await sender_task
        deadline = asyncio.get_event_loop().time() + 5.0
        while len(delivered) < total_sent and asyncio.get_event_loop().time() < deadline:
            delivered.extend(await mon.read_lines(timeout=0.2))

        assert delivered == [f"echo:m{i}" for i in range(total_sent)], (
            f"lines lost, reordered or duplicated: {delivered}"
        )

asyncio.run(main())
"#,
        );
    });
}

/// AT-P8 (async): `reset_device(board=None, wait_for_output=True,
/// timeout=5)` against the fake daemon's `/api/reset` stub — returns
/// `True`; the session is usable afterwards (write + read round trip).
#[test]
#[ignore = "embeds CPython; run by the python-facade CI job"]
fn at_p8_async_reset_device_against_stub() {
    init_python();
    let (_port, _env_guard) = start_daemon_and_set_env(DaemonKnobs {
        reset_success: true,
        emit_boot_line: true,
        ..Default::default()
    });
    pyo3::Python::attach(|py| {
        run_snippet(
            py,
            r#"
import asyncio, faulthandler
faulthandler.dump_traceback_later(30, exit=True)
from _native import AsyncSerialMonitor

async def main():
    async with AsyncSerialMonitor(port="TEST_PORT", baud_rate=115200) as mon:
        # Drain the boot line the fake daemon sends right after the
        # initial __aenter__ attach, so it doesn't get mixed into the
        # post-reset assertions below.
        await mon.read_lines(timeout=0.5)

        ok = await mon.reset_device(board=None, wait_for_output=True, timeout=5.0)
        assert ok is True, ok
        n = await mon.write("post-reset")
        assert isinstance(n, int) and n > 0, n
        lines = await mon.read_lines(timeout=3.0)
        assert lines == ["echo:post-reset"], lines

asyncio.run(main())
"#,
        );
    });
}
