# fbuild-paths

Single source of truth for all `.fbuild` directory paths, with dev/prod isolation controlled by `FBUILD_DEV_MODE`.

## Key Types and Functions

- `is_dev_mode()` -- Checks `FBUILD_DEV_MODE=1` environment variable
- `get_fbuild_root()` -- Returns `~/.fbuild/dev` or `~/.fbuild/prod` based on mode
- `get_other_fbuild_root()` -- Returns the opposite mode's root (for cross-mode daemon discovery)
- `get_daemon_dir()` / `get_daemon_pid_file()` / `get_daemon_port_file()` / `get_daemon_log_file()` / `get_daemon_status_file()` -- Daemon file paths
- `daemon_ownership` module -- `RootOwnershipGuard` (version-blind, per-cache-root exclusive lock at `root-owner.lock`, held by the daemon for its whole lifetime; `fbuild clean cache` takes it exclusively before deleting the zccache store), `SpawnLockGuard` (`spawn.lock` single-flight election so concurrent CLI spawns don't race), and `OwnerClaim`/`write_owner_claim()`/`read_owner_claim()`/`remove_owner_claim()` (a `root-owner.json` claim recording pid/exe/version/mode/cache_root_key/port, written after the daemon acquires ownership and knows its port; never authoritative on its own — always verified against pid liveness + exe identity before acting on it)
- `get_daemon_port()` -- Port resolution with four-level priority: env var, current mode port file, cross-mode port file, default (8865 dev / 8765 prod)
- `get_daemon_url()` -- Daemon HTTP URL (`http://127.0.0.1:{port}`)
- `get_cache_root()` -- Global cache dir (`FBUILD_CACHE_DIR` override or `~/.fbuild/{mode}/cache`)
- `get_project_build_root()` -- Per-project build-dir root (`FBUILD_BUILD_DIR` override or `<project>/.fbuild/build`). Returns the *root* — does not append `<env>/<profile>`. Most callers should use `BuildLayout` instead.
- `BuildLayout` -- Single source of truth for the env-and-profile-rooted build dir. Encapsulates: explicit per-request `override_root` (highest precedence), `FBUILD_BUILD_DIR` env var, default `<project>/.fbuild/build`, and the `<env>/<profile>` append. Drops the `<env>` segment when `flatten_env` is set or when `project_dir.file_name() == env_name` (the FastLED `.build/pio/<board>/` case, FastLED/fbuild#432).
- `get_platformio_home()` / `get_platformio_package()` -- PlatformIO directory resolution
- `find_firmware()` / `find_firmware_dir()` -- Firmware file discovery across profile subdirs and legacy `.pio/build`

## Modules

- **lib.rs** -- All path functions in a single module
- **daemon_ownership** -- `RootOwnershipGuard`, `SpawnLockGuard`, and `OwnerClaim` (root-owner.lock / spawn.lock / root-owner.json) — see Key Types above
