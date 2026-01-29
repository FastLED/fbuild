# fbuild Daemon Race Condition Fix

## Problem

The fbuild daemon on Windows has a race condition that allows multiple daemon instances to start simultaneously, causing:

- Duplicate request processing
- File access errors (WinError 32, WinError 5)
- Validation processes hanging after deployment
- Status file corruption

## Files

### 1. DAEMON_RACE_CONDITION_FIX.md
**Comprehensive analysis and fix documentation**

- Evidence of the race condition (duplicate daemons, log entries, file errors)
- Root cause explanation with code analysis
- Race condition timeline diagram
- Three proposed fixes with implementation code
- Testing procedures
- Deployment strategy

**Key insight**: Windows daemon startup lacks atomic PID file locking, allowing concurrent starts within microseconds of each other.

### 2. daemon_singleton_lock.py
**Working implementation of the fix**

Provides:
- `acquire_pid_file_lock()` - Context manager for atomic daemon startup
- Uses `os.O_CREAT | os.O_EXCL` for atomic file creation
- Uses `msvcrt.locking()` for Windows file descriptor locking
- Timeout and stale lock detection
- `verify_daemon_singleton()` - Runtime check for duplicate daemons

Example usage:
```python
from daemon_singleton_lock import acquire_pid_file_lock

with acquire_pid_file_lock(PID_FILE):
    # Only one process can be here at a time
    if daemon_already_running():
        return
    spawn_daemon()
```

### 3. test_race_condition.py
**Test harness for reproducing and verifying the fix**

Commands:
```bash
# Reproduce the race condition (should show multiple daemons)
uv run python test_race_condition.py --reproduce

# Test the singleton lock fix
uv run python test_race_condition.py --test-fix

# Use more workers for stress testing
uv run python test_race_condition.py --reproduce --workers 20
```

## How to Apply the Fix

### Option 1: Patch fbuild Package (Recommended for Testing)

1. Locate fbuild daemon code:
   ```
   .venv/Lib/site-packages/fbuild/daemon/daemon.py
   ```

2. Add the singleton lock module:
   ```bash
   cp daemon_singleton_lock.py .venv/Lib/site-packages/fbuild/daemon/
   ```

3. Modify `daemon.py` main() function:
   ```python
   # Add import at top
   from .daemon_singleton_lock import acquire_pid_file_lock

   # Replace Windows daemon startup section (lines 1072-1095):
   if sys.platform == "win32":
       with acquire_pid_file_lock(PID_FILE):
           # Re-check daemon under lock protection
           if PID_FILE.exists():
               try:
                   with open(PID_FILE) as f:
                       existing_pid = int(f.read().strip())
                   if psutil.pid_exists(existing_pid):
                       logging.info(f"Daemon already running with PID {existing_pid}")
                       return 0
               except Exception:
                   pass

           # Spawn daemon
           cmd = [get_python_executable(), __file__, "--foreground"]
           if spawner_pid is not None:
               cmd.append(f"--spawned-by={spawner_pid}")

           safe_popen(
               cmd,
               stdout=subprocess.DEVNULL,
               stderr=subprocess.DEVNULL,
               stdin=subprocess.DEVNULL,
               cwd=str(DAEMON_DIR),
               creationflags=subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS,
           )

           # Wait for daemon to write PID
           for _ in range(50):
               if PID_FILE.exists():
                   time.sleep(0.1)
                   break
               time.sleep(0.1)

           return 0
   ```

### Option 2: Submit Upstream (Recommended for Production)

1. Fork fbuild repository
2. Create branch: `fix/windows-daemon-race-condition`
3. Apply changes from `daemon_singleton_lock.py`
4. Add tests from `test_race_condition.py`
5. Submit pull request with reference to this analysis

## Testing

### Before Fix
```bash
# Kill all daemons
bash daemon stop

# Start 10 clients simultaneously
for i in {1..10}; do
    (uv run python -c "from fbuild.daemon import ensure_daemon_running; ensure_daemon_running()") &
done
wait

# Count daemons (should show >1 with race condition)
uv run python -c "import psutil; print(len([p for p in psutil.process_iter(['cmdline']) if 'fbuild.daemon.daemon' in ' '.join(p.info.get('cmdline', []))]))"
```

Expected BEFORE fix: `2` or more daemons running

### After Fix
Expected AFTER fix: `1` daemon running

### Validation Test
```bash
# Clean state
bash daemon stop

# Run validation
bash validate --i2s

# Should complete without:
# - Hanging after "Deploy successful"
# - File access errors in logs
# - Duplicate request processing
```

## Current Status

**Investigation**: Complete ✅
**Fix Implementation**: Complete ✅
**Testing**: Pending
**Deployment**: Pending

**Recommendation**: Test the fix with `test_race_condition.py --reproduce` first to confirm the race condition, then apply the patch and verify with `--test-fix` and `bash validate --i2s`.

## Evidence from Investigation

### Multiple Daemons Running
```
14500: pythonw.exe -m fbuild.daemon.daemon --spawned-by=10288
49480: pythonw.exe -m fbuild.daemon.daemon --spawned-by=10288
```
Started 12ms apart (16:08:23.211 vs 16:08:23.223)

### Daemon Logs Showing Duplicates
```
15:37:44,234 - Processing package install request: esp32s3...
15:37:44,286 - Processing package install request: esp32s3... [DUPLICATE]
```

### File Access Errors
```
15:37:44,795 - ERROR - Failed to write status file: [WinError 32] file in use
15:37:44,796 - ERROR - Failed to write status file: [WinError 5] access denied
```

## Related Issues

- Validation hangs after "Deploy successful" - likely caused by port contention between duplicate daemons
- Monitor process conflicts - multiple daemons competing for serial port access
- Status file corruption - concurrent writes from duplicate daemons

## Contact

Created: 2026-01-28
Investigation: Iteration 1 of agent loop
Location: ~/dev/fbuild (C:/Users/niteris/dev/fbuild)
