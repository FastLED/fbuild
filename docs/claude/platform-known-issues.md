# Platform Known Issues

> Reference doc for Claude Code. Read when debugging platform-specific failures.

## Auto Mode (jobs=None) Bug

**Status**: Open

Using `--jobs` without a value (auto mode) currently fails due to a module reload bug in the daemon.

**Workaround**: Always specify an explicit `--jobs N` value (e.g., `--jobs 4`, `--jobs 2`).

**Future Fix**: Pass compilation queue directly from daemon context instead of using global accessor.

## Windows USB-CDC Serial Port Timeout

**Status**: Fixed in v1.3.36+

On Windows, when esptool or other serial tools interact with ESP32 devices, the process can hang indefinitely if the device or USB-CDC driver is in a stuck state. This occurs because Windows serial port I/O operations can block in kernel space, making them immune to normal process termination signals.

**Root Cause**: When a process is blocked in a Windows kernel-level I/O operation (e.g., `ReadFile()` or `WriteFile()` on a serial port handle), the `subprocess.run(timeout=N)` mechanism cannot interrupt it. The termination signal (SIGTERM/SIGKILL) is not delivered until the I/O operation completes or the driver releases the handle.

**Symptoms**:
- Upload process hangs for 20+ minutes instead of timing out after 120 seconds
- Port remains locked even after killing the daemon
- No error message or timeout exception is raised
- Physical device reset required to unlock the port

**Solution** (implemented in v1.3.36+):
- **Watchdog Timeout**: The ESP32 deployer now uses `run_with_watchdog_timeout()` which monitors process output in real-time
- **Inactivity Detection**: If no output is received for 30 seconds, the process is forcefully terminated
- **Force Kill**: Uses `TerminateProcess()` on Windows for forceful termination when graceful termination fails
- **Better Error Messages**: Provides actionable guidance when timeout occurs

**Implementation**: See `src/fbuild/deploy/deployer_esp32.py:run_with_watchdog_timeout()`

**User Workarounds** (if issue persists):
1. Unplug and replug the USB cable
2. Try a different USB port (preferably USB 2.0, not USB 3.0)
3. Reset the device manually (hold BOOT button, press RESET)
4. Check Device Manager for driver issues (yellow exclamation marks)
5. Update USB-CDC drivers:
   - ESP32-S3 USB-Serial/JTAG: CH343/CH340 drivers
   - Other ESP32: CP210x or FTDI drivers
6. As a last resort, use PlatformIO directly with `--no-fbuild` flag

**Reference**: For detailed technical analysis, see `docs/windows_serial_limitations.md`

## RP2040 Platform Bug

**Status**: Open (pre-existing, not related to fbuild)

**Platform**: Raspberry Pi Pico (RP2040)

**Issue**: Missing ArduinoCore-API dependency. Affects all build modes (serial and parallel).

## STM32 Platform Bug

**Status**: Open (pre-existing, not related to fbuild)

**Platform**: BluePill, other STM32 boards

**Issue**: Missing include paths. Affects all build modes (serial and parallel).
