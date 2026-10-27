# fbuild-paths

Single source of truth for all `.fbuild` directory paths, with dev/prod isolation controlled by `FBUILD_DEV_MODE`.

## Key Types and Functions

- `is_dev_mode()` -- Checks `FBUILD_DEV_MODE=1` environment variable
- `get_fbuild_root()` -- Returns `~/.fbuild/dev` or `~/.fbuild/prod` based on mode
- `get_other_fbuild_root()` -- Returns the opposite mode's root (for cross-mode daemon discovery)
- `get_daemon_dir()` / `get_daemon_pid_file()` / `get_daemon_port_file()` / `get_daemon_log_file()` / `get_daemon_status_file()` -- Daemon file paths
- `daemon_ownership` module -- `RootOwnershipGuard` (version-blind, per-cache-root exclusive lock at `root-owner.lock`, held by the daemon for its whole lifetime; `fbuild clean cache` takes it exclusively before deleting the zccache store), `SpawnLockGuard` (`spawn.lock` single-flight election so concurrent CLI spawns don't race), and `OwnerClaim`/`write_owner_claim()`/`read_owner_claim()`/`remove_owner_claim()` (a `root-owner.json` claim recording pid/exe/version/mode/cache_root_key/port, written after the daemon acquires ownership and knows its port; never authoritative on its own — always verified against pid liveness + exe identity before acting on it)
- `get_daemon_port()` -- Port resolution priority: explicit environment override, live endpoint-keyed port file, then a deterministic version/cache-identity default (10000–49151 on Windows to avoid dynamic exclusion blocks; 10000–32767 elsewhere, below the OS ephemeral ranges)
- `get_daemon_url()` -- Daemon HTTP URL (`http://127.0.0.1:{port}`)
- `get_cache_root()` -- Global cache dir (`FBUILD_CACHE_DIR` override or `~/.fbuild/{mode}/cache`)
- `long_paths::initialize()` -- Captures the Windows long-path policy and the requested/effective paths once, before cache or daemon work. When the policy is disabled or cannot be read, selects a writable short cache/scratch root. Automatic roots are direct local-drive children such as `C:\fb-<home-hash>\{dev|prod}`, protected for the current user; foreign-owned directories and links/reparse points are rejected. A suitably short `FBUILD_CACHE_DIR` wins and uses `t`/`b` sidecars within that intentionally configured cache, without requiring access to the drive root or changing the configured root's ACL. A deep override is reported and redirected. The automatic selection is persisted under the normal fbuild root and its private hierarchy verified on reuse, so enabling long paths later reuses downloaded packages. Environment changes take effect in the next process.
- `long_paths::advisory()` -- Plain warning text for CLI/HTTP callers; terminal callers render the warning yellow. Includes the effective fallback paths and `fbuild --fix-win` guidance.
- `long_paths::validate_legacy_path()` -- Rejects paths exceeding legacy Windows limits while the fallback is active, with repair guidance. Shorter roots cannot make arbitrary project or archive depths safe.
- `long_paths::prepare_build_directory()` -- Creates and verifies the full selected automatic build hierarchy before output writes, rejecting foreign ownership and links/reparse points. Intentional cache overrides and project-local output directories retain their existing ACLs.
- `get_project_build_root()` -- Per-project build-dir root (`FBUILD_BUILD_DIR` override or `<project>/.fbuild/build`). Returns the *root* — does not append `<env>/<profile>`. Most callers should use `BuildLayout` instead.
- `BuildLayout` -- Single source of truth for the env-and-profile-rooted build dir. Encapsulates: explicit per-request `override_root` (highest precedence), `FBUILD_BUILD_DIR` env var, default `<project>/.fbuild/build`, and the `<env>/<profile>` append. Drops the `<env>` segment when `flatten_env` is set or when `project_dir.file_name() == env_name` (the FastLED `.build/pio/<board>/` case, FastLED/fbuild#432).
- `get_platformio_home()` / `get_platformio_package()` -- PlatformIO directory resolution
- `find_firmware()` / `find_firmware_dir()` -- Firmware file discovery across profile subdirs and legacy `.pio/build`

## Modules

- **lib.rs** -- All path functions in a single module
- **daemon_ownership** -- `RootOwnershipGuard`, `SpawnLockGuard`, and `OwnerClaim` (root-owner.lock / spawn.lock / root-owner.json) — see Key Types above
- **long_paths** -- Immutable Windows policy snapshot, short-path selection and durable cache retention. Fallback build roots are keyed by normalized project and requested output root so different projects and explicit build overrides cannot overwrite one another.
