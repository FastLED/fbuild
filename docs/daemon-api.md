# fbuild Daemon API Reference

## Overview

The fbuild daemon provides a background service for managing concurrent build, deploy, and monitor operations. This document describes the daemon's internal API, client interface, and message protocol.

**Version:** 1.0
**Last Updated:** 2026-01-08

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                         CLI Layer                            │
│                      (src/fbuild/cli.py)                      │
└─────────────────────┬───────────────────────────────────────┘
                      │
                      │ DaemonClient API
                      ▼
┌─────────────────────────────────────────────────────────────┐
│                     Daemon Client                            │
│                (src/fbuild/daemon/client.py)                  │
│  - submit_deploy_request()                                   │
│  - submit_monitor_request()                                  │
│  - poll_status()                                             │
└─────────────────────┬───────────────────────────────────────┘
                      │
                      │ Request/Status Files
                      ▼
┌─────────────────────────────────────────────────────────────┐
│                       Daemon Server                          │
│                 (src/fbuild/daemon/daemon.py)                 │
│  - Process Requests                                          │
│  - Manage Locks (Port + Project)                            │
│  - Execute Operations                                        │
│  - Update Status Files                                       │
└─────────────────────┬───────────────────────────────────────┘
                      │
                      │ Direct Function Calls
                      ▼
┌─────────────────────────────────────────────────────────────┐
│                   Operation Modules                          │
│  - deployer.py      (Deploy operations)                      │
│  - monitor.py       (Serial monitoring)                      │
│  - orchestrator.py  (Build operations)                       │
└─────────────────────────────────────────────────────────────┘
```

---

## Client API

### Module: `fbuild.daemon.client`

#### Class: `DaemonClient`

The main interface for interacting with the daemon from CLI commands.

##### Constructor

```python
def __init__(self, daemon_dir: Path) -> None:
    """
    Initialize daemon client.

    Args:
        daemon_dir: Directory for daemon state files (typically ~/.fbuild)
    """
```

##### Method: `is_daemon_running`

```python
def is_daemon_running(self) -> bool:
    """
    Check if daemon is currently running.

    Returns:
        True if daemon is running, False otherwise

    Implementation:
        - Checks PID file existence
        - Verifies process is alive (not stale PID)
        - Cross-platform (Windows/Unix)
    """
```

##### Method: `start_daemon`

```python
def start_daemon(self, verbose: bool = False) -> bool:
    """
    Start the daemon process.

    Args:
        verbose: Enable verbose daemon logging

    Returns:
        True if daemon started successfully, False otherwise

    Implementation:
        - Checks if daemon already running
        - Forks new daemon process (Unix) or spawns (Windows)
        - Detaches from parent process
        - Writes PID file
        - Sets up signal handlers

    Platform Notes:
        - Unix: Uses os.fork() for clean daemonization
        - Windows: Uses subprocess.Popen with detach flag
    """
```

##### Method: `stop_daemon`

```python
def stop_daemon(self) -> bool:
    """
    Stop the daemon gracefully.

    Returns:
        True if daemon stopped, False if not running

    Implementation:
        - Creates shutdown signal file
        - Waits for daemon to exit (up to 10 seconds)
        - Verifies PID file removed
        - Forces kill if daemon doesn't exit gracefully
    """
```

##### Method: `restart_daemon`

```python
def restart_daemon(self, verbose: bool = False) -> bool:
    """
    Restart the daemon.

    Args:
        verbose: Enable verbose logging after restart

    Returns:
        True if restart successful

    Implementation:
        - Stops existing daemon
        - Waits for clean exit
        - Starts new daemon instance
    """
```

##### Method: `get_daemon_status`

```python
def get_daemon_status(self) -> tuple[bool, str, int | None]:
    """
    Get detailed daemon status.

    Returns:
        Tuple of (is_running, status_message, pid)

    Example:
        (True, "Daemon is running (PID: 12345, uptime: 1h 23m)", 12345)
        (False, "Daemon is not running", None)
    """
```

---

## Daemon Lifetime Management API

### Overview

The daemon lifetime management API provides tools for discovering, monitoring, and controlling daemon instances. This is particularly useful for:

- Detecting multiple daemon instances (race condition debugging)
- Graceful and force shutdown of daemons
- Cleaning up orphaned daemon processes
- Troubleshooting lock contention issues

### Functions

#### `list_all_daemons()`

```python
def list_all_daemons() -> list[dict[str, Any]]:
    """
    List all running fbuild daemon instances by scanning processes.

    Returns:
        List of dictionaries with daemon info:
        - pid: Process ID
        - cmdline: Command line arguments
        - uptime: Time since process started (seconds)
        - is_primary: True if this matches the PID file (primary daemon)

    Example:
        >>> daemons = list_all_daemons()
        >>> for d in daemons:
        ...     print(f"PID {d['pid']}: uptime {d['uptime']:.1f}s")
    """
```

#### `force_kill_daemon(pid)`

```python
def force_kill_daemon(pid: int) -> bool:
    """
    Force kill a daemon process by PID using SIGKILL.

    This is a forceful termination that doesn't give the daemon
    time to clean up. Use graceful_kill_daemon() when possible.

    Args:
        pid: Process ID to kill

    Returns:
        True if process was killed, False if it didn't exist

    Example:
        >>> if force_kill_daemon(12345):
        ...     print("Daemon killed")
    """
```

#### `graceful_kill_daemon(pid, timeout)`

```python
def graceful_kill_daemon(pid: int, timeout: int = 10) -> bool:
    """
    Gracefully terminate a daemon process with fallback to force kill.

    Sends SIGTERM first to allow cleanup, then SIGKILL if the process
    doesn't exit within the timeout period.

    Args:
        pid: Process ID to terminate
        timeout: Seconds to wait before force killing (default: 10)

    Returns:
        True if process was terminated, False if it didn't exist

    Example:
        >>> if graceful_kill_daemon(12345, timeout=5):
        ...     print("Daemon terminated gracefully")
    """
```

#### `kill_all_daemons(force)`

```python
def kill_all_daemons(force: bool = False) -> int:
    """
    Kill all running daemon instances.

    Useful when multiple daemons have started due to race conditions
    or when the daemon system is in an inconsistent state.

    Args:
        force: If True, use SIGKILL immediately. If False, try SIGTERM first.

    Returns:
        Number of daemons killed

    Example:
        >>> killed = kill_all_daemons(force=False)
        >>> print(f"Killed {killed} daemon(s)")
    """
```

#### `display_daemon_list()`

```python
def display_daemon_list() -> None:
    """
    Display all running daemon instances in a human-readable format.

    Output includes:
    - PID of each daemon
    - Uptime (formatted as seconds, minutes, or hours)
    - PRIMARY/ORPHAN status
    - Warning if multiple daemons detected
    """
```

### CLI Commands

The daemon management commands are accessible via the `fbuild daemon` subcommand:

```bash
# List all daemon instances
fbuild daemon list

# Show lock status
fbuild daemon locks

# Clear stale locks
fbuild daemon clear-locks

# Kill specific daemon by PID (graceful)
fbuild daemon kill --pid 12345

# Force kill specific daemon by PID
fbuild daemon kill --pid 12345 --force

# Kill all daemon instances (graceful)
fbuild daemon kill-all

# Force kill all daemon instances
fbuild daemon kill-all --force
```

### Use Cases

#### 1. Detecting Multiple Daemon Instances

```bash
$ fbuild daemon list

=== Running fbuild Daemons (2 found) ===

  PID 28752: uptime 12.6m (ORPHAN)
  PID 37192: uptime 12.6m (PRIMARY)

⚠️  Multiple daemon instances detected!
   This can cause lock conflicts and unexpected behavior.
   Use 'fbuild daemon kill-all' to clean up, then restart.
```

#### 2. Cleaning Up After a Crash

```bash
# Kill all daemons and restart fresh
$ fbuild daemon kill-all
Gracefully terminated daemon (PID 28752)
Gracefully terminated daemon (PID 37192)
✅ Killed 2 daemon instance(s)

$ fbuild daemon restart
🔗 Starting fbuild daemon...
✅ Daemon started successfully
```

#### 3. Debugging Lock Issues

```bash
# Check current lock status
$ fbuild daemon locks

=== Lock Status ===

Port Locks:
  COM13: HELD by deploy_1234567890 (45.2s)

Project Locks:
  C:\Users\user\project: HELD by deploy_1234567890 (45.2s)

# Clear stale locks if daemon crashed
$ fbuild daemon clear-locks
Signal sent to daemon to clear stale locks
```

#### 4. Force Kill Unresponsive Daemon

```bash
# Graceful kill didn't work, use force
$ fbuild daemon kill --pid 12345 --force
✅ Daemon (PID 12345) terminated
```

---

##### Method: `submit_deploy_request`

```python
def submit_deploy_request(
    self,
    project_dir: Path,
    environment: str,
    port: str | None = None,
    clean_build: bool = False,
    monitor_after: bool = False,
    monitor_timeout: int | None = None,
    halt_on_error: bool = False,
    halt_on_success: bool = False,
    verbose: bool = False,
) -> OperationResult:
    """
    Submit a deploy request to the daemon.

    Args:
        project_dir: Project directory path
        environment: PlatformIO environment name
        port: Serial port (None for auto-detect)
        clean_build: Clean build before deploy
        monitor_after: Start monitor after deploy
        monitor_timeout: Monitor timeout in seconds (for monitor_after)
        halt_on_error: Stop monitor on error pattern
        halt_on_success: Stop monitor on success pattern
        verbose: Enable verbose output

    Returns:
        OperationResult with status, output, and metadata

    Raises:
        DaemonError: If daemon unavailable or request fails

    Implementation:
        - Generates unique request_id
        - Writes request file to daemon directory
        - Polls status file until completion
        - Handles Ctrl-C (prompts for detach/cancel)
        - Cleans up request/status files

    Status Updates:
        - PENDING: Request queued
        - RUNNING: Operation in progress
        - SUCCESS: Completed successfully
        - FAILED: Error occurred
    """
```

##### Method: `submit_monitor_request`

```python
def submit_monitor_request(
    self,
    project_dir: Path,
    environment: str,
    port: str | None = None,
    baud: int | None = None,
    timeout: int | None = None,
    halt_on_error: bool = False,
    halt_on_success: bool = False,
    verbose: bool = False,
) -> OperationResult:
    """
    Submit a monitor request to the daemon.

    Args:
        project_dir: Project directory path
        environment: PlatformIO environment name
        port: Serial port (None for auto-detect)
        baud: Baud rate (None for config default)
        timeout: Monitor timeout in seconds
        halt_on_error: Stop on error pattern
        halt_on_success: Stop on success pattern
        verbose: Enable verbose output

    Returns:
        OperationResult with status and monitor output

    Implementation:
        - Similar to submit_deploy_request
        - Streams serial output to client
        - Handles port detection
        - Respects timeout and halt patterns
    """
```

---

## Message Protocol

### Module: `fbuild.daemon.messages`

#### Class: `DeployRequest`

```python
@dataclass
class DeployRequest:
    """Request to deploy firmware to device."""

    request_id: str              # Unique request identifier
    project_dir: str             # Project directory path
    environment: str             # PlatformIO environment name
    caller_pid: int              # Client process ID

    # Optional parameters
    port: str | None = None      # Serial port (None = auto-detect)
    clean_build: bool = False    # Clean before build
    monitor_after: bool = False  # Start monitor after deploy
    monitor_timeout: int | None = None  # Timeout for monitor_after
    halt_on_error: bool = False  # Monitor halt-on-error flag
    halt_on_success: bool = False  # Monitor halt-on-success flag
    verbose: bool = False        # Verbose logging

    def to_dict(self) -> dict:
        """Convert to dictionary for JSON serialization."""

    @staticmethod
    def from_dict(data: dict) -> 'DeployRequest':
        """Create from dictionary (JSON deserialization)."""
```

#### Class: `MonitorRequest`

```python
@dataclass
class MonitorRequest:
    """Request to monitor serial port."""

    request_id: str              # Unique request identifier
    project_dir: str             # Project directory path
    environment: str             # PlatformIO environment name
    caller_pid: int              # Client process ID

    # Optional parameters
    port: str | None = None      # Serial port
    baud: int | None = None      # Baud rate
    timeout: int | None = None   # Monitor timeout
    halt_on_error: bool = False  # Stop on error pattern
    halt_on_success: bool = False  # Stop on success pattern
    verbose: bool = False        # Verbose logging

    def to_dict(self) -> dict:
        """Convert to dictionary for JSON serialization."""

    @staticmethod
    def from_dict(data: dict) -> 'MonitorRequest':
        """Create from dictionary (JSON deserialization)."""
```

#### Class: `OperationStatus`

```python
@dataclass
class OperationStatus:
    """Status of an ongoing operation."""

    request_id: str              # Request identifier
    status: str                  # PENDING, RUNNING, SUCCESS, FAILED
    message: str                 # Status message
    output: list[str]            # Output lines

    # Optional metadata
    started_at: str | None = None    # ISO timestamp
    completed_at: str | None = None  # ISO timestamp
    error: str | None = None         # Error message if failed
    result_data: dict | None = None  # Operation-specific results

    def to_dict(self) -> dict:
        """Convert to dictionary for JSON serialization."""

    @staticmethod
    def from_dict(data: dict) -> 'OperationStatus':
        """Create from dictionary (JSON deserialization)."""
```

#### Enum: `OperationStatusType`

```python
class OperationStatusType(Enum):
    """Operation status types."""

    PENDING = "PENDING"      # Queued, not started
    RUNNING = "RUNNING"      # Currently executing
    SUCCESS = "SUCCESS"      # Completed successfully
    FAILED = "FAILED"        # Failed with error
```

---

## Daemon Server API

### Module: `fbuild.daemon.daemon`

#### Class: `FbuildDaemon`

The main daemon server that processes requests.

##### Constructor

```python
def __init__(self, daemon_dir: Path, verbose: bool = False) -> None:
    """
    Initialize daemon server.

    Args:
        daemon_dir: Directory for state files
        verbose: Enable verbose logging
    """
```

##### Method: `run`

```python
def run(self) -> None:
    """
    Main daemon loop.

    Implementation:
        - Writes PID file
        - Initializes signal handlers
        - Polls for new requests (every 0.1s)
        - Processes requests in threads
        - Cleans up stale cancel signals (every 60s)
        - Exits after 12h idle timeout
    """
```

##### Internal Method: `process_deploy_request`

```python
def _process_deploy_request(
    self,
    request: DeployRequest,
    status_file: Path
) -> None:
    """
    Process a deploy request.

    Args:
        request: DeployRequest object
        status_file: Path to write status updates

    Implementation:
        - Acquires project lock (non-blocking)
        - Acquires port lock if port specified (non-blocking)
        - Calls deployer.deploy() directly
        - Updates status file with progress
        - Handles monitor-after-deploy if requested
        - Checks for cancel signals periodically
        - Releases locks on completion
    """
```

##### Internal Method: `process_monitor_request`

```python
def _process_monitor_request(
    self,
    request: MonitorRequest,
    status_file: Path
) -> None:
    """
    Process a monitor request.

    Args:
        request: MonitorRequest object
        status_file: Path to write status updates

    Implementation:
        - Acquires port lock (non-blocking)
        - Calls monitor.Monitor() directly
        - Streams output to status file
        - Respects timeout and halt patterns
        - Checks for cancel signals
        - Releases lock on completion
    """
```

##### Internal Method: `should_cancel_operation`

```python
def _should_cancel_operation(self, request_id: str) -> bool:
    """
    Check if operation should be cancelled.

    Args:
        request_id: Request identifier to check

    Returns:
        True if cancel signal file exists

    Implementation:
        - Checks for cancel_{request_id}.signal file
        - Deletes signal file if found
        - Logs cancellation event

    Note:
        Currently only performs cleanup logging.
        Operations complete naturally (blocking operations
        like deploy/monitor cannot be interrupted mid-stream).
    """
```

##### Internal Method: `cleanup_stale_cancel_signals`

```python
def _cleanup_stale_cancel_signals(self) -> None:
    """
    Clean up cancel signal files older than 5 minutes.

    Implementation:
        - Scans daemon_dir for cancel_*.signal files
        - Checks modification time
        - Removes files older than 5 minutes
        - Logs cleanup activity

    Called:
        - Every 60 seconds in main daemon loop
    """
```

---

## File System Protocol

### Directory Structure

```
~/.fbuild/
├── daemon.pid              # Daemon process ID
├── daemon_status.json      # Overall daemon status
├── shutdown.signal         # Shutdown signal file
├── cancel_<request_id>.signal  # Per-request cancel signals
├── deploy_<request_id>.request # Deploy request files
├── monitor_<request_id>.request # Monitor request files
├── deploy_<request_id>.status  # Deploy status files
└── monitor_<request_id>.status # Monitor status files
```

### File Formats

#### `daemon.pid`

Plain text file containing process ID:

```
12345
```

#### `daemon_status.json`

JSON file with daemon health info:

```json
{
  "running": true,
  "pid": 12345,
  "started_at": "2026-01-08T10:30:00Z",
  "last_heartbeat": "2026-01-08T11:45:23Z"
}
```

#### `<operation>_<request_id>.request`

JSON file with request details:

```json
{
  "request_id": "deploy_1704706200123",
  "project_dir": "/path/to/project",
  "environment": "esp32dev",
  "caller_pid": 54321,
  "port": null,
  "clean_build": false,
  "monitor_after": true,
  "monitor_timeout": 10,
  "verbose": false
}
```

#### `<operation>_<request_id>.status`

JSON file with operation status:

```json
{
  "request_id": "deploy_1704706200123",
  "status": "RUNNING",
  "message": "Uploading firmware...",
  "output": [
    "Building project...",
    "Compilation successful",
    "Detecting serial port..."
  ],
  "started_at": "2026-01-08T10:30:05Z",
  "completed_at": null,
  "error": null,
  "result_data": {
    "detected_port": "COM3"
  }
}
```

#### `shutdown.signal`

Empty file that signals daemon to shutdown:

```
(empty file)
```

#### `cancel_<request_id>.signal`

Empty file that signals request cancellation:

```
(empty file)
```

---

## Lock Management

### Lock Types

#### 1. Port Locks

```python
port_locks: dict[str, threading.Lock]
```

- **Purpose:** Prevent concurrent access to same serial port
- **Scope:** Per serial port (e.g., "COM3", "/dev/ttyUSB0")
- **Used By:** deploy (if port specified), monitor
- **Acquisition:** Non-blocking (`acquire(blocking=False)`)
- **Behavior:** If unavailable, operation fails immediately

#### 2. Project Locks

```python
project_locks: dict[str, threading.Lock]
```

- **Purpose:** Prevent concurrent builds of same project
- **Scope:** Per project directory path
- **Used By:** deploy, build
- **Acquisition:** Non-blocking
- **Behavior:** If unavailable, operation fails immediately

#### 3. Operation Lock

```python
operation_lock: threading.Lock
```

- **Purpose:** Track if any operation is active (for shutdown handling)
- **Scope:** Global
- **Used By:** All operations
- **Acquisition:** Non-blocking
- **Behavior:** Daemon refuses shutdown if operations active

### Lock Lifecycle

```python
# Acquire locks
with self.operation_lock:
    project_lock = self.project_locks.setdefault(project_path, threading.Lock())
    if not project_lock.acquire(blocking=False):
        return FAILED("Project is locked by another operation")

    if port:
        port_lock = self.port_locks.setdefault(port, threading.Lock())
        if not port_lock.acquire(blocking=False):
            project_lock.release()
            return FAILED("Port is locked by another operation")

    try:
        # Perform operation
        ...
    finally:
        # Release locks
        if port:
            port_lock.release()
        project_lock.release()
```

---

## Signal Handling

### Client-Side Signals (Ctrl-C)

When user presses Ctrl-C in client:

1. **Signal Handler** catches SIGINT
2. **Prompt User:** "Keep operation running in background? (y/n)"
3. **If Yes (detach):**
   - Client exits
   - Daemon continues operation
   - No cancel signal created
4. **If No (cancel):**
   - Client creates `cancel_<request_id>.signal` file
   - Client exits
   - Daemon detects signal and logs cancellation
   - Operation completes naturally (cleanup)

### Daemon-Side Signals

#### Shutdown Signal

Created by: `fbuild daemon stop`

```python
# Create shutdown signal
shutdown_signal = daemon_dir / "shutdown.signal"
shutdown_signal.touch()
```

Daemon behavior:
- If idle: Exits immediately
- If busy: Refuses to exit (logs warning)
- After operation: Exits on next loop iteration

#### Cancel Signal

Created by: Client on Ctrl-C + 'n'

```python
# Create cancel signal
cancel_signal = daemon_dir / f"cancel_{request_id}.signal"
cancel_signal.touch()
```

Daemon behavior:
- Checks `should_cancel_operation(request_id)` periodically
- If found: Logs cancellation, deletes signal
- Operation completes naturally (currently no mid-stream interrupt)
- Future: Could add cancellation hooks to deployer/monitor

---

## Error Handling

### Client Errors

```python
class DaemonError(Exception):
    """Base daemon error."""
    pass

class DaemonNotRunningError(DaemonError):
    """Daemon is not running."""
    pass

class OperationFailedError(DaemonError):
    """Operation failed."""

    def __init__(self, message: str, output: list[str]):
        self.output = output
        super().__init__(message)
```

### Daemon Errors

Daemon catches and logs all errors internally:

```python
try:
    # Process request
    ...
except Exception as e:
    logger.error(f"Operation failed: {e}")
    update_status(
        status_file,
        OperationStatus(
            request_id=request.request_id,
            status="FAILED",
            message=str(e),
            error=traceback.format_exc(),
            ...
        )
    )
```

---

## Usage Examples

### Example 1: Deploy with Monitor

```python
from pathlib import Path
from fbuild.daemon.client import DaemonClient

# Initialize client
client = DaemonClient(Path.home() / ".fbuild")

# Ensure daemon is running
if not client.is_daemon_running():
    client.start_daemon()

# Submit deploy request
result = client.submit_deploy_request(
    project_dir=Path("tests/esp32c6"),
    environment="esp32c6",
    port=None,  # Auto-detect
    monitor_after=True,
    monitor_timeout=10,
    halt_on_success=True,
)

# Check result
if result.status == "SUCCESS":
    print("Deploy successful!")
    print(f"Detected port: {result.result_data.get('detected_port')}")
else:
    print(f"Deploy failed: {result.message}")
```

### Example 2: Concurrent Deploys

```python
import threading
from pathlib import Path
from fbuild.daemon.client import DaemonClient

client = DaemonClient(Path.home() / ".fbuild")

def deploy_project_1():
    result = client.submit_deploy_request(
        project_dir=Path("tests/esp32c6"),
        environment="esp32c6",
        port="COM3",
        monitor_after=True,
    )
    print(f"Project 1: {result.status}")

def deploy_project_2():
    result = client.submit_deploy_request(
        project_dir=Path("tests/esp32c6-2"),
        environment="esp32c6",
        port="COM4",
        monitor_after=True,
    )
    print(f"Project 2: {result.status}")

# Run concurrently
t1 = threading.Thread(target=deploy_project_1)
t2 = threading.Thread(target=deploy_project_2)

t1.start()
t2.start()

t1.join()
t2.join()
```

### Example 3: Monitor with Patterns

```python
from pathlib import Path
from fbuild.daemon.client import DaemonClient

client = DaemonClient(Path.home() / ".fbuild")

result = client.submit_monitor_request(
    project_dir=Path("tests/esp32c6"),
    environment="esp32c6",
    timeout=60,
    halt_on_error=True,
    halt_on_success=True,
)

if result.status == "SUCCESS":
    print("Monitor completed:")
    for line in result.output:
        print(line)
else:
    print(f"Monitor failed: {result.message}")
```

---

## Performance Characteristics

### Daemon Overhead

- **Startup Time:** < 0.5 seconds
- **Memory Usage:** ~50-100 MB (Python interpreter + dependencies)
- **CPU Usage (idle):** < 1%
- **CPU Usage (active):** Depends on operation (build/deploy/monitor)

### Operation Latency

- **Request Submission:** < 10ms (file write)
- **Lock Acquisition:** < 1ms (in-memory)
- **Status Poll Interval:** 100ms (client-side)
- **Status Update Interval:** Variable (operation-dependent)

### Scalability

- **Concurrent Operations:** Limited by hardware (ports/devices)
- **Request Queue:** Unlimited (file-based)
- **Lock Contention:** Minimal (operations on different projects/ports)

---

## Security Considerations

### File Permissions

- Daemon directory: `~/.fbuild/` (user-only access)
- PID file: Read-only for other users
- Request files: Temporary, deleted after processing
- Status files: Readable by user only

### Process Isolation

- Daemon runs as user process (not root/admin)
- No privilege escalation
- Operations inherit user permissions

### Input Validation

- Project paths validated (must exist)
- Environment names validated (must be in config)
- Port names validated (must be valid serial port)
- Request IDs validated (alphanumeric + underscore)

---

## Troubleshooting

### Daemon Won't Start

**Symptoms:** `fbuild daemon status` shows "not running"

**Diagnosis:**
1. Check PID file: `cat ~/.fbuild/daemon.pid`
2. Check for stale PID: `ps <pid>`
3. Check daemon logs

**Solution:**
- Remove stale PID file: `rm ~/.fbuild/daemon.pid`
- Restart: `fbuild daemon restart`

### Operation Hangs

**Symptoms:** Status shows "RUNNING" indefinitely

**Diagnosis:**
1. Check daemon is alive: `fbuild daemon status`
2. Check for deadlock: Look for multiple locks
3. Check status file: `cat ~/.fbuild/<operation>_<request_id>.status`

**Solution:**
- Press Ctrl-C in client and detach
- Stop daemon: `fbuild daemon stop`
- Remove status file: `rm ~/.fbuild/<operation>_<request_id>.status`

### Lock Errors

**Symptoms:** "Project is locked" or "Port is locked"

**Diagnosis:**
1. Check for concurrent operations: `fbuild daemon status`
2. Check for stale locks (daemon crash)

**Solution:**
- Wait for current operation to complete
- Restart daemon to clear stale locks: `fbuild daemon restart`

---

## Future Enhancements

### Potential Improvements

1. **Real-time Output Streaming:** WebSocket or SSE for live output
2. **Operation Cancellation:** Mid-stream interrupt of operations
3. **Progress Reporting:** Percentage complete for long operations
4. **Operation History:** Log of past operations
5. **Web UI:** Browser-based daemon dashboard
6. **Remote Daemon:** Network-based daemon for remote flashing
7. **Queue Management:** Priority queue for operations
8. **Resource Limits:** Max concurrent operations, memory limits

---

## Appendix: Type Definitions

### OperationResult

```python
@dataclass
class OperationResult:
    """Result of an operation."""

    status: str                  # SUCCESS, FAILED
    message: str                 # Human-readable message
    output: list[str]            # Output lines
    result_data: dict | None     # Operation-specific data
```

### CallerContext

```python
@dataclass
class CallerContext:
    """Context about the calling client."""

    pid: int                     # Client process ID
    start_time: float            # Operation start timestamp
```

---

## Document Revision History

- **v1.0** (2026-01-08): Initial API documentation
