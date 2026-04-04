# fbuild-paths

Single source of truth for all `.fbuild` directory paths, with dev/prod isolation controlled by `FBUILD_DEV_MODE`.

## Key Types and Functions

- `is_dev_mode()` -- Checks `FBUILD_DEV_MODE=1` environment variable
- `get_fbuild_root()` -- Returns `~/.fbuild/dev` or `~/.fbuild/prod` based on mode
- `get_other_fbuild_root()` -- Returns the opposite mode's root (for cross-mode daemon discovery)
- `get_daemon_dir()` / `get_daemon_pid_file()` / `get_daemon_port_file()` / `get_daemon_log_file()` / `get_daemon_status_file()` -- Daemon file paths
- `get_daemon_port()` -- Port resolution with four-level priority: env var, current mode port file, cross-mode port file, default (8865 dev / 8765 prod)
- `get_daemon_url()` -- Daemon HTTP URL (`http://127.0.0.1:{port}`)
- `get_cache_root()` -- Global cache dir (`FBUILD_CACHE_DIR` override or `~/.fbuild/{mode}/cache`)
- `get_project_build_root()` -- Per-project build dir (`FBUILD_BUILD_DIR` override or `<project>/.fbuild/build`)
- `get_platformio_home()` / `get_platformio_package()` -- PlatformIO directory resolution
- `find_firmware()` / `find_firmware_dir()` -- Firmware file discovery across profile subdirs and legacy `.pio/build`

## Modules

- **lib.rs** -- All path functions in a single module
