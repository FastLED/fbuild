# Platform Portability

## Windows (MSYS2/Git Bash)

### USB-CDC Serial

ESP32-S3 and similar chips use USB CDC for serial communication. Windows has significant quirks:

- **Re-enumeration delay**: After hard reset, Windows needs 20-30s to re-enumerate the USB device
- **Write blocking**: USB CDC device's TX buffer fills → host driver blocks on `serial.write()`
- **Strategy v5**: Aggressive input buffer draining + rapid write attempts with 50ms timeout
- **DTR/RTS toggling**: Toggle flow control lines before writes to unstick CDC

### Path Handling

- Use forward slashes internally, convert at OS boundary
- `symlink://` in platformio.ini → auto-converted to copies on Windows (symlinks require admin)
- `USERPROFILE` for home dir (not `HOME` which may not exist)

### Process Management

- `pythonw.exe` vs `python.exe` — subprocess safety wrapper handles this
- Never use `taskkill /IM python.exe /F` — kills everything including Claude Code
- Named pipes for IPC if ever needed (currently HTTP)

## Linux / macOS

### Serial Ports

- `/dev/ttyUSB*` or `/dev/ttyACM*` (Linux)
- `/dev/cu.*` or `/dev/tty.*` (macOS)
- 15 retries (vs 30 on Windows) for port re-enumeration
- No USB-CDC write blocking issues

### Permissions

- Serial port access may require `dialout` group on Linux
- No admin needed for symlinks

## Cross-Platform

- Rust `serialport` crate handles OS abstraction
- `tokio` for async I/O on all platforms
- `axum` for HTTP server on all platforms
- Test on all three platforms in CI (`.github/workflows/ci.yml`)
