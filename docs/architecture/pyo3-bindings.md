# PyO3 Python Bindings

## Consumer Contract

FastLED (`~/dev/fastled`) imports these from the `fbuild` Python package:

```python
from fbuild import connect_daemon, Daemon
from fbuild.api import SerialMonitor
from fbuild.daemon import ensure_daemon_running, stop_daemon
```

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
```

### Implementation Strategy

The PyO3 `SerialMonitor` wraps the Rust `SharedSerialManager` via WebSocket:

1. `__enter__`: Connect to daemon WebSocket at `/ws/serial-monitor`, send `attach` message
2. `read_lines`: Poll WebSocket for `data` messages, yield lines
3. `write`: Send `write` message via WebSocket, wait for `write_ack`
4. `write_json_rpc`: Write JSON-RPC request, scan responses for matching `id`
5. `__exit__`: Send `detach`, close WebSocket

Internally uses a tokio runtime (`Runtime::new()`) with `block_on()` to bridge sync Python calls to async Rust.

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
